//! Tests for league mode: setup, the recurring sealed-bid auction (uniform
//! Nth-price clearing in rarity-then-name order, bid amendment, carryover,
//! stipend), deadline-based swiss matchmaking, and manual inventory edits.

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

/// A 4-player league with the given in-flight cap N (players get N matches
/// with N weeks to play them). Returns the game and a "now" on the
/// matchmaking start day.
fn league4(cap: u32) -> (Game, u64) {
    let mut cfg = league_cfg();
    cfg.player_names.push("Dave".into());
    cfg.league_pending_per_player = cap;
    let g = Game::setup(cfg, CardPool::default()).unwrap();
    (g, ((FIRST_AUCTION_DAY - 7) * 86_400) as u64)
}

/// The scheduled matches, as (id, a, b) tuples.
fn scheduled(g: &Game) -> Vec<(u64, u32, u32)> {
    g.ladder
        .matches
        .iter()
        .filter(|m| m.status == MatchStatus::Scheduled)
        .map(|m| (m.id, m.a, m.b))
        .collect()
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
fn cards_clear_at_the_nth_highest_bid_and_unsold_cards_carry_over() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 2), ("Torch Bearer", 1)], 1_000);
    assert_eq!(g.round, 1);
    let close = g.round_deadline.unwrap();
    // 20:00 BST is 19:00 UTC, on the configured first-auction day.
    assert_eq!(mtg_auction::engine::league_day_of(close, 60), FIRST_AUCTION_DAY, "on the first-auction day");
    assert_eq!(close % 86_400, 19 * 3_600, "20:00 BST = 19:00 UTC");

    let rat = card_id(&g, "Bog Rat");
    let torch = card_id(&g, "Torch Bearer");
    // Three bids on 2 rats: with qty 2 the card clears at the 2nd-highest bid.
    g.place_league_bid(1, rat, 500).unwrap();
    g.place_league_bid(2, rat, 300).unwrap();
    g.place_league_bid(3, rat, 200).unwrap();
    // Nobody bids on the torch — someone's implicit 0 bid wins it for free.

    let result = g.close_league_auction(&mut Rng::new(1)).unwrap();
    assert_eq!(result.trades.len(), 3);
    let alice = &g.players[&1];
    let bob = &g.players[&2];
    // The two rat winners both pay the uniform clearing price ($3), plus stipend.
    assert_eq!(alice.balance, 10_000 - 300 + 2_500);
    assert_eq!(bob.balance, 10_000 - 300 + 2_500);
    assert_eq!(alice.held(rat), 1, "winnings are added automatically");
    assert_eq!(bob.held(rat), 1);
    assert_eq!(g.players[&3].balance, 10_000 + 2_500, "the outbid player pays nothing");
    assert_eq!(g.house.held(rat), 0);
    assert_eq!(g.house.held(torch), 0, "every copy finds a home");
    let torch_trade = result.trades.iter().find(|t| t.card == torch).unwrap();
    assert_eq!(torch_trade.price, 0, "an unbid card trades at 0");
    assert_eq!(g.house.balance, 600);
    assert!(g.league_bids.is_empty(), "bids don't rest across weeks");
    assert!(g.round_deadline.is_none(), "auction closed until restocked");

    // League mode doesn't track deliveries (the host hands cards over in person).
    g.record_deliveries(&result, 2_000);
    assert!(g.deliveries.is_empty(), "no deliveries are recorded in league mode");

    // Everything sold, so reopening needs a fresh pool.
    assert!(g.open_league_auction(3_000).is_err(), "the pool is empty");
    stock_and_open(&mut g, &[("Bog Rat", 1)], 3_000);
    assert_eq!(g.round, 2);
}

/// The per-card auction history behind the History tab: clearing price, the
/// highest bid, and the cover (highest bid that took nothing).
#[test]
fn auction_history_records_the_clearing_price_high_bid_and_cover() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 2), ("Torch Bearer", 1)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    let torch = card_id(&g, "Torch Bearer");
    g.place_league_bid(1, rat, 500).unwrap();
    g.place_league_bid(2, rat, 300).unwrap();
    g.place_league_bid(3, rat, 200).unwrap();
    g.close_league_auction(&mut Rng::new(1)).unwrap();

    let rat_clear = g.league_clears.iter().find(|c| c.card == rat).unwrap();
    assert_eq!(rat_clear.round, 1);
    assert_eq!(rat_clear.copies, 2);
    assert_eq!(rat_clear.cleared, 300, "2 copies clear at the 2nd-highest bid");
    assert_eq!(rat_clear.high, Some(500));
    assert_eq!(rat_clear.cover, Some(200), "the highest bid that won nothing");
    assert_eq!(rat_clear.bids.len(), 3, "every bid is kept, winners and losers");
    assert_eq!(rat_clear.winners, vec![1, 2]);

    // Nobody bid on the torch: no high, no cover, and its free winner has no bid.
    let torch_clear = g.league_clears.iter().find(|c| c.card == torch).unwrap();
    assert_eq!(torch_clear.cleared, 0);
    assert_eq!(torch_clear.high, None);
    assert_eq!(torch_clear.cover, None);
    assert!(torch_clear.bids.is_empty());
    assert_eq!(torch_clear.winners.len(), 1, "a free copy still records its winner");
}

