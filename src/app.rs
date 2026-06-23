//! Shared application state: the game behind a mutex, an SSE change-broadcaster
//! for live updates, on-disk persistence, and the round-timer task.

use crate::engine::Game;
use crate::model::Phase;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

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
        let game = match &state_file {
            Some(path) => load(path).unwrap_or_default(),
            None => Game::default(),
        };
        let (tx, _rx) = broadcast::channel(64);
        Arc::new(App { game: Mutex::new(game), tx, state_file })
    }

    /// Notify SSE subscribers that the game changed.
    pub fn notify(&self) {
        let _ = self.tx.send(());
    }

    /// Persist the game to disk (no-op if persistence is disabled). Writes to a
    /// sibling temp file then atomically renames it over the target, so a crash
    /// mid-write can never corrupt an existing save.
    pub fn save(&self) {
        let Some(path) = &self.state_file else { return };
        let json = match serde_json::to_string(&*self.game.lock().unwrap_or_else(|e| e.into_inner())) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("warning: could not serialize game: {e}");
                return;
            }
        };
        let mut tmp = path.clone();
        let mut name = tmp.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        tmp.set_file_name(name);
        if let Err(e) = std::fs::write(&tmp, json) {
            eprintln!("warning: could not write {}: {e}", tmp.display());
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!("warning: could not replace {}: {e}", path.display());
        }
    }

    /// Convenience: persist and notify after a mutation.
    pub fn save_and_notify(&self) {
        self.save();
        self.notify();
    }
}

fn load(path: &PathBuf) -> Option<Game> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Game>(&data) {
        Ok(game) => {
            println!("Resumed saved game from {}", path.display());
            Some(game)
        }
        Err(e) => {
            eprintln!("warning: ignoring unreadable save file {}: {e}", path.display());
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

/// Background task: once a second, auto-close any round whose timer has expired.
pub async fn timer_loop(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let closed = {
            let mut game = state.game.lock().unwrap_or_else(|e| e.into_inner());
            let due = game.phase == Phase::Bidding
                && game.round_deadline.is_some_and(|dl| now_epoch() >= dl);
            if due {
                let _ = game.close_round();
                game.arm_timer(now_epoch());
                true
            } else {
                false
            }
        };
        if closed {
            state.save_and_notify();
        }
    }
}
