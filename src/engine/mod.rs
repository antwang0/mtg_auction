//! Game state and the auction matching engine.
//!
//! The `Game` struct and its shared helpers live here; the behaviour is split
//! across submodules: [`setup`] (pack opening, dealing, mid-game additions),
//! [`matching`] (order placement and the call auction), [`deliveries`]
//! (settlement), and [`reports`] (bug reports / feature requests).

mod deliveries;
mod matching;
mod reports;
mod setup;

pub use deliveries::DELIVERY_DEADLINE_SECS;

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
    /// Settlement obligations created by trades (deliver/retrieve cards). See
    /// [`Delivery`].
    #[serde(default)]
    pub deliveries: Vec<Delivery>,
    #[serde(default)]
    delivery_seq: u64,
    /// User-submitted bug reports / feature requests, shown to the host. Kept
    /// across game resets (they're feedback about the app, not the game).
    #[serde(default)]
    pub reports: Vec<Report>,
    #[serde(default)]
    report_seq: u64,
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
            deliveries: Vec::new(),
            delivery_seq: 0,
            reports: Vec::new(),
            report_seq: 0,
        }
    }
}

impl Game {
    /// The auto-close timer (seconds) for the phase currently open, or 0 if that
    /// phase closes only on a manual host action.
    pub fn round_seconds(&self) -> u32 {
        match self.phase {
            Phase::Primary => self.config.primary_round_seconds,
            Phase::Secondary => self.config.secondary_round_seconds,
            _ => 0,
        }
    }

    /// How many rounds the phase currently open runs in total.
    pub fn phase_rounds(&self) -> u32 {
        match self.phase {
            Phase::Primary => self.config.primary_rounds,
            Phase::Secondary => self.config.secondary_rounds,
            _ => 0,
        }
    }

    /// (Re)arm the round timer from the current phase's configured timer, given
    /// the current epoch second. Clears the deadline when there's no timer or the
    /// game isn't taking orders.
    pub fn arm_timer(&mut self, now_epoch: u64) {
        let secs = self.round_seconds();
        self.round_deadline = if secs > 0 { Some(now_epoch + secs as u64) } else { None };
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

    fn require_trading(&self) -> Result<(), String> {
        match self.phase {
            Phase::Primary | Phase::Secondary => Ok(()),
            Phase::Setup => Err("no game in progress".into()),
            Phase::Finished => Err("the game is over".into()),
        }
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
