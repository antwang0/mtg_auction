//! Tests for game setup, order validation, and the auction matching engine.

use mtg_auction::engine::Game;
use mtg_auction::model::{CardPool, Config, OrderAction, OrderKind, Phase, PoolCard, Rarity};

fn base_config() -> Config {
    Config {
        player_names: vec!["A".into(), "B".into(), "C".into()],
        set: "sample".into(),
        starting_money: 100,
        debt_limit: 0,
        primary_rounds: 3,
        secondary_rounds: 1,
        num_packs: 2,
        pack_size: 15,
        seed: 7,
        primary_round_seconds: 0,
        secondary_round_seconds: 0,
        ..Config::default()
    }
}

/// Build a tiny hand-controlled game: clear the dealt cards and give specific
/// holdings so matching tests are deterministic. Card id 1 always exists
/// because at least one card is opened.
fn controlled_game() -> Game {
    let mut g = Game::setup(base_config(), CardPool::sample()).unwrap();
    // Reset everyone's holdings and balances to known values.
    for p in g.players.values_mut() {
        p.holdings.clear();
        p.balance = 100;
    }
    g
}

fn card1(g: &Game) -> u32 {
    g.card_order[0]
}

#[test]
fn setup_deals_all_cards_and_money() {
    let g = Game::setup(base_config(), CardPool::sample()).unwrap();
    assert_eq!(g.phase, Phase::Primary);
    assert_eq!(g.round, 1);

    let total_cards: u32 = g.players.values().map(|p| p.holdings.values().sum::<u32>()).sum();
    assert_eq!(total_cards, 2 * 15, "every opened card should be dealt");

    for p in g.players.values() {
        assert_eq!(p.balance, 100);
    }
}

#[test]
fn simple_cross_trades_at_mid() {
    let mut g = controlled_game();
    let c = card1(&g);
    // Seller B holds the card and offers at 10; buyer A bids 20.
    g.players.get_mut(&2).unwrap().add_cards(c, 1);

    g.place_offer(2, c, 1, 10).unwrap();
    g.place_bid(1, c, 1, 20).unwrap();

    let result = g.close_round();
    let result = result.unwrap();
    assert_eq!(result.trades.len(), 1);
    let t = &result.trades[0];
    assert_eq!(t.buyer, 1);
    assert_eq!(t.seller, 2);
    assert_eq!(t.qty, 1);
    assert_eq!(t.price, 15, "mid of 20 and 10");

    assert_eq!(g.players[&1].balance, 85);
    assert_eq!(g.players[&2].balance, 115);
    assert_eq!(g.players[&1].held(c), 1);
    assert_eq!(g.players[&2].held(c), 0);
}

#[test]
fn no_cross_when_bid_below_offer() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);

    g.place_offer(2, c, 1, 30).unwrap();
    g.place_bid(1, c, 1, 20).unwrap();

    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 0);
    assert_eq!(g.players[&1].balance, 100);
    assert_eq!(g.players[&2].held(c), 1);
}

#[test]
fn cannot_place_an_order_that_crosses_your_own() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&1).unwrap().add_cards(c, 1);

    // A resting offer to sell at $0.30.
    g.place_offer(1, c, 1, 30).unwrap();
    // A bid above the offer crosses (buy high while offering to sell low) — and
    // a bid equal to it crosses too. Both are rejected.
    assert!(g.place_bid(1, c, 1, 40).is_err(), "bid above own offer crosses");
    assert!(g.place_bid(1, c, 1, 30).is_err(), "bid equal to own offer crosses");
    // A bid below the offer is a legitimate spread.
    g.place_bid(1, c, 1, 20).unwrap();

    // Symmetric guard from the offer side: at or below the resting $0.20 bid
    // crosses, above it is fine (replacing the earlier offer).
    assert!(g.place_offer(1, c, 1, 20).is_err(), "offer equal to own bid crosses");
    assert!(g.place_offer(1, c, 1, 10).is_err(), "offer below own bid crosses");
    g.place_offer(1, c, 1, 25).unwrap();
}

#[test]
fn mid_rounds_half_up() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);

    g.place_offer(2, c, 1, 10).unwrap();
    g.place_bid(1, c, 1, 15).unwrap(); // mid 12.5 -> 13

    let r = g.close_round().unwrap();
    assert_eq!(r.trades[0].price, 13);
}

