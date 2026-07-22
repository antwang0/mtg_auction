//! Tests for league mode: setup, the weekly sealed-bid auction (top-N pay-as-bid
//! wins, carryover, stipend), and manual inventory edits.

use mtg_auction::engine::{Game, Rng};
use mtg_auction::model::*;

// A first auction day comfortably in the future so opening always finds a close.
// 2027-01-03 is a Sunday (epoch day 20821).
const FIRST_AUCTION_DAY: i64 = 20_821;

fn league_cfg() -> Config {
    Config {
        mode: GameMode::League,
        player_names: vec!["Alice".into(), "Bob".into(), "Carol".into()],
        starting_money: 10_000, // $100
        weekly_stipend: 2_500,  // $25
        league_tz_offset_mins: 60, // BST
        league_close_hour: 20,
        league_period_weeks: 1,
        league_first_auction_day: FIRST_AUCTION_DAY,
        league_matchmaking_start_day: FIRST_AUCTION_DAY - 7,
        seed: 7,
        ..Config::default()
    }
}

fn pool_of(cards: &[(&str, u32)]) -> CardPool {
    let sample = CardPool::sample();
    let find = |name: &str| {
        sample
            .commons
            .iter()
            .chain(&sample.uncommons)
            .chain(&sample.rares)
            .chain(&sample.mythics)
            .find(|c| c.name == name)
            .expect("sample card")
            .clone()
    };
    CardPool { exact: Some(cards.iter().map(|(n, q)| (find(n), *q)).collect()), ..CardPool::default() }
}

fn stock_and_open(g: &mut Game, cards: &[(&str, u32)], now: u64) {
    g.add_cards(pool_of(cards)).unwrap();
    g.open_league_auction(now).unwrap();
}

fn card_id(g: &Game, name: &str) -> CardId {
    g.cards.values().find(|c| c.name == name).unwrap().id
}

#[test]
fn league_setup_deals_nothing_and_starts_the_ladder_phase() {
    let g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    assert_eq!(g.phase, Phase::League);
    assert_eq!(g.round, 0);
    assert!(g.cards.is_empty());
    assert!(g.round_deadline.is_none());
    for p in g.players.values() {
        assert_eq!(p.balance, 10_000);
        assert!(p.holdings.is_empty());
    }
    // Regular orders are rejected in league mode.
    let mut g = g;
    assert!(g.place_bid(1, 1, 1, 100).is_err());
}

#[test]
fn top_bids_win_pay_as_bid_and_unsold_cards_carry_over() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 2), ("Torch Bearer", 1)], 1_000);
    assert_eq!(g.round, 1);
    let close = g.round_deadline.unwrap();
    // 20:00 BST is 19:00 UTC, on the configured first-auction day.
    assert_eq!(mtg_auction::engine::league_day_of(close, 60), FIRST_AUCTION_DAY, "on the first-auction day");
    assert_eq!(close % 86_400, 19 * 3_600, "20:00 BST = 19:00 UTC");

    let rat = card_id(&g, "Bog Rat");
    let torch = card_id(&g, "Torch Bearer");
    // Three bids on 2 rats: Alice $5, Bob $3 and $2 (multiple bids allowed).
    g.place_league_bid(1, rat, 500).unwrap();
    g.place_league_bid(2, rat, 300).unwrap();
    g.place_league_bid(2, rat, 200).unwrap();
    // Nobody bids on the torch — it carries over.

    let result = g.close_league_auction(&mut Rng::new(1)).unwrap();
    assert_eq!(result.trades.len(), 2);
    let alice = &g.players[&1];
    let bob = &g.players[&2];
    let carol = &g.players[&3];
    // Winners pay their own bid, plus everyone gets the stipend.
    assert_eq!(alice.balance, 10_000 - 500 + 2_500);
    assert_eq!(bob.balance, 10_000 - 300 + 2_500);
    assert_eq!(carol.balance, 10_000 + 2_500);
    assert_eq!(alice.held(rat), 1, "winnings are added automatically");
    assert_eq!(bob.held(rat), 1);
    assert_eq!(g.house.held(rat), 0);
    assert_eq!(g.house.held(torch), 1, "unsold cards stay with the house");
    assert_eq!(g.house.balance, 800);
    assert!(g.league_bids.is_empty(), "bids don't rest across weeks");
    assert!(g.round_deadline.is_none(), "auction closed until restocked");

    // League mode doesn't track deliveries (the host hands cards over in person).
    g.record_deliveries(&result, 2_000);
    assert!(g.deliveries.is_empty(), "no deliveries are recorded in league mode");

    // Reopening uses the carried-over pool (no new cards needed).
    g.open_league_auction(3_000).unwrap();
    assert_eq!(g.round, 2);
}

