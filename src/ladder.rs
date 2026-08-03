//! Matchmaking and standings, in two flavours.
//!
//! Standard games run an ELO ladder: players publish weekly availability and a
//! per-week game target, the system matches the closest-rated, least-recently-
//! met available players into calendar slots, and a player may cancel a
//! scheduled match for an ELO penalty.
//!
//! League games run deadline-based swiss instead: matches are *assigned* (no
//! availability or slots), each with a play-by deadline; pairing prefers equal
//! swiss records; standings rank by points (3 per match win, 1 for taking a
//! game without winning), then opponents' match-win percentage, then game
//! difference. Cancelling is not allowed.

use crate::engine::Game;
use crate::model::*;
use std::collections::{HashMap, HashSet};

const N_BLOCKS: i64 = DAY_BLOCKS.len() as i64;

/// Upper bound on how many availability slots a player may submit, so a single
/// request can't store (and persist) an unbounded list.
const MAX_AVAIL_SLOTS: usize = 2000;

/// Weekly match target for a player who hasn't set one — so games schedule by
/// default and a player opts out by setting it to 0.
pub const DEFAULT_GAMES_PER_WEEK: u32 = 1;

/// The Unix epoch second a slot begins, given the game's configured block start
/// hours (see [`Config::ladder_block_hours`]). Falls back to [`DAY_BLOCKS`] for
/// any block index the hours slice doesn't cover.
pub fn slot_start_epoch(slot: i64, hours: &[u32]) -> u64 {
    let day = slot.div_euclid(N_BLOCKS).max(0);
    let block = slot.rem_euclid(N_BLOCKS) as usize;
    let hour = hours.get(block).copied().unwrap_or(DAY_BLOCKS[block]);
    day as u64 * 86_400 + hour as u64 * 3_600
}

/// The number of distinct recurring "weekly slots": one per (weekday, block).
const N_WEEKLY_SLOTS: u32 = 7 * DAY_BLOCKS.len() as u32;

/// The recurring "weekly slot" a concrete slot maps to:
/// `weekday * blocks + block`, with weekday 0 = Sunday. A recurring pattern is a
/// set of these, matched against every future slot.
pub fn weekly_slot(slot: i64) -> u32 {
    let weekday = (slot.div_euclid(N_BLOCKS) + 4).rem_euclid(7); // epoch day 0 = Thursday
    let block = slot.rem_euclid(N_BLOCKS);
    (weekday * N_BLOCKS + block) as u32
}

/// The calendar week a slot falls in, with weeks running Monday→Sunday (UTC).
/// Epoch day 0 (1970-01-01) is a Thursday, so we shift by 3 days before
/// dividing so a week boundary lands on Monday rather than Thursday.
fn week_of(slot: i64) -> i64 {
    (slot.div_euclid(N_BLOCKS) + 3).div_euclid(7)
}

/// An unordered pair key so "A vs B" and "B vs A" collapse together.
fn pair_key(a: PlayerId, b: PlayerId) -> (PlayerId, PlayerId) {
    if a <= b { (a, b) } else { (b, a) }
}

impl Game {
    // ---- player preferences -------------------------------------------------

    /// Replace a player's availability with the given slot ids (kept sorted and
    /// de-duplicated so the scheduler can binary-search them).
    pub fn set_availability(&mut self, player: PlayerId, mut slots: Vec<i64>) -> Result<(), String> {
        if !self.players.contains_key(&player) {
            return Err("no such player".into());
        }
        if slots.len() > MAX_AVAIL_SLOTS {
            return Err(format!("too many availability slots (max {MAX_AVAIL_SLOTS})"));
        }
        slots.retain(|&s| s >= 0);
        slots.sort_unstable();
        slots.dedup();
        self.ladder.availability.insert(player, slots);
        Ok(())
    }

    /// Replace a player's recurring weekly availability with the given weekly-slot
    /// indices (`weekday * blocks + block`), kept sorted and de-duplicated. These
    /// apply to every future week, on top of any explicit availability slots.
    pub fn set_recurring(&mut self, player: PlayerId, mut slots: Vec<u32>) -> Result<(), String> {
        if !self.players.contains_key(&player) {
            return Err("no such player".into());
        }
        slots.retain(|&s| s < N_WEEKLY_SLOTS);
        slots.sort_unstable();
        slots.dedup();
        self.ladder.recurring.insert(player, slots);
        Ok(())
    }

    /// Whether a player is available for a slot: an explicit availability slot,
    /// or one matching their recurring weekly pattern.
    fn is_available(&self, player: PlayerId, slot: i64) -> bool {
        self.ladder.availability.get(&player).is_some_and(|s| s.binary_search(&slot).is_ok())
            || self.ladder.recurring.get(&player).is_some_and(|w| w.binary_search(&weekly_slot(slot)).is_ok())
    }

