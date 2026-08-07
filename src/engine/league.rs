//! League mode: the recurring sealed-bid bank auction and manual inventory.
//!
//! Each cycle the host stocks the house with a card pool, which opens an
//! auction closing at a fixed calendar point (a configured UTC weekday +
//! hour). Players place at most one single-copy bid per card, with no cap on
//! the total money tied up across bids. At the close, cards resolve one at a
//! time — rarest first, then alphabetically. Every player implicitly bids 0
//! on every card, so a card with `qty` copies clears at a uniform price: the
//! `qty`-th highest real bid, or 0 when there are fewer real bids than
//! copies. Real bidders win copies first (ties broken randomly); remaining
//! copies go to random non-bidders for free, one per player (only copies
//! beyond one-per-player carry over). As winners pay, any of their remaining
//! bids that exceed their remaining balance are amended down to it. Winnings
//! go straight into the winner's holdings, and every player then receives the
//! stipend. Players never sell.

use super::{Game, Rng, HOUSE_ID, MAX_PRICE};
use crate::model::*;
use std::collections::HashMap;

/// Cap on a single player's resting league bids, to bound memory and close work.
const MAX_LEAGUE_BIDS: usize = 500;

/// How many rounds of per-card auction history to keep. A weekly league would
/// have to run for a year to reach this, but the bid lists are the one part of
/// the save file that grows with players × cards × rounds, so it is bounded.
const LEAGUE_CLEAR_ROUNDS: u32 = 52;

impl Game {
    /// Whether this game runs in league mode.
    pub fn is_league(&self) -> bool {
        self.config.mode == GameMode::League
    }

    /// Whether a league auction is currently taking bids.
    pub fn league_open(&self) -> bool {
        self.phase == Phase::League && self.round_deadline.is_some()
    }

    fn require_league(&self) -> Result<(), String> {
        if self.phase == Phase::League {
            Ok(())
        } else {
            Err("this game is not in league mode".into())
        }
    }

    /// Total cents a player has committed across their resting league bids.
    pub fn league_committed(&self, player: PlayerId) -> Cents {
        self.league_bids
            .iter()
            .filter(|b| b.player == player)
            .map(|b| b.price)
            .fold(0i64, i64::saturating_add)
    }

    /// The next auction close on the configured series (first day, then every
    /// `period_weeks` weeks), at the close hour in the league timezone — or
    /// `None` once the last auction date has passed.
    pub fn next_league_close_at(&self, now_epoch: u64) -> Option<u64> {
        next_league_close(
            now_epoch,
            self.config.league_first_auction_day,
            self.config.league_last_auction_day,
            self.config.league_close_hour,
            self.config.league_period_weeks,
            self.config.league_tz_offset_mins,
        )
    }

    /// Whether the league's last auction has passed, so no further auction can
    /// be opened (the ladder can still run).
    pub fn league_ended(&self, now_epoch: u64) -> bool {
        self.phase == Phase::League
            && self.round_deadline.is_none()
            && self.next_league_close_at(now_epoch).is_none()
    }

    /// Open the next auction over whatever the house currently holds, closing on
    /// the next scheduled auction date. `round` counts auctions (weeks).
    pub fn open_league_auction(&mut self, now_epoch: u64) -> Result<u64, String> {
        self.require_league()?;
        if self.round_deadline.is_some() {
            return Err("an auction is already open".into());
        }
        if self.house.holdings.is_empty() {
            return Err("the pool is empty — add cards first".into());
        }
        let close = self
            .next_league_close_at(now_epoch)
            .ok_or("the league's last auction date has passed — no more auctions can be opened")?;
        self.round += 1;
        self.round_deadline = Some(close);
        Ok(close)
    }

    /// Place (or re-price) a single-copy bid on a card in the open auction.
    /// Each player may bid for at most one copy of a given card, so bidding on
    /// a card you already bid on replaces the old bid. No single bid may
    /// exceed the player's current balance, but there is no cap on the total
    /// tied up across bids — bids that exceed the remaining balance at
    /// resolution time are amended down to it.
    pub fn place_league_bid(&mut self, player: PlayerId, card: CardId, price: Cents) -> Result<u64, String> {
        self.require_league()?;
        if self.round_deadline.is_none() {
            return Err("the auction is closed — wait for the next pool".into());
        }
        let balance = self.players.get(&player).ok_or("no such player")?.balance;
        if !self.cards.contains_key(&card) {
            return Err("no such card".into());
        }
        if self.house.held(card) == 0 {
            return Err("that card isn't in this auction's pool".into());
        }
        if price < 0 {
            return Err("price cannot be negative".into());
        }
        if price > MAX_PRICE {
            return Err("price is too high".into());
        }
        // No single bid may exceed the current balance (the *total* across
        // bids is uncapped; over-committed bids amend down at the close).
        if price > balance {
            return Err(format!("your balance is {balance} — a single bid can't exceed it"));
        }
        // One bid per (player, card): a new bid replaces the old one.
        self.league_bids.retain(|b| !(b.player == player && b.card == card));
        if self.league_bids.iter().filter(|b| b.player == player).count() >= MAX_LEAGUE_BIDS {
            return Err("too many resting bids — cancel some first".into());
        }
        self.league_bid_seq += 1;
        let id = self.league_bid_seq;
        self.league_bids.push(LeagueBid { id, player, card, price });
        self.record(OrderKind::Bid, OrderAction::Place, player, card, 1, price);
        Ok(id)
    }

