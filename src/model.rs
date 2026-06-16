//! Domain types for the D&D draft auction game.
//!
//! All money and prices are integer **US cents**. A trade price that falls on
//! a half-cent is rounded to the nearest cent (half rounds up).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type PlayerId = u32;
pub type CardId = u32;
/// Money, in whole US cents (e.g. `1234` = $12.34).
pub type Cents = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Mythic,
}

/// A distinct tradeable card. Copies of the same card are fungible: a player
/// holds a *quantity* of a card, and every copy shares one order book.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub name: String,
    pub rarity: Rarity,
    /// Card image URL (from Scryfall); `None` for the offline sample set.
    pub image: Option<String>,
    /// Reference market price in cents (Scryfall's TCGplayer-derived `usd`).
    pub ref_price: Option<Cents>,
    /// Full type line, e.g. "Legendary Creature — Elf Druid".
    pub type_line: Option<String>,
    /// Mana value (converted mana cost).
    pub cmc: Option<f64>,
    /// Mana cost string, e.g. "{2}{G}".
    pub mana_cost: Option<String>,
}

/// One card available to be opened in a pack, before it's interned into the
/// game's catalog. The card pool is grouped by rarity so packs can be built
/// with the usual rare/uncommon/common slots.
#[derive(Clone, Debug)]
pub struct PoolCard {
    pub name: String,
    pub rarity: Rarity,
    pub image: Option<String>,
    pub ref_price: Option<Cents>,
    pub type_line: Option<String>,
    pub cmc: Option<f64>,
    pub mana_cost: Option<String>,
}

/// The set of cards a game draws its packs from.
#[derive(Clone, Debug, Default)]
pub struct CardPool {
    pub set_name: String,
    pub commons: Vec<PoolCard>,
    pub uncommons: Vec<PoolCard>,
    pub rares: Vec<PoolCard>,
    pub mythics: Vec<PoolCard>,
}

impl CardPool {
    pub fn total(&self) -> usize {
        self.commons.len() + self.uncommons.len() + self.rares.len() + self.mythics.len()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// A built-in, network-free pool of flavorful fantasy cards (no images).
    /// Used for the `"sample"` set code and in tests.
    pub fn sample() -> CardPool {
        fn cards(names: &[&str], rarity: Rarity) -> Vec<PoolCard> {
            // Give the offline set plausible reference prices by rarity so the
            // market grid is meaningful without a network.
            let ref_price = match rarity {
                Rarity::Common => 10,    // $0.10
                Rarity::Uncommon => 25,  // $0.25
                Rarity::Rare => 150,     // $1.50
                Rarity::Mythic => 600,   // $6.00
            };
            // Rotate types and derive a mana value so type/MV sorting and
            // filtering work offline too.
            const TYPES: &[&str] = &[
                "Creature — Warrior", "Instant", "Sorcery", "Artifact",
                "Enchantment", "Creature — Beast", "Land", "Creature — Wizard",
            ];
            names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let type_line = TYPES[(i + rarity as usize) % TYPES.len()];
                    let cmc = if type_line.contains("Land") { 0.0 } else { ((n.len() % 6) + 1) as f64 };
                    PoolCard {
                        name: (*n).to_string(),
                        rarity,
                        image: None,
                        ref_price: Some(ref_price),
                        type_line: Some(type_line.to_string()),
                        cmc: Some(cmc),
                        mana_cost: None,
                    }
                })
                .collect()
        }
        CardPool {
            set_name: "Sample Set".to_string(),
            commons: cards(SAMPLE_COMMONS, Rarity::Common),
            uncommons: cards(SAMPLE_UNCOMMONS, Rarity::Uncommon),
            rares: cards(SAMPLE_RARES, Rarity::Rare),
            mythics: cards(SAMPLE_MYTHICS, Rarity::Mythic),
        }
    }
}

