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
        "rounds": 3,
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
        "rounds": 3,
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
    assert_eq!(post["phase"], "bidding");
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
async fn bid_and_offer_same_price_rejected_over_http() {
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

    let bid = c.post(format!("{base}/api/bid"))
        .header("x-token", &alice)
        .json(&json!({ "player": 1, "card": owned, "qty": 1, "price": 500 }))
        .send().await.unwrap();
    assert_eq!(bid.status(), 400);
    let body: Value = bid.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("same price"));
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
    body["round_seconds"] = json!(1);
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

    // Bob (non-host, player 2) reports himself winning; it stays pending until
    // Alice confirms (a host reporting would instead finalize as an override).
    let (aw, bw) = if a == 2 { (2, 0) } else { (0, 2) }; // Bob wins, in seat order
    c.post(format!("{base}/api/ladder/report")).header("x-token", &bob).json(&json!({ "match_id": id, "a_wins": aw, "b_wins": bw })).send().await.unwrap();
    let pending: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    assert_eq!(pending["matches"][0]["status"], "scheduled");
    assert_eq!(pending["matches"][0]["proposed_by"], 2);

    // Bob can't confirm his own proposal; Alice (the opponent) can.
    assert_eq!(c.post(format!("{base}/api/ladder/confirm")).header("x-token", &bob).json(&json!({ "match_id": id })).send().await.unwrap().status(), 400);
    let ok = c.post(format!("{base}/api/ladder/confirm")).header("x-token", &alice).json(&json!({ "match_id": id })).send().await.unwrap();
    assert_eq!(ok.status(), 200);
    let done: Value = c.get(format!("{base}/api/ladder")).send().await.unwrap().json().await.unwrap();
    assert_eq!(done["matches"][0]["status"], "completed");
    assert_eq!(done["standings"][0]["player"], 2, "Bob, the winner, leads on ELO");
    assert_eq!(done["standings"][0]["elo"], 1216);
}

#[tokio::test]
async fn ladder_cancel_costs_elo() {
    let base = spawn().await;
    let c = reqwest::Client::new();
    let (alice, bob) = setup_game(&c, &base).await;

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
        "starting_money": 10000, "debt_limit": 0, "rounds": 2, "num_packs": 1, "pack_size": 6, "seed": 1
    });
    let r = c.post(format!("{base}/api/setup")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.json::<Value>().await.unwrap()["error"].as_str().unwrap().contains("set code"));
}