/// When every bid wins there is no losing bid, so there is no cover — that is
/// distinct from a cover of 0, which would mean someone bid nothing.
#[test]
fn auction_history_has_no_cover_when_every_bid_wins() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 5)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    g.place_league_bid(1, rat, 500).unwrap();
    g.place_league_bid(2, rat, 200).unwrap();
    g.close_league_auction(&mut Rng::new(1)).unwrap();

    let c = g.league_clears.iter().find(|c| c.card == rat).unwrap();
    assert_eq!(c.cleared, 0, "fewer real bids than copies clears at 0");
    assert_eq!(c.high, Some(500));
    assert_eq!(c.cover, None, "both bidders won, so nothing was covered");
    assert_eq!(c.winners.len(), 3, "two bidders plus one free non-bidder");
}

/// A bid trimmed to the bidder's remaining balance is reported at the trimmed
/// price — that is the number the clearing maths actually used.
#[test]
fn auction_history_reports_bids_after_amendment() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Avatar of Eternity", 1), ("Bog Rat", 1)], 1_000);
    let avatar = card_id(&g, "Avatar of Eternity");
    let rat = card_id(&g, "Bog Rat");
    // Alice commits nearly everything to the avatar (mythic, resolves first),
    // leaving her rat bid unaffordable; it is amended down as the round runs.
    g.place_league_bid(1, avatar, 9_000).unwrap();
    g.place_league_bid(1, rat, 5_000).unwrap();
    g.place_league_bid(2, rat, 800).unwrap();
    g.close_league_auction(&mut Rng::new(1)).unwrap();

    let c = g.league_clears.iter().find(|c| c.card == rat).unwrap();
    let alice_bid = c.bids.iter().find(|(p, _)| *p == 1).unwrap().1;
    assert_eq!(alice_bid, 1_000, "trimmed to her $10 left after the avatar");
    assert_eq!(c.high, Some(1_000), "the high is the amended bid, not the $50 asked");
    assert_eq!(c.cover, Some(800), "Bob's losing bid");
    assert_eq!(c.winners, vec![1]);
}

#[test]
fn fewer_real_bids_than_copies_clear_at_zero_and_nonbidders_fill_the_rest() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 5)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    g.place_league_bid(1, rat, 500).unwrap();
    g.place_league_bid(2, rat, 200).unwrap();
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    // Two real bids on 5 copies: everyone's implicit 0 bid sets the price at
    // 0. The real bidders win first, the non-bidder gets one for free, and
    // the copies beyond one-per-player carry over.
    assert_eq!(r.trades.len(), 3);
    assert!(r.trades.iter().all(|t| t.price == 0), "all copies trade at 0");
    for p in 1..=3 {
        assert_eq!(g.players[&p].held(rat), 1);
        assert_eq!(g.players[&p].balance, 10_000 + 2_500, "nobody paid anything");
    }
    assert_eq!(g.house.held(rat), 2, "copies beyond one-per-player carry over");
}

#[test]
fn an_unbid_card_is_given_to_random_distinct_players() {
    // 3 copies, no bids at all, 3 players: everyone gets exactly one, free.
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 3)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    let r = g.close_league_auction(&mut Rng::new(9)).unwrap();
    assert_eq!(r.trades.len(), 3);
    assert!(r.trades.iter().all(|t| t.price == 0));
    for p in 1..=3 {
        assert_eq!(g.players[&p].held(rat), 1, "one copy per player");
    }
    // Over many seeds the lone copy of a card lands on different players.
    let mut winners = std::collections::HashSet::new();
    for seed in 0..30 {
        let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
        stock_and_open(&mut g, &[("Torch Bearer", 1)], 1_000);
        let r = g.close_league_auction(&mut Rng::new(seed)).unwrap();
        winners.insert(r.trades[0].buyer);
    }
    assert!(winners.len() > 1, "the free copy is assigned randomly: {winners:?}");
}

#[test]
fn one_bid_per_card_and_no_total_commitment_cap() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Bog Rat", 5), ("Torch Bearer", 1)], 1_000);
    let rat = card_id(&g, "Bog Rat");
    let torch = card_id(&g, "Torch Bearer");
    // A single bid can't exceed the balance ($100)…
    assert!(g.place_league_bid(1, rat, 19_900).is_err(), "single bid capped at the balance");
    // …but the total across bids may: no aggregate cap.
    g.place_league_bid(1, rat, 9_000).unwrap();
    g.place_league_bid(1, torch, 8_000).unwrap();
    assert_eq!(g.league_committed(1), 17_000);
    // Re-bidding a card replaces the earlier bid rather than adding a second.
    g.place_league_bid(1, rat, 400).unwrap();
    assert_eq!(g.league_bids.iter().filter(|b| b.player == 1 && b.card == rat).count(), 1);
    assert_eq!(g.league_committed(1), 8_400);
    // You can't cancel someone else's bid.
    let alice_bid = g.league_bids.iter().find(|b| b.player == 1).unwrap().id;
    assert!(g.cancel_league_bid(2, alice_bid).is_err());
}

