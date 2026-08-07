//! HTTP-level integration tests: spin up the real router on an ephemeral port
//! and drive it with reqwest. Uses the offline `sample` set (no network).

use mtg_auction::model::DAY_BLOCKS;
use serde_json::{json, Value};

/// Blocks per day, derived so the tests track [`DAY_BLOCKS`] rather than a literal.
const NB: i64 = DAY_BLOCKS.len() as i64;

/// Start the API server on a random port and return its base URL. When
/// `with_timer` is set, the round auto-close task runs too.
async fn spawn_opt(with_timer: bool) -> String {
    let state = mtg_auction::app::App::new(None); // no persistence
    if with_timer {
        tokio::spawn(mtg_auction::app::timer_loop(state.clone()));
    }
    let app = mtg_auction::api::api_router().with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}

async fn spawn() -> String {
    spawn_opt(false).await
}

fn setup_body() -> Value {
    json!({
        "player_names": ["Alice", "Bob"],
        "set": "sample",
        "starting_money": 10000,
        "debt_limit": 0,
        "primary_rounds": 3,
        "num_packs": 1,
        "pack_size": 6,
        "seed": 1
    })
}

async fn get_state(c: &reqwest::Client, base: &str, token: Option<&str>) -> Value {
    let mut req = c.get(format!("{base}/api/state"));
    if let Some(t) = token {
        req = req.header("x-token", t);
    }
    req.send().await.unwrap().json().await.unwrap()
}

/// A sample-set game that holds cards back in the house: each player is dealt one
/// common, so the rest of the opened cards stay unallocated.
fn setup_body_house() -> Value {
    json!({
        "player_names": ["Alice", "Bob"],
        "pool_source": "sample",
        "starting_money": 1_000_000,
        "debt_limit": 0,
        "primary_rounds": 3,
        "num_packs": 4,
        "pack_size": 6,
        "seed": 1,
        "deal_commons": 1
    })
}

async fn setup_game_with(c: &reqwest::Client, base: &str, body: &Value) -> (String, String) {
    let resp: Value = c.post(format!("{base}/api/setup")).json(body).send().await.unwrap().json().await.unwrap();
    let players = resp["players"].as_array().unwrap();
    (
        players[0]["token"].as_str().unwrap().to_string(),
        players[1]["token"].as_str().unwrap().to_string(),
    )
}

fn house_total(state: &Value) -> u64 {
    state["house"].as_array().unwrap().iter().map(|h| h["qty"].as_u64().unwrap()).sum()
}

