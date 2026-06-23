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