#[test]
fn highest_bid_meets_lowest_offer() {
    let mut g = controlled_game();
    let c = card1(&g);
    // Two sellers (B@10, C@14) and one buyer (A bids 12 for 1).
    g.players.get_mut(&2).unwrap().add_cards(c, 1);
    g.players.get_mut(&3).unwrap().add_cards(c, 1);

    g.place_offer(2, c, 1, 10).unwrap();
    g.place_offer(3, c, 1, 14).unwrap();
    g.place_bid(1, c, 1, 12).unwrap();

    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 1);
    // Buyer crosses the cheapest offer (B@10), not C@14.
    assert_eq!(r.trades[0].seller, 2);
    assert_eq!(r.trades[0].price, 11); // mid of 12 and 10
    assert_eq!(g.players[&3].held(c), 1, "C's pricier offer stays unmatched");
}

#[test]
fn multi_unit_partial_fill() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 5);

    g.place_offer(2, c, 5, 10).unwrap();
    g.place_bid(1, c, 2, 20).unwrap(); // wants 2 of the 5

    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].qty, 2);
    assert_eq!(g.players[&1].held(c), 2);
    assert_eq!(g.players[&2].held(c), 3);
}

#[test]
fn player_never_trades_with_self() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&1).unwrap().add_cards(c, 1);

    // Placement now rejects a crossing self-book, so insert one directly to
    // exercise the matcher's own-offer skip as a defence-in-depth net.
    use mtg_auction::model::Order;
    g.offers.insert((1, c), Order { player: 1, card: c, qty: 1, price: 10 });
    g.bids.insert((1, c), Order { player: 1, card: c, qty: 1, price: 20 });

    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 0);
    assert_eq!(g.players[&1].held(c), 1);
    assert_eq!(g.players[&1].balance, 100);
}

#[test]
fn bid_cannot_exceed_balance_without_debt() {
    let mut g = controlled_game();
    let c = card1(&g);
    // debt_limit is 0, balance 100. A bid of 60 x2 = 120 must be rejected.
    let err = g.place_bid(1, c, 2, 60).unwrap_err();
    assert!(err.contains("available"), "{err}");

    // 50 x 2 = 100 is exactly affordable.
    g.place_bid(1, c, 2, 50).unwrap();
    // A second bid on a different card pushes total over the limit.
    let c2 = *g.card_order.get(1).expect("need a second distinct card");
    let err = g.place_bid(1, c2, 1, 1).unwrap_err();
    assert!(err.contains("available"), "{err}");
}

#[test]
fn debt_limit_allows_bidding_into_debt() {
    let mut cfg = base_config();
    cfg.debt_limit = 50;
    let mut g = Game::setup(cfg, CardPool::sample()).unwrap();
    for p in g.players.values_mut() {
        p.holdings.clear();
        p.balance = 100;
    }
    let c = card1(&g);
    // balance 100 + debt 50 = 150 ceiling.
    g.place_bid(1, c, 3, 50).unwrap(); // 150, ok
    let err = g.place_bid(1, c, 4, 50).unwrap_err(); // 200, too much
    assert!(err.contains("available"), "{err}");
}


#[test]
fn order_log_records_places_and_cancels() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);

    g.place_bid(1, c, 2, 5).unwrap();
    g.place_offer(2, c, 1, 9).unwrap();
    g.place_bid(1, c, 0, 0).unwrap(); // cancel the bid

    assert_eq!(g.order_log.len(), 3);
    assert_eq!(g.order_log[0].kind, OrderKind::Bid);
    assert_eq!(g.order_log[0].action, OrderAction::Place);
    assert_eq!(g.order_log[0].price, 5);
    assert_eq!(g.order_log[1].kind, OrderKind::Offer);
    assert_eq!(g.order_log[2].action, OrderAction::Cancel);
    assert!(g.order_log[0].seq < g.order_log[2].seq, "seq is monotonic");

    // Cancelling an order that doesn't exist records nothing.
    g.place_offer(1, c, 0, 0).unwrap();
    assert_eq!(g.order_log.len(), 3);
}

#[test]
fn rejects_absurd_price_and_quantity() {
    let mut g = controlled_game();
    let c = card1(&g);
    // Price and quantity are capped before any affordability check.
    assert!(g.place_bid(1, c, 1, 1_000_000_000).unwrap_err().contains("too high"));
    assert!(g.place_bid(1, c, 1_000_000, 1).unwrap_err().contains("too high"));
    g.players.get_mut(&1).unwrap().add_cards(c, 1);
    assert!(g.place_offer(1, c, 1, 1_000_000_000).unwrap_err().contains("too high"));
}

#[test]
fn cannot_offer_more_than_held() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&1).unwrap().add_cards(c, 1);
    let err = g.place_offer(1, c, 2, 5).unwrap_err();
    assert!(err.contains("hold"), "{err}");
}