    /// Set how many matches a player wants scheduled per week (0..=max).
    pub fn set_games_per_week(&mut self, player: PlayerId, n: u32) -> Result<(), String> {
        if !self.players.contains_key(&player) {
            return Err("no such player".into());
        }
        let max = self.config.max_games_per_week;
        if n > max {
            return Err(format!("the limit is {max} games per week"));
        }
        self.ladder.games_per_week.insert(player, n);
        Ok(())
    }

    /// A player's weekly match target. Players who haven't chosen one default to
    /// [`DEFAULT_GAMES_PER_WEEK`]; setting it to 0 opts out.
    pub fn quota(&self, player: PlayerId) -> u32 {
        self.ladder.games_per_week.get(&player).copied().unwrap_or(DEFAULT_GAMES_PER_WEEK)
    }

    // ---- automatic matchmaking ---------------------------------------------
}

/// Pairing preference for a league candidate pair, compared lexicographically:
/// combined in-flight count (top up the least-assigned first), prior meetings,
/// match-point and game-difference gaps, then the shuffled ranks and player
/// ids for a deterministic total order.
type PairKey = (u32, u32, i64, i64, u32, u32, PlayerId, PlayerId);

impl Game {

    /// Whether a match has been played (its slot has started) but has no final
    /// result yet. Such matches count as 1-1 ties for matchmaking until the
    /// real result is added, which can happen at any later time.
    fn is_unreported(m: &Match, now_epoch: u64) -> bool {
        matches!(m.status, MatchStatus::Scheduled if m.slot_start <= now_epoch)
            || m.status == MatchStatus::Expired // legacy no-shows from old saves
    }

    /// Swiss score per player: match points ([`match_points`]: 3 per win, 1
    /// for taking a game without winning) and game difference (games won −
    /// lost, so a 2-0 outranks a 2-1). Matches whose slot has passed without
    /// a reported result count as 1-1 ties until the result is added. Used to
    /// pair and rank league play.
    fn swiss_scores(&self, now_epoch: u64) -> HashMap<PlayerId, (i64, i64)> {
        let mut scores: HashMap<PlayerId, (i64, i64)> =
            self.player_order.iter().map(|&p| (p, (0, 0))).collect();
        for m in &self.ladder.matches {
            if Self::is_unreported(m, now_epoch) {
                if let Some(s) = scores.get_mut(&m.a) { s.0 += 1; }
                if let Some(s) = scores.get_mut(&m.b) { s.0 += 1; }
                continue;
            }
            if m.status != MatchStatus::Completed {
                continue;
            }
            let (pa, pb) = (
                match_points(m.a_wins, m.b_wins),
                match_points(m.b_wins, m.a_wins),
            );
            if let Some(s) = scores.get_mut(&m.a) {
                s.0 += pa;
                s.1 += m.a_wins as i64 - m.b_wins as i64;
            }
            if let Some(s) = scores.get_mut(&m.b) {
                s.0 += pb;
                s.1 += m.b_wins as i64 - m.a_wins as i64;
            }
        }
        scores
    }