    /// Cancel one of your own resting league bids.
    pub fn cancel_league_bid(&mut self, player: PlayerId, bid_id: u64) -> Result<(), String> {
        self.require_league()?;
        let idx = self
            .league_bids
            .iter()
            .position(|b| b.id == bid_id && b.player == player)
            .ok_or("no such bid")?;
        let b = self.league_bids.remove(idx);
        self.record(OrderKind::Bid, OrderAction::Cancel, player, b.card, 0, b.price);
        Ok(())
    }

    /// Close the open auction. Cards resolve one at a time, rarest first and
    /// then alphabetically. Every player implicitly bids 0 on every card, so
    /// each copy always finds a home: with `qty` copies the clearing price is
    /// the `qty`-th highest bid counting those implicit zeros — i.e. the
    /// `qty`-th highest real bid, or **0 whenever there are fewer real bids
    /// than copies**. All copies of a card trade at that one price. The real
    /// bidders win copies first (ties broken randomly via the pre-sort
    /// shuffle); copies beyond the real bids go to random players who didn't
    /// bid on the card, one each, for free. Bids are amended down to the
    /// bidder's remaining balance as it is spent, so a resolution never pushes
    /// anyone into debt. All bids are then cleared, the auction closes (the
    /// host re-opens it by stocking next week's pool), and every player
    /// receives the stipend.
    pub fn close_league_auction(&mut self, rng: &mut Rng) -> Result<RoundResult, String> {
        self.require_league()?;
        if self.round_deadline.is_none() {
            return Err("no auction is open".into());
        }

        // Resolution order: rarity (mythic → common), then name.
        let mut order: Vec<CardId> = self.house.holdings.keys().copied().collect();
        order.sort_by(|&a, &b| {
            let (ca, cb) = (&self.cards[&a], &self.cards[&b]);
            cb.rarity.cmp(&ca.rarity).then_with(|| ca.name.cmp(&cb.name))
        });

        let mut trades: Vec<Trade> = Vec::new();
        let mut clears: Vec<CardClear> = Vec::new();
        for card in order {
            let avail = self.house.held(card);
            if avail == 0 {
                continue;
            }
            let mut bids: Vec<LeagueBid> =
                self.league_bids.iter().filter(|b| b.card == card).cloned().collect();
            // Amend each bid down to the bidder's current balance — money spent
            // on earlier (rarer) cards is no longer available here.
            for b in &mut bids {
                b.price = b.price.min(self.players[&b.player].balance).max(0);
            }
            // Random tie-break: shuffle, then a *stable* sort by price leaves
            // equal-priced bids in shuffled order.
            rng.shuffle(&mut bids);
            bids.sort_by_key(|b| std::cmp::Reverse(b.price));

            // Uniform clearing price: the `avail`-th highest bid, counting the
            // implicit 0 bid every player has — 0 when real bids < copies.
            let real_winners = bids.len().min(avail as usize);
            let price = if bids.len() >= avail as usize {
                bids[avail as usize - 1].price
            } else {
                0
            };

            let card_name = self.cards[&card].name.clone();
            let best_bid = bids.first().map(|b| b.price);
            let mut sold = 0u32;
            let settle = |game: &mut Game, player: PlayerId, bid: Cents, trades: &mut Vec<Trade>| {
                game.adjust_balance(player, -price);
                game.adjust_balance(HOUSE_ID, price);
                game.house.remove_cards(card, 1);
                game.give_cards(player, card, 1);
                trades.push(Trade {
                    card,
                    card_name: card_name.clone(),
                    buyer: player,
                    buyer_name: game.name_of(player),
                    seller: HOUSE_ID,
                    seller_name: game.name_of(HOUSE_ID),
                    qty: 1,
                    price,
                    bid,
                    offer: price,
                });
            };
            let winners: Vec<LeagueBid> = bids.iter().take(real_winners).cloned().collect();
            let mut winner_ids: Vec<PlayerId> = winners.iter().map(|b| b.player).collect();
            for b in &winners {
                settle(self, b.player, b.price, &mut trades);
                sold += 1;
            }
            // Copies beyond the real bids go to random non-bidders (their
            // implicit 0 bids won), one per player, at the price of 0.
            let leftover = avail - sold;
            if leftover > 0 {
                let bidders: std::collections::HashSet<PlayerId> =
                    bids.iter().map(|b| b.player).collect();
                let mut others: Vec<PlayerId> = self
                    .player_order
                    .iter()
                    .copied()
                    .filter(|p| !bidders.contains(p))
                    .collect();
                rng.shuffle(&mut others);
                for &p in others.iter().take(leftover as usize) {
                    settle(self, p, 0, &mut trades);
                    winner_ids.push(p);
                    sold += 1;
                }
            }
            clears.push(CardClear { card, card_name: self.cards[&card].name.clone(), best_bid, best_offer: None, cleared: Some(price), volume: sold });
            // The cover is the first bid that missed out: `bids` is sorted
            // high-to-low and the top `real_winners` of them took the copies.
            // None when every bid won — there was no losing bid.
            self.league_clears.push(LeagueClear {
                round: self.round,
                card,
                card_name: card_name.clone(),
                copies: avail,
                cleared: price,
                high: best_bid,
                cover: bids.get(real_winners).map(|b| b.price),
                bids: bids.iter().map(|b| (b.player, b.price)).collect(),
                winners: winner_ids,
            });
        }

        let result = RoundResult { round: self.round, trades, clears };
        self.history.push(result.clone());
        self.league_bids.clear();
        self.round_deadline = None;
        let oldest_kept = self.round.saturating_sub(LEAGUE_CLEAR_ROUNDS - 1);
        self.league_clears.retain(|c| c.round >= oldest_kept);

        // Stipend, paid immediately after the close.
        let stipend = self.config.weekly_stipend.max(0);
        if stipend > 0 {
            for id in self.player_order.clone() {
                self.adjust_balance(id, stipend);
            }
        }
        Ok(result)
    }

