//! Game state and the auction matching engine.

use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    fn shuffle<T>(&mut self, v: &mut [T]) {
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
    pub history: Vec<RoundResult>,
    /// Append-only ledger of every bid/offer placed or cancelled.
    pub order_log: Vec<OrderEvent>,
    order_seq: u64,
    /// Unix epoch second when the current round auto-closes, if a timer is set.
    pub round_deadline: Option<u64>,
}

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
        if config.num_packs == 0 || config.pack_size == 0 {
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
        if config.num_packs as u64 * config.pack_size as u64 > 100_000 {
            return Err("too many cards opened — reduce packs or pack size".into());
        }
        if config.starting_money > MAX_PRICE || config.debt_limit > MAX_PRICE {
            return Err("starting money / debt limit is too large".into());
        }
        if pool.is_empty() {
            return Err("the chosen set has no cards".into());
        }

        let mut rng = Rng::new(config.seed);

        // Open packs into a pile of cards drawn from the pool.
        let mut pile: Vec<&PoolCard> = Vec::new();
        for _ in 0..config.num_packs {
            pile.extend(open_pack(config.pack_size, &pool, &mut rng));
        }
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

        // Create players and deal the pile round-robin.
        let mut players: HashMap<PlayerId, Player> = HashMap::new();
        let mut player_order: Vec<PlayerId> = Vec::new();
        for (i, name) in config.player_names.iter().enumerate() {
            let id = (i + 1) as PlayerId;
            players.insert(id, Player {
                id,
                name: name.trim().to_string(),
                balance: config.starting_money,
                holdings: HashMap::new(),
            });
            player_order.push(id);
        }
        for (i, card) in dealt.iter().enumerate() {
            let pid = player_order[i % player_order.len()];
            players.get_mut(&pid).unwrap().add_cards(*card, 1);
        }

        // A fresh, unguessable login token per player (independent of the game
        // seed, which is only for reproducible deals).
        let tokens: HashMap<PlayerId, String> =
            player_order.iter().map(|&id| (id, random_token(id as u64))).collect();
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

    /// Append an entry to the order ledger.
    fn record(&mut self, kind: OrderKind, action: OrderAction, player: PlayerId, card: CardId, qty: u32, price: Cents) {
        self.order_seq += 1;
        self.order_log.push(OrderEvent {
            seq: self.order_seq,
            round: self.round,
            player,
            player_name: self.players[&player].name.clone(),
            kind,
            action,
            card,
            card_name: self.cards[&card].name.clone(),
            qty,
            price,
        });
    }

    /// Resolve a login token to the player it belongs to, if any.
    pub fn player_for_token(&self, token: &str) -> Option<PlayerId> {
        if token.is_empty() {
            return None;
        }
        self.tokens.iter().find(|(_, t)| t.as_str() == token).map(|(&id, _)| id)
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
            .map(|o| o.price * o.qty as i64)
            .sum()
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
            let cleared = trades.iter().filter(|t| t.card == card).next_back().map(|t| t.price);
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
                (((buyer_balance + debt_limit) / bid_price).max(0) as u32).min(bids[bi].qty)
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
                if self.players[&o.player].held(card) == 0 {
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
            let held = self.players[&seller].held(card);
            let qty = affordable.min(offers[oi].qty).min(held);

            // Move cards and money (price per copy is the mid of bid and offer).
            let price = mid(bid_price, offer_price);
            let total = price * qty as i64;
            self.players.get_mut(&bidder).unwrap().balance -= total;
            self.players.get_mut(&bidder).unwrap().add_cards(card, qty);
            self.players.get_mut(&seller).unwrap().balance += total;
            self.players.get_mut(&seller).unwrap().remove_cards(card, qty);

            trades.push(Trade {
                card,
                card_name: card_name.clone(),
                buyer: bidder,
                buyer_name: self.players[&bidder].name.clone(),
                seller,
                seller_name: self.players[&seller].name.clone(),
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
}

/// Mid price of a bid and an offer, in cents, rounded to the nearest cent
/// (half rounds up).
fn mid(bid: Cents, offer: Cents) -> Cents {
    let sum = bid + offer;
    // Both prices are non-negative, so this is a plain round-half-up.
    (sum + 1) / 2
}

/// Generate a 64-hex-character token. Entropy comes from `RandomState`, which
/// is seeded from the OS once per process, so tokens are unguessable and
/// independent of the (reproducible) game seed.
fn random_token(salt: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(64);
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u64(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Pick a random card from the first non-empty tier in preference order, so a
/// slot still fills even if a set is missing that rarity entirely.
fn draw<'a>(rng: &mut Rng, tiers: &[&'a [PoolCard]]) -> Option<&'a PoolCard> {
    for tier in tiers {
        if !tier.is_empty() {
            return Some(&tier[rng.below(tier.len())]);
        }
    }
    None
}

/// Open one pack from the pool: one rare-or-better slot, a few uncommons, the
/// rest commons, falling back across rarities when a tier is empty.
fn open_pack<'a>(size: u32, pool: &'a CardPool, rng: &mut Rng) -> Vec<&'a PoolCard> {
    let size = size as usize;
    let mut pack = Vec::with_capacity(size);

    // Rare slot (roughly 1 in 8 is a mythic, when the set has any).
    if size >= 1 {
        let rare = if !pool.mythics.is_empty() && rng.below(8) == 0 {
            draw(rng, &[&pool.mythics, &pool.rares, &pool.uncommons, &pool.commons])
        } else {
            draw(rng, &[&pool.rares, &pool.mythics, &pool.uncommons, &pool.commons])
        };
        if let Some(c) = rare {
            pack.push(c);
        }
    }
    // Up to 3 uncommons.
    let uncommons = 3.min(size.saturating_sub(1));
    for _ in 0..uncommons {
        if let Some(c) = draw(rng, &[&pool.uncommons, &pool.commons, &pool.rares]) {
            pack.push(c);
        }
    }
    // The rest are commons (falling back to whatever exists).
    while pack.len() < size {
        match draw(rng, &[&pool.commons, &pool.uncommons, &pool.rares, &pool.mythics]) {
            Some(c) => pack.push(c),
            None => break,
        }
    }
    pack
}