/// Set up a sample game and return (alice_token, bob_token).
async fn setup_game(c: &reqwest::Client, base: &str) -> (String, String) {
    let resp: Value = c
        .post(format!("{base}/api/setup"))
        .json(&setup_body())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let players = resp["players"].as_array().unwrap();
    (
        players[0]["token"].as_str().unwrap().to_string(),
        players[1]["token"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn setup_then_state_reports_bidding() {
    let base = spawn().await;
    let c = reqwest::Client::new();

    let pre = get_state(&c, &base, None).await;
    assert_eq!(pre["phase"], "setup");

    setup_game(&c, &base).await;

    let post = get_state(&c, &base, None).await;
    assert_eq!(post["phase"], "primary");
    assert_eq!(post["round"], 1);
    assert!(!post["cards"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn orders_require_your_own_token() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;

    let card = get_state(&c, &base, Some(&alice)).await["cards"][0]["id"].as_u64().unwrap();
    let bid = |tok: Option<&str>| {
        let req = c.post(format!("{base}/api/bid")).json(&json!({ "player": 1, "card": card, "qty": 1, "price": 100 }));
        match tok {
            Some(t) => req.header("x-token", t.to_string()),
            None => req,
        }
    };

    // No token, then Bob's token acting as player 1: both rejected.
    assert_eq!(bid(None).send().await.unwrap().status(), 401);
    assert_eq!(bid(Some(&bob)).send().await.unwrap().status(), 401);
    // Alice (player 1) with her own token: accepted.
    assert_eq!(bid(Some(&alice)).send().await.unwrap().status(), 200);
}

#[tokio::test]
async fn committed_and_available_track_bids() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, _bob) = setup_game(&c, &base).await;

    let card = get_state(&c, &base, Some(&alice)).await["cards"][0]["id"].as_u64().unwrap();
    c.post(format!("{base}/api/bid"))
        .header("x-token", &alice)
        .json(&json!({ "player": 1, "card": card, "qty": 2, "price": 1500 }))
        .send()
        .await
        .unwrap();

    let s = get_state(&c, &base, Some(&alice)).await;
    assert_eq!(s["my_committed"], 3000); // 2 × $15.00
    assert_eq!(s["my_available"], 7000); // $100.00 + $0 debt − $30.00
}

#[tokio::test]
async fn bid_crossing_own_offer_rejected_over_http() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, _bob) = setup_game(&c, &base).await;

    // Find a card Alice holds so she can offer it.
    let me = get_state(&c, &base, Some(&alice)).await;
    let owned = me["players"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == 1)
        .unwrap()["holdings"][0]["card"]
        .as_u64()
        .unwrap();

    let offer = c.post(format!("{base}/api/offer"))
        .header("x-token", &alice)
        .json(&json!({ "player": 1, "card": owned, "qty": 1, "price": 500 }))
        .send().await.unwrap();
    assert_eq!(offer.status(), 200);

    // A bid above her own $5.00 offer crosses (she'd buy high while offering to
    // sell low) — the server must reject it.
    let bid = c.post(format!("{base}/api/bid"))
        .header("x-token", &alice)
        .json(&json!({ "player": 1, "card": owned, "qty": 1, "price": 600 }))
        .send().await.unwrap();
    assert_eq!(bid.status(), 400);
    let body: Value = bid.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("cross"));
}

#[tokio::test]
async fn close_is_admin_only_and_advances_round() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;

    // Bob (not host) cannot close.
    let r = c.post(format!("{base}/api/close")).header("x-token", &bob).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Alice (host) can; the round advances.
    let r = c.post(format!("{base}/api/close")).header("x-token", &alice).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(get_state(&c, &base, None).await["round"], 2);
}

#[tokio::test]
async fn round_auto_closes_when_timer_expires() {
    let base = spawn_opt(true).await;
    let c = reqwest::Client::new();
    // 1-second round timer.
    let mut body = setup_body();
    body["primary_round_seconds"] = json!(1);
    c.post(format!("{base}/api/setup")).json(&body).send().await.unwrap();

    assert_eq!(get_state(&c, &base, None).await["round"], 1);
    // Wait past the deadline; the background task should close the round.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let round = get_state(&c, &base, None).await["round"].as_u64().unwrap();
    assert!(round >= 2, "round should auto-advance, got {round}");
}

#[tokio::test]
async fn ledger_is_admin_only() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;

    let card = get_state(&c, &base, Some(&alice)).await["cards"][0]["id"].as_u64().unwrap();
    c.post(format!("{base}/api/bid"))
        .header("x-token", &alice)
        .json(&json!({ "player": 1, "card": card, "qty": 1, "price": 100 }))
        .send().await.unwrap();

    // Bob can't read the ledger.
    let r = c.get(format!("{base}/api/log")).header("x-token", &bob).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Alice (host) sees the recorded order.
    let log: Value = c.get(format!("{base}/api/log")).header("x-token", &alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(log["orders"].as_array().unwrap().len(), 1);
    assert_eq!(log["orders"][0]["action"], "place");
}

