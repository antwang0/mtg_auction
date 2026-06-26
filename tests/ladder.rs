//! Tests for the ELO ladder: availability-driven matchmaking, weekly caps,
//! rematch avoidance, ELO updates, cancellations, and standings.

use mtg_auction::engine::Game;
use mtg_auction::model::{CardPool, Config, MatchStatus, Phase, DAY_BLOCKS};
use std::collections::HashSet;

/// Blocks per day, derived so the tests track [`DAY_BLOCKS`] rather than a literal.
const NB: i64 = DAY_BLOCKS.len() as i64;

const DAY: u64 = 86_400;
/// A Monday (epoch day 102) at 00:00 UTC, so the small day-offsets the tests use
/// stay inside one Monday→Sunday week.
const NOW: u64 = 102 * DAY;

fn game(players: &[&str]) -> Game {
    let cfg = Config {
        player_names: players.iter().map(|s| s.to_string()).collect(),
        set: "sample".into(),
        primary_rounds: 1,
        ..Config::default()
    };
    let mut g = Game::setup(cfg, CardPool::sample()).unwrap();
    // Matchmaking only runs once the primary phase is over; close the single
    // primary round to reach the secondary phase.
    g.close_round().unwrap();
    assert_eq!(g.phase, Phase::Secondary);
    g
}

/// Slot id for a day offset from "today" (NOW) and a block index.
fn slot(day_off: i64, block: i64) -> i64 {
    (((NOW / DAY) as i64) + day_off) * NB + block
}

/// Give every player a weekly target and the same availability.
fn prefs(g: &mut Game, ids: &[u32], per_week: u32, slots: &[i64]) {
    for &p in ids {
        g.set_games_per_week(p, per_week).unwrap();
        g.set_availability(p, slots.to_vec()).unwrap();
    }
}

fn pair(a: u32, b: u32) -> (u32, u32) {
    if a <= b { (a, b) } else { (b, a) }
}

#[test]
fn games_per_week_is_capped() {
    let mut g = game(&["A", "B"]);
    let err = g.set_games_per_week(1, 999).unwrap_err();
    assert!(err.contains("limit"), "{err}");
    g.set_games_per_week(1, 3).unwrap(); // within the default cap of 5
}

#[test]
fn schedules_available_players_in_a_slot() {
    let mut g = game(&["A", "B", "C", "D"]);
    prefs(&mut g, &[1, 2, 3, 4], 1, &[slot(1, 0)]);
    assert_eq!(g.auto_schedule(NOW), 2);

    let s = slot(1, 0);
    assert!(g.ladder.matches.iter().all(|m| m.slot == s && m.status == MatchStatus::Scheduled));
    let mut seen = HashSet::new();
    for m in &g.ladder.matches {
        assert!(seen.insert(m.a), "a player is double-booked in one slot");
        assert!(seen.insert(m.b));
    }
    assert_eq!(seen.len(), 4);
}

#[test]
fn respects_the_weekly_cap() {
    let mut g = game(&["A", "B"]);
    // Plenty of shared slots in one week, but each wants only one game.
    let slots: Vec<i64> = (0..NB).map(|b| slot(1, b)).chain((0..NB).map(|b| slot(2, b))).collect();
    prefs(&mut g, &[1, 2], 1, &slots);
    assert_eq!(g.auto_schedule(NOW), 1, "a weekly cap of 1 yields a single match");
}

#[test]
fn weekly_quota_resets_on_monday() {
    let mut g = game(&["A", "B"]);
    // A slot on Sunday and one the following Monday fall in different weeks, so
    // a cap of one game per week still allows a match in each.
    prefs(&mut g, &[1, 2], 1, &[slot(6, 0), slot(7, 0)]);
    assert_eq!(g.auto_schedule(NOW), 2);
}

#[test]
fn only_schedules_future_slots() {
    let mut g = game(&["A", "B"]);
    // A slot in the past (yesterday) is never scheduled.
    prefs(&mut g, &[1, 2], 2, &[slot(-1, 0)]);
    assert_eq!(g.auto_schedule(NOW), 0);
}