#[test]
fn rarer_cards_resolve_first_and_later_bids_amend_to_the_remaining_balance() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    // A mythic and a common: the mythic resolves first even though the common
    // sorts earlier alphabetically.
    stock_and_open(&mut g, &[("Avatar of Eternity", 1), ("Bog Rat", 1)], 1_000);
    let avatar = card_id(&g, "Avatar of Eternity");
    let rat = card_id(&g, "Bog Rat");
    // Alice ($100) bids $90 on the mythic and $50 on the common.
    g.place_league_bid(1, avatar, 9_000).unwrap();
    g.place_league_bid(1, rat, 5_000).unwrap();
    g.place_league_bid(2, rat, 800).unwrap();
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    // Mythic first: sole bid, so it clears at that bid ($90), leaving Alice $10.
    // Her $50 rat bid is amended down to $10, which still beats Bob's $8 and,
    // as the highest bid on a single copy, sets the price.
    assert_eq!(r.trades[0].card, avatar);
    assert_eq!(r.trades[0].price, 9_000);
    assert_eq!(r.trades[1].card, rat);
    assert_eq!(r.trades[1].buyer, 1);
    assert_eq!(r.trades[1].price, 1_000, "amended to the remaining balance");
    assert_eq!(g.players[&1].balance, 2_500, "fully spent, then the stipend");
}

#[test]
fn league_matches_are_best_of_three_and_scheduled_in_swiss_batches() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2, "one in-flight match per player");
    assert_eq!(g.auto_schedule(now), 0, "everyone is at their in-flight cap");

    let ids: Vec<u64> = g
        .ladder
        .matches
        .iter()
        .filter(|m| m.status == MatchStatus::Scheduled)
        .map(|m| m.id)
        .collect();
    // Best-of-three: more than 3 games (or 3 wins) is rejected.
    assert!(g.submit_match_result(g.ladder.matches[0].a, ids[0], 3, 0, 0).is_err());
    assert!(g.submit_match_result(g.ladder.matches[0].a, ids[0], 2, 2, 0).is_err());
    let first = g.ladder.matches.iter().find(|m| m.id == ids[0]).unwrap().a;
    g.submit_match_result(first, ids[0], 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "no instant rematch — the freed pair waits for fresh opponents");
    let second = g.ladder.matches.iter().find(|m| m.id == ids[1]).unwrap().a;
    g.submit_match_result(second, ids[1], 2, 1, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "round 2 waits for round 1's close");
    let round2 = now + 8 * 86_400; // past round 1's close
    assert_eq!(g.auto_schedule(round2), 2, "round 2 assigns together after the close");

    // Standings rank by points (3 per win), and a 2-0 win outranks a 2-1 win.
    // The 2-1 loser earns 1 point for the game taken; the 2-0 loser earns 0.
    let s = g.standings();
    assert_eq!(s[0].points, 3);
    assert_eq!(s[1].points, 3);
    assert_eq!(s[2].points, 1);
    assert_eq!(s[3].points, 0);
    assert!(s[0].game_wins - s[0].game_losses > s[1].game_wins - s[1].game_losses);
    assert_eq!((s[0].game_wins, s[0].game_losses), (2, 0));
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
fn matchmaking_is_gated_before_the_start_day_then_assigns_instantly() {
    // Matchmaking opens the week before the first auction (FIRST - 7).
    let mm_day = FIRST_AUCTION_DAY - 7;
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap(); // 3 players, default N = 2

    // Before the start day: nothing is assigned.
    let before = ((mm_day - 3) * 86_400 + 12 * 3_600) as u64;
    assert_eq!(g.auto_schedule(before), 0, "no matches before the matchmaking start day");

    // On the start day, matches are assigned immediately — no availability or
    // calendar slots needed. 3 players × 2 in flight → all 3 pairings exist.
    let now = (mm_day * 86_400) as u64;
    assert_eq!(g.auto_schedule(now), 3);
    // Play-by deadlines land exactly on the next auction close of the series.
    let close = mtg_auction::engine::next_league_close(now, FIRST_AUCTION_DAY, 0, 20, 1, 60).unwrap();
    for m in &g.ladder.matches {
        assert_eq!(m.slot_start, close, "play-by deadline is the auction close");
    }
    assert_eq!(g.auto_schedule(now), 0, "everyone is at their in-flight cap");
}

#[test]
fn unreported_league_matches_count_as_ties_and_do_not_block_the_next_batch() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let first_ids: Vec<u64> = g.ladder.matches.iter().map(|m| m.id).collect();
    assert_eq!(g.auto_schedule(now), 0, "upcoming matches block the batch");

    // Once the play-by deadlines (N = 1 week) have passed, the unreported
    // matches stop blocking (they count as ties) and new matches are assigned.
    let later = now + 8 * 86_400;
    assert_eq!(g.auto_schedule(later), 2);
    for id in &first_ids {
        let m = g.ladder.matches.iter().find(|m| m.id == *id).unwrap();
        assert_eq!(m.status, MatchStatus::Scheduled, "unreported matches never expire");
    }

    // The real result can still be added later.
    let m0 = g.ladder.matches.iter().find(|m| m.id == first_ids[0]).unwrap();
    let (a, id0) = (m0.a, m0.id);
    g.submit_match_result(a, id0, 2, 1, 0).unwrap();
    assert_eq!(
        g.ladder.matches.iter().find(|m| m.id == id0).unwrap().status,
        MatchStatus::Completed
    );
}