#[test]
fn bids_cannot_exceed_the_balance() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 5)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    g.place_league_bid(1, rat, 9_000).unwrap();
    assert!(g.place_league_bid(1, rat, 2_000).is_err(), "no debt in league mode");
    g.place_league_bid(1, rat, 1_000).unwrap(); // exactly the balance is fine
    // Cancelling frees the commitment.
    let id = g.place_league_bid(2, rat, 10_000).unwrap();
    assert!(g.place_league_bid(2, rat, 1).is_err());
    g.cancel_league_bid(2, id).unwrap();
    g.place_league_bid(2, rat, 1).unwrap();
    // You can't cancel someone else's bid.
    let alice_bid = g.league_bids.iter().find(|b| b.player == 1).unwrap().id;
    assert!(g.cancel_league_bid(2, alice_bid).is_err());
}

#[test]
fn ties_break_randomly_and_fairly_at_equal_prices() {
    // 1 copy, two equal bids: over many seeds both players should win sometimes.
    let mut wins = [0u32; 2];
    for seed in 0..40 {
        let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
        stock_and_open(&mut g, &[("Bog Rat", 1)], 1_000);
        let rat = card_id(&g, "Bog Rat");
        g.place_league_bid(1, rat, 500).unwrap();
        g.place_league_bid(2, rat, 500).unwrap();
        let r = g.close_league_auction(&mut Rng::new(seed)).unwrap();
        assert_eq!(r.trades.len(), 1);
        wins[(r.trades[0].buyer - 1) as usize] += 1;
    }
    assert!(wins[0] > 0 && wins[1] > 0, "tie-break should not be one-sided: {wins:?}");
}

#[test]
fn matchmaking_is_gated_before_the_start_day() {
    // Matchmaking opens the week before the first auction (FIRST - 7).
    let mm_day = FIRST_AUCTION_DAY - 7;
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();

    // "Now" is a few days before matchmaking opens.
    let now = ((mm_day - 3) * 86_400 + 12 * 3_600) as u64;
    // Two blocks/day; put Alice & Bob free the evening before the start day and
    // the evening after it (both strictly-future, within the schedule window).
    let before = (mm_day - 1) * 2 + 1;
    let after = (mm_day + 1) * 2 + 1;
    g.set_availability(1, vec![before, after]).unwrap();
    g.set_availability(2, vec![before, after]).unwrap();
    g.set_games_per_week(1, 2).unwrap();
    g.set_games_per_week(2, 2).unwrap();

    let created = g.auto_schedule(now);
    assert_eq!(created, 1, "only the post-start slot should schedule");
    let m = g.ladder.matches.iter().find(|m| m.status == MatchStatus::Scheduled).unwrap();
    assert!(m.slot >= mm_day * 2, "no match is placed before the matchmaking start day");
}

#[test]
fn manual_inventory_is_league_only_and_bounded() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    let added = g.inventory_add(1, pool_of(&[("Sunlit Field", 3)])).unwrap();
    assert_eq!(added, 3);
    let field = card_id(&g, "Sunlit Field");
    assert_eq!(g.players[&1].held(field), 3);
    assert!(g.inventory_remove(1, field, 4).is_err(), "can't remove more than held");
    g.inventory_remove(1, field, 2).unwrap();
    assert_eq!(g.players[&1].held(field), 1);

    // Not available in the standard economy.
    let mut std = Game::setup(
        Config { player_names: vec!["A".into(), "B".into()], ..Config::default() },
        CardPool::sample(),
    )
    .unwrap();
    assert!(std.inventory_add(1, pool_of(&[("Bog Rat", 1)])).is_err());
}