#[tokio::test]
async fn ladder_schedule_report_confirm_flow() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await; // Alice is host + player 1

    // Matchmaking only begins after the primary phase. setup_body runs 3 primary
    // rounds, so close them (as host) to reach the secondary phase.
    for _ in 0..3 {
        assert_eq!(c.post(format!("{base}/api/close")).header("x-token", &alice).send().await.unwrap().status(), 200);
    }

    // Both players set availability for the same upcoming slot + a weekly target.
    let now = c.get(format!("{base}/api/ladder")).send().await.unwrap().json::<Value>().await.unwrap()["server_now"].as_u64().unwrap();
    let slot = ((now / 86_400) as i64 + 1) * NB; // tomorrow, first block
    for tok in [&alice, &bob] {
        let r = c.post(format!("{base}/api/ladder/availability")).header("x-token", tok).json(&json!({ "slots": [slot] })).send().await.unwrap();
        assert_eq!(r.status(), 200);
        c.post(format!("{base}/api/ladder/games")).header("x-token", tok).json(&json!({ "games_per_week": 1 })).send().await.unwrap();
    }

    // Setting availability is event-driven: the one possible match is already
    // scheduled, no manual pass needed.
    let ladder: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    assert_eq!(ladder["matches"].as_array().unwrap().len(), 1, "availability auto-schedules");

    // The scheduler endpoint is still host-only (and idempotent here).
    assert_eq!(c.post(format!("{base}/api/ladder/schedule")).header("x-token", &bob).send().await.unwrap().status(), 401);
    assert_eq!(c.post(format!("{base}/api/ladder/schedule")).header("x-token", &alice).send().await.unwrap().status(), 200);

    let m = &ladder["matches"][0];
    let id = m["id"].as_u64().unwrap();
    let a = m["a"].as_u64().unwrap();

    // Bob (a participant) reports himself winning; it's final immediately, with
    // no opponent confirmation needed.
    let (aw, bw) = if a == 2 { (2, 0) } else { (0, 2) }; // Bob wins, in seat order
    let r = c.post(format!("{base}/api/ladder/report")).header("x-token", &bob).json(&json!({ "match_id": id, "a_wins": aw, "b_wins": bw })).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let done: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    assert_eq!(done["matches"][0]["status"], "completed");
    assert_eq!(done["standings"][0]["player"], 2, "Bob, the winner, leads on ELO");
    assert_eq!(done["standings"][0]["elo"], 1216);

    // The host can correct a mistaken result even after it's final (reverting
    // the old ELO and applying the new one — a draw here evens it back out).
    let fix = c.post(format!("{base}/api/ladder/report")).header("x-token", &alice).json(&json!({ "match_id": id, "a_wins": 1, "b_wins": 1 })).send().await.unwrap();
    assert_eq!(fix.status(), 200);
    let fixed: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    assert_eq!(fixed["matches"][0]["status"], "completed");
    for s in fixed["standings"].as_array().unwrap() {
        assert_eq!(s["elo"], 1200, "a corrected draw restores even ELO");
    }
}

#[tokio::test]
async fn ladder_cancel_costs_elo() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;

    // Reach the secondary phase (matchmaking is gated until primary is over).
    for _ in 0..3 {
        c.post(format!("{base}/api/close")).header("x-token", &alice).send().await.unwrap();
    }

    let now = c.get(format!("{base}/api/ladder")).send().await.unwrap().json::<Value>().await.unwrap()["server_now"].as_u64().unwrap();
    let slot = ((now / 86_400) as i64 + 1) * NB;
    for tok in [&alice, &bob] {
        c.post(format!("{base}/api/ladder/availability")).header("x-token", tok).json(&json!({ "slots": [slot] })).send().await.unwrap();
        c.post(format!("{base}/api/ladder/games")).header("x-token", tok).json(&json!({ "games_per_week": 1 })).send().await.unwrap();
    }
    c.post(format!("{base}/api/ladder/schedule")).header("x-token", &alice).send().await.unwrap();

    let ladder: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    let id = ladder["matches"][0]["id"].as_u64().unwrap();

    // Alice cancels; she takes the ELO penalty (default 16 → 1184).
    let r = c.post(format!("{base}/api/ladder/cancel")).header("x-token", &alice).json(&json!({ "match_id": id })).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let after = get_state(&c, &base, Some(&alice)).await;
    let alice_elo = after["players"].as_array().unwrap().iter().find(|p| p["id"] == 1).unwrap()["elo"].as_i64().unwrap();
    assert_eq!(alice_elo, 1184);
}