#[test]
fn resolution_order_is_rarity_then_alphabetical() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    // A mythic, two rares (stocked in reverse alphabetical order), a common.
    stock_and_open(
        &mut g,
        &[("Bog Rat", 1), ("Throne of Ages", 1), ("Archmage Vesper", 1), ("Nyx, the Endless", 1)],
        1_000,
    );
    // One bid on each card, small enough that no amendment kicks in — the
    // trade sequence exposes the resolution order directly.
    for name in ["Bog Rat", "Throne of Ages", "Archmage Vesper", "Nyx, the Endless"] {
        let c = card_id(&g, name);
        g.place_league_bid(1, c, 2_000).unwrap();
    }
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    let order: Vec<&str> = r.trades.iter().map(|t| t.card_name.as_str()).collect();
    assert_eq!(
        order,
        vec!["Nyx, the Endless", "Archmage Vesper", "Throne of Ages", "Bog Rat"],
        "mythic first, then rares alphabetically, then the common"
    );
}

#[test]
fn a_bid_amended_to_zero_can_still_win_a_sole_bid_card_for_free() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Avatar of Eternity", 1), ("Bog Rat", 1)], 1_000);
    let avatar = card_id(&g, "Avatar of Eternity");
    let rat = card_id(&g, "Bog Rat");
    // Alice's sole mythic bid takes her entire $100 balance; her rat bid is
    // then amended to $0, and as the only bid it wins the copy for free.
    g.place_league_bid(1, avatar, 10_000).unwrap();
    g.place_league_bid(1, rat, 3_000).unwrap();
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    assert_eq!(r.trades.len(), 2);
    assert_eq!(r.trades[1].price, 0, "the amended-to-zero bid clears at zero");
    assert_eq!(g.players[&1].held(rat), 1);
    assert_eq!(g.players[&1].balance, 2_500, "everything spent, then the stipend");
}

#[test]
fn amendment_can_change_who_wins_a_card() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(&mut g, &[("Avatar of Eternity", 1), ("Bog Rat", 1)], 1_000);
    let avatar = card_id(&g, "Avatar of Eternity");
    let rat = card_id(&g, "Bog Rat");
    // Alice wins the mythic for $90, leaving her $10. Her raw $50 rat bid
    // would beat Bob's $20, but amended to $10 it loses to him.
    g.place_league_bid(1, avatar, 9_000).unwrap();
    g.place_league_bid(1, rat, 5_000).unwrap();
    g.place_league_bid(2, rat, 2_000).unwrap();
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    let rat_trade = r.trades.iter().find(|t| t.card == rat).unwrap();
    assert_eq!(rat_trade.buyer, 2, "the amended bid drops below Bob's");
    assert_eq!(rat_trade.price, 2_000, "single copy clears at the highest (Bob's) bid");
    assert_eq!(g.players[&1].held(rat), 0);
    assert_eq!(g.players[&2].held(rat), 1);
}

#[test]
fn amendment_cascades_across_many_cards() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    stock_and_open(
        &mut g,
        &[("Avatar of Eternity", 1), ("Archmage Vesper", 1), ("Bog Rat", 1)],
        1_000,
    );
    // Sole bidder on all three: pays $60 for the mythic, her $50 rare bid is
    // amended to the remaining $40, and her rat bid is amended to $0.
    g.place_league_bid(1, card_id(&g, "Avatar of Eternity"), 6_000).unwrap();
    g.place_league_bid(1, card_id(&g, "Archmage Vesper"), 5_000).unwrap();
    g.place_league_bid(1, card_id(&g, "Bog Rat"), 3_000).unwrap();
    let r = g.close_league_auction(&mut Rng::new(1)).unwrap();
    assert_eq!(r.trades.iter().map(|t| t.price).collect::<Vec<_>>(), vec![6_000, 4_000, 0]);
    assert_eq!(g.players[&1].balance, 2_500, "fully spent, then the stipend");
    assert_eq!(g.players[&1].holdings.values().sum::<u32>(), 3, "won all three cards");
}

#[test]
fn swiss_pairs_winners_together_in_the_next_batch() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let first = scheduled(&g);
    // Seat A wins both matches 2-0.
    for &(id, a, _) in &first {
        g.submit_match_result(a, id, 2, 0, 0).unwrap();
    }
    let (winners, losers): (Vec<u32>, Vec<u32>) =
        (first.iter().map(|m| m.1).collect(), first.iter().map(|m| m.2).collect());

    // Round 2 (posted after round 1's close) pairs the two 3-point players
    // together and the two 0-point players together (rematches are avoided).
    assert_eq!(g.auto_schedule(now + 8 * 86_400), 2);
    let second = scheduled(&g);
    let pair_of = |p: u32| second.iter().find(|m| m.1 == p || m.2 == p).unwrap();
    let w = pair_of(winners[0]);
    assert!(
        (w.1 == winners[0] && w.2 == winners[1]) || (w.1 == winners[1] && w.2 == winners[0]),
        "winners are paired together: {second:?} (winners {winners:?})"
    );
    let l = pair_of(losers[0]);
    assert!(
        (l.1 == losers[0] && l.2 == losers[1]) || (l.1 == losers[1] && l.2 == losers[0]),
        "losers are paired together: {second:?} (losers {losers:?})"
    );
}

