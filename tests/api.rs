//! HTTP-level integration tests: spin up the real router on an ephemeral port
//! and drive it with reqwest. Uses the offline `sample` set (no network).

use serde_json::{json, Value};

/// Start the API server on a random port and return its base URL.
async fn spawn() -> String {
    let state = mtg_auction::app::App::new(None); // no persistence
    let app = mtg_auction::api::api_router().with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
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
    assert_eq!(post["cards"].as_array().unwrap().len() > 0, true);
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