#[tokio::test]
async fn tokens_are_short() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;
    assert_eq!(alice.len(), 4, "tokens are truncated to 4 chars");
    assert_eq!(bob.len(), 4);
    assert_ne!(alice, bob, "and stay distinct");
}

#[tokio::test]
async fn password_login_flow() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, _bob) = setup_game(&c, &base).await;

    // Setting a password needs a valid token.
    let r = c.post(format!("{base}/api/set-password")).json(&json!({ "password": "swordfish" })).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Alice sets a password.
    let r = c.post(format!("{base}/api/set-password")).header("x-token", &alice).json(&json!({ "password": "swordfish" })).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(get_state(&c, &base, Some(&alice)).await["my_has_password"].as_bool().unwrap());

    // Wrong password is rejected.
    let r = c.post(format!("{base}/api/password-login")).json(&json!({ "name": "Alice", "password": "nope" })).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Right name + password (case-insensitive name) returns Alice's own token and admin flag.
    let r = c.post(format!("{base}/api/password-login")).json(&json!({ "name": "alice", "password": "swordfish" })).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["player"], 1);
    assert!(body["admin"].as_bool().unwrap());
    assert_eq!(body["token"].as_str().unwrap(), alice);
}

#[tokio::test]
async fn house_offer_clears_against_a_bid() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game_with(&c, &base, &setup_body_house()).await;

    // Per-rarity dealing leaves leftovers in the house.
    let st = get_state(&c, &base, Some(&alice)).await;
    assert!(!st["house"].as_array().unwrap().is_empty(), "leftovers go to the house");

    // Offering house cards is host-only.
    assert_eq!(c.post(format!("{base}/api/house/offer")).header("x-token", &bob).send().await.unwrap().status(), 401);
    let r = c.post(format!("{base}/api/house/offer")).header("x-token", &alice).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.json::<Value>().await.unwrap()["listed"].as_u64().unwrap() > 0);

    // Bob bids well above any noisy reference price on a house card, then the host closes.
    let card = get_state(&c, &base, Some(&bob)).await["house"][0]["card"].as_u64().unwrap();
    c.post(format!("{base}/api/bid")).header("x-token", &bob)
        .json(&json!({ "player": 2, "card": card, "qty": 1, "price": 90000 }))
        .send().await.unwrap();
    c.post(format!("{base}/api/close")).header("x-token", &alice).send().await.unwrap();

    // Bob's personal trade history shows the buy from the house, which collected the cash.
    let bob_state = get_state(&c, &base, Some(&bob)).await;
    let trades = bob_state["my_trades"].as_array().unwrap();
    assert!(
        trades.iter().any(|t| t["side"] == "bought" && t["counterparty"] == "House" && t["card"] == card),
        "Bob bought the card from the house"
    );
    assert!(bob_state["house_balance"].as_i64().unwrap() > 0, "the house collected the proceeds");
}

#[tokio::test]
async fn add_player_and_card_auth() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game_with(&c, &base, &setup_body_house()).await;

    // Mid-game additions are host-only (auth is checked before any work).
    assert_eq!(c.post(format!("{base}/api/players/add")).header("x-token", &bob).json(&json!({ "name": "Zed" })).send().await.unwrap().status(), 401);
    assert_eq!(c.post(format!("{base}/api/cards/add")).header("x-token", &bob).json(&json!({ "card_list": "1 X" })).send().await.unwrap().status(), 401);

    // The host adds a late player, who is dealt from the house and gets a short token.
    let house_before = house_total(&get_state(&c, &base, Some(&alice)).await);
    let r = c.post(format!("{base}/api/players/add")).header("x-token", &alice).json(&json!({ "name": "Zed" })).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let ztok = body["token"].as_str().unwrap();
    assert_eq!(ztok.len(), 4);

    // Zed's token logs in as the new player, and the house shrank by what they were dealt.
    let login: Value = c.post(format!("{base}/api/login")).json(&json!({ "token": ztok })).send().await.unwrap().json().await.unwrap();
    assert_eq!(login["player"], body["player"]);
    let zed_state = get_state(&c, &base, Some(ztok)).await;
    let zed_cards: u64 = zed_state["players"].as_array().unwrap().iter()
        .find(|p| p["id"] == body["player"]).unwrap()["card_count"].as_u64().unwrap();
    assert!(zed_cards > 0, "the late player got an allocation");
    assert_eq!(house_total(&zed_state), house_before - zed_cards, "their cards came from the house");
}