#[test]
fn provisional_ties_steer_pairing_of_unreported_players() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let first = scheduled(&g);
    let (m1, m2) = (first[0], first[1]);
    // Only the first match is reported (seat A wins 2-0): its winner sits on
    // 3 points and its loser on 0. The other match stays unreported, so both
    // of its players count as tied on 1 point.
    g.submit_match_result(m1.1, m1.0, 2, 0, 0).unwrap();
    let later = now + 8 * 86_400; // past the N = 1 week play-by deadline
    assert_eq!(g.auto_schedule(later), 2);

    // Closest-points pairing: the winner (3) and loser (0) each draw one of
    // the 1-point unreported players rather than each other.
    let second: Vec<(u64, u32, u32)> =
        scheduled(&g).into_iter().filter(|m| m.0 != m2.0).collect();
    assert_eq!(second.len(), 2);
    let unreported = [m2.1, m2.2];
    for &(_, a, b) in &second {
        assert!(
            unreported.contains(&a) || unreported.contains(&b),
            "each new match includes a provisionally-tied player: {second:?}"
        );
    }
}

#[test]
fn pending_cap_is_validated_and_limits_matches_in_flight_per_player() {
    // Out-of-range caps are rejected at setup.
    for bad in [0u32, 21] {
        let mut cfg = league_cfg();
        cfg.league_pending_per_player = bad;
        assert!(Game::setup(cfg, CardPool::default()).is_err(), "cap {bad} should be rejected");
    }

    // Cap 1: every player gets one upcoming match, then nothing more until
    // theirs resolves — and a freed pair waits for fresh opponents rather
    // than instantly rematching each other.
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2, "one match per player");
    assert_eq!(g.auto_schedule(now), 0);
    let (id, a, _) = scheduled(&g)[0];
    g.submit_match_result(a, id, 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "the freed pair waits — no rematch");
    let (id2, c, _) = scheduled(&g)[0];
    g.submit_match_result(c, id2, 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "round 2 waits for round 1's close even when all report early");
    assert_eq!(g.auto_schedule(now + 8 * 86_400), 2, "fresh cross pairings once the round closes");

    // N = 2 (the default): each player is assigned two matches at once, each
    // with a two-week play-by deadline, to be played whenever suits them.
    let (mut g, now) = league4(2);
    assert_eq!(g.auto_schedule(now), 4, "two matches per player");
    assert_eq!(g.auto_schedule(now), 0);
    let close = mtg_auction::engine::next_league_close(now, FIRST_AUCTION_DAY, 0, 20, 1, 60).unwrap();
    for p in 1..=4u32 {
        let mine: Vec<&Match> = g.ladder.matches.iter().filter(|m| m.involves(p)).collect();
        assert_eq!(mine.len(), 2);
        assert!(
            mine.iter().all(|m| m.slot_start == close),
            "both deadlines land on the auction close"
        );
        assert_ne!(
            (mine[0].a, mine[0].b),
            (mine[1].a, mine[1].b),
            "the two in-flight matches are against different opponents"
        );
    }
}

#[test]
fn the_season_ends_after_rounds_times_n_matches_per_player() {
    let mut cfg = league_cfg();
    cfg.player_names.push("Dave".into());
    cfg.league_pending_per_player = 1;
    cfg.league_rounds = 2; // season = 2 × 1 = 2 matches per player
    let mut g = Game::setup(cfg, CardPool::default()).unwrap();
    let now = ((FIRST_AUCTION_DAY - 7) * 86_400) as u64;

    // Round 1: one match each; round 2 posts together after round 1's close.
    assert_eq!(g.auto_schedule(now), 2);
    for (id, a, _) in scheduled(&g) {
        g.submit_match_result(a, id, 2, 0, 0).unwrap();
    }
    let round2 = now + 8 * 86_400;
    assert_eq!(g.auto_schedule(round2), 2);
    for (id, a, _) in scheduled(&g) {
        g.submit_match_result(a, id, 2, 1, 0).unwrap();
    }

    // Two rounds played: the season is over.
    assert_eq!(g.auto_schedule(now + 15 * 86_400), 0, "no matches beyond the season's rounds");
    assert_eq!(g.ladder.matches.len(), 4);
}