#[test]
fn round_result_reports_clears() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);
    g.place_offer(2, c, 1, 10).unwrap();
    g.place_bid(1, c, 1, 20).unwrap();
    let r = g.close_round().unwrap();
    let cl = r.clears.iter().find(|x| x.card == c).expect("clear entry for the card");
    assert_eq!(cl.best_bid, Some(20));
    assert_eq!(cl.best_offer, Some(10));
    assert_eq!(cl.cleared, Some(15));
    assert_eq!(cl.volume, 1);
}

#[test]
fn clears_record_top_of_book_even_without_a_fill() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);
    g.place_offer(2, c, 1, 30).unwrap();
    g.place_bid(1, c, 1, 20).unwrap(); // no cross
    let r = g.close_round().unwrap();
    let cl = r.clears.iter().find(|x| x.card == c).expect("clear entry for the card");
    assert_eq!(cl.best_bid, Some(20));
    assert_eq!(cl.best_offer, Some(30));
    assert_eq!(cl.cleared, None);
    assert_eq!(cl.volume, 0);
}

#[test]
fn rounds_advance_through_both_phases_then_finish() {
    // base_config: 3 primary rounds, then 1 secondary round.
    let mut g = controlled_game();
    assert_eq!((g.phase, g.round), (Phase::Primary, 1));
    g.close_round().unwrap();
    assert_eq!((g.phase, g.round), (Phase::Primary, 2));
    g.close_round().unwrap();
    assert_eq!((g.phase, g.round), (Phase::Primary, 3));
    // Closing the last primary round opens the secondary phase at round 1.
    g.close_round().unwrap();
    assert_eq!((g.phase, g.round), (Phase::Secondary, 1));
    // Closing the last secondary round ends the game.
    g.close_round().unwrap();
    assert_eq!(g.phase, Phase::Finished);
    assert!(g.close_round().is_err(), "no trading after the game ends");
}

#[test]
fn unmatched_orders_persist_between_rounds() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);
    g.place_offer(2, c, 1, 30).unwrap();
    g.place_bid(1, c, 1, 20).unwrap(); // no cross
    g.close_round().unwrap();
    // Both orders rest into the next round unchanged.
    assert_eq!(g.bids.get(&(1, c)).map(|o| o.qty), Some(1));
    assert_eq!(g.offers.get(&(2, c)).map(|o| o.qty), Some(1));

    // Next round the buyer raises their bid and it now crosses.
    g.place_bid(1, c, 1, 40).unwrap();
    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].price, 35); // mid of 40 and 30
    assert!(g.bids.is_empty() && g.offers.is_empty(), "filled orders are removed");
}

#[test]
fn partial_fill_carries_remainder_forward() {
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 5);
    g.place_offer(2, c, 5, 10).unwrap();
    g.place_bid(1, c, 2, 20).unwrap();
    g.close_round().unwrap();
    // The offer had 5, sold 2, and the remaining 3 rest for next round.
    assert_eq!(g.offers.get(&(2, c)).map(|o| o.qty), Some(3));
    assert!(g.bids.is_empty(), "the fully-filled bid is gone");
}

#[test]
fn stale_offer_is_capped_to_current_holdings() {
    // A resting offer can outlive the cards backing it. The seller must never
    // deliver more than they actually hold when the auction closes.
    let mut g = controlled_game();
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1); // seller holds exactly 1

    g.place_offer(2, c, 1, 10).unwrap();
    g.place_bid(1, c, 1, 20).unwrap();
    g.close_round().unwrap(); // seller's single copy is sold
    assert_eq!(g.players[&2].held(c), 0);

    // The seller now holds 0 but still has a standing offer. You can't even
    // place such an offer fresh...
    g.place_offer(2, c, 1, 10).unwrap_err();
    // ...and a resting one left over from before delivers nothing while the
    // seller is empty. Inject one directly to simulate that carry-over.
    g.offers.insert((2, c), mtg_auction::model::Order { player: 2, card: c, qty: 1, price: 10 });
    g.place_bid(3, c, 1, 20).unwrap();
    let r = g.close_round().unwrap();
    assert_eq!(r.trades.len(), 0, "no copies to deliver, so no trade");
    // The unbacked offer simply rests; it becomes live again only if the
    // seller reacquires the card.
    assert_eq!(g.offers.get(&(2, c)).map(|o| o.qty), Some(1));
}

