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
    /// The card's colors as a canonical `WUBRG`-ordered string (empty =
    /// colorless), e.g. `"WU"`. Used for the card-pool picker's colour filter.
    pub colors: String,
}

/// The set of cards a game draws its packs from.
#[derive(Clone, Debug, Default)]
pub struct CardPool {
    pub set_name: String,
    pub commons: Vec<PoolCard>,
    pub uncommons: Vec<PoolCard>,
    pub rares: Vec<PoolCard>,
    pub mythics: Vec<PoolCard>,
    /// A manually-specified pool: the exact cards to deal, each paired with how
    /// many copies of it exist. When `Some`, setup deals this multiset directly
    /// (the per-rarity buckets and pack opening are not used).
    pub exact: Option<Vec<(PoolCard, u32)>>,
}

impl CardPool {
    pub fn total(&self) -> usize {
        self.commons.len() + self.uncommons.len() + self.rares.len() + self.mythics.len()
            + self.exact.as_ref().map_or(0, |e| e.len())
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
            // Rotate a spread of mono-, multi- and colorless cards so the
            // picker's colour filter is exercisable offline too.
            const COLORS: &[&str] = &["W", "U", "B", "R", "G", "WU", "BR", "GW", "WUB", "RG"];
            names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let type_line = TYPES[(i + rarity as usize) % TYPES.len()];
                    let cmc = if type_line.contains("Land") { 0.0 } else { ((n.len() % 6) + 1) as f64 };
                    // Lands and artifacts read as colorless; everything else
                    // cycles through the colour combos.
                    let colors = if type_line.contains("Land") || type_line.contains("Artifact") {
                        String::new()
                    } else {
                        COLORS[(i + rarity as usize) % COLORS.len()].to_string()
                    };
                    PoolCard {
                        name: (*n).to_string(),
                        rarity,
                        image: None,
                        ref_price: Some(ref_price),
                        type_line: Some(type_line.to_string()),
                        cmc: Some(cmc),
                        mana_cost: None,
                        colors,
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
            exact: None,
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
    /// Ladder ELO rating (starts at [`Config::starting_elo`]).
    #[serde(default = "default_elo")]
    pub elo: i64,
}

pub fn default_elo() -> i64 {
    1200
}

/// The auction "house": cards opened but not allocated to any player. The host
/// can offer these into the auction (at a noisy reference price) and they back
/// the cards handed to players who join after the game has started. The house
/// never bids; it only sells, accruing the proceeds in its balance.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct House {
    pub balance: Cents,
    /// card id -> unallocated quantity.
    pub holdings: HashMap<CardId, u32>,
}

impl House {
    pub fn held(&self, card: CardId) -> u32 {
        self.holdings.get(&card).copied().unwrap_or(0)
    }
    pub fn add_cards(&mut self, card: CardId, qty: u32) {
        if qty > 0 {
            *self.holdings.entry(card).or_insert(0) += qty;
        }
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

// ---- ELO ladder -----------------------------------------------------------

/// Availability is expressed in discrete time blocks. Each day is split into
/// these block start hours (UTC); a slot id packs the day (days since the Unix
/// epoch) and the block index together as `slot = day * DAY_BLOCKS.len() + block`.
///
/// Two blocks per day — a morning and an evening slot. The hours are fixed UTC
/// instants rendered in each viewer's local timezone, and the UI labels block 0
/// "Morning" and block 1 "Evening". Adjust these hours to suit the group's
/// timezones; changing the *count* also reshapes slot ids, so prefer doing it
/// before a ladder has availability/matches saved.
pub const DAY_BLOCKS: [u32; 2] = [9, 21];

/// A self-scheduling ELO ladder layered on a game: every match (upcoming,
/// played, or cancelled), plus each player's availability and weekly target.
/// Per-player ELO lives on [`Player`]; standings are derived on demand.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ladder {
    pub matches: Vec<Match>,
    /// Monotonic id source for matches.
    pub next_id: u64,
    /// Player → the (sorted) slot ids they have marked themselves free for.
    #[serde(default)]
    pub availability: HashMap<PlayerId, Vec<i64>>,
    /// Player → how many matches they want auto-scheduled per week.
    #[serde(default)]
    pub games_per_week: HashMap<PlayerId, u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchStatus {
    /// On the calendar for a future slot; may carry an unconfirmed result.
    Scheduled,
    /// Result confirmed and ELO applied.
    Completed,
    /// Called off by one player, who took the ELO penalty.
    Cancelled,
    /// The slot passed without a confirmed result (a no-show). No ELO change;
    /// the pair can be rescheduled.
    Expired,
}

/// A scheduled match between two players at a time slot. Results use the same
/// propose/confirm flow as matches did before; confirming applies the ELO
/// change. A cancelled match records who cancelled and the penalty they took.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    pub id: u64,
    pub a: PlayerId,
    pub a_name: String,
    pub b: PlayerId,
    pub b_name: String,
    /// Slot id (see [`DAY_BLOCKS`]).
    pub slot: i64,
    /// Unix epoch second the slot begins, for client display.
    pub slot_start: u64,
    pub status: MatchStatus,
    /// Game counts: a proposed score while scheduled, the final score once
    /// completed.
    pub a_wins: u32,
    pub b_wins: u32,
    pub draws: u32,
    /// Who entered the current unconfirmed result, if any (the opponent must
    /// confirm before it becomes final).
    #[serde(default)]
    pub proposed_by: Option<PlayerId>,
    /// Who cancelled the match, if it was cancelled.
    #[serde(default)]
    pub cancelled_by: Option<PlayerId>,
    /// ELO change applied to each player on completion (or the penalty to the
    /// canceller on cancellation).
    #[serde(default)]
    pub a_delta: i32,
    #[serde(default)]
    pub b_delta: i32,
}

impl Match {
    pub fn involves(&self, p: PlayerId) -> bool {
        self.a == p || self.b == p
    }
}

/// A player's ladder standing, ranked by ELO.
#[derive(Clone, Debug, Serialize)]
pub struct Standing {
    pub rank: u32,
    pub player: PlayerId,
    pub name: String,
    pub elo: i64,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    /// Completed matches.
    pub played: u32,
    /// Upcoming (still-scheduled) matches.
    pub scheduled: u32,
    pub cancellations: u32,
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

/// Where a game's card pool comes from. The three sources are mutually
/// exclusive — exactly one is used, picked by [`Config::pool_source`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolSource {
    /// The built-in offline sample set.
    #[default]
    Sample,
    /// A Scryfall set code (see [`Config::set`]).
    Scryfall,
    /// A pasted decklist (see [`Config::card_list`]).
    Manual,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub player_names: Vec<String>,
    /// Which of the mutually-exclusive pool sources this game uses.
    #[serde(default)]
    pub pool_source: PoolSource,
    /// Scryfall set code to draft from (e.g. `"dom"`); used when
    /// `pool_source == Scryfall`.
    #[serde(default = "default_set")]
    pub set: String,
    /// A manual card pool — a decklist-style text, one `<qty> <name>` per line;
    /// used when `pool_source == Manual`.
    #[serde(default)]
    pub card_list: String,
    /// How many cards of each rarity each player is dealt at the start. When all
    /// four are 0, the legacy behaviour applies: every opened card is dealt
    /// round-robin and nothing is held back. Otherwise the leftover (unallocated)
    /// cards go to the house.
    #[serde(default)]
    pub deal_commons: u32,
    #[serde(default)]
    pub deal_uncommons: u32,
    #[serde(default)]
    pub deal_rares: u32,
    #[serde(default)]
    pub deal_mythics: u32,
    /// House offer pricing: the price is the card's reference price plus Gaussian
    /// noise whose standard deviation is this percent of the reference price,
    /// with the deviation capped at [`house_offer_cap_pct`](Self::house_offer_cap_pct)
    /// percent of it.
    #[serde(default = "default_house_stdev_pct")]
    pub house_offer_stdev_pct: f64,
    #[serde(default = "default_house_cap_pct")]
    pub house_offer_cap_pct: f64,
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