#[test]
fn league_matches_cannot_be_cancelled_and_unreported_sweep_records_draws() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let (id, a, _) = scheduled(&g)[0];
    assert!(g.cancel_match(a, id).unwrap_err().contains("can't be cancelled"));

    // Report one match; leave the other to pass its deadline unreported.
    g.submit_match_result(a, id, 2, 0, 0).unwrap();
    let later = now + 8 * 86_400; // past the N = 1 week play-by deadline
    assert_eq!(g.record_unreported_as_draws(later), 1, "one unreported match swept");
    assert_eq!(g.record_unreported_as_draws(later), 0, "idempotent once recorded");
    let swept = g.ladder.matches.iter().find(|m| m.id != id).unwrap();
    assert_eq!(swept.status, MatchStatus::Completed);
    assert_eq!((swept.a_wins, swept.b_wins), (1, 1), "recorded as a 1-1 draw");

    // The host can still correct a swept (or any completed) result afterwards.
    g.force_match_result(swept.id, 2, 0, 0).unwrap();
    let s = g.standings();
    assert_eq!(s.iter().filter(|st| st.wins == 1).count(), 2);
}

#[test]
fn removing_a_player_returns_cards_and_drops_their_upcoming_matches() {
    let (mut g, now) = league4(1);
    // Give player 4 a card (via the league inventory path), then schedule.
    g.inventory_add(4, pool_of(&[("Bog Rat", 1)])).unwrap();
    let rat = card_id(&g, "Bog Rat");
    assert_eq!(g.auto_schedule(now), 2);

    assert!(g.remove_player(1).is_err(), "the host can't be removed");
    g.remove_player(4).unwrap();
    assert!(!g.players.contains_key(&4));
    assert!(!g.player_order.contains(&4));
    assert!(!g.tokens.contains_key(&4));
    assert_eq!(g.house.held(rat), 1, "their card returns to the house");
    assert!(
        g.ladder.matches.iter().all(|m| !(m.involves(4) && m.status == MatchStatus::Scheduled)),
        "their upcoming matches are dropped"
    );
    assert_eq!(g.standings().len(), 3);
}

#[test]
fn deleting_a_match_reverts_elo_and_recomputes_standings() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let (id, a, _) = scheduled(&g)[0];
    g.submit_match_result(a, id, 2, 0, 0).unwrap();
    assert_eq!(g.players[&a].elo, 1216);
    let winner_points = g.standings().iter().find(|s| s.player == a).unwrap().points;
    assert_eq!(winner_points, 3);

    // Deleting the completed match reverts the ELO and the swiss record.
    g.delete_match(id).unwrap();
    assert!(g.ladder.matches.iter().all(|m| m.id != id));
    assert_eq!(g.players[&a].elo, 1200, "ELO reverted");
    let s = g.standings();
    let st = s.iter().find(|st| st.player == a).unwrap();
    assert_eq!((st.points, st.played), (0, 0), "swiss record recomputed without it");

    // Deleting an upcoming match frees the pair (no ELO involved).
    let (id2, _, _) = scheduled(&g)[0];
    g.delete_match(id2).unwrap();
    assert!(g.delete_match(id2).is_err(), "already gone");
}

#[test]
fn host_can_override_pairings() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);

    // Replace everyone's upcoming matches with an explicit pairing.
    g.override_pairings(&[(1, 3), (2, 4)], now).unwrap();
    let pairs: Vec<(u32, u32)> = scheduled(&g)
        .iter()
        .map(|&(_, a, b)| if a < b { (a, b) } else { (b, a) })
        .collect();
    assert_eq!(pairs, vec![(1, 3), (2, 4)], "old matches replaced by the override");
    let close = mtg_auction::engine::next_league_close(now, FIRST_AUCTION_DAY, 0, 20, 1, 60).unwrap();
    assert!(scheduled(&g).iter().all(|&(id, _, _)| {
        g.ladder.matches.iter().find(|m| m.id == id).unwrap().slot_start == close
    }), "overridden matches carry the normal play-by deadline");

    // Bad input is rejected.
    assert!(g.override_pairings(&[(1, 1)], now).is_err(), "self-pair");
    assert!(g.override_pairings(&[(1, 2), (2, 1)], now).is_err(), "duplicate pair");
    assert!(g.override_pairings(&[(1, 99)], now).is_err(), "unknown player");
}

#[test]
fn reporting_early_never_creates_an_instant_rematch() {
    // The reported bug: a player entered their results and immediately got
    // the same matches again — the scheduler re-paired the freed players
    // against each other because nobody else was available yet.
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    let originals: std::collections::HashSet<(u32, u32)> = scheduled(&g)
        .iter()
        .map(|&(_, a, b)| if a < b { (a, b) } else { (b, a) })
        .collect();

    let (id, a, _) = scheduled(&g)[0];
    g.submit_match_result(a, id, 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "no rematch while fresh opponents exist");
    let (id2, c, _) = scheduled(&g)[0];
    g.submit_match_result(c, id2, 2, 1, 0).unwrap();
    assert_eq!(g.auto_schedule(now), 0, "the next round only posts at the close");
    assert_eq!(g.auto_schedule(now + 8 * 86_400), 2);
    for (_, x, y) in scheduled(&g) {
        let key = if x < y { (x, y) } else { (y, x) };
        assert!(!originals.contains(&key), "new matches use fresh pairings: {key:?}");
    }
}

