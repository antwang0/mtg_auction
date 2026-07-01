//! Delivery settlement: trades create obligations to physically hand over the
//! cards, which the buyer confirms, the host can reverse, and the timer
//! auto-reverses (with a penalty) once overdue.

use super::{Game, HOUSE_ID};
use crate::model::*;
use std::collections::HashMap;

/// How long after a trade a delivery must be completed before it's reversed.
pub const DELIVERY_DEADLINE_SECS: u64 = 2 * 86_400; // 2 days

impl Game {
    /// Turn a closed round's trades into delivery obligations. Fills of the same
    /// (seller, buyer, card) within the round are merged into one delivery due
    /// [`DELIVERY_DEADLINE_SECS`] after `now_epoch`.
    pub fn record_deliveries(&mut self, result: &RoundResult, now_epoch: u64) {
        let mut agg: HashMap<(PlayerId, PlayerId, CardId), (u32, Cents)> = HashMap::new();
        let mut order: Vec<(PlayerId, PlayerId, CardId)> = Vec::new();
        for t in &result.trades {
            let key = (t.seller, t.buyer, t.card);
            let e = agg.entry(key).or_insert_with(|| {
                order.push(key);
                (0, 0)
            });
            e.0 += t.qty;
            e.1 += t.price * t.qty as i64;
        }
        for key in order {
            let (seller, buyer, card) = key;
            let (qty, total) = agg[&key];
            self.delivery_seq += 1;
            self.deliveries.push(Delivery {
                id: self.delivery_seq,
                card,
                card_name: self.cards[&card].name.clone(),
                seller,
                seller_name: self.name_of(seller),
                buyer,
                buyer_name: self.name_of(buyer),
                qty,
                total,
                created: now_epoch,
                deadline: now_epoch + DELIVERY_DEADLINE_SECS,
                status: DeliveryStatus::Pending,
                note: String::new(),
            });
        }
    }

    /// The buyer confirms they received a pending delivery (settling it).
    pub fn mark_delivery_received(&mut self, player: PlayerId, id: u64) -> Result<(), String> {
        let d = self.deliveries.iter_mut().find(|d| d.id == id).ok_or("no such delivery")?;
        if d.buyer != player {
            return Err("only the buyer can mark a delivery received".into());
        }
        if d.status != DeliveryStatus::Pending {
            return Err("this delivery is no longer pending".into());
        }
        d.status = DeliveryStatus::Received;
        Ok(())
    }

    /// Admin: reverse a delivery to fix an error (no penalty).
    pub fn reverse_delivery(&mut self, id: u64) -> Result<(), String> {
        let idx = self.deliveries.iter().position(|d| d.id == id).ok_or("no such delivery")?;
        if self.deliveries[idx].status == DeliveryStatus::Reversed {
            return Err("this delivery is already reversed".into());
        }
        self.apply_reversal(idx, false);
        Ok(())
    }

    /// Reverse every pending delivery whose deadline has passed, charging the
    /// defaulting seller a penalty. Bank deliveries never default. Returns how
    /// many were reversed.
    pub fn expire_overdue_deliveries(&mut self, now_epoch: u64) -> usize {
        let due: Vec<usize> = self
            .deliveries
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.status == DeliveryStatus::Pending && d.seller != HOUSE_ID && now_epoch >= d.deadline
            })
            .map(|(i, _)| i)
            .collect();
        for &idx in &due {
            self.apply_reversal(idx, true);
        }
        due.len()
    }

    /// Undo a delivery's trade: return the cards (best effort — only what the
    /// buyer still holds) and refund the money, optionally charging the seller a
    /// penalty (paid to the bank) for missing the deadline.
    fn apply_reversal(&mut self, idx: usize, charge_penalty: bool) {
        let (card, qty, total, buyer, seller) = {
            let d = &self.deliveries[idx];
            (d.card, d.qty, d.total, d.buyer, d.seller)
        };
        // Return cards buyer → seller; the buyer may have moved some on.
        let returned = self.held_by(buyer, card).min(qty);
        self.take_cards(buyer, card, returned);
        self.give_cards(seller, card, returned);
        // Undo the money.
        self.adjust_balance(buyer, total);
        self.adjust_balance(seller, -total);
        // The defaulting (non-bank) seller pays the bank a penalty.
        if charge_penalty && seller != HOUSE_ID {
            let pct = self.config.delivery_penalty_pct.max(0.0);
            let pen = ((total as f64) * pct / 100.0).ceil() as i64;
            if pen > 0 {
                self.adjust_balance(seller, -pen);
                self.adjust_balance(HOUSE_ID, pen);
            }
        }
        let note = if returned < qty {
            format!("reversal reclaimed only {returned}/{qty} copies (buyer no longer held them)")
        } else {
            String::new()
        };
        let d = &mut self.deliveries[idx];
        d.status = DeliveryStatus::Reversed;
        d.note = note;
    }
}