    /// Schedule new matches, returning how many were created. League games use
    /// deadline-based swiss assignment ([`Self::league_schedule`]); standard
    /// games pair available players into calendar slots, preferring the fewest
    /// prior meetings and then the closest ELO, respecting one match per player
    /// per slot and each player's weekly target. Idempotent until availability,
    /// results, or the calendar change, so it is safe to call on a timer.
    pub fn auto_schedule(&mut self, now_epoch: u64) -> usize {
        // Matchmaking only begins once the primary (bank-issue) phase is over.
        if matches!(self.phase, Phase::Setup | Phase::Primary) || self.players.len() < 2 {
            return 0;
        }
        if self.is_league() {
            return self.league_schedule(now_epoch);
        }
        let window = self.config.schedule_window_days.max(1) as i64;
        let today = (now_epoch / 86_400) as i64;
        let first_slot = today * N_BLOCKS;
        // Cover exactly `window` days starting today: days [today, today+window),
        // i.e. slots up to (but not including) (today+window)*N_BLOCKS.
        let last_slot = (today + window) * N_BLOCKS;

        // Snapshot weekly targets so the scheduling loop doesn't borrow `self`
        // while it also pushes new matches.
        let quotas: HashMap<PlayerId, u32> =
            self.player_order.iter().map(|&p| (p, self.quota(p))).collect();

        // Reconstruct history from existing matches: prior meetings (any status,
        // so a cancelled pair isn't instantly re-matched), per-week games used
        // (excluding cancellations), and which (player, slot)s are taken.
        let mut meetings: HashMap<(PlayerId, PlayerId), u32> = HashMap::new();
        let mut used: HashMap<(i64, PlayerId), u32> = HashMap::new();
        let mut booked: HashSet<(PlayerId, i64)> = HashSet::new();
        for m in &self.ladder.matches {
            *meetings.entry(pair_key(m.a, m.b)).or_insert(0) += 1;
            // Only live matches (still on, or already played) hold a slot and
            // consume a weekly game; cancelled/expired ones free both up.
            if matches!(m.status, MatchStatus::Scheduled | MatchStatus::Completed) {
                let w = week_of(m.slot);
                *used.entry((w, m.a)).or_insert(0) += 1;
                *used.entry((w, m.b)).or_insert(0) += 1;
                booked.insert((m.a, m.slot));
                booked.insert((m.b, m.slot));
            }
        }

        let block_hours = self.config.ladder_block_hours.clone();
        let mut created = 0usize;
        for slot in first_slot..last_slot {
            let slot_epoch = slot_start_epoch(slot, &block_hours);
            if slot_epoch <= now_epoch {
                continue; // only schedule strictly-future slots
            }
            let w = week_of(slot);
            let has_quota = |p: PlayerId, used: &HashMap<(i64, PlayerId), u32>| {
                let q = quotas.get(&p).copied().unwrap_or(0);
                q > 0 && used.get(&(w, p)).copied().unwrap_or(0) < q
            };
            let avail: Vec<PlayerId> = self
                .player_order
                .iter()
                .copied()
                .filter(|&p| {
                    has_quota(p, &used) && !booked.contains(&(p, slot)) && self.is_available(p, slot)
                })
                .collect();
            if avail.len() < 2 {
                continue;
            }

            // Rank candidate pairs: fewest meetings first, then closest ELO.
            let mut cands: Vec<(u32, i64, PlayerId, PlayerId)> = Vec::new();
            for i in 0..avail.len() {
                for j in (i + 1)..avail.len() {
                    let (a, b) = (avail[i], avail[j]);
                    let met = meetings.get(&pair_key(a, b)).copied().unwrap_or(0);
                    let diff = (self.players[&a].elo - self.players[&b].elo).abs();
                    cands.push((met, diff, a, b));
                }
            }
            cands.sort();

            let mut taken: HashSet<PlayerId> = HashSet::new();
            for (_, _, a, b) in cands {
                if taken.contains(&a) || taken.contains(&b) {
                    continue;
                }
                if !has_quota(a, &used) || !has_quota(b, &used) {
                    continue;
                }
                let id = self.ladder.next_id + 1;
                self.ladder.next_id = id;
                self.ladder.matches.push(Match {
                    id,
                    a,
                    a_name: self.players[&a].name.clone(),
                    b,
                    b_name: self.players[&b].name.clone(),
                    slot,
                    slot_start: slot_start_epoch(slot, &block_hours),
                    status: MatchStatus::Scheduled,
                    a_wins: 0,
                    b_wins: 0,
                    draws: 0,
                    proposed_by: None,
                    cancelled_by: None,
                    a_delta: 0,
                    b_delta: 0,
                });
                created += 1;
                taken.insert(a);
                taken.insert(b);
                *used.entry((w, a)).or_insert(0) += 1;
                *used.entry((w, b)).or_insert(0) += 1;
                booked.insert((a, slot));
                booked.insert((b, slot));
                *meetings.entry(pair_key(a, b)).or_insert(0) += 1;
            }
        }
        created
    }

    /// The play-by deadline for a league match assigned now: the next close
    /// on the league's auction series (its cadence continues past the last
    /// auction for the final round), so match deadlines and auction closes
    /// share the same instant. Falls back to N weeks out if no series is
    /// configured.
    fn league_match_deadline(&self, now_epoch: u64) -> u64 {
        let cfg = &self.config;
        crate::engine::next_league_close(
            now_epoch,
            cfg.league_first_auction_day,
            0, // ignore the last-auction cutoff: the final round keeps the cadence
            cfg.league_close_hour,
            cfg.league_period_weeks,
            cfg.league_tz_offset_mins,
        )
        .unwrap_or(now_epoch + cfg.league_pending_per_player.max(1) as u64 * 7 * 86_400)
    }