    // ---- manual inventory (league mode) ----------------------------------
    //
    // Players open physical packs the app never sees, so in league mode they
    // may curate their own holdings by hand (purely for planning). In the
    // standard economy holdings back offers, so manual edits stay league-only.

    /// Add cards (from a parsed decklist pool) to a player's own holdings.
    /// Returns how many copies were added.
    pub fn inventory_add(&mut self, player: PlayerId, pool: CardPool) -> Result<usize, String> {
        self.require_league()?;
        if !self.players.contains_key(&player) {
            return Err("no such player".into());
        }
        let list = pool.exact.ok_or("adding cards needs a card list with quantities")?;
        let total: u64 = list.iter().map(|(_, q)| *q as u64).sum();
        if total == 0 {
            return Err("the card list has no cards".into());
        }
        if total > 10_000 {
            return Err("too many cards in the list — reduce the quantities".into());
        }
        let mut by_name: HashMap<String, CardId> =
            self.cards.values().map(|c| (c.name.to_lowercase(), c.id)).collect();
        let mut added = 0usize;
        for (pc, qty) in list {
            if qty == 0 {
                continue;
            }
            let id = self.intern_card(&pc, &mut by_name);
            self.give_cards(player, id, qty);
            added += qty as usize;
        }
        Ok(added)
    }

    /// Remove copies of a card from a player's own holdings.
    pub fn inventory_remove(&mut self, player: PlayerId, card: CardId, qty: u32) -> Result<(), String> {
        self.require_league()?;
        let p = self.players.get(&player).ok_or("no such player")?;
        if qty == 0 {
            return Err("quantity must be at least 1".into());
        }
        if p.held(card) < qty {
            return Err(format!("you only hold {} of that card", p.held(card)));
        }
        self.take_cards(player, card, qty);
        Ok(())
    }
}

/// The epoch second when a given league-timezone day closes at `hour`.
/// `close_at(day) = day·86400 + hour·3600 − offset·60`, so a day+hour expressed
/// in the league timezone maps back to the right UTC instant.
fn league_close_at(day: i64, hour: u32, tz_offset_mins: i32) -> i64 {
    day * 86_400 + (hour % 24) as i64 * 3_600 - tz_offset_mins as i64 * 60
}

/// The epoch day (in the league timezone) an epoch second falls on.
pub fn league_day_of(epoch: u64, tz_offset_mins: i32) -> i64 {
    (epoch as i64 + tz_offset_mins as i64 * 60).div_euclid(86_400)
}

