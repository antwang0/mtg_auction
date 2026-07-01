//! Game creation — validation, pack opening, dealing — plus mid-game additions
//! (cards, players) and the house's primary-issue offers.

use super::{unique_token, Game, Rng, HOUSE_ID, MAX_PRICE};
use crate::model::*;
use std::collections::{HashMap, HashSet};

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
        if config.primary_rounds == 0 || config.secondary_rounds == 0 {
            return Err("each phase needs at least 1 round".into());
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
        if config.primary_rounds > 10_000 || config.secondary_rounds > 10_000 {
            return Err("too many rounds (max 10000 per phase)".into());
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
                    colors: pc.colors.clone(),
                    color_identity: pc.color_identity.clone(),
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

        let mut game = Game {
            config,
            set_name: pool.set_name,
            cards,
            card_order,
            players,
            player_order,
            tokens,
            admin_id,
            round: 1,
            phase: Phase::Primary,
            house,
            ..Game::default()
        };
        // Primary issue: the bank lists all its leftover (unallocated) cards into
        // the auction so players can acquire them in the primary phase.
        let _ = game.offer_house_cards(&mut rng);
        Ok(game)
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
            colors: pc.colors.clone(),
            color_identity: pc.color_identity.clone(),
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

    /// Cancel every resting house offer (the bank keeps the cards). Used at the
    /// primary→secondary transition so the bank stops selling.
    pub(super) fn withdraw_house_offers(&mut self) {
        let cards: Vec<CardId> =
            self.offers.keys().filter(|(p, _)| *p == HOUSE_ID).map(|(_, c)| *c).collect();
        for card in cards {
            if let Some(o) = self.offers.remove(&(HOUSE_ID, card)) {
                self.record(OrderKind::Offer, OrderAction::Cancel, HOUSE_ID, card, o.qty, o.price);
            }
        }
    }

    /// List the house's unallocated cards into the auction as offers priced at
    /// each card's reference price plus Gaussian noise (cards without a reference
    /// price are skipped). Replaces any existing house offer on a card. Returns
    /// how many cards were listed.
    pub fn offer_house_cards(&mut self, rng: &mut Rng) -> Result<usize, String> {
        if self.phase != Phase::Primary {
            return Err("the bank only issues cards during the primary phase".into());
        }
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
            players.get_mut(&pid).expect("player_order ids exist in players").add_cards(card, 1);
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
                players.get_mut(&pid).expect("player_order ids exist in players").add_cards(pile[idx], 1);
                idx += 1;
            }
        }
        for &card in &pile[idx..] {
            house.add_cards(card, 1);
        }
    }
}