    /// League matchmaking: strictly synchronized rounds. The season is
    /// [`Config::league_rounds`] rounds of [`Config::league_pending_per_player`]
    /// matches per player; round r's matches are all assigned once round r−1's
    /// close has passed and all share round r's close as their play-by
    /// deadline (an unreported match past its deadline counts as a provisional
    /// tie until the result is added — it never delays the next round).
    /// Pairing prefers the fewest prior meetings, then the closest swiss score
    /// (match points, then game difference); rematches are forbidden up to the
    /// per-pair quota the season size forces (1 in any realistically-sized
    /// league). No calendar slots or availability — [`Match::slot_start`]
    /// holds the deadline.
    fn league_schedule(&mut self, now_epoch: u64) -> usize {
        // Matchmaking opens on the configured day (0 = immediately).
        if self.config.league_matchmaking_start_day > 0 {
            let start = (self.config.league_matchmaking_start_day * 86_400
                - self.config.league_tz_offset_mins as i64 * 60)
                .max(0) as u64;
            if now_epoch < start {
                return 0;
            }
        }
        let cap = self.config.league_pending_per_player.max(1);
        let rounds = self.config.league_rounds.max(1) as u64;
        // A pair may meet at most ⌈season ÷ (players − 1)⌉ times — no
        // rematches while fresh opponents can still cover the season.
        let season_cap = rounds as u32 * cap;
        let max_met = season_cap.div_ceil(self.players.len().saturating_sub(1).max(1) as u32).max(1);

        // Which round are we in? Round k runs up to the k-th close of the
        // series; once the final round's close has passed, the season is over.
        let cfg = &self.config;
        let Some(first_close) = crate::engine::next_league_close(
            0,
            cfg.league_first_auction_day,
            0,
            cfg.league_close_hour,
            cfg.league_period_weeks,
            cfg.league_tz_offset_mins,
        ) else {
            return 0;
        };
        let period_secs = cfg.league_period_weeks.max(1) as u64 * 7 * 86_400;
        let round = if now_epoch < first_close {
            1
        } else {
            (now_epoch - first_close) / period_secs + 2
        };
        if round > rounds {
            return 0; // the season is over
        }
        let deadline = first_close + (round - 1) * period_secs;

        // This round's shortfall per player, and the pairs already made in it
        // (matches are tagged with the round by their shared deadline).
        // Cancelled matches don't count — the pair gets re-matched.
        let mut meetings: HashMap<(PlayerId, PlayerId), u32> = HashMap::new();
        let mut need: HashMap<PlayerId, u32> =
            self.player_order.iter().map(|&p| (p, cap)).collect();
        let mut round_pairs: HashSet<(PlayerId, PlayerId)> = HashSet::new();
        for m in &self.ladder.matches {
            *meetings.entry(pair_key(m.a, m.b)).or_insert(0) += 1;
            if m.slot_start == deadline && m.status != MatchStatus::Cancelled {
                for p in [m.a, m.b] {
                    if let Some(n) = need.get_mut(&p) {
                        *n = n.saturating_sub(1);
                    }
                }
                round_pairs.insert(pair_key(m.a, m.b));
            }
        }
        let swiss = self.swiss_scores(now_epoch);

        // Shuffle the tie-break order so equal candidates (same need, meetings
        // and swiss score — e.g. everyone at round one) pair randomly instead
        // of by roster order. Seeded from the game seed and the match counter,
        // so a given state pairs reproducibly but each assignment wave draws a
        // fresh order.
        let mut order = self.player_order.clone();
        let mut rng = crate::engine::Rng::new(self.config.seed ^ self.ladder.next_id.wrapping_mul(0x9E37_79B9));
        rng.shuffle(&mut order);
        let rank: HashMap<PlayerId, u32> =
            order.iter().enumerate().map(|(i, &p)| (p, i as u32)).collect();

        let mut created = 0usize;
        loop {
            // Players still short of this round's allotment.
            let elig: Vec<PlayerId> = self
                .player_order
                .iter()
                .copied()
                .filter(|&p| need.get(&p).copied().unwrap_or(0) > 0)
                .collect();
            // The best eligible pair: the least-assigned players first (so
            // everyone is topped up round-robin before anyone gets a further
            // match of the round), then fewest prior meetings, closest match
            // points, and closest game difference; full ties break by the
            // shuffled player order. A pair never meets twice in one round.
            let mut best: Option<PairKey> = None;
            for i in 0..elig.len() {
                for j in (i + 1)..elig.len() {
                    let (a, b) = (elig[i], elig[j]);
                    if round_pairs.contains(&pair_key(a, b)) {
                        continue;
                    }
                    let met = meetings.get(&pair_key(a, b)).copied().unwrap_or(0);
                    if met >= max_met {
                        continue; // no rematches beyond the season quota
                    }
                    let done = (cap - need[&a]) + (cap - need[&b]);
                    let (ra, rb) = (rank[&a], rank[&b]);
                    let key = (
                        done,
                        met,
                        (swiss[&a].0 - swiss[&b].0).abs(),
                        (swiss[&a].1 - swiss[&b].1).abs(),
                        ra.min(rb),
                        ra.max(rb),
                        a,
                        b,
                    );
                    if best.is_none_or(|k| key < k) {
                        best = Some(key);
                    }
                }
            }
            let Some((.., a, b)) = best else { break };
            self.push_league_match(a, b, deadline);
            created += 1;
            for p in [a, b] {
                *need.get_mut(&p).expect("eligible players are indexed") -= 1;
            }
            *meetings.entry(pair_key(a, b)).or_insert(0) += 1;
            round_pairs.insert(pair_key(a, b));
        }

        // Repair pass: greedy matching can strand a final pair blocked by the
        // no-rematch rules. Since the whole round is assigned at once, fix it
        // now by swapping partners with another of this round's matches —
        // dissolve (c, d) and create (a, c) + (b, d) when both are legal —
        // preferring rematch-free swaps, then relaxing to any swap, and as a
        // last resort allowing a direct rematch (which beats a missing match).
        loop {
            let left: Vec<PlayerId> = self
                .player_order
                .iter()
                .copied()
                .filter(|&p| need.get(&p).copied().unwrap_or(0) > 0)
                .collect();
            if left.len() < 2 {
                break;
            }
            let (a, b) = (left[0], left[1]);
            let mut plan: Option<(u64, PlayerId, PlayerId)> = None;
            'search: for relax in [false, true] {
                for m in &self.ladder.matches {
                    if m.status != MatchStatus::Scheduled || m.slot_start != deadline {
                        continue;
                    }
                    let (c, d) = (m.a, m.b);
                    if c == a || c == b || d == a || d == b {
                        continue;
                    }
                    let ok = |p: PlayerId, q: PlayerId| {
                        !round_pairs.contains(&pair_key(p, q))
                            && (relax || meetings.get(&pair_key(p, q)).copied().unwrap_or(0) < max_met)
                    };
                    if ok(a, c) && ok(b, d) {
                        plan = Some((m.id, c, d));
                        break 'search;
                    }
                    if ok(a, d) && ok(b, c) {
                        plan = Some((m.id, d, c));
                        break 'search;
                    }
                }
            }
            let Some((old_id, x, y)) = plan else {
                // No swap available: pair them directly unless they already
                // met this round (then one of them simply sits short).
                if round_pairs.contains(&pair_key(a, b)) {
                    break;
                }
                self.push_league_match(a, b, deadline);
                created += 1;
                for p in [a, b] {
                    if let Some(n) = need.get_mut(&p) {
                        *n = n.saturating_sub(1);
                    }
                }
                *meetings.entry(pair_key(a, b)).or_insert(0) += 1;
                round_pairs.insert(pair_key(a, b));
                continue;
            };
            // Dissolve the old (x, y) match…
            let idx = self.ladder.matches.iter().position(|m| m.id == old_id).expect("plan references a live match");
            let old = self.ladder.matches.remove(idx);
            round_pairs.remove(&pair_key(old.a, old.b));
            if let Some(n) = meetings.get_mut(&pair_key(old.a, old.b)) { *n = n.saturating_sub(1); }
            for p in [old.a, old.b] {
                if let Some(n) = need.get_mut(&p) { *n += 1; }
            }
            created = created.saturating_sub(1);
            // …and re-pair everyone: (a, x) and (b, y).
            for (p, q) in [(a, x), (b, y)] {
                self.push_league_match(p, q, deadline);
                created += 1;
                for r in [p, q] {
                    if let Some(n) = need.get_mut(&r) { *n = n.saturating_sub(1); }
                }
                *meetings.entry(pair_key(p, q)).or_insert(0) += 1;
                round_pairs.insert(pair_key(p, q));
            }
        }
        created
    }

    /// Host: manually override pairings. Every listed player's *upcoming*
    /// matches are removed (their freed opponents get re-paired by the
    /// scheduler on its next pass), and the given pairs are created with the
    /// normal play-by deadline. League games only.
    pub fn override_pairings(&mut self, pairs: &[(PlayerId, PlayerId)], now_epoch: u64) -> Result<usize, String> {
        if !self.is_league() {
            return Err("manual pairings are only for league games".into());
        }
        if pairs.is_empty() {
            return Err("no pairings given".into());
        }
        let mut seen: HashSet<(PlayerId, PlayerId)> = HashSet::new();
        for &(a, b) in pairs {
            if a == b {
                return Err("a player can't be paired with themselves".into());
            }
            if !self.players.contains_key(&a) || !self.players.contains_key(&b) {
                return Err("unknown player in the pairings".into());
            }
            if !seen.insert(pair_key(a, b)) {
                return Err("the same pair is listed twice".into());
            }
        }
        let listed: HashSet<PlayerId> = pairs.iter().flat_map(|&(a, b)| [a, b]).collect();
        self.ladder.matches.retain(|m| {
            !(m.status == MatchStatus::Scheduled
                && m.slot_start > now_epoch
                && (listed.contains(&m.a) || listed.contains(&m.b)))
        });
        let deadline = self.league_match_deadline(now_epoch);
        for &(a, b) in pairs {
            self.push_league_match(a, b, deadline);
        }
        Ok(pairs.len())
    }

    /// Append a new scheduled league match with the given play-by deadline.
    fn push_league_match(&mut self, a: PlayerId, b: PlayerId, deadline: u64) -> u64 {
        let id = self.ladder.next_id + 1;
        self.ladder.next_id = id;
        self.ladder.matches.push(Match {
            id,
            a,
            a_name: self.players[&a].name.clone(),
            b,
            b_name: self.players[&b].name.clone(),
            slot: 0, // league matches aren't tied to a calendar slot
            slot_start: deadline,
            status: MatchStatus::Scheduled,
            a_wins: 0,
            b_wins: 0,
            draws: 0,
            proposed_by: None,
            cancelled_by: None,
            a_delta: 0,
            b_delta: 0,
        });
        id
    }

    // ---- result reporting (propose / confirm / host override) --------------

    fn match_mut(&mut self, id: u64) -> Result<&mut Match, String> {
        self.ladder
            .matches
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| "no such match".to_string())
    }

    /// A player proposes the result for their own match; it stays pending until
    /// the opponent confirms (or the host overrides).
    pub fn propose_match_result(&mut self, reporter: PlayerId, id: u64, a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
        self.validate_result(a_wins, b_wins, draws)?;
        let m = self.match_mut(id)?;
        match m.status {
            MatchStatus::Completed => return Err("that match is already final".into()),
            MatchStatus::Cancelled => return Err("that match was cancelled".into()),
            // Unreported matches never expire — a result can always be added
            // later (Expired only survives in saves from older versions).
            MatchStatus::Scheduled | MatchStatus::Expired => {}
        }
        if !m.involves(reporter) {
            return Err("you are not playing in that match".into());
        }
        m.a_wins = a_wins;
        m.b_wins = b_wins;
        m.draws = draws;
        m.proposed_by = Some(reporter);
        Ok(())
    }

    /// The opponent confirms a pending result, finalising it and applying ELO.
    pub fn confirm_match_result(&mut self, confirmer: PlayerId, id: u64) -> Result<(), String> {
        let (a, b, a_wins, b_wins) = {
            let m = self.match_mut(id)?;
            match m.status {
                MatchStatus::Completed => return Err("that result is already final".into()),
                MatchStatus::Cancelled => return Err("that match was cancelled".into()),
                MatchStatus::Scheduled | MatchStatus::Expired => {}
            }
            let proposer = m.proposed_by.ok_or("there is no result to confirm yet")?;
            if !m.involves(confirmer) {
                return Err("you are not playing in that match".into());
            }
            if confirmer == proposer {
                return Err("your opponent has to confirm the result you reported".into());
            }
            (m.a, m.b, m.a_wins, m.b_wins)
        };
        self.complete_match(id, a, b, a_wins, b_wins);
        Ok(())
    }

    /// A participant records the result for their own match, finalising it
    /// immediately (no opponent confirmation) and applying the ELO change.
    pub fn submit_match_result(&mut self, reporter: PlayerId, id: u64, a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
        self.validate_result(a_wins, b_wins, draws)?;
        let (a, b) = {
            let m = self.match_mut(id)?;
            match m.status {
                MatchStatus::Completed => return Err("that match is already final — ask the host to correct it".into()),
                MatchStatus::Cancelled => return Err("that match was cancelled".into()),
                MatchStatus::Scheduled | MatchStatus::Expired => {}
            }
            if !m.involves(reporter) {
                return Err("you are not playing in that match".into());
            }
            m.a_wins = a_wins;
            m.b_wins = b_wins;
            m.draws = draws;
            (m.a, m.b)
        };
        self.complete_match(id, a, b, a_wins, b_wins);
        Ok(())
    }

    /// Host override: record a final result directly. Works on a scheduled match,
    /// an expired no-show, or an already-completed match — the last case reverts
    /// the prior ELO change first, so a mistaken single-player entry can be fixed.
    pub fn force_match_result(&mut self, id: u64, a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
        self.validate_result(a_wins, b_wins, draws)?;
        let (a, b, undo_a, undo_b) = {
            let m = self.match_mut(id)?;
            if m.status == MatchStatus::Cancelled {
                return Err("that match was cancelled".into());
            }
            // Revert a previously-applied result before re-applying the new one.
            let (undo_a, undo_b) = if m.status == MatchStatus::Completed { (m.a_delta, m.b_delta) } else { (0, 0) };
            m.a_wins = a_wins;
            m.b_wins = b_wins;
            m.draws = draws;
            (m.a, m.b, undo_a, undo_b)
        };
        if undo_a != 0 {
            self.players.get_mut(&a).expect("match references a known player").elo -= undo_a as i64;
        }
        if undo_b != 0 {
            self.players.get_mut(&b).expect("match references a known player").elo -= undo_b as i64;
        }
        self.complete_match(id, a, b, a_wins, b_wins);
        Ok(())
    }

    /// Apply the ELO change for a finished match and mark it completed.
    fn complete_match(&mut self, id: u64, a: PlayerId, b: PlayerId, a_wins: u32, b_wins: u32) {
        let sa = match a_wins.cmp(&b_wins) {
            std::cmp::Ordering::Greater => 1.0,
            std::cmp::Ordering::Less => 0.0,
            std::cmp::Ordering::Equal => 0.5,
        };
        let (da, db) = elo_deltas(self.players[&a].elo, self.players[&b].elo, sa, self.config.elo_k);
        self.players.get_mut(&a).expect("match references a known player").elo += da as i64;
        self.players.get_mut(&b).expect("match references a known player").elo += db as i64;
        let m = self.match_mut(id).expect("match id validated by caller");
        m.status = MatchStatus::Completed;
        m.proposed_by = None;
        m.a_delta = da;
        m.b_delta = db;
    }

    // ---- cancellation -------------------------------------------------------

    /// A player calls off a scheduled match, taking the ELO penalty. The slot
    /// frees up and the match no longer counts toward either weekly target.
    /// Not available in league mode: an assigned match must be played (or its
    /// deadline passes and it scores as a tie until reported).
    pub fn cancel_match(&mut self, canceller: PlayerId, id: u64) -> Result<(), String> {
        if self.is_league() {
            return Err("league matches can't be cancelled — play by the deadline, or the match counts as a tie until a result is reported".into());
        }
        let penalty = self.config.cancel_penalty;
        let m = self.match_mut(id)?;
        match m.status {
            MatchStatus::Completed => return Err("a finished match can't be cancelled".into()),
            MatchStatus::Cancelled => return Err("that match is already cancelled".into()),
            MatchStatus::Expired => return Err("that match has already expired".into()),
            MatchStatus::Scheduled => {}
        }
        if !m.involves(canceller) {
            return Err("you are not playing in that match".into());
        }
        m.status = MatchStatus::Cancelled;
        m.cancelled_by = Some(canceller);
        m.proposed_by = None;
        m.a_delta = if m.a == canceller { -(penalty as i32) } else { 0 };
        m.b_delta = if m.b == canceller { -(penalty as i32) } else { 0 };
        self.players.get_mut(&canceller).expect("canceller verified to be in the match").elo -= penalty;
        Ok(())
    }

    /// Validate a reported result. League matches are best-of-three: at most
    /// two game wins per side and three games in total.
    fn validate_result(&self, a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
        validate_games(a_wins, b_wins, draws)?;
        if self.is_league() && (a_wins > 2 || b_wins > 2 || a_wins + b_wins + draws > 3) {
            return Err("league matches are best of three — at most 2 wins per side and 3 games".into());
        }
        Ok(())
    }

    /// Host: delete a match outright — an accidental pairing or a record that
    /// shouldn't exist (e.g. a duplicate). A completed or cancelled match's
    /// ELO change is reverted; swiss points, game records and OMW recompute
    /// from the remaining matches automatically. Deleting an upcoming match
    /// frees both players for re-pairing on the scheduler's next pass.
    pub fn delete_match(&mut self, id: u64) -> Result<(), String> {
        let idx = self
            .ladder
            .matches
            .iter()
            .position(|m| m.id == id)
            .ok_or("no such match")?;
        let m = self.ladder.matches.remove(idx);
        if matches!(m.status, MatchStatus::Completed | MatchStatus::Cancelled) {
            if let Some(p) = self.players.get_mut(&m.a) {
                p.elo -= m.a_delta as i64;
            }
            if let Some(p) = self.players.get_mut(&m.b) {
                p.elo -= m.b_delta as i64;
            }
        }
        Ok(())
    }

    /// Host: finalise every match whose play-by deadline has passed without a
    /// reported result as a 1-1 draw (matching how it already scores for
    /// pairing). Returns how many were recorded. Individual results can still
    /// be corrected afterwards via [`Self::force_match_result`].
    pub fn record_unreported_as_draws(&mut self, now_epoch: u64) -> usize {
        let ids: Vec<u64> = self
            .ladder
            .matches
            .iter()
            .filter(|m| Self::is_unreported(m, now_epoch))
            .map(|m| m.id)
            .collect();
        for &id in &ids {
            self.force_match_result(id, 1, 1, 0)
                .expect("an unreported match accepts a forced 1-1 draw");
        }
        ids.len()
    }

    // ---- standings ----------------------------------------------------------

    /// Players ranked by ELO — or, in league mode, by points ([`match_points`]:
    /// 3 per match win, 1 for taking a game without winning), then opponents'
    /// match-win percentage (strength of schedule), then game difference (so
    /// a 2-0 win outranks a 2-1 win) — with win/loss records. Ties break by
    /// name. ELO plays no part in league standings, so its seeding can never
    /// bias the final cut.
    pub fn standings(&self) -> Vec<Standing> {
        let mut by_id: HashMap<PlayerId, Standing> = self
            .player_order
            .iter()
            .map(|&p| {
                (p, Standing {
                    rank: 0,
                    player: p,
                    name: self.players[&p].name.clone(),
                    elo: self.players[&p].elo,
                    wins: 0,
                    losses: 0,
                    draws: 0,
                    played: 0,
                    scheduled: 0,
                    cancellations: 0,
                    points: 0,
                    game_wins: 0,
                    game_losses: 0,
                    omw: 0.0,
                })
            })
            .collect();

        for m in &self.ladder.matches {
            match m.status {
                MatchStatus::Scheduled => {
                    if let Some(s) = by_id.get_mut(&m.a) { s.scheduled += 1; }
                    if let Some(s) = by_id.get_mut(&m.b) { s.scheduled += 1; }
                }
                MatchStatus::Cancelled => {
                    if let Some(c) = m.cancelled_by
                        && let Some(s) = by_id.get_mut(&c) { s.cancellations += 1; }
                }
                MatchStatus::Completed => {
                    record_completed(by_id.get_mut(&m.a), m.a_wins, m.b_wins);
                    record_completed(by_id.get_mut(&m.b), m.b_wins, m.a_wins);
                }
                MatchStatus::Expired => {} // no-show: no effect on the record
            }
        }

        // Opponents' match-win percentage: each player's match-win rate
        // (points over possible points, floored at 1/3 with the usual swiss
        // convention), averaged over a player's opponents — one term per
        // completed match, so a rematch counts twice.
        let mw: HashMap<PlayerId, f64> = by_id
            .iter()
            .map(|(&p, s)| {
                let rate = if s.played > 0 {
                    (s.points as f64 / (3.0 * s.played as f64)).max(1.0 / 3.0)
                } else {
                    0.0
                };
                (p, rate)
            })
            .collect();
        for m in &self.ladder.matches {
            if m.status != MatchStatus::Completed {
                continue;
            }
            if let Some(s) = by_id.get_mut(&m.a) { s.omw += mw.get(&m.b).copied().unwrap_or(0.0); }
            if let Some(s) = by_id.get_mut(&m.b) { s.omw += mw.get(&m.a).copied().unwrap_or(0.0); }
        }
        for s in by_id.values_mut() {
            if s.played > 0 {
                s.omw /= s.played as f64;
            }
        }

        let mut out: Vec<Standing> = self.player_order.iter().map(|p| by_id.remove(p).expect("by_id was built from player_order")).collect();
        if self.is_league() {
            out.sort_by(|a, b| {
                b.points
                    .cmp(&a.points)
                    .then_with(|| b.omw.partial_cmp(&a.omw).expect("omw is always finite"))
                    .then_with(|| {
                        (b.game_wins as i64 - b.game_losses as i64)
                            .cmp(&(a.game_wins as i64 - a.game_losses as i64))
                    })
                    .then_with(|| a.name.cmp(&b.name))
            });
        } else {
            out.sort_by(|a, b| b.elo.cmp(&a.elo).then(a.name.cmp(&b.name)));
        }
        for (i, s) in out.iter_mut().enumerate() {
            s.rank = i as u32 + 1;
        }
        out
    }
}

