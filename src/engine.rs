//! Game state and the auction matching engine.

use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// serde adapter for the order books: JSON can't key a map on a tuple, so the
/// `(player, card)` maps are stored as a flat list of orders (the key is
/// recoverable from each order's `player`/`card`).
mod order_map {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(m: &HashMap<(PlayerId, CardId), Order>, s: S) -> Result<S::Ok, S::Error> {
        let orders: Vec<&Order> = m.values().collect();
        serde::Serialize::serialize(&orders, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashMap<(PlayerId, CardId), Order>, D::Error> {
        let orders: Vec<Order> = serde::Deserialize::deserialize(d)?;
        Ok(orders.into_iter().map(|o| ((o.player, o.card), o)).collect())
    }
}

/// A small deterministic PRNG (xorshift64*) so games are reproducible from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Avoid the all-zero state, which is a fixed point for xorshift.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform integer in `[0, n)`. `n` must be > 0.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    /// Uniform float in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        // 53 significant bits gives a full-precision uniform double.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// A standard-normal sample (mean 0, stdev 1) via the Box–Muller transform.
    pub fn next_gaussian(&mut self) -> f64 {
        // Avoid u1 == 0, which would make ln() diverge.
        let u1 = (self.next_f64() + f64::MIN_POSITIVE).min(1.0);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
    pub(crate) fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

/// Sanity caps on order inputs, to reject absurd values and keep all the
/// `price * qty` arithmetic comfortably inside `i64`.
pub const MAX_PRICE: Cents = 100_000_000; // $1,000,000.00 per copy
pub const MAX_QTY: u32 = 100_000;

fn validate_amounts(qty: u32, price: Cents) -> Result<(), String> {
    if price < 0 {
        return Err("price cannot be negative".into());
    }
    if price > MAX_PRICE {
        return Err("price is too high".into());
    }
    if qty > MAX_QTY {
        return Err("quantity is too high".into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct Game {
    pub config: Config,
    /// Display name of the set the packs were opened from.
    pub set_name: String,
    pub cards: HashMap<CardId, Card>,
    /// Stable display ordering for the catalog.
    pub card_order: Vec<CardId>,
    pub players: HashMap<PlayerId, Player>,
    pub player_order: Vec<PlayerId>,
    #[serde(with = "order_map")]
    pub bids: HashMap<(PlayerId, CardId), Order>,
    #[serde(with = "order_map")]
    pub offers: HashMap<(PlayerId, CardId), Order>,
    /// Secret login token per player.
    pub tokens: HashMap<PlayerId, String>,
    /// The player who may run admin actions (close rounds, reset). This is the
    /// first player — the host who set the game up.
    pub admin_id: PlayerId,
    /// 1-based index of the auction round currently open.
    pub round: u32,
    pub phase: Phase,
    #[serde(default)]
    pub history: Vec<RoundResult>,
    /// Append-only ledger of every bid/offer placed or cancelled.
    #[serde(default)]
    pub order_log: Vec<OrderEvent>,
    #[serde(default)]
    order_seq: u64,
    /// Unix epoch second when the current round auto-closes, if a timer is set.
    #[serde(default)]
    pub round_deadline: Option<u64>,
    /// ELO ladder played on top of the draft: availability, scheduled matches,
    /// and results. `default` so older saves load cleanly.
    #[serde(default)]
    pub ladder: Ladder,
    /// Cards opened but not dealt to any player; the host can offer these into
    /// the auction and they back late-joining players. The house "sells" via
    /// offers keyed on [`HOUSE_ID`].
    #[serde(default)]
    pub house: House,
    /// Per-player salted password hash (`salt:hex`), set by the player so they
    /// can log in by name. Optional — a player can always use their token.
    #[serde(default)]
    pub passwords: HashMap<PlayerId, String>,
}

/// The reserved player id for the auction house. It is never a real player (real
/// ids start at 1), never appears in `player_order`, and only ever sells.
pub const HOUSE_ID: PlayerId = 0;

impl Default for Game {
    fn default() -> Self {
        Game {
            config: Config::default(),
            set_name: String::new(),
            cards: HashMap::new(),
            card_order: Vec::new(),
            players: HashMap::new(),
            player_order: Vec::new(),
            bids: HashMap::new(),
            offers: HashMap::new(),
            tokens: HashMap::new(),
            admin_id: 0,
            round: 0,
            phase: Phase::Setup,
            history: Vec::new(),
            order_log: Vec::new(),
            order_seq: 0,
            round_deadline: None,
            ladder: Ladder::default(),
            house: House::default(),
            passwords: HashMap::new(),
        }
    }
}

impl Game {
    /// Build a brand-new game from a configuration and a card pool: open packs,
    /// deal cards and money, and open the first auction round.
    pub fn setup(config: Config, pool: CardPool) -> Result<Game, String> {
        if config.player_names.len() < 2 {
            return Err("need at least 2 players".into());
        }
        if config.player_names.iter().any(|n| n.trim().is_empty()) {
            return Err("player names cannot be empty".into());
        }
        if config.rounds == 0 {
            return Err("need at least 1 round".into());
        }
        // A manual pool deals its listed cards directly, so the pack settings
        // don't apply to it; for a set-drafted pool they must be sensible.
        let manual = pool.exact.is_some();
        if !manual && (config.num_packs == 0 || config.pack_size == 0) {
            return Err("need at least 1 pack of at least 1 card".into());
        }
        if config.debt_limit < 0 {
            return Err("debt limit cannot be negative".into());
        }
        if config.starting_money < 0 {
            return Err("starting money cannot be negative".into());
        }
        // Upper bounds so absurd configs can't exhaust memory or overflow money.
        if config.player_names.len() > 64 {
            return Err("too many players (max 64)".into());
        }
        if config.rounds > 10_000 {
            return Err("too many rounds (max 10000)".into());
        }
        if !manual && config.num_packs as u64 * config.pack_size as u64 > 100_000 {
            return Err("too many cards opened — reduce packs or pack size".into());
        }
        if config.starting_money > MAX_PRICE || config.debt_limit > MAX_PRICE {
            return Err("starting money / debt limit is too large".into());
        }
        // Bound the ladder settings too, so they can't drive runaway work or
        // perverse ELO behaviour. `schedule_window_days` is the important one:
        // `auto_schedule` loops O(window × players²) and runs on every
        // availability edit, so an unbounded window is a CPU-DoS.
        if config.schedule_window_days > 60 {
            return Err("schedule window is too long (max 60 days)".into());
        }
        if config.max_games_per_week > 50 {
            return Err("too many games per week (max 50)".into());
        }
        if !(0..=1000).contains(&config.elo_k) {
            return Err("elo K-factor must be between 0 and 1000".into());
        }
        if !(0..=1000).contains(&config.cancel_penalty) {
            return Err("cancel penalty must be between 0 and 1000".into());
        }
        if !(0..=100_000).contains(&config.starting_elo) {
            return Err("starting ELO must be between 0 and 100000".into());
        }
        // Exactly two daily blocks (morning + evening), each a valid UTC hour.
        if config.ladder_block_hours.len() != DAY_BLOCKS.len() {
            return Err("need exactly two ladder block hours (morning and evening)".into());
        }
        if config.ladder_block_hours.iter().any(|&h| h > 23) {
            return Err("ladder block hours must be between 0 and 23 (UTC)".into());
        }
        if pool.is_empty() {
            return Err("the chosen set has no cards".into());
        }

        let mut rng = Rng::new(config.seed);

        // Build the pile of physical cards to deal. A manual pool is its exact
        // multiset (each card repeated by its quantity); otherwise open packs
        // from the rarity buckets.
        let mut pile: Vec<&PoolCard> = match &pool.exact {
            Some(list) => {
                let total: u64 = list.iter().map(|(_, qty)| *qty as u64).sum();
                if total == 0 {
                    return Err("the card list has no cards".into());
                }
                if total > 100_000 {
                    return Err("too many cards in the list — reduce the quantities".into());
                }
                let mut pile = Vec::with_capacity(total as usize);
                for (card, qty) in list {
                    for _ in 0..*qty {
                        pile.push(card);
                    }
                }
                pile
            }
            None => {
                let mut pile: Vec<&PoolCard> = Vec::new();
                for _ in 0..config.num_packs {
                    pile.extend(open_pack(config.pack_size, &pool, &mut rng));
                }
                pile
            }
        };
        rng.shuffle(&mut pile);

        // Intern cards (by name) into a catalog of distinct cards.
        let mut cards: HashMap<CardId, Card> = HashMap::new();
        let mut card_order: Vec<CardId> = Vec::new();
        let mut name_to_id: HashMap<&str, CardId> = HashMap::new();
        let mut next_card_id: CardId = 1;
        let mut dealt: Vec<CardId> = Vec::with_capacity(pile.len());
        for pc in &pile {
            let id = *name_to_id.entry(pc.name.as_str()).or_insert_with(|| {
                let id = next_card_id;
                next_card_id += 1;
                cards.insert(id, Card {
                    id,
                    name: pc.name.clone(),
                    rarity: pc.rarity,
                    image: pc.image.clone(),
                    ref_price: pc.ref_price,
                    type_line: pc.type_line.clone(),
                    cmc: pc.cmc,
                    mana_cost: pc.mana_cost.clone(),
                });
                card_order.push(id);
                id
            });
            dealt.push(id);
        }
        card_order.sort_by(|a, b| cards[a].name.cmp(&cards[b].name));

        // Names double as login identities once a password is set, so they must
        // be distinct (case-insensitively).
        {
            let mut seen: HashSet<String> = HashSet::new();
            for name in &config.player_names {
                if !seen.insert(name.trim().to_lowercase()) {
                    return Err(format!("duplicate player name: {}", name.trim()));
                }
            }
        }

        // Create players (ids start at 1; 0 is the house).
        let mut players: HashMap<PlayerId, Player> = HashMap::new();
        let mut player_order: Vec<PlayerId> = Vec::new();
        for (i, name) in config.player_names.iter().enumerate() {
            let id = (i + 1) as PlayerId;
            players.insert(id, Player {
                id,
                name: name.trim().to_string(),
                balance: config.starting_money,
                holdings: HashMap::new(),
                elo: config.starting_elo,
            });
            player_order.push(id);
        }

        // Deal cards. With per-rarity targets, each player gets up to that many
        // of each rarity and the remainder is held by the house; otherwise the
        // whole pile is dealt round-robin (legacy behaviour, nothing held back).
        let mut house = House::default();
        deal_cards(&dealt, &cards, &config, &player_order, &mut players, &mut house);

        // A fresh, unguessable login token per player.
        let mut tokens: HashMap<PlayerId, String> = HashMap::new();
        for &id in &player_order {
            let t = unique_token(&tokens);
            tokens.insert(id, t);
        }
        let admin_id = player_order[0];

        Ok(Game {
            config,
            set_name: pool.set_name,
            cards,
            card_order,
            players,
            player_order,
            bids: HashMap::new(),
            offers: HashMap::new(),
            tokens,
            admin_id,
            round: 1,
            phase: Phase::Bidding,
            history: Vec::new(),
            order_log: Vec::new(),
            order_seq: 0,
            round_deadline: None,
            ladder: Ladder::default(),
            house,
            passwords: HashMap::new(),
        })
    }

    /// (Re)arm the round timer from the configured `round_seconds`, given the
    /// current epoch second. Clears the deadline when there's no timer or the
    /// game isn't taking orders.
    pub fn arm_timer(&mut self, now_epoch: u64) {
        self.round_deadline = if self.phase == Phase::Bidding && self.config.round_seconds > 0 {
            Some(now_epoch + self.config.round_seconds as u64)
        } else {
            None
        };
    }

    /// Total value a player has committed to resting bids.
    pub fn committed(&self, player: PlayerId) -> Cents {
        self.committed_bids(player, None)
    }

    /// Display name for any holder id, including the house.
    fn name_of(&self, who: PlayerId) -> String {
        if who == HOUSE_ID {
            "House".to_string()
        } else {
            self.players.get(&who).map(|p| p.name.clone()).unwrap_or_default()
        }
    }

    /// Copies of a card held by any holder, including the house.
    fn held_by(&self, who: PlayerId, card: CardId) -> u32 {
        if who == HOUSE_ID {
            self.house.held(card)
        } else {
            self.players.get(&who).map_or(0, |p| p.held(card))
        }
    }

    /// Give cards to any holder, including the house.
    fn give_cards(&mut self, who: PlayerId, card: CardId, qty: u32) {
        if who == HOUSE_ID {
            self.house.add_cards(card, qty);
        } else if let Some(p) = self.players.get_mut(&who) {
            p.add_cards(card, qty);
        }
    }

    /// Take cards from any holder, including the house.
    fn take_cards(&mut self, who: PlayerId, card: CardId, qty: u32) {
        if who == HOUSE_ID {
            self.house.remove_cards(card, qty);
        } else if let Some(p) = self.players.get_mut(&who) {
            p.remove_cards(card, qty);
        }
    }

    /// Adjust any holder's balance, including the house.
    fn adjust_balance(&mut self, who: PlayerId, delta: Cents) {
        if who == HOUSE_ID {
            self.house.balance += delta;
        } else if let Some(p) = self.players.get_mut(&who) {
            p.balance += delta;
        }
    }

    /// Append an entry to the order ledger.
    fn record(&mut self, kind: OrderKind, action: OrderAction, player: PlayerId, card: CardId, qty: u32, price: Cents) {
        self.order_seq += 1;
        self.order_log.push(OrderEvent {
            seq: self.order_seq,
            round: self.round,
            player,
            player_name: self.name_of(player),
            kind,
            action,
            card,
            card_name: self.cards[&card].name.clone(),
            qty,
            price,
        });
    }

    /// Resolve a login token to the player it belongs to, if any. The token is
    /// compared in constant time so a network attacker can't recover it byte by
    /// byte from response timing. (Tokens are still bearer credentials sent over
    /// plain HTTP — see the README's auth note — this just removes the easy leak.)
    pub fn player_for_token(&self, token: &str) -> Option<PlayerId> {
        if token.is_empty() {
            return None;
        }
        self.tokens.iter().find(|(_, t)| ct_eq(t, token)).map(|(&id, _)| id)
    }

    /// Whether a token grants admin rights (it's the host/first player's token).
    pub fn is_admin(&self, token: &str) -> bool {
        self.player_for_token(token) == Some(self.admin_id)
    }

    fn require_bidding(&self) -> Result<(), String> {
        match self.phase {
            Phase::Bidding => Ok(()),
            Phase::Setup => Err("no game in progress".into()),
            Phase::Finished => Err("the game is over".into()),
        }
    }

    /// Sum of a player's resting bid commitments, optionally excluding one card
    /// (used when re-pricing a bid on that card).
    fn committed_bids(&self, player: PlayerId, exclude_card: Option<CardId>) -> Cents {
        self.bids
            .values()
            .filter(|o| o.player == player && Some(o.card) != exclude_card)
            .map(|o| o.price.saturating_mul(o.qty as i64))
            .fold(0i64, i64::saturating_add)
    }

    /// Place or replace a bid. A bid may be for any card. The player's total
    /// resting bids may not commit them past `balance + debt_limit`.
    pub fn place_bid(&mut self, player: PlayerId, card: CardId, qty: u32, price: Cents) -> Result<(), String> {
        self.require_bidding()?;
        let p = self.players.get(&player).ok_or("no such player")?;
        if !self.cards.contains_key(&card) {
            return Err("no such card".into());
        }
        let key = (player, card);
        if qty == 0 {
            if self.bids.remove(&key).is_some() {
                self.record(OrderKind::Bid, OrderAction::Cancel, player, card, 0, price);
            }
            return Ok(());
        }
        validate_amounts(qty, price)?;
        if self.offers.get(&key).is_some_and(|o| o.price == price) {
            return Err("your bid and offer on the same card can't be the same price".into());
        }
        let new_commit = price.checked_mul(qty as i64).ok_or("order is too large")?;
        let others = self.committed_bids(player, Some(card));
        let ceiling = p.balance + self.config.debt_limit;
        if others + new_commit > ceiling {
            return Err(format!(
                "bids would commit {} but only {} is available (balance {} + debt limit {})",
                others + new_commit, ceiling, p.balance, self.config.debt_limit
            ));
        }
        self.bids.insert(key, Order { player, card, qty, price });
        self.record(OrderKind::Bid, OrderAction::Place, player, card, qty, price);
        Ok(())
    }

    /// Place or replace an offer. A player may only offer cards they currently
    /// hold, and cannot offer more copies than they hold.
    pub fn place_offer(&mut self, player: PlayerId, card: CardId, qty: u32, price: Cents) -> Result<(), String> {
        self.require_bidding()?;
        let p = self.players.get(&player).ok_or("no such player")?;
        if !self.cards.contains_key(&card) {
            return Err("no such card".into());
        }
        let key = (player, card);
        if qty == 0 {
            if self.offers.remove(&key).is_some() {
                self.record(OrderKind::Offer, OrderAction::Cancel, player, card, 0, price);
            }
            return Ok(());
        }
        validate_amounts(qty, price)?;
        if qty > p.held(card) {
            return Err(format!("you only hold {} of that card", p.held(card)));
        }
        if self.bids.get(&key).is_some_and(|o| o.price == price) {
            return Err("your bid and offer on the same card can't be the same price".into());
        }
        self.offers.insert(key, Order { player, card, qty, price });
        self.record(OrderKind::Offer, OrderAction::Place, player, card, qty, price);
        Ok(())
    }

    /// Close the current auction: match every card's order book, apply the
    /// trades, and advance the round. Unmatched (and partially filled) orders
    /// rest and carry over to the next round.
    pub fn close_round(&mut self) -> Result<RoundResult, String> {
        self.require_bidding()?;

        let cards: Vec<CardId> = self.card_order.clone();

        // Snapshot the top of each book before matching consumes it, so the
        // round summary can show how close unfilled orders were.
        let mut tob: HashMap<CardId, (Option<Cents>, Option<Cents>)> = HashMap::new();
        for &card in &cards {
            let best_bid = self.bids.values().filter(|o| o.card == card && o.qty > 0).map(|o| o.price).max();
            let best_offer = self.offers.values().filter(|o| o.card == card && o.qty > 0).map(|o| o.price).min();
            tob.insert(card, (best_bid, best_offer));
        }

        let mut trades: Vec<Trade> = Vec::new();
        for &card in &cards {
            let mut card_trades = self.match_card(card);
            trades.append(&mut card_trades);
        }

        // Per-card clearing summary (only cards that had any order).
        let mut clears: Vec<CardClear> = Vec::new();
        for &card in &cards {
            let (best_bid, best_offer) = tob[&card];
            if best_bid.is_none() && best_offer.is_none() {
                continue;
            }
            let volume: u32 = trades.iter().filter(|t| t.card == card).map(|t| t.qty).sum();
            let cleared = trades.iter().rfind(|t| t.card == card).map(|t| t.price);
            clears.push(CardClear {
                card,
                card_name: self.cards[&card].name.clone(),
                best_bid,
                best_offer,
                cleared,
                volume,
            });
        }

        let result = RoundResult { round: self.round, trades, clears };
        self.history.push(result.clone());

        if self.round >= self.config.rounds {
            self.phase = Phase::Finished;
        } else {
            self.round += 1;
        }
        Ok(result)
    }

    /// Run the call-auction match for a single card. Highest bid is paired with
    /// the lowest offer; if they cross, they trade at the mid price. This
    /// repeats until the best bid and best offer no longer cross. A player never
    /// trades with themselves.
    ///
    /// Because orders rest across rounds, the books are re-validated as they
    /// fill: a seller never delivers more copies than they currently hold, and
    /// a buyer is never pushed past their debt limit. Whatever remains of each
    /// order is written back so it carries over to the next round.
    fn match_card(&mut self, card: CardId) -> Vec<Trade> {
        // Highest bid first; ties broken by player id for determinism.
        let mut bids: Vec<Order> = self
            .bids
            .values()
            .filter(|o| o.card == card && o.qty > 0)
            .cloned()
            .collect();
        bids.sort_by(|a, b| b.price.cmp(&a.price).then(a.player.cmp(&b.player)));

        // Lowest offer first.
        let mut offers: Vec<Order> = self
            .offers
            .values()
            .filter(|o| o.card == card && o.qty > 0)
            .cloned()
            .collect();
        offers.sort_by(|a, b| a.price.cmp(&b.price).then(a.player.cmp(&b.player)));

        let card_name = self.cards[&card].name.clone();
        let debt_limit = self.config.debt_limit;
        let mut trades: Vec<Trade> = Vec::new();
        let mut bi = 0usize;

        while bi < bids.len() {
            let bid_price = bids[bi].price;
            let bidder = bids[bi].player;

            // How many copies can the buyer still afford at this price without
            // breaching the debt limit?
            let buyer_balance = self.players[&bidder].balance;
            let affordable: u32 = if bid_price <= 0 {
                bids[bi].qty
            } else {
                // clamp before the cast so a huge balance can't truncate/wrap.
                ((buyer_balance + debt_limit) / bid_price).clamp(0, bids[bi].qty as i64) as u32
            };
            if affordable == 0 {
                bi += 1; // buyer can't afford even one copy of their own bid
                continue;
            }

            // Find the cheapest crossing offer that isn't the bidder's own and
            // whose seller actually still holds copies.
            let mut chosen: Option<usize> = None;
            for (i, o) in offers.iter().enumerate() {
                if o.qty == 0 || o.player == bidder {
                    continue;
                }
                if o.price > bid_price {
                    break; // offers are sorted ascending: nothing else crosses
                }
                if self.held_by(o.player, card) == 0 {
                    continue; // seller sold out elsewhere; this offer is stale
                }
                chosen = Some(i);
                break;
            }
            let oi = match chosen {
                Some(i) => i,
                None => {
                    bi += 1; // this bid cannot be filled
                    continue;
                }
            };

            let offer_price = offers[oi].price;
            let seller = offers[oi].player;
            let held = self.held_by(seller, card);
            let qty = affordable.min(offers[oi].qty).min(held);

            // Move cards and money (price per copy is the mid of bid and offer).
            // The seller may be the house (id 0), which only ever sells.
            let price = mid(bid_price, offer_price);
            let total = price * qty as i64;
            self.adjust_balance(bidder, -total);
            self.give_cards(bidder, card, qty);
            self.adjust_balance(seller, total);
            self.take_cards(seller, card, qty);

            trades.push(Trade {
                card,
                card_name: card_name.clone(),
                buyer: bidder,
                buyer_name: self.name_of(bidder),
                seller,
                seller_name: self.name_of(seller),
                qty,
                price,
                bid: bid_price,
                offer: offer_price,
            });

            bids[bi].qty -= qty;
            offers[oi].qty -= qty;
            if bids[bi].qty == 0 {
                bi += 1;
            }
        }

        // Write the remaining quantities back so unfilled orders rest for the
        // next round; drop anything fully consumed.
        for o in &bids {
            let key = (o.player, card);
            if o.qty > 0 {
                self.bids.entry(key).and_modify(|stored| stored.qty = o.qty);
            } else {
                self.bids.remove(&key);
            }
        }
        for o in &offers {
            let key = (o.player, card);
            if o.qty > 0 {
                self.offers.entry(key).and_modify(|stored| stored.qty = o.qty);
            } else {
                self.offers.remove(&key);
            }
        }

        trades
    }

    // ---- mid-game additions -------------------------------------------------

    /// Intern a pool card into the catalog by name, returning its id (creating a
    /// new `Card` if the name is new). Keeps the display order sorted by name.
    ///
    /// `by_name` is a name→id index over the existing catalog, consulted and
    /// updated in place so a whole batch interns in O(n) rather than rescanning
    /// the catalog (and re-sorting `card_order`) per card.
    fn intern_card(&mut self, pc: &PoolCard, by_name: &mut HashMap<String, CardId>) -> CardId {
        if let Some(&id) = by_name.get(&pc.name) {
            return id;
        }
        // Cards are never removed, so ids stay dense (1..=len) and the next id is
        // simply the current count + 1 — no need to scan the keys for the max.
        let id = self.cards.len() as CardId + 1;
        self.cards.insert(id, Card {
            id,
            name: pc.name.clone(),
            rarity: pc.rarity,
            image: pc.image.clone(),
            ref_price: pc.ref_price,
            type_line: pc.type_line.clone(),
            cmc: pc.cmc,
            mana_cost: pc.mana_cost.clone(),
        });
        // Insert into the already-sorted display order at its place by name,
        // instead of re-sorting the whole vector on every add.
        let pos = self.card_order.partition_point(|c| self.cards[c].name <= pc.name);
        self.card_order.insert(pos, id);
        by_name.insert(pc.name.clone(), id);
        id
    }

    /// Add cards to the house (unallocated) inventory from a manual card list,
    /// after the game has started. Returns how many copies were added.
    pub fn add_cards(&mut self, pool: CardPool) -> Result<usize, String> {
        if self.phase == Phase::Setup {
            return Err("no game in progress".into());
        }
        let list = pool.exact.ok_or("adding cards needs a card list with quantities")?;
        let total: u64 = list.iter().map(|(_, q)| *q as u64).sum();
        if total == 0 {
            return Err("the card list has no cards".into());
        }
        if total > 100_000 {
            return Err("too many cards in the list — reduce the quantities".into());
        }
        // Build a name→id index over the current catalog once, then intern the
        // whole batch against it (kept up to date as new names are added).
        let mut by_name: HashMap<String, CardId> =
            self.cards.values().map(|c| (c.name.clone(), c.id)).collect();
        let mut added = 0usize;
        for (pc, qty) in list {
            if qty == 0 {
                continue;
            }
            let id = self.intern_card(&pc, &mut by_name);
            self.house.add_cards(id, qty);
            added += qty as usize;
        }
        Ok(added)
    }

    /// Add a new player after the game has started, dealing them their initial
    /// allocation from the house per the configured per-rarity deal counts.
    /// Returns the new player's id.
    pub fn add_player(&mut self, name: String) -> Result<PlayerId, String> {
        if self.phase == Phase::Setup {
            return Err("no game in progress".into());
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("player name cannot be empty".into());
        }
        if self.players.values().any(|p| p.name.eq_ignore_ascii_case(&name)) {
            return Err("a player with that name already exists".into());
        }
        if self.players.len() >= 64 {
            return Err("too many players (max 64)".into());
        }
        let id = self.player_order.iter().copied().max().unwrap_or(0) + 1;
        self.players.insert(id, Player {
            id,
            name,
            balance: self.config.starting_money,
            holdings: HashMap::new(),
            elo: self.config.starting_elo,
        });
        self.player_order.push(id);
        let token = unique_token(&self.tokens);
        self.tokens.insert(id, token);
        self.allocate_from_house(id);
        Ok(id)
    }

    /// Move a player's initial per-rarity allocation out of the house to them
    /// (as far as the house can supply).
    fn allocate_from_house(&mut self, player: PlayerId) {
        let targets = [
            (Rarity::Common, self.config.deal_commons),
            (Rarity::Uncommon, self.config.deal_uncommons),
            (Rarity::Rare, self.config.deal_rares),
            (Rarity::Mythic, self.config.deal_mythics),
        ];
        for (rarity, target) in targets {
            let mut given = 0u32;
            let cards: Vec<CardId> =
                self.house.holdings.keys().copied().filter(|c| self.cards[c].rarity == rarity).collect();
            for card in cards {
                while given < target && self.house.held(card) > 0 {
                    self.house.remove_cards(card, 1);
                    self.give_cards(player, card, 1);
                    given += 1;
                }
                if given >= target {
                    break;
                }
            }
        }
    }

    /// List the house's unallocated cards into the auction as offers priced at
    /// each card's reference price plus Gaussian noise (cards without a reference
    /// price are skipped). Replaces any existing house offer on a card. Returns
    /// how many cards were listed.
    pub fn offer_house_cards(&mut self, rng: &mut Rng) -> Result<usize, String> {
        self.require_bidding()?;
        let stdev = self.config.house_offer_stdev_pct;
        let cap = self.config.house_offer_cap_pct;
        let cards: Vec<CardId> = self.house.holdings.keys().copied().collect();
        let mut placed = 0;
        for card in cards {
            let qty = self.house.held(card);
            if qty == 0 {
                continue;
            }
            let Some(ref_price) = self.cards[&card].ref_price else {
                continue; // nothing to price off
            };
            let price = noisy_price(ref_price, stdev, cap, rng);
            self.offers.insert((HOUSE_ID, card), Order { player: HOUSE_ID, card, qty, price });
            self.record(OrderKind::Offer, OrderAction::Place, HOUSE_ID, card, qty, price);
            placed += 1;
        }
        Ok(placed)
    }

    // ---- passwords ----------------------------------------------------------

    /// Set (or change) a player's login password.
    pub fn set_password(&mut self, player: PlayerId, password: &str) -> Result<(), String> {
        if !self.players.contains_key(&player) {
            return Err("no such player".into());
        }
        if password.chars().count() < 4 {
            return Err("password must be at least 4 characters".into());
        }
        if password.len() > 256 {
            return Err("password is too long".into());
        }
        let salt = random_salt();
        let hash = crate::hash::salted_hex(&salt, password);
        self.passwords.insert(player, format!("{salt}:{hash}"));
        Ok(())
    }

    /// Resolve a (name, password) pair to a player, in constant time on the hash.
    pub fn player_for_name_password(&self, name: &str, password: &str) -> Option<PlayerId> {
        let name = name.trim();
        let id = self.players.values().find(|p| p.name.eq_ignore_ascii_case(name)).map(|p| p.id)?;
        let stored = self.passwords.get(&id)?;
        let (salt, hash) = stored.split_once(':')?;
        ct_eq(&crate::hash::salted_hex(salt, password), hash).then_some(id)
    }

    /// Whether a player has a password set.
    pub fn has_password(&self, player: PlayerId) -> bool {
        self.passwords.contains_key(&player)
    }

    // ---- per-player trade history ------------------------------------------

    /// Every trade the player was a party to, paired with the round it cleared.
    pub fn player_trades(&self, player: PlayerId) -> Vec<(u32, Trade)> {
        let mut out = Vec::new();
        for r in &self.history {
            for t in &r.trades {
                if t.buyer == player || t.seller == player {
                    out.push((r.round, t.clone()));
                }
            }
        }
        out
    }
}

/// The reference price plus Gaussian noise: a normal sample with standard
/// deviation `stdev_pct`% of the reference, clamped to ±`cap_pct`% of it, and
/// floored at 1 cent (and capped at [`MAX_PRICE`]).
fn noisy_price(ref_price: Cents, stdev_pct: f64, cap_pct: f64, rng: &mut Rng) -> Cents {
    if ref_price <= 0 {
        return 1;
    }
    let base = ref_price as f64;
    let dev = rng.next_gaussian() * (stdev_pct.max(0.0) / 100.0) * base;
    let cap = (cap_pct.max(0.0) / 100.0) * base;
    let dev = dev.clamp(-cap, cap);
    ((base + dev).round() as i64).clamp(1, MAX_PRICE)
}

/// Compare two strings in constant time (for equal lengths), so token checks
/// don't leak how many leading bytes matched via timing. An unequal length
/// returns early, but our tokens are all the same fixed length.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Mid price of a bid and an offer, in cents, rounded to the nearest cent
/// (half rounds up).
fn mid(bid: Cents, offer: Cents) -> Cents {
    let sum = bid + offer;
    // Both prices are non-negative, so this is a plain round-half-up.
    (sum + 1) / 2
}

/// Number of hex characters in a login token. Short and convenient to share by
/// voice; collisions are resolved at generation time so they stay unique. (This
/// is a deliberately low-security bearer credential — see the README auth note
/// and the password warning in the UI.)
pub const TOKEN_LEN: usize = 4;

/// Generate a short hex login token from the OS CSPRNG. Independent of the
/// (reproducible) game seed.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS random source unavailable");
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in &bytes {
        let _ = write!(out, "{b:02x}");
    }
    out.truncate(TOKEN_LEN);
    out
}

/// A short token that doesn't collide with any already in `existing`.
fn unique_token(existing: &HashMap<PlayerId, String>) -> String {
    let taken: HashSet<&str> = existing.values().map(|s| s.as_str()).collect();
    // 16^4 = 65536 possibilities, far more than the player cap, so this retries
    // only rarely and always terminates in practice.
    loop {
        let t = random_token();
        if !taken.contains(t.as_str()) {
            return t;
        }
    }
}

/// A random hex salt for password hashing.
fn random_salt() -> String {
    let mut bytes = [0u8; 8];
    getrandom::getrandom(&mut bytes).expect("OS random source unavailable");
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(16), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Pick a random card from the first non-empty tier in preference order, so a
/// slot still fills even if a set is missing that rarity entirely. Prefers a
/// card not already in `used` (the names taken by this pack) so a single pack
/// doesn't repeat a card; only when the chosen tier is fully used does it allow
/// a duplicate rather than downgrading the slot's rarity.
fn draw<'a>(rng: &mut Rng, tiers: &[&'a [PoolCard]], used: &HashSet<&str>) -> Option<&'a PoolCard> {
    for tier in tiers {
        if tier.is_empty() {
            continue;
        }
        let fresh: Vec<&'a PoolCard> = tier.iter().filter(|c| !used.contains(c.name.as_str())).collect();
        return Some(if fresh.is_empty() {
            &tier[rng.below(tier.len())]
        } else {
            fresh[rng.below(fresh.len())]
        });
    }
    None
}

/// Open one pack from the pool: one rare-or-better slot, a few uncommons, the
/// rest commons, falling back across rarities when a tier is empty. Avoids
/// repeating a card within the same pack where the pool allows.
fn open_pack<'a>(size: u32, pool: &'a CardPool, rng: &mut Rng) -> Vec<&'a PoolCard> {
    let size = size as usize;
    let mut pack: Vec<&'a PoolCard> = Vec::with_capacity(size);
    let mut used: HashSet<&str> = HashSet::new();
    let take = |pack: &mut Vec<&'a PoolCard>, used: &mut HashSet<&'a str>, c: &'a PoolCard| {
        pack.push(c);
        used.insert(c.name.as_str());
    };

