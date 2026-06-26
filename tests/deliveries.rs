//! Tests for the delivery/settlement subsystem: obligations created by trades,
//! buyer confirmation, deadline reversal with a penalty to the bank, admin
//! reversal without a penalty, and best-effort card reclaim.

use mtg_auction::engine::{Game, DELIVERY_DEADLINE_SECS, HOUSE_ID};
use mtg_auction::model::{CardPool, Config, DeliveryStatus};

const T0: u64 = 1_700_000_000;

fn base() -> Config {
    Config {
        player_names: vec!["A".into(), "B".into()],
        set: "sample".into(),
        starting_money: 1000,
        debt_limit: 0,
        primary_rounds: 1,
        secondary_rounds: 1,
        num_packs: 1,
        pack_size: 6,
        seed: 3,
        delivery_penalty_pct: 10.0,
        ..Config::default()
    }
}

/// A game where player 2 (seller) holds 5 copies of card 1 and both players have
/// 1000 cents; player 1 (buyer) bids and player 2 offers so a trade clears.
fn traded_game() -> Game {
    let mut g = Game::setup(base(), CardPool::sample()).unwrap();
    for p in g.players.values_mut() {
        p.holdings.clear();
        p.balance = 1000;
    }
    let card = g.card_order[0];
    g.players.get_mut(&2).unwrap().add_cards(card, 5);
    g.place_bid(1, card, 2, 100).unwrap();
    g.place_offer(2, card, 2, 80).unwrap();
    let result = g.close_round().unwrap();
    g.record_deliveries(&result, T0);
    g
}

fn only(g: &Game) -> &mtg_auction::model::Delivery {
    assert_eq!(g.deliveries.len(), 1, "exactly one delivery expected");
    &g.deliveries[0]
}

#[test]
fn a_trade_creates_a_pending_delivery() {
    let g = traded_game();
    let d = only(&g);
    assert_eq!((d.seller, d.buyer, d.qty), (2, 1, 2));
    assert_eq!(d.total, 180, "2 copies at the mid price of 90");
    assert_eq!(d.status, DeliveryStatus::Pending);
    assert_eq!(d.deadline, T0 + DELIVERY_DEADLINE_SECS);
}

#[test]
fn buyer_marks_received_and_only_the_buyer_may() {
    let mut g = traded_game();
    let id = only(&g).id;
    assert!(g.mark_delivery_received(2, id).is_err(), "the seller can't confirm");
    g.mark_delivery_received(1, id).unwrap();
    assert_eq!(g.deliveries[0].status, DeliveryStatus::Received);
    assert!(g.mark_delivery_received(1, id).is_err(), "already settled");
}

#[test]
fn missing_the_deadline_reverses_the_trade_and_penalises_the_seller() {
    let mut g = traded_game();
    // Just before the deadline: nothing happens.
    assert_eq!(g.expire_overdue_deliveries(T0 + DELIVERY_DEADLINE_SECS - 1), 0);
    assert_eq!(g.deliveries[0].status, DeliveryStatus::Pending);

    let house_before = g.house.balance;
    assert_eq!(g.expire_overdue_deliveries(T0 + DELIVERY_DEADLINE_SECS), 1);

    // Trade undone: cards back to the seller, money refunded to the buyer.
    let card = g.card_order[0];
    assert_eq!(g.players[&1].held(card), 0);
    assert_eq!(g.players[&2].held(card), 5);
    assert_eq!(g.players[&1].balance, 1000, "buyer fully refunded");
    // Seller loses the proceeds and a 10% penalty (ceil(180 * 0.10) = 18).
    assert_eq!(g.players[&2].balance, 1000 - 18);
    assert_eq!(g.house.balance, house_before + 18, "penalty paid to the bank");
    assert_eq!(g.deliveries[0].status, DeliveryStatus::Reversed);
}

#[test]
fn admin_reversal_charges_no_penalty() {
    let mut g = traded_game();
    let id = only(&g).id;
    let house_before = g.house.balance;
    g.reverse_delivery(id).unwrap();

    let card = g.card_order[0];
    assert_eq!(g.players[&2].held(card), 5);
    assert_eq!(g.players[&1].balance, 1000);
    assert_eq!(g.players[&2].balance, 1000, "no penalty on an admin fix");
    assert_eq!(g.house.balance, house_before);
    assert_eq!(g.deliveries[0].status, DeliveryStatus::Reversed);
    assert!(g.reverse_delivery(id).is_err(), "can't reverse twice");
}

#[test]
fn the_bank_never_defaults() {
    // A fresh game leaves leftovers in the bank and lists them; a player buys.
    let mut cfg = base();
    cfg.num_packs = 4;
    cfg.deal_commons = 1;
    cfg.starting_money = 1_000_000;
    let mut g = Game::setup(cfg, CardPool::sample()).unwrap();
    let card = *g.house.holdings.keys().next().unwrap();
    let price = g.offers[&(HOUSE_ID, card)].price;
    g.place_bid(1, card, 1, price + 1000).unwrap();
    let result = g.close_round().unwrap();
    g.record_deliveries(&result, T0);

    let bank_delivery = g.deliveries.iter().find(|d| d.seller == HOUSE_ID).expect("a bank sale");
    assert_eq!(bank_delivery.buyer, 1);
    // Way past the deadline, the bank delivery is never auto-reversed.
    assert_eq!(g.expire_overdue_deliveries(T0 + 10 * DELIVERY_DEADLINE_SECS), 0);
    assert_eq!(g.deliveries.iter().find(|d| d.seller == HOUSE_ID).unwrap().status, DeliveryStatus::Pending);
}

#[test]
fn reversal_is_best_effort_and_flags_a_shortfall() {
    let mut g = traded_game();
    // The buyer moved the cards on before delivery failed.
    g.players.get_mut(&1).unwrap().holdings.clear();
    let id = only(&g).id;
    g.reverse_delivery(id).unwrap();

    let card = g.card_order[0];
    // Only what the buyer still held (0) is returned; the money is still refunded.
    assert_eq!(g.players[&2].held(card), 3, "seller gets back nothing reclaimable");
    assert_eq!(g.players[&1].balance, 1000, "buyer still refunded");
    assert!(g.deliveries[0].note.contains("0/2"), "shortfall flagged: {}", g.deliveries[0].note);
}