#[test]
fn tiny_leagues_allow_the_rematches_the_season_requires() {
    // 2 players, 2 rounds of 1: the season is impossible without a rematch,
    // and the meeting quota (⌈2 ÷ 1⌉ = 2) allows exactly that.
    let mut cfg = league_cfg();
    cfg.player_names = vec!["A".into(), "B".into()];
    cfg.league_pending_per_player = 1;
    cfg.league_rounds = 2;
    let mut g = Game::setup(cfg, CardPool::default()).unwrap();
    let now = ((FIRST_AUCTION_DAY - 7) * 86_400) as u64;
    assert_eq!(g.auto_schedule(now), 1);
    let (id, a, _) = scheduled(&g)[0];
    g.submit_match_result(a, id, 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now + 8 * 86_400), 1, "round 2's required rematch is allowed");
    let (id2, a2, _) = scheduled(&g)[0];
    g.submit_match_result(a2, id2, 2, 0, 0).unwrap();
    assert_eq!(g.auto_schedule(now + 15 * 86_400), 0, "season complete");
}

#[test]
fn round_one_pairings_vary_with_the_seed() {
    let mut distinct: std::collections::HashSet<Vec<(u32, u32)>> = Default::default();
    for seed in 0..10 {
        let mut cfg = league_cfg();
        cfg.seed = seed;
        cfg.player_names = (1..=6).map(|i| format!("P{i}")).collect();
        cfg.league_pending_per_player = 1;
        let mut g = Game::setup(cfg, CardPool::default()).unwrap();
        let now = ((FIRST_AUCTION_DAY - 7) * 86_400) as u64;
        assert_eq!(g.auto_schedule(now), 3);
        let mut pairs: Vec<(u32, u32)> = g
            .ladder
            .matches
            .iter()
            .map(|m| if m.a < m.b { (m.a, m.b) } else { (m.b, m.a) })
            .collect();
        pairs.sort();
        distinct.insert(pairs);
    }
    assert!(distinct.len() > 1, "pairings should differ across seeds: {distinct:?}");
}

#[test]
fn every_player_gets_their_full_match_allotment_even_at_scale() {
    // Greedy pairing can strand the last two players when their only mutual
    // option is a pair that already has an open match (6 players reproduces
    // it; 118 is the real-world case). The repair pass must fix it.
    for n in [6usize, 118] {
        let mut cfg = league_cfg();
        cfg.player_names = (1..=n).map(|i| format!("P{i:03}")).collect();
        let mut g = Game::setup(cfg, CardPool::default()).unwrap();
        let now = ((FIRST_AUCTION_DAY - 7) * 86_400) as u64;
        assert_eq!(g.auto_schedule(now), n, "{n} players × 2 in flight = {n} matches");

        let mut per: std::collections::HashMap<u32, u32> = Default::default();
        let mut pairs: std::collections::HashSet<(u32, u32)> = Default::default();
        for m in g.ladder.matches.iter().filter(|m| m.status == MatchStatus::Scheduled) {
            *per.entry(m.a).or_insert(0) += 1;
            *per.entry(m.b).or_insert(0) += 1;
            let key = if m.a < m.b { (m.a, m.b) } else { (m.b, m.a) };
            assert!(pairs.insert(key), "no pair has two concurrent matches ({n} players)");
        }
        assert_eq!(per.len(), n, "everyone has matches");
        assert!(per.values().all(|&c| c == 2), "everyone has exactly two in flight ({n} players)");
    }
}

#[test]
fn standings_tiebreak_falls_through_to_name_and_ignores_elo() {
    let (mut g, now) = league4(1);
    assert_eq!(g.auto_schedule(now), 2);
    // Both seat-A players win 2-0: identical points (3), OMW (both beat a
    // floored 0-1 opponent), and game diff (+2).
    let first = scheduled(&g);
    for &(id, a, _) in &first {
        g.submit_match_result(a, id, 2, 0, 0).unwrap();
    }
    let (w1, w2) = (first[0].1, first[1].1);

    // The full tie breaks alphabetically by name.
    let s = g.standings();
    assert_eq!((s[0].points, s[1].points), (3, 3));
    assert_eq!(s[0].omw, s[1].omw);
    let expect_first =
        if g.players[&w1].name < g.players[&w2].name { w1 } else { w2 };
    assert_eq!(s[0].player, expect_first, "names break the full tie");

    // ELO plays no part in league standings: inflating the other winner's
    // rating must not reorder them (so seeded ratings can't bias the cut).
    let other = if expect_first == w1 { w2 } else { w1 };
    g.players.get_mut(&other).unwrap().elo = 1_400;
    let s = g.standings();
    assert_eq!(s[0].player, expect_first, "ELO is ignored in league standings");
}

/// Push a completed match (winner in seat A, 2-0) directly onto the ladder.
fn play(g: &mut Game, winner: PlayerId, loser: PlayerId, w_wins: u32, l_wins: u32) {
    let id = g.ladder.next_id + 1;
    g.ladder.next_id = id;
    g.ladder.matches.push(Match {
        id,
        a: winner,
        a_name: g.players[&winner].name.clone(),
        b: loser,
        b_name: g.players[&loser].name.clone(),
        slot: 0,
        slot_start: 0,
        status: MatchStatus::Scheduled,
        a_wins: 0,
        b_wins: 0,
        draws: 0,
        proposed_by: None,
        cancelled_by: None,
        a_delta: 0,
        b_delta: 0,
    });
    g.force_match_result(id, w_wins, l_wins, 0).unwrap();
}

