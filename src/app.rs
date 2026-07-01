//! Shared application state: the game behind a mutex, an SSE change-broadcaster
//! for live updates, on-disk persistence, and the round-timer task.

use crate::engine::Game;
use crate::model::Phase;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// How many hourly backups of the save file to keep (48 = two days' worth).
const BACKUP_KEEP: usize = 48;

pub type AppState = Arc<App>;

pub struct App {
    pub game: Mutex<Game>,
    /// Fires once whenever the game changes, so SSE clients refresh.
    pub tx: broadcast::Sender<()>,
    /// Where to persist the game as JSON, if persistence is enabled.
    pub state_file: Option<PathBuf>,
}

impl App {
    /// Build shared state, loading a saved game from `state_file` if present.
    pub fn new(state_file: Option<PathBuf>) -> AppState {
        let mut game = match &state_file {
            Some(path) => load(path).unwrap_or_default(),
            None => Game::default(),
        };
        // Re-arm the round timer relative to *now* rather than trusting the
        // persisted deadline: if the server was down past a round's deadline,
        // we give the round a fresh full duration instead of slamming it closed
        // the instant we come back up.
        game.arm_timer(now_epoch());
        let (tx, _rx) = broadcast::channel(64);
        Arc::new(App { game: Mutex::new(game), tx, state_file })
    }

    /// Lock the game state, recovering from a poisoned mutex rather than
    /// panicking. The mutex is poisoned only if a previous handler panicked
    /// while holding it; the game data itself is still consistent, so we'd
    /// rather keep serving than turn one panic into a permanent 500 for every
    /// subsequent request.
    pub fn lock_game(&self) -> std::sync::MutexGuard<'_, Game> {
        self.game.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Notify SSE subscribers that the game changed.
    pub fn notify(&self) {
        let _ = self.tx.send(());
    }

    /// Serialize the current game to JSON under the lock, if persistence is on.
    /// Returns the target path and the serialized bytes so the (blocking) disk
    /// write can be done separately — off the async runtime where appropriate.
    fn snapshot(&self) -> Option<(PathBuf, String)> {
        let path = self.state_file.clone()?;
        match serde_json::to_string(&*self.lock_game()) {
            Ok(json) => Some((path, json)),
            Err(e) => {
                tracing::warn!("could not serialize game: {e}");
                None
            }
        }
    }

    /// Persist the game to disk synchronously (no-op if persistence is disabled).
    /// Prefer [`save_and_notify`](App::save_and_notify) from async handlers, which
    /// offloads the blocking write; this is for startup/tests and the timer path.
    pub fn save(&self) {
        if let Some((path, json)) = self.snapshot() {
            write_atomic(&path, &json);
        }
    }

    /// Persist and notify after a mutation. The atomic disk write runs on a
    /// blocking thread so it doesn't stall the async runtime under load; clients
    /// are notified once it has been handed off.
    pub async fn save_and_notify(&self) {
        if let Some((path, json)) = self.snapshot() {
            let _ = tokio::task::spawn_blocking(move || write_atomic(&path, &json)).await;
        }
        self.notify();
    }

    /// Write a dated snapshot of the game at most once per UTC hour, then prune
    /// to the most recent [`BACKUP_KEEP`]. Idempotent — a no-op if this hour's
    /// backup already exists — so it's safe to call on a timer. No-op when
    /// persistence is off or no game has started. Returns whether a new backup
    /// was written.
    ///
    /// Backups sit next to the save file as `<name>.YYYY-MM-DD-HH.bak`, so a save
    /// corrupted by a bug (which atomic writes don't protect against) can be
    /// recovered by copying a recent backup over it.
    pub fn backup_hourly(&self, now_epoch: u64) -> bool {
        let Some(path) = self.state_file.clone() else { return false };
        let backup = backup_path(&path, &hour_string(now_epoch));
        if backup.exists() {
            return false;
        }
        // Serialize the in-memory game (a consistent snapshot, like `save`).
        let json = {
            let game = self.lock_game();
            if game.phase == Phase::Setup {
                return false; // nothing worth backing up yet
            }
            match serde_json::to_string(&*game) {
                Ok(json) => json,
                Err(e) => {
                    tracing::warn!("could not serialize backup: {e}");
                    return false;
                }
            }
        };
        let mut tmp = backup.clone();
        let mut name = tmp.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        tmp.set_file_name(name);
        if let Err(e) = std::fs::write(&tmp, &json) {
            tracing::warn!("could not write backup {}: {e}", tmp.display());
            return false;
        }
        if let Err(e) = std::fs::rename(&tmp, &backup) {
            tracing::warn!("could not finalize backup {}: {e}", backup.display());
            return false;
        }
        prune_backups(&path, BACKUP_KEEP);
        tracing::info!("wrote hourly backup {}", backup.display());
        true
    }
}