const SAMPLE_COMMONS: &[&str] = &[
    "Goblin Skirmisher", "Healing Salve", "Town Guard", "Wandering Scout",
    "Stone Golem", "Forest Sprite", "Torch Bearer", "River Serpent",
    "Dwarven Miner", "Acolyte of Dawn", "Bog Rat", "Gust of Wind",
    "Sunlit Field", "Cave Spider", "Tidepool Imp", "Brushland Boar",
];
const SAMPLE_UNCOMMONS: &[&str] = &[
    "Knight of the Vale", "Pyromancer's Gift", "Verdant Growth", "Soul Warden",
    "Stormcaller", "Grave Robber", "Mistform Drake", "Ironbark Treant",
    "Veteran Duelist", "Tidal Wave", "Shadow Stalker", "Beast Tamer",
];
const SAMPLE_RARES: &[&str] = &[
    "Archmage Vesper", "Crown of Command", "Dragonlord Aether", "Necropolis Gate",
    "Phoenix of the Pyre", "Throne of Ages", "Worldsoul Colossus",
];
const SAMPLE_MYTHICS: &[&str] = &[
    "Avatar of Eternity", "The Sundering Blade", "Nyx, the Endless",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub balance: Cents,
    /// card id -> quantity held (only positive quantities are kept)
    pub holdings: HashMap<CardId, u32>,
}

impl Player {
    pub fn held(&self, card: CardId) -> u32 {
        self.holdings.get(&card).copied().unwrap_or(0)
    }

    pub fn add_cards(&mut self, card: CardId, qty: u32) {
        if qty == 0 {
            return;
        }
        *self.holdings.entry(card).or_insert(0) += qty;
    }

    pub fn remove_cards(&mut self, card: CardId, qty: u32) {
        if let Some(h) = self.holdings.get_mut(&card) {
            *h = h.saturating_sub(qty);
            if *h == 0 {
                self.holdings.remove(&card);
            }
        }
    }
}

/// A single resting order. A player keeps at most one bid and one offer per
/// card; re-submitting replaces the previous one, and a quantity of 0 cancels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    pub player: PlayerId,
    pub card: CardId,
    pub qty: u32,
    /// For a bid: the maximum the player will pay per copy.
    /// For an offer: the minimum the player will accept per copy.
    pub price: Cents,
}

/// A trade produced when a bid and an offer cross during an auction close.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub card: CardId,
    pub card_name: String,
    pub buyer: PlayerId,
    pub buyer_name: String,
    pub seller: PlayerId,
    pub seller_name: String,
    pub qty: u32,
    /// Mid price actually paid per copy.
    pub price: Cents,
    pub bid: Cents,
    pub offer: Cents,
}

/// Per-card clearing summary at a round close: the top of book before matching
/// (so players see how close they were) and what actually traded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardClear {
    pub card: CardId,
    pub card_name: String,
    pub best_bid: Option<Cents>,
    pub best_offer: Option<Cents>,
    /// Price the last trade for this card cleared at, if any traded.
    pub cleared: Option<Cents>,
    pub volume: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoundResult {
    pub round: u32,
    pub trades: Vec<Trade>,
    #[serde(default)]
    pub clears: Vec<CardClear>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderKind {
    Bid,
    Offer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderAction {
    Place,
    Cancel,
}

/// One entry in the order ledger: a player placing, re-pricing, or cancelling a
/// bid or offer. Every order action a player takes is recorded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderEvent {
    pub seq: u64,
    pub round: u32,
    pub player: PlayerId,
    pub player_name: String,
    pub kind: OrderKind,
    pub action: OrderAction,
    pub card: CardId,
    pub card_name: String,
    pub qty: u32,
    pub price: Cents,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// No game configured yet.
    Setup,
    /// An auction round is open for orders.
    Bidding,
    /// All rounds have closed.
    Finished,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub player_names: Vec<String>,
    /// Scryfall set code to draft from (e.g. `"dom"`), or `"sample"` for the
    /// built-in offline pool.
    #[serde(default = "default_set")]
    pub set: String,
    pub starting_money: Cents,
    /// How far below zero a balance is allowed to go. Total resting bids may
    /// not commit a player past `balance + debt_limit`.
    pub debt_limit: Cents,
    pub rounds: u32,
    pub num_packs: u32,
    pub pack_size: u32,
    /// PRNG seed so a game can be reproduced.
    pub seed: u64,
    /// Seconds before a round auto-closes. `0` means rounds only close when the
    /// host closes them manually.
    #[serde(default)]
    pub round_seconds: u32,
}

fn default_set() -> String {
    "sample".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            player_names: vec!["Alice".into(), "Bob".into(), "Carol".into(), "Dave".into()],
            set: default_set(),
            starting_money: 10_000, // $100.00
            debt_limit: 0,
            rounds: 4,
            num_packs: 4,
            pack_size: 15,
            seed: 42,
            round_seconds: 0,
        }
    }
}
