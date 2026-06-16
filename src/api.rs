//! HTTP API: JSON handlers over the shared game state.
//!
//! Auth is token-based and deliberately simple. Each player gets a secret
//! token at setup; a request acts as that player by sending it in the
//! `X-Token` header. The first player (the host) is the admin and is the only
//! one who may close rounds or start a new game.

use crate::app::{now_epoch, AppState};
use crate::engine::Game;
use crate::model::*;
use crate::scryfall;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

/// All `/api/*` routes, ready to be given state (and merged with static routes).
pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/api/state", get(get_state))
        .route("/api/events", get(events))
        .route("/api/login", post(login))
        .route("/api/setup", post(setup))
        .route("/api/bid", post(place_bid))
        .route("/api/offer", post(place_offer))
        .route("/api/close", post(close_round))
        .route("/api/log", get(get_log))
}

/// An API error rendered as `{ "error": "..." }` with a status code.
pub struct ApiError {
    status: StatusCode,
    msg: String,
}

impl ApiError {
    fn unauthorized(msg: impl Into<String>) -> Self {
        ApiError { status: StatusCode::UNAUTHORIZED, msg: msg.into() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.msg }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        ApiError { status: StatusCode::BAD_REQUEST, msg }
    }
}

/// Read the `X-Token` header (empty string if absent).
fn token_of(headers: &HeaderMap) -> String {
    headers
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

// ---- Views (what the client sees) ---------------------------------------

#[derive(Serialize)]
pub struct CardView {
    id: CardId,
    name: String,
    rarity: Rarity,
    image: Option<String>,
    ref_price: Option<Cents>,
    type_line: Option<String>,
    cmc: Option<f64>,
    mana_cost: Option<String>,
    /// Total copies of this card held across all players.
    supply: u32,
}

#[derive(Serialize)]
pub struct HoldingView {
    card: CardId,
    name: String,
    qty: u32,
}

#[derive(Serialize)]
pub struct PlayerView {
    id: PlayerId,
    name: String,
    balance: Cents,
    /// Total number of card copies held (who-holds-what is public).
    card_count: u32,
    holdings: Vec<HoldingView>,
}

#[derive(Serialize)]
pub struct OrderView {
    card: CardId,
    name: String,
    qty: u32,
    price: Cents,
}

/// Full state for the client. Public info plus, for the player identified by
/// the request token, that player's own (private) resting orders.
#[derive(Serialize)]
pub struct StateView {
    phase: Phase,
    round: u32,
    total_rounds: u32,
    debt_limit: Cents,
    starting_money: Cents,
    set_name: String,
    cards: Vec<CardView>,
    players: Vec<PlayerView>,
    history: Vec<RoundResult>,
    /// The player the request token belongs to, if any.
    me: Option<PlayerId>,
    am_admin: bool,
    my_bids: Vec<OrderView>,
    my_offers: Vec<OrderView>,
    /// Value the logged-in player has tied up in resting bids, and what's still
    /// free to bid (`balance + debt_limit - committed`).
    my_committed: Cents,
    my_available: Cents,
    /// Auto-close timer: epoch second the round closes (if any) and the
    /// server's current epoch second so the client can show a countdown without
    /// clock-skew.
    round_deadline: Option<u64>,
    round_seconds: u32,
    server_now: u64,
}

fn holdings_of(game: &Game, p: &Player) -> Vec<HoldingView> {
    let mut hs: Vec<HoldingView> = p
        .holdings
        .iter()
        .map(|(&card, &qty)| HoldingView { card, name: game.cards[&card].name.clone(), qty })
        .collect();
    hs.sort_by(|a, b| a.name.cmp(&b.name));
    hs
}

fn orders_view(game: &Game, orders: &HashMap<(PlayerId, CardId), Order>, player: PlayerId) -> Vec<OrderView> {
    let mut v: Vec<OrderView> = orders
        .values()
        .filter(|o| o.player == player)
        .map(|o| OrderView { card: o.card, name: game.cards[&o.card].name.clone(), qty: o.qty, price: o.price })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub async fn get_state(State(state): State<AppState>, headers: HeaderMap) -> Json<StateView> {
    let game = state.game.lock().unwrap();
    let token = token_of(&headers);

    // Total copies of each card in circulation (public market depth).
    let mut supply: HashMap<CardId, u32> = HashMap::new();
    for p in game.players.values() {
        for (&card, &qty) in &p.holdings {
            *supply.entry(card).or_insert(0) += qty;
        }
    }
    let cards = game
        .card_order
        .iter()
        .map(|id| {
            let c = &game.cards[id];
            CardView {
                id: c.id,
                name: c.name.clone(),
                rarity: c.rarity,
                image: c.image.clone(),
                ref_price: c.ref_price,
                type_line: c.type_line.clone(),
                cmc: c.cmc,
                mana_cost: c.mana_cost.clone(),
                supply: supply.get(id).copied().unwrap_or(0),
            }
        })
        .collect();

    let players = game
        .player_order
        .iter()
        .map(|id| {
            let p = &game.players[id];
            PlayerView {
                id: p.id,
                name: p.name.clone(),
                balance: p.balance,
                card_count: p.holdings.values().sum(),
                holdings: holdings_of(&game, p),
            }
        })
        .collect();

    let me = game.player_for_token(&token);
    let (my_bids, my_offers) = match me {
        Some(id) => (orders_view(&game, &game.bids, id), orders_view(&game, &game.offers, id)),
        None => (Vec::new(), Vec::new()),
    };
    let (my_committed, my_available) = match me {
        Some(id) => {
            let committed = game.committed(id);
            let avail = game.players[&id].balance + game.config.debt_limit - committed;
            (committed, avail)
        }
        None => (0, 0),
    };

    Json(StateView {
        phase: game.phase,
        round: game.round,
        total_rounds: game.config.rounds,
        debt_limit: game.config.debt_limit,
        starting_money: game.config.starting_money,
        set_name: game.set_name.clone(),
        cards,
        players,
        history: game.history.clone(),
        me,
        am_admin: game.is_admin(&token),
        my_bids,
        my_offers,
        my_committed,
        my_available,
        round_deadline: game.round_deadline,
        round_seconds: game.config.round_seconds,
        server_now: now_epoch(),
    })
}

/// Server-sent events: emit a tick whenever the game changes so clients refresh.
pub async fn events(State(state): State<AppState>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|_| Ok(Event::default().data("update")));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---- Auth ---------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    token: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    player: Option<PlayerId>,
    name: Option<String>,
    admin: bool,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Result<Json<LoginResponse>, ApiError> {
    let game = state.game.lock().unwrap();
    match game.player_for_token(&req.token) {
        Some(id) => Ok(Json(LoginResponse {
            player: Some(id),
            name: Some(game.players[&id].name.clone()),
            admin: id == game.admin_id,
        })),
        None => Err(ApiError::unauthorized("invalid token")),
    }
}

// ---- Mutations ----------------------------------------------------------

#[derive(Serialize)]
pub struct PlayerToken {
    id: PlayerId,
    name: String,
    token: String,
    admin: bool,
}

#[derive(Serialize)]
pub struct SetupResponse {
    players: Vec<PlayerToken>,
}

pub async fn setup(State(state): State<AppState>, headers: HeaderMap, Json(config): Json<Config>) -> Result<Json<SetupResponse>, ApiError> {
    // A fresh server has no game and anyone may start the first one. Once a
    // game exists, only its host may replace it. Check auth, then release the
    // lock so we don't hold it across the (slow) Scryfall fetch.
    {
        let guard = state.game.lock().unwrap();
        if guard.phase != Phase::Setup && !guard.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can start a new game"));
        }
    }

    let pool = scryfall::fetch_pool(&config.set).await?;
    let mut game = Game::setup(config, pool)?;
    game.arm_timer(now_epoch());
    let players = game
        .player_order
        .iter()
        .map(|&id| PlayerToken {
            id,
            name: game.players[&id].name.clone(),
            token: game.tokens[&id].clone(),
            admin: id == game.admin_id,
        })
        .collect();
    *state.game.lock().unwrap() = game;
    state.save_and_notify();
    Ok(Json(SetupResponse { players }))
}

#[derive(Deserialize)]
pub struct OrderRequest {
    player: PlayerId,
    card: CardId,
    qty: u32,
    price: Cents,
}

/// Confirm the request token belongs to the player it claims to act as.
fn authorize_player(game: &Game, headers: &HeaderMap, player: PlayerId) -> Result<(), ApiError> {
    match game.player_for_token(&token_of(headers)) {
        Some(id) if id == player => Ok(()),
        Some(_) => Err(ApiError::unauthorized("you can only place orders as yourself")),
        None => Err(ApiError::unauthorized("log in first")),
    }
}

pub async fn place_bid(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<OrderRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.game.lock().unwrap();
        authorize_player(&game, &headers, req.player)?;
        game.place_bid(req.player, req.card, req.qty, req.price)?;
    }
    state.save_and_notify();
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn place_offer(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<OrderRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.game.lock().unwrap();
        authorize_player(&game, &headers, req.player)?;
        game.place_offer(req.player, req.card, req.qty, req.price)?;
    }
    state.save_and_notify();
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn close_round(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<RoundResult>, ApiError> {
    let result = {
        let mut game = state.game.lock().unwrap();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can close the auction"));
        }
        let result = game.close_round()?;
        game.arm_timer(now_epoch());
        result
    };
    state.save_and_notify();
    Ok(Json(result))
}

#[derive(Serialize)]
pub struct LedgerView {
    orders: Vec<OrderEvent>,
    trades: Vec<RoundResult>,
}

/// The full order ledger and trade history — admin only, since it reveals
/// everyone's (otherwise sealed) bids and offers.
pub async fn get_log(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<LedgerView>, ApiError> {
    let game = state.game.lock().unwrap();
    if !game.is_admin(&token_of(&headers)) {
        return Err(ApiError::unauthorized("only the host can view the order ledger"));
    }
    Ok(Json(LedgerView {
        orders: game.order_log.clone(),
        trades: game.history.clone(),
    }))
}