/// Write `data` to `path` atomically: to a sibling temp file, then rename over
/// the target, so a crash mid-write can never corrupt an existing file.
fn write_atomic(path: &Path, data: &str) {
    let mut tmp = path.to_path_buf();
    let mut name = tmp.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    tmp.set_file_name(name);
    if let Err(e) = std::fs::write(&tmp, data) {
        tracing::warn!("could not write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!("could not replace {}: {e}", path.display());
    }
}

/// Path of the dated backup beside the save file: `<name>.<stamp>.bak`.
fn backup_path(path: &Path, stamp: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "game_state.json".to_string());
    path.with_file_name(format!("{name}.{stamp}.bak"))
}

/// Delete all but the newest `keep` backups for `path`. Backup names embed the
/// timestamp as `YYYY-MM-DD-HH`, so lexicographic order is chronological.
fn prune_backups(path: &Path, keep: usize) {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut backups: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
        })
        .collect();
    if backups.len() <= keep {
        return;
    }
    backups.sort();
    for old in &backups[..backups.len() - keep] {
        let _ = std::fs::remove_file(old);
    }
}

/// `YYYY-MM-DD-HH` (UTC) for a Unix timestamp, without pulling in a date crate.
/// The lexicographic order of these stamps is chronological, so backup pruning
/// can keep the newest by sorting filenames.
fn hour_string(now_epoch: u64) -> String {
    let (y, m, d) = civil_from_days((now_epoch / 86_400) as i64);
    let hour = (now_epoch / 3_600) % 24;
    format!("{y:04}-{m:02}-{d:02}-{hour:02}")
}

/// Civil (year, month, day) from days since 1970-01-01, via Howard Hinnant's
/// well-known algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn load(path: &PathBuf) -> Option<Game> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Game>(&data) {
        Ok(game) => {
            tracing::info!("resumed saved game from {}", path.display());
            Some(game)
        }
        Err(e) => {
            tracing::warn!("ignoring unreadable save file {}: {e}", path.display());
            None
        }
    }
}

/// Current Unix time in whole seconds.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How often (in seconds) the background task does the time-driven ladder work
/// (expiring no-shows, scheduling into newly-reachable slots / new weeks).
/// Scheduling is otherwise event-driven — it runs immediately after the changes
/// that create opportunities (availability/target edits, cancellations) — so
/// this only needs a coarse cadence to handle the passage of time.
const LADDER_TICK_SECS: u64 = 60;

/// Background task: once a second, auto-close any round whose timer has expired;
/// once a minute, expire ladder no-shows and run a scheduling pass.
pub async fn timer_loop(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut tick: u64 = 0;
    state.backup_hourly(now_epoch()); // snapshot promptly on startup
    loop {
        interval.tick().await;
        tick += 1;
        let changed = {
            let mut game = state.lock_game();
            let due = matches!(game.phase, Phase::Primary | Phase::Secondary)
                && game.round_deadline.is_some_and(|dl| now_epoch() >= dl);
            if due {
                if let Ok(result) = game.close_round() {
                    game.record_deliveries(&result, now_epoch());
                }
                game.arm_timer(now_epoch());
            }
            let ladder_changed = if tick.is_multiple_of(LADDER_TICK_SECS) {
                let expired = game.expire_stale_matches(now_epoch());
                let scheduled = game.auto_schedule(now_epoch());
                let reversed = game.expire_overdue_deliveries(now_epoch());
                expired > 0 || scheduled > 0 || reversed > 0
            } else {
                false
            };
            due || ladder_changed
        };
        if changed {
            state.save_and_notify().await;
        }
        // Daily backup check (cheap once-a-minute existence check); must run
        // without the game lock held, as `backup_hourly` takes it itself.
        if tick.is_multiple_of(LADDER_TICK_SECS) {
            state.backup_hourly(now_epoch());
        }
    }
}