/// Tally one side of a completed match into a standing.
fn record_completed(s: Option<&mut Standing>, my_games: u32, their_games: u32) {
    let Some(s) = s else { return };
    s.played += 1;
    s.game_wins += my_games;
    s.game_losses += their_games;
    s.points += match_points(my_games, their_games);
    match my_games.cmp(&their_games) {
        std::cmp::Ordering::Greater => s.wins += 1,
        std::cmp::Ordering::Less => s.losses += 1,
        std::cmp::Ordering::Equal => s.draws += 1,
    }
}

/// League match points for one side of a completed match: 3 for winning the
/// match, 1 for taking at least a game without winning (a 2-1 loss or a 1-1
/// draw), 0 otherwise.
fn match_points(my_games: u32, their_games: u32) -> i64 {
    if my_games > their_games { 3 } else if my_games > 0 { 1 } else { 0 }
}

/// Standard ELO update for a match. `sa` is player A's score (1 win / 0.5 draw /
/// 0 loss); returns the integer rating change for (A, B).
fn elo_deltas(ra: i64, rb: i64, sa: f64, k: i64) -> (i32, i32) {
    let ea = 1.0 / (1.0 + 10f64.powf((rb - ra) as f64 / 400.0));
    let eb = 1.0 - ea;
    let da = (k as f64 * (sa - ea)).round() as i32;
    let db = (k as f64 * ((1.0 - sa) - eb)).round() as i32;
    (da, db)
}

/// Validate the game counts of a reported match.
fn validate_games(a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
    const MAX_GAMES: u32 = 100; // sanity cap on a single match
    if a_wins + b_wins + draws == 0 {
        return Err("a result needs at least one game".into());
    }
    if a_wins > MAX_GAMES || b_wins > MAX_GAMES || draws > MAX_GAMES {
        return Err("that's an implausible number of games".into());
    }
    Ok(())
}
