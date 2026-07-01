//! Order placement and the call-auction matching algorithm.

use super::{Game, Phase, MAX_PRICE, MAX_QTY};
use crate::model::*;
use std::collections::HashMap;

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

impl Game {
    /// Total value a player has committed to resting bids.
    pub fn committed(&self, player: PlayerId) -> Cents {
        self.committed_bids(player, None)
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
        self.require_trading()?;
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
        // A bid at or above your own offer would cross — you'd be bidding to buy
        // a card you're simultaneously offering to sell for less. Reject it
        // rather than let a nonsensical book rest (the matcher skips self-trades).
        if self.offers.get(&key).is_some_and(|o| price >= o.price) {
            return Err("this bid would cross your own offer on the same card — your bid must be below your offer price".into());
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
        self.require_trading()?;
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
        // Symmetric to place_bid: an offer at or below your own bid would cross.
        if self.bids.get(&key).is_some_and(|o| price <= o.price) {
            return Err("this offer would cross your own bid on the same card — your offer must be above your bid price".into());
        }
        self.offers.insert(key, Order { player, card, qty, price });
        self.record(OrderKind::Offer, OrderAction::Place, player, card, qty, price);
        Ok(())
    }

    /// Close the current auction: match every card's order book, apply the
    /// trades, and advance the round. Unmatched (and partially filled) orders
    /// rest and carry over to the next round.
    pub fn close_round(&mut self) -> Result<RoundResult, String> {
        self.require_trading()?;

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

        if self.round >= self.phase_rounds() {
            match self.phase {
                // Primary done → secondary: the bank withdraws its remaining
                // offers (keeping the unsold cards) and players trade each other.
                Phase::Primary => {
                    self.withdraw_house_offers();
                    self.phase = Phase::Secondary;
                    self.round = 1;
                }
                _ => self.phase = Phase::Finished,
            }
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
}

/// Mid price of a bid and an offer, in cents, rounded to the nearest cent
/// (half rounds up).
fn mid(bid: Cents, offer: Cents) -> Cents {
    let sum = bid + offer;
    // Both prices are non-negative, so this is a plain round-half-up.
    (sum + 1) / 2
}