#[test]
fn opponents_match_win_percentage_separates_equal_records_at_the_cut() {
    let mut cfg = league_cfg();
    cfg.player_names = vec!["A".into(), "X".into(), "Y".into(), "C".into(), "B".into(), "D".into()];
    let mut g = Game::setup(cfg, CardPool::default()).unwrap();
    let by_name = |g: &Game, n: &str| g.players.values().find(|p| p.name == n).unwrap().id;
    let (a, x, y, c, b, d) =
        (by_name(&g, "A"), by_name(&g, "X"), by_name(&g, "Y"), by_name(&g, "C"), by_name(&g, "B"), by_name(&g, "D"));

    // A goes 2-0 against X and B. X, C, and Y all finish 1-1 with a 0 game
    // diff, but against very different schedules:
    //   X lost to A (MW 1.0) and beat C (MW 0.5)      → OMW 0.75
    //   C lost to X (MW 0.5) and beat Y (MW 0.5)      → OMW 0.50
    //   Y lost to C (MW 0.5) and beat D (0-1, floored) → OMW ~0.42
    play(&mut g, a, x, 2, 0);
    play(&mut g, a, b, 2, 0);
    play(&mut g, x, c, 2, 0);
    play(&mut g, c, y, 2, 0);
    play(&mut g, y, d, 2, 0);

    let s = g.standings();
    let order: Vec<&str> = s.iter().map(|st| st.name.as_str()).collect();
    // Strength of schedule orders the three 1-1 players X > C > Y, and the
    // two 0-1 players B (lost to the undefeated A) above D (lost to a 1-1).
    assert_eq!(order, vec!["A", "X", "C", "Y", "B", "D"]);
    let omw_of = |n: &str| s.iter().find(|st| st.name == n).unwrap().omw;
    assert!((omw_of("X") - 0.75).abs() < 1e-9);
    assert!((omw_of("C") - 0.50).abs() < 1e-9);
    assert!((omw_of("Y") - (0.5 + 1.0 / 3.0) / 2.0).abs() < 1e-9, "floored at 1/3 per opponent");
}

#[test]
fn re_adding_a_card_backfills_missing_metadata_case_insensitively() {
    let mut g = Game::setup(league_cfg(), CardPool::default()).unwrap();
    // A bare card, as created when the metadata lookup failed at add time.
    let bare = PoolCard {
        name: "arcane signet".into(),
        rarity: Rarity::Common,
        image: None,
        ref_price: None,
        type_line: None,
        cmc: None,
        mana_cost: None,
        colors: String::new(),
        color_identity: String::new(),
    };
    g.add_cards(CardPool { exact: Some(vec![(bare, 1)]), ..CardPool::default() }).unwrap();

    // Re-adding the same card with full metadata (and canonical casing) heals
    // the existing entry instead of creating a case-mismatched duplicate.
    let full = PoolCard {
        name: "Arcane Signet".into(),
        rarity: Rarity::Uncommon,
        image: Some("https://img.example/arcane-signet.jpg".into()),
        ref_price: Some(150),
        type_line: Some("Artifact".into()),
        cmc: Some(2.0),
        mana_cost: Some("{2}".into()),
        colors: String::new(),
        color_identity: String::new(),
    };
    g.add_cards(CardPool { exact: Some(vec![(full, 1)]), ..CardPool::default() }).unwrap();

    assert_eq!(g.cards.len(), 1, "no duplicate card is created");
    let c = g.cards.values().next().unwrap().clone();
    assert_eq!(c.name, "Arcane Signet", "canonical casing adopted");
    assert_eq!(c.rarity, Rarity::Uncommon);
    assert!(c.image.is_some());
    assert_eq!(c.ref_price, Some(150));
    assert_eq!(g.house.held(c.id), 2, "both copies live on the one card");

    // A later fetched re-add is authoritative: after fixing the set code, the
    // right printing's rarity and image replace the wrong ones.
    let corrected = PoolCard {
        name: "Arcane Signet".into(),
        rarity: Rarity::Mythic,
        image: Some("https://img.example/arcane-signet-right-set.jpg".into()),
        ref_price: Some(900),
        type_line: Some("Artifact".into()),
        cmc: Some(2.0),
        mana_cost: Some("{2}".into()),
        colors: String::new(),
        color_identity: String::new(),
    };
    g.add_cards(CardPool { exact: Some(vec![(corrected, 1)]), ..CardPool::default() }).unwrap();
    let c = g.cards.values().next().unwrap();
    assert_eq!(c.rarity, Rarity::Mythic, "re-adding corrects the printing");
    assert_eq!(c.image.as_deref(), Some("https://img.example/arcane-signet-right-set.jpg"));
    assert_eq!(c.ref_price, Some(900));
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
