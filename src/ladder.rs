//! The ELO ladder: players publish weekly availability and a per-week game
//! target, the system automatically matches the closest-rated, least-recently-
//! met available players, results are reported and opponent-confirmed (which
//! applies the ELO change), and a player may cancel a scheduled match for an
//! ELO penalty.

use crate::engine::Game;
use crate::model::*;
use std::collections::{HashMap, HashSet};

const N_BLOCKS: i64 = DAY_BLOCKS.len() as i64;

/// Upper bound on how many availability slots a player may submit, so a single
/// request can't store (and persist) an unbounded list.
const MAX_AVAIL_SLOTS: usize = 2000;

/// Grace period after a slot starts before an unreported match is treated as a
/// no-show and expired (so players have time to report late).
const NO_SHOW_GRACE_SECS: u64 = 24 * 3600;

/// The Unix epoch second a slot begins, given the game's configured block start
/// hours (see [`Config::ladder_block_hours`]). Falls back to [`DAY_BLOCKS`] for
/// any block index the hours slice doesn't cover.
pub fn slot_start_epoch(slot: i64, hours: &[u32]) -> u64 {
    let day = slot.div_euclid(N_BLOCKS).max(0);
    let block = slot.rem_euclid(N_BLOCKS) as usize;
    let hour = hours.get(block).copied().unwrap_or(DAY_BLOCKS[block]);
    day as u64 * 86_400 + hour as u64 * 3_600
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

    fn quota(&self, player: PlayerId) -> u32 {
        self.ladder.games_per_week.get(&player).copied().unwrap_or(0)
    }

    // ---- automatic matchmaking ---------------------------------------------

    /// Schedule new matches from current availability, returning how many were
    /// created. In each upcoming slot it pairs the available players preferring
    /// the fewest prior meetings, then the closest ELO — respecting one match
    /// per player per slot and each player's weekly target. Idempotent until
    /// availability, results, or the calendar change, so it is safe to call on a
    /// timer.
    pub fn auto_schedule(&mut self, now_epoch: u64) -> usize {
        if self.phase == Phase::Setup || self.players.len() < 2 {
            return 0;
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
            if slot_start_epoch(slot, &block_hours) <= now_epoch {
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
                    has_quota(p, &used)
                        && !booked.contains(&(p, slot))
                        && self.ladder.availability.get(&p).is_some_and(|s| s.binary_search(&slot).is_ok())
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

    /// Mark scheduled matches whose slot has passed (beyond the grace period)
    /// without a confirmed result as no-shows. Returns how many expired. No ELO
    /// is applied; the pair becomes eligible for rescheduling.
    pub fn expire_stale_matches(&mut self, now_epoch: u64) -> usize {
        let mut expired = 0;
        for m in &mut self.ladder.matches {
            if m.status == MatchStatus::Scheduled && m.slot_start.saturating_add(NO_SHOW_GRACE_SECS) < now_epoch {
                m.status = MatchStatus::Expired;
                m.proposed_by = None;
                expired += 1;
            }
        }
        expired
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
        validate_games(a_wins, b_wins, draws)?;
        let m = self.match_mut(id)?;
        match m.status {
            MatchStatus::Completed => return Err("that match is already final".into()),
            MatchStatus::Cancelled => return Err("that match was cancelled".into()),
            MatchStatus::Expired => return Err("that match expired — ask the host to record it".into()),
            MatchStatus::Scheduled => {}
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
                MatchStatus::Expired => return Err("that match expired — ask the host to record it".into()),
                MatchStatus::Scheduled => {}
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

    /// Host override: record a final result directly, skipping confirmation.
    pub fn force_match_result(&mut self, id: u64, a_wins: u32, b_wins: u32, draws: u32) -> Result<(), String> {
        validate_games(a_wins, b_wins, draws)?;
        let (a, b) = {
            let m = self.match_mut(id)?;
            match m.status {
                MatchStatus::Completed => return Err("that match is already final".into()),
                MatchStatus::Cancelled => return Err("that match was cancelled".into()),
                // Scheduled or Expired (a no-show the host is resolving) are fine.
                MatchStatus::Scheduled | MatchStatus::Expired => {}
            }
            m.a_wins = a_wins;
            m.b_wins = b_wins;
            m.draws = draws;
            (m.a, m.b)
        };
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
        self.players.get_mut(&a).unwrap().elo += da as i64;
        self.players.get_mut(&b).unwrap().elo += db as i64;
        let m = self.match_mut(id).unwrap();
        m.status = MatchStatus::Completed;
        m.proposed_by = None;
        m.a_delta = da;
        m.b_delta = db;
    }

    // ---- cancellation -------------------------------------------------------

    /// A player calls off a scheduled match, taking the ELO penalty. The slot
    /// frees up and the match no longer counts toward either weekly target.
    pub fn cancel_match(&mut self, canceller: PlayerId, id: u64) -> Result<(), String> {
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
        self.players.get_mut(&canceller).unwrap().elo -= penalty;
        Ok(())
    }

    // ---- standings ----------------------------------------------------------

    /// Players ranked by ELO (ties broken by name), with win/loss records.
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
                    if let Some(c) = m.cancelled_by {
                        if let Some(s) = by_id.get_mut(&c) { s.cancellations += 1; }
                    }
                }
                MatchStatus::Completed => {
                    record_completed(by_id.get_mut(&m.a), m.a_wins, m.b_wins);
                    record_completed(by_id.get_mut(&m.b), m.b_wins, m.a_wins);
                }
                MatchStatus::Expired => {} // no-show: no effect on the record
            }
        }

        let mut out: Vec<Standing> = self.player_order.iter().map(|p| by_id.remove(p).unwrap()).collect();
        out.sort_by(|a, b| b.elo.cmp(&a.elo).then(a.name.cmp(&b.name)));
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
    match my_games.cmp(&their_games) {
        std::cmp::Ordering::Greater => s.wins += 1,
        std::cmp::Ordering::Less => s.losses += 1,
        std::cmp::Ordering::Equal => s.draws += 1,
    }
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