    // Rare slot. Roughly 1 in 8 is a mythic when the set has any; the two
    // branches differ only in which tier is tried first (mythic vs rare), then
    // fall back through the remaining rarities so the slot always fills.
    if size >= 1 {
        let rare = if !pool.mythics.is_empty() && rng.below(8) == 0 {
            draw(rng, &[&pool.mythics, &pool.rares, &pool.uncommons, &pool.commons], &used)
        } else {
            draw(rng, &[&pool.rares, &pool.mythics, &pool.uncommons, &pool.commons], &used)
        };
        if let Some(c) = rare {
            take(&mut pack, &mut used, c);
        }
    }
    // Up to 3 uncommons.
    let uncommons = 3.min(size.saturating_sub(1));
    for _ in 0..uncommons {
        if let Some(c) = draw(rng, &[&pool.uncommons, &pool.commons, &pool.rares], &used) {
            take(&mut pack, &mut used, c);
        }
    }
    // The rest are commons (falling back to whatever exists).
    while pack.len() < size {
        match draw(rng, &[&pool.commons, &pool.uncommons, &pool.rares, &pool.mythics], &used) {
            Some(c) => take(&mut pack, &mut used, c),
            None => break,
        }
    }
    pack
}

/// Deal the pile of opened cards to players. With per-rarity targets set, each
/// player gets up to that many of each rarity (dealt one-per-player per round so
/// shortages fall evenly) and the leftover goes to the house; with all targets
/// zero, the whole pile is dealt round-robin and nothing is held back.
fn deal_cards(
    dealt: &[CardId],
    cards: &HashMap<CardId, Card>,
    config: &Config,
    player_order: &[PlayerId],
    players: &mut HashMap<PlayerId, Player>,
    house: &mut House,
) {
    let targets = [
        (Rarity::Common, config.deal_commons),
        (Rarity::Uncommon, config.deal_uncommons),
        (Rarity::Rare, config.deal_rares),
        (Rarity::Mythic, config.deal_mythics),
    ];

    if targets.iter().all(|&(_, t)| t == 0) {
        for (i, &card) in dealt.iter().enumerate() {
            let pid = player_order[i % player_order.len()];
            players.get_mut(&pid).unwrap().add_cards(card, 1);
        }
        return;
    }

    for (rarity, target) in targets {
        let pile: Vec<CardId> = dealt.iter().copied().filter(|c| cards[c].rarity == rarity).collect();
        let mut idx = 0;
        'rounds: for _ in 0..target {
            for &pid in player_order {
                if idx >= pile.len() {
                    break 'rounds;
                }
                players.get_mut(&pid).unwrap().add_cards(pile[idx], 1);
                idx += 1;
            }
        }
        for &card in &pile[idx..] {
            house.add_cards(card, 1);
        }
    }
}