#[test]
fn avoids_rematches_across_a_week() {
    let mut g = game(&["A", "B", "C", "D"]);
    // Three slots in the same week, everyone free, three games each.
    prefs(&mut g, &[1, 2, 3, 4], 3, &[slot(1, 0), slot(2, 0), slot(3, 0)]);
    assert_eq!(g.auto_schedule(NOW), 6); // 3 slots × 2 matches

    let mut pairs = HashSet::new();
    for m in &g.ladder.matches {
        assert!(pairs.insert(pair(m.a, m.b)), "{} vs {} scheduled twice", m.a, m.b);
    }
    assert_eq!(pairs.len(), 6, "all 6 distinct pairs of 4 players, each once");
}

#[test]
fn pairs_closest_elo_first() {
    let mut g = game(&["A", "B", "C", "D"]);
    g.players.get_mut(&1).unwrap().elo = 1000;
    g.players.get_mut(&2).unwrap().elo = 1100;
    g.players.get_mut(&3).unwrap().elo = 1200;
    g.players.get_mut(&4).unwrap().elo = 1300;
    prefs(&mut g, &[1, 2, 3, 4], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);

    let played = |x: u32, y: u32| g.ladder.matches.iter().any(|m| pair(m.a, m.b) == pair(x, y));
    assert!(played(1, 2) && played(3, 4), "closest-rated players are paired");
}