#[tokio::test]
async fn set_cards_lists_a_set_for_the_picker() {
    let base = spawn().await;
    let c = reqwest::Client::new();

    // Before any game, anyone may browse a set (here the offline sample set).
    let r = c.get(format!("{base}/api/set-cards?set=sample")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let cards = body["cards"].as_array().unwrap();
    assert!(cards.len() >= 30, "the sample set has many cards, got {}", cards.len());
    // Sorted by name, and carrying rarity + reference price for the picker.
    assert!(cards.windows(2).all(|w| w[0]["name"].as_str().unwrap() <= w[1]["name"].as_str().unwrap()));
    assert!(cards.iter().any(|c| c["name"] == "Black Lotus" || c["rarity"] == "mythic"));
    assert!(cards[0]["ref_price"].is_number());

    // Once a game is in progress, browsing is host-only.
    let (_alice, bob) = setup_game(&c, &base).await;
    let r = c.get(format!("{base}/api/set-cards?set=sample")).header("x-token", &bob).send().await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn scryfall_source_needs_a_set_code() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    // A scryfall pool with no set code is rejected before any network fetch.
    let body = json!({
        "player_names": ["A", "B"], "pool_source": "scryfall", "set": "",
        "starting_money": 10000, "debt_limit": 0, "primary_rounds": 2, "num_packs": 1, "pack_size": 6, "seed": 1
    });
    let r = c.post(format!("{base}/api/setup")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.json::<Value>().await.unwrap()["error"].as_str().unwrap().contains("set code"));
}

/// League auction history: the per-card aggregates are public, but the bid
/// column is not — a sealed-bid auction must never hand a player the bids of
/// their rivals, so each caller sees only their own.
#[tokio::test]
async fn league_history_serves_aggregates_but_only_your_own_bid() {
    use mtg_auction::engine::{Game, Rng};
    use mtg_auction::model::{CardPool, Config, GameMode};

    // Build the closed auction directly: stocking a pool over HTTP would need
    // Scryfall, and these tests stay offline.
    let state = mtg_auction::app::App::new(None);
    let tokens: Vec<String> = {
        let mut game = state.lock_game();
        let cfg = Config {
            mode: GameMode::League,
            player_names: vec!["Alice".into(), "Bob".into(), "Carol".into()],
            starting_money: 10_000,
            league_close_hour: 20,
            league_period_weeks: 1,
            league_first_auction_day: 20_821,
            league_matchmaking_start_day: 20_814,
            seed: 7,
            ..Config::default()
        };
        *game = Game::setup(cfg, CardPool::default()).unwrap();
        let sample = CardPool::sample();
        let rat = sample.commons.iter().find(|c| c.name == "Bog Rat").unwrap().clone();
        game.add_cards(CardPool { exact: Some(vec![(rat, 2)]), ..CardPool::default() }).unwrap();
        game.open_league_auction(1_000).unwrap();
        let card = game.cards.values().find(|c| c.name == "Bog Rat").unwrap().id;
        // Two copies, three bids: clears at 300, high 500, cover 200.
        game.place_league_bid(1, card, 500).unwrap();
        game.place_league_bid(2, card, 300).unwrap();
        game.place_league_bid(3, card, 200).unwrap();
        game.close_league_auction(&mut Rng::new(1)).unwrap();
        (1..=3).map(|id| game.tokens[&id].clone()).collect()
    };

    let app = mtg_auction::api::api_router().with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let c = reqwest::Client::new();

    let fetch = |tok: Option<String>| {
        let c = c.clone();
        let base = base.clone();
        async move {
            let mut req = c.get(format!("{base}/api/league/history"));
            if let Some(t) = tok {
                req = req.header("x-token", t);
            }
            req.send().await.unwrap().json::<Value>().await.unwrap()
        }
    };

    // Alice won at the clearing price, having bid the high.
    let alice = fetch(Some(tokens[0].clone())).await;
    let row = &alice["rows"][0];
    assert_eq!(row["round"], 1);
    assert_eq!(row["copies"], 2);
    assert_eq!(row["cleared"], 300, "both copies clear at the 2nd-highest bid");
    assert_eq!(row["high"], 500);
    assert_eq!(row["cover"], 200, "the highest bid that took nothing");
    assert_eq!(row["my_bid"], 500);
    assert_eq!(row["won"], true);
    assert!(row.get("bids").is_none(), "the raw bid list must never be served");
    assert!(row.get("winners").is_none(), "nor the winner list");

    // Carol was the cover: she sees her own losing bid, not Alice's or Bob's.
    let carol = fetch(Some(tokens[2].clone())).await;
    let row = &carol["rows"][0];
    assert_eq!(row["my_bid"], 200);
    assert_eq!(row["won"], false);
    assert_eq!(row["cleared"], 300, "the public aggregates are the same for everyone");
    assert_eq!(row["high"], 500);

    // A logged-out caller gets the aggregates and no bid at all.
    let anon = fetch(None).await;
    let row = &anon["rows"][0];
    assert_eq!(row["cleared"], 300);
    assert!(row["my_bid"].is_null(), "no token, no bid");
    assert_eq!(row["won"], false);
}

/// The crossing bid and offer are the traders' private limit prices. `/api/log`
/// is admin-gated to protect them, so they must not ride along in the round
/// history that `/api/state` serves to every client.
#[tokio::test]
async fn public_round_history_hides_the_crossing_bid_and_offer() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game_with(&c, &base, &setup_body_house()).await;

    c.post(format!("{base}/api/house/offer")).header("x-token", &alice).send().await.unwrap();
    let card = get_state(&c, &base, Some(&bob)).await["house"][0]["card"].as_u64().unwrap();
    // Bob's limit price is well above what he ends up paying, so a leak here
    // would hand every rival his true valuation.
    c.post(format!("{base}/api/bid")).header("x-token", &bob)
        .json(&json!({ "player": 2, "card": card, "qty": 1, "price": 90000 }))
        .send().await.unwrap();
    c.post(format!("{base}/api/close")).header("x-token", &alice).send().await.unwrap();

    for (who, token) in [("a player", Some(bob.as_str())), ("an anonymous caller", None)] {
        let st = get_state(&c, &base, token).await;
        let rounds = st["history"].as_array().unwrap();
        let traded: Vec<&Value> = rounds.iter().flat_map(|r| r["trades"].as_array().unwrap()).collect();
        assert!(!traded.is_empty(), "the round produced a trade for {who} to see");
        for t in traded {
            assert!(t.get("price").is_some(), "the cleared price stays public");
            assert!(t.get("buyer_name").is_some(), "so does who traded");
            assert!(t.get("bid").is_none(), "the buyer's limit price leaked to {who}");
            assert!(t.get("offer").is_none(), "the seller's limit price leaked to {who}");
        }
    }

    // The host's ledger still has them — that is what it is gated for.
    let log: Value = c.get(format!("{base}/api/log")).header("x-token", &alice)
        .send().await.unwrap().json().await.unwrap();
    let logged: Vec<&Value> = log["trades"].as_array().unwrap()
        .iter().flat_map(|r| r["trades"].as_array().unwrap()).collect();
    assert!(logged.iter().any(|t| t.get("bid").is_some()), "the host can still audit bids");
}