/// The epoch second of the next auction close strictly after `now_epoch`: the
/// earliest day in the series `first_day, first_day + 7·period, …` at `hour` in
/// the league timezone. Returns `None` once that day would exceed `last_day`
/// (`last_day == 0` means the series never ends).
pub fn next_league_close(
    now_epoch: u64,
    first_day: i64,
    last_day: i64,
    hour: u32,
    period_weeks: u32,
    tz_offset_mins: i32,
) -> Option<u64> {
    let step_days = 7 * period_weeks.max(1) as i64;
    let now = now_epoch as i64;
    // The first series day whose close is strictly after `now`.
    let day = if league_close_at(first_day, hour, tz_offset_mins) > now {
        first_day
    } else {
        let deficit = now - league_close_at(first_day, hour, tz_offset_mins);
        let periods = deficit / (step_days * 86_400) + 1; // one past the deficit
        first_day + periods * step_days
    };
    if last_day > 0 && day > last_day {
        return None;
    }
    let epoch = league_close_at(day, hour, tz_offset_mins);
    (epoch >= 0).then_some(epoch as u64)
}

/// Default league schedule days derived at setup from the current time and
/// timezone: `(matchmaking_start, first_auction)` = (the coming Sunday, the
/// Sunday after it). "Coming Sunday" is today when today is already Sunday.
pub fn default_league_days(now_epoch: u64, tz_offset_mins: i32) -> (i64, i64) {
    let today = league_day_of(now_epoch, tz_offset_mins);
    let dow = (today + 4).rem_euclid(7); // 0 = Sunday
    let coming_sunday = today + (7 - dow) % 7;
    (coming_sunday, coming_sunday + 7)
}

#[cfg(test)]
mod tests {
    use super::{default_league_days, league_day_of, next_league_close};

    // 2026-07-19 00:00 UTC is a Sunday.
    const SUN_UTC: u64 = 1_784_419_200 / 86_400 * 86_400;

    #[test]
    fn close_lands_on_the_series_day_and_hour_in_tz() {
        let first = league_day_of(SUN_UTC, 60); // Sunday, in BST
        // With a +60 (BST) offset, 20:00 local = 19:00 UTC.
        let t = next_league_close(SUN_UTC, first, 0, 20, 1, 60).unwrap();
        assert_eq!(t % 86_400, 19 * 3_600, "20:00 BST is 19:00 UTC");
        assert_eq!(league_day_of(t, 60), first, "lands on the first-auction day");
        // At exactly the close instant, the next close is a week out.
        assert_eq!(next_league_close(t, first, 0, 20, 1, 60).unwrap(), t + 7 * 86_400);
        // Bi-weekly cadence steps two weeks between closes.
        let t2 = next_league_close(t, first, 0, 20, 2, 60).unwrap();
        assert_eq!(t2, t + 14 * 86_400);
    }

    #[test]
    fn no_close_after_the_last_auction_day() {
        let first = league_day_of(SUN_UTC, 60);
        let last = first + 7; // two auctions: first and first+7
        // Just after the last auction's close there is nothing more.
        let last_close = next_league_close(SUN_UTC + 20 * 86_400, first, last, 20, 1, 60);
        assert!(last_close.is_none());
        // Before the last date, a close is still available.
        assert!(next_league_close(SUN_UTC, first, last, 20, 1, 60).is_some());
    }

    #[test]
    fn far_past_first_day_jumps_forward_without_looping() {
        // A first day years in the past resolves straight to the next future
        // occurrence (arithmetic, not iteration).
        let first = league_day_of(SUN_UTC, 60) - 700; // 100 weeks ago
        let t = next_league_close(SUN_UTC, first, 0, 20, 1, 60).unwrap();
        assert!(t > SUN_UTC);
        assert_eq!((league_day_of(t, 60) - first) % 7, 0, "still a Sunday-aligned day");
    }

    #[test]
    fn default_days_are_this_sunday_and_next() {
        let (mm, first) = default_league_days(SUN_UTC, 60);
        assert_eq!(mm, league_day_of(SUN_UTC, 60), "today is Sunday → matchmaking today");
        assert_eq!(first, mm + 7, "first auction the following Sunday");
        // Midweek: the coming Sunday, then the one after.
        let wed = SUN_UTC + 3 * 86_400;
        let (mm2, first2) = default_league_days(wed, 60);
        assert_eq!((mm2 + 4) % 7, 0, "matchmaking start is a Sunday");
        assert!(mm2 > league_day_of(wed, 60));
        assert_eq!(first2, mm2 + 7);
    }
}