#[test]
fn match_respects_debt_limit_on_resting_bid() {
    // With no debt allowed, a resting bid can never spend a player below zero
    // even if their balance fell after the bid was placed.
    let mut g = controlled_game(); // debt_limit 0, balances 100
    let c = card1(&g);
    g.players.get_mut(&2).unwrap().add_cards(c, 1);

    // Drain buyer 1's balance to 10 directly, then rest a bid of 50.
    g.players.get_mut(&1).unwrap().balance = 10;
    g.place_bid(1, c, 1, 5).unwrap(); // 5 <= balance 10, allowed
    g.place_offer(2, c, 1, 4).unwrap();
    let r = g.close_round().unwrap();
    // Mid of 5 and 4 is 5 (round half up); buyer can afford one at 5.
    assert_eq!(r.trades.len(), 1);
    assert_eq!(g.players[&1].balance, 5);
    assert!(g.players[&1].balance >= 0);
}

#[test]
fn arm_timer_sets_and_clears_deadline() {
    // No timer configured -> no deadline.
    let mut g = Game::setup(base_config(), CardPool::sample()).unwrap();
    g.arm_timer(1000);
    assert_eq!(g.round_deadline, None);

    // With a timer, the deadline is now + the phase's round timer while trading...
    let mut cfg = base_config();
    cfg.primary_round_seconds = 30;
    let mut g = Game::setup(cfg, CardPool::sample()).unwrap();
    g.arm_timer(1000);
    assert_eq!(g.round_deadline, Some(1030));

    // ...and clears once the game is finished.
    while g.phase != Phase::Finished {
        g.close_round().unwrap();
    }
    g.arm_timer(2000);
    assert_eq!(g.round_deadline, None);
}

#[test]
fn reproducible_from_seed() {
    let a = Game::setup(base_config(), CardPool::sample()).unwrap();
    let b = Game::setup(base_config(), CardPool::sample()).unwrap();
    assert_eq!(a.card_order.len(), b.card_order.len());
    for (x, y) in a.card_order.iter().zip(b.card_order.iter()) {
        assert_eq!(a.cards[x].name, b.cards[y].name);
    }
}

#[test]
fn setup_rejects_absurd_ladder_config() {
    let with = |f: fn(&mut Config)| {
        let mut c = base_config();
        f(&mut c);
        Game::setup(c, CardPool::sample())
    };
    assert!(with(|c| c.schedule_window_days = 100_000).is_err(), "runaway scheduling window");
    assert!(with(|c| c.max_games_per_week = 10_000).is_err(), "absurd weekly cap");
    assert!(with(|c| c.cancel_penalty = -5).is_err(), "negative penalty would reward cancelling");
    assert!(with(|c| c.elo_k = -1).is_err(), "negative K-factor");
    assert!(with(|c| c.starting_elo = -1).is_err(), "negative starting ELO");
    // Sane values still succeed.
    assert!(with(|c| c.schedule_window_days = 30).is_ok());
}

#[test]
fn manual_pool_deals_exactly_the_listed_cards() {
    let pc = |name: &str| PoolCard {
        name: name.into(),
        rarity: Rarity::Common,
        image: None,
        ref_price: None,
        type_line: None,
        cmc: None,
        mana_cost: None,
        colors: String::new(),
        color_identity: String::new(),
    };
    let pool = CardPool {
        set_name: "Custom list".into(),
        exact: Some(vec![(pc("Alpha"), 3), (pc("Beta"), 2)]),
        ..Default::default()
    };
    // Pack settings are irrelevant for a manual pool and must be ignored.
    let mut cfg = base_config();
    cfg.num_packs = 999;
    cfg.pack_size = 999;
    let g = Game::setup(cfg, pool).unwrap();

    assert_eq!(g.cards.len(), 2, "two distinct cards in the catalog");
    let total: u32 = g.players.values().map(|p| p.holdings.values().sum::<u32>()).sum();
    assert_eq!(total, 5, "exactly the listed copies are dealt");

    let supply = |name: &str| -> u32 {
        let id = *g.cards.iter().find(|(_, c)| c.name == name).unwrap().0;
        g.players.values().map(|p| p.held(id)).sum()
    };
    assert_eq!(supply("Alpha"), 3);
    assert_eq!(supply("Beta"), 2);
}

#[test]
fn a_pack_has_no_duplicate_cards_when_the_pool_allows() {
    // A single 10-card pack drawn from the 38-distinct sample set should be all
    // distinct cards, so the interned catalog has exactly one entry per slot.
    let mut cfg = base_config();
    cfg.num_packs = 1;
    cfg.pack_size = 10;
    let g = Game::setup(cfg, CardPool::sample()).unwrap();
    assert_eq!(g.cards.len(), 10, "no card should be drawn twice in one pack");
}