#[test]
fn confirmed_result_applies_symmetric_elo() {
    let mut g = game(&["A", "B", "C", "D"]);
    prefs(&mut g, &[1, 2, 3, 4], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let m = g.ladder.matches[0].clone();

    g.propose_match_result(m.a, m.id, 2, 0, 0).unwrap();
    assert!(g.confirm_match_result(m.a, m.id).unwrap_err().contains("opponent"));
    g.confirm_match_result(m.b, m.id).unwrap();

    // Even ratings (1200 each), K=32: winner +16, loser −16.
    assert_eq!(g.players[&m.a].elo, 1216);
    assert_eq!(g.players[&m.b].elo, 1184);
    let done = g.ladder.matches.iter().find(|x| x.id == m.id).unwrap();
    assert_eq!(done.status, MatchStatus::Completed);
    assert!(done.proposed_by.is_none());
}

#[test]
fn cancel_penalises_and_frees_the_weekly_slot() {
    let mut g = game(&["A", "B"]);
    prefs(&mut g, &[1, 2], 1, &[slot(1, 0), slot(1, 1)]);
    assert_eq!(g.auto_schedule(NOW), 1);

    let id = g.ladder.matches[0].id;
    let canceller = g.ladder.matches[0].a;
    g.cancel_match(canceller, id).unwrap();
    assert_eq!(g.players[&canceller].elo, 1200 - g.config.cancel_penalty);
    assert_eq!(g.ladder.matches[0].status, MatchStatus::Cancelled);

    // The cancelled match no longer consumes the weekly quota, so a replacement
    // is scheduled on the next pass.
    assert_eq!(g.auto_schedule(NOW), 1);
    assert_eq!(g.ladder.matches.len(), 2);
    assert_eq!(g.standings().iter().find(|s| s.player == canceller).unwrap().cancellations, 1);
}

#[test]
fn finished_match_cannot_be_cancelled() {
    let mut g = game(&["A", "B"]);
    prefs(&mut g, &[1, 2], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let m = g.ladder.matches[0].clone();
    g.propose_match_result(m.a, m.id, 2, 0, 0).unwrap();
    g.confirm_match_result(m.b, m.id).unwrap();
    assert!(g.cancel_match(m.a, m.id).unwrap_err().contains("finished"));
}

#[test]
fn only_participants_can_report_or_cancel() {
    let mut g = game(&["A", "B", "C", "D"]);
    prefs(&mut g, &[1, 2, 3, 4], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let m = g.ladder.matches[0].clone();
    let outsider = (1..=4).find(|p| !m.involves(*p)).unwrap();
    assert!(g.propose_match_result(outsider, m.id, 2, 0, 0).unwrap_err().contains("not playing"));
    assert!(g.cancel_match(outsider, m.id).unwrap_err().contains("not playing"));
}

#[test]
fn host_override_skips_confirmation() {
    let mut g = game(&["A", "B"]);
    prefs(&mut g, &[1, 2], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let id = g.ladder.matches[0].id;
    g.force_match_result(id, 2, 1, 0).unwrap();
    assert_eq!(g.ladder.matches[0].status, MatchStatus::Completed);
}

#[test]
fn standings_ranked_by_elo() {
    let mut g = game(&["A", "B", "C"]);
    g.players.get_mut(&1).unwrap().elo = 1250;
    g.players.get_mut(&2).unwrap().elo = 1300;
    g.players.get_mut(&3).unwrap().elo = 1100;
    let s = g.standings();
    assert_eq!((s[0].player, s[1].player, s[2].player), (2, 1, 3));
    assert_eq!(s.iter().map(|x| x.rank).collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[test]
fn availability_input_is_capped() {
    let mut g = game(&["A", "B"]);
    let huge: Vec<i64> = (0..5000).collect();
    assert!(g.set_availability(1, huge).unwrap_err().contains("too many"));
    g.set_availability(1, (0..100).collect()).unwrap(); // a reasonable list is fine
}

#[test]
fn stale_matches_expire_as_no_shows() {
    let mut g = game(&["A", "B"]);
    prefs(&mut g, &[1, 2], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    assert_eq!(g.ladder.matches.len(), 1);
    let start = g.ladder.matches[0].slot_start;

    // Before the grace period passes, it stays scheduled.
    assert_eq!(g.expire_stale_matches(start + 3600), 0);
    assert_eq!(g.ladder.matches[0].status, MatchStatus::Scheduled);

    // A day past the slot, it's a no-show: expired, no ELO change.
    assert_eq!(g.expire_stale_matches(start + 2 * 86_400), 1);
    assert_eq!(g.ladder.matches[0].status, MatchStatus::Expired);
    assert_eq!(g.players[&1].elo, 1200);
    assert_eq!(g.players[&2].elo, 1200);
    assert_eq!(g.standings().iter().find(|s| s.player == 1).unwrap().scheduled, 0);

    // Expired matches free the weekly quota, so the pair can be rescheduled.
    assert_eq!(g.auto_schedule(NOW), 1);
}

#[test]
fn host_can_record_an_expired_match_but_not_a_completed_one() {
    let mut g = game(&["A", "B"]);
    prefs(&mut g, &[1, 2], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let id = g.ladder.matches[0].id;
    let start = g.ladder.matches[0].slot_start;
    g.expire_stale_matches(start + 2 * 86_400);
    // Host resolves the no-show retroactively.
    g.force_match_result(id, 2, 0, 0).unwrap();
    assert_eq!(g.ladder.matches[0].status, MatchStatus::Completed);
    // ELO was applied exactly once; a second override is rejected.
    assert_eq!(g.players[&g.ladder.matches[0].a].elo, 1216);
    assert!(g.force_match_result(id, 1, 1, 0).unwrap_err().contains("already final"));
}

#[test]
fn ladder_survives_serde_round_trip() {
    let mut g = game(&["A", "B", "C", "D"]);
    prefs(&mut g, &[1, 2, 3, 4], 1, &[slot(1, 0)]);
    g.auto_schedule(NOW);
    let id = g.ladder.matches[0].id;
    g.propose_match_result(g.ladder.matches[0].a, id, 2, 1, 0).unwrap();
    let json = serde_json::to_string(&g).unwrap();
    let g2: Game = serde_json::from_str(&json).unwrap();
    assert_eq!(g2.ladder.matches.len(), g.ladder.matches.len());
    assert_eq!(g2.players[&1].elo, g.players[&1].elo);
    assert_eq!(g2.ladder.games_per_week.get(&1), Some(&1));
}
