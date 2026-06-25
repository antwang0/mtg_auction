//! Persistence round-trip: a game saved to disk reloads identically, including
//! resting orders and login tokens.

use mtg_auction::app::App;
use mtg_auction::engine::Game;
use mtg_auction::model::{CardPool, Config, Phase};

fn cfg() -> Config {
    Config {
        player_names: vec!["A".into(), "B".into()],
        set: "sample".into(),
        starting_money: 10_000,
        debt_limit: 0,
        rounds: 3,
        num_packs: 1,
        pack_size: 6,
        seed: 1,
        round_seconds: 0,
        ..Config::default()
    }
}

#[test]
fn game_survives_save_and_reload() {
    let path = std::env::temp_dir().join(format!("mtg_auction_persist_{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let (token, card) = {
        let app = App::new(Some(path.clone()));
        let mut g = app.game.lock().unwrap();
        *g = Game::setup(cfg(), CardPool::sample()).unwrap();
        let card = g.card_order[0];
        let token = g.tokens[&1].clone();
        g.place_bid(1, card, 1, 500).unwrap();
        drop(g);
        app.save();
        (token, card)
    };

    // A fresh App pointed at the same file resumes the game.
    let app2 = App::new(Some(path.clone()));
    let g2 = app2.game.lock().unwrap();
    assert_eq!(g2.phase, Phase::Bidding);
    assert_eq!(g2.round, 1);
    assert_eq!(g2.players.len(), 2);
    assert_eq!(g2.tokens[&1], token, "tokens persist so players stay logged in");
    assert_eq!(g2.bids.get(&(1, card)).map(|o| o.qty), Some(1), "resting orders persist");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn hourly_backups_are_dated_idempotent_and_pruned() {
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let dir = std::env::temp_dir().join(format!("mtg_auction_bak_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("game_state.json");

    let count_baks = || {
        std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".bak"))
            .count()
    };

    // A game that hasn't started isn't backed up.
    let app = App::new(Some(path.clone()));
    assert!(!app.backup_hourly(20_000 * DAY), "no backup before a game starts");
    assert_eq!(count_baks(), 0);

    // Once a game exists, the first call writes a stamped backup...
    *app.game.lock().unwrap() = Game::setup(cfg(), CardPool::sample()).unwrap();
    let base = 20_000 * DAY; // 2024-10-04 00:00 UTC
    assert!(app.backup_hourly(base));
    assert!(dir.join("game_state.json.2024-10-04-00.bak").exists(), "named by UTC date+hour");
    // ...and a second call the same hour is a no-op (idempotent).
    assert!(!app.backup_hourly(base + 600));
    assert_eq!(count_baks(), 1);

    // Across 60 distinct hours, only the most recent 48 are kept.
    for h in 1..60 {
        app.backup_hourly(base + h * HOUR);
    }
    assert_eq!(count_baks(), 48);
    // The oldest were pruned; the newest survive.
    assert!(!dir.join("game_state.json.2024-10-04-00.bak").exists());
    assert!(dir.join("game_state.json.2024-10-06-11.bak").exists()); // base + 59h

    let _ = std::fs::remove_dir_all(&dir);
}