    // ---- ELO ladder settings ----
    /// ELO every player starts the ladder at.
    #[serde(default = "default_starting_elo")]
    pub starting_elo: i64,
    /// ELO K-factor: the maximum rating swing from a single match.
    #[serde(default = "default_elo_k")]
    pub elo_k: i64,
    /// ELO points a player loses for cancelling a scheduled match.
    #[serde(default = "default_cancel_penalty")]
    pub cancel_penalty: i64,
    /// Hard cap on how many games per week any player may request.
    #[serde(default = "default_max_games")]
    pub max_games_per_week: u32,
    /// How many days ahead the auto-scheduler looks for slots.
    #[serde(default = "default_window_days")]
    pub schedule_window_days: u32,
}

fn default_set() -> String {
    "sample".to_string()
}
fn default_house_stdev_pct() -> f64 {
    10.0
}
fn default_house_cap_pct() -> f64 {
    25.0
}
fn default_starting_elo() -> i64 {
    1200
}
fn default_elo_k() -> i64 {
    32
}
fn default_cancel_penalty() -> i64 {
    16
}
fn default_max_games() -> u32 {
    5
}
fn default_window_days() -> u32 {
    14
}

impl Default for Config {
    fn default() -> Self {
        Config {
            player_names: vec!["Alice".into(), "Bob".into(), "Carol".into(), "Dave".into()],
            pool_source: PoolSource::Sample,
            set: default_set(),
            card_list: String::new(),
            deal_commons: 0,
            deal_uncommons: 0,
            deal_rares: 0,
            deal_mythics: 0,
            house_offer_stdev_pct: default_house_stdev_pct(),
            house_offer_cap_pct: default_house_cap_pct(),
            starting_money: 10_000, // $100.00
            debt_limit: 0,
            rounds: 4,
            num_packs: 4,
            pack_size: 15,
            seed: 42,
            round_seconds: 0,
            starting_elo: default_starting_elo(),
            elo_k: default_elo_k(),
            cancel_penalty: default_cancel_penalty(),
            max_games_per_week: default_max_games(),
            schedule_window_days: default_window_days(),
        }
    }
}
