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
    extract::{Query, State},
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
        .route("/api/password-login", post(password_login))
        .route("/api/set-password", post(set_password))
        .route("/api/setup", post(setup))
        .route("/api/set-cards", get(get_set_cards))
        .route("/api/bid", post(place_bid))
        .route("/api/offer", post(place_offer))
        .route("/api/close", post(close_round))
        .route("/api/deliveries/receive", post(receive_delivery))
        .route("/api/deliveries/reverse", post(reverse_delivery))
        .route("/api/reports", post(add_report))
        .route("/api/reports/resolve", post(resolve_report))
        .route("/api/reports/amend", post(amend_report))
        .route("/api/reports/delete", post(delete_report))
        .route("/api/cards/add", post(add_cards))
        .route("/api/players/add", post(add_player))
        .route("/api/house/offer", post(offer_house))
        .route("/api/log", get(get_log))
        .route("/api/ladder", get(get_ladder))
        .route("/api/ladder/availability", post(set_availability))
        .route("/api/ladder/games", post(set_games_per_week))
        .route("/api/ladder/schedule", post(schedule_matches))
        .route("/api/ladder/report", post(report_result))
        .route("/api/ladder/confirm", post(confirm_result))
        .route("/api/ladder/cancel", post(cancel_match))
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
    /// Canonical `WUBRG` colour string (empty = colorless), for drawing pips.
    colors: String,
    /// Canonical `WUBRG` colour-identity string (empty = colorless), for the
    /// colour filter.
    color_identity: String,
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
    /// Ladder ELO rating.
    elo: i64,
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

/// One row of a player's personal trade history.
#[derive(Serialize)]
pub struct PlayerTradeView {
    round: u32,
    card: CardId,
    name: String,
    /// "bought" or "sold", from this player's perspective.
    side: &'static str,
    counterparty: String,
    qty: u32,
    price: Cents,
}

fn player_trade_views(game: &Game, player: PlayerId) -> Vec<PlayerTradeView> {
    game.player_trades(player)
        .into_iter()
        .map(|(round, t)| {
            let bought = t.buyer == player;
            PlayerTradeView {
                round,
                card: t.card,
                name: t.card_name,
                side: if bought { "bought" } else { "sold" },
                counterparty: if bought { t.seller_name } else { t.buyer_name },
                qty: t.qty,
                price: t.price,
            }
        })
        .collect()
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
    /// Whether the logged-in player has set a password.
    my_has_password: bool,
    my_bids: Vec<OrderView>,
    my_offers: Vec<OrderView>,
    /// The logged-in player's own trade history (most recent last).
    my_trades: Vec<PlayerTradeView>,
    /// Value the logged-in player has tied up in resting bids, and what's still
    /// free to bid (`balance + debt_limit - committed`).
    my_committed: Cents,
    my_available: Cents,
    /// Unallocated (house) cards available to be offered or dealt to joiners.
    house: Vec<HoldingView>,
    house_balance: Cents,
    /// Auto-close timer: epoch second the round closes (if any) and the
    /// server's current epoch second so the client can show a countdown without
    /// clock-skew.
    round_deadline: Option<u64>,
    round_seconds: u32,
    server_now: u64,
    /// Deliveries involving the logged-in player (as buyer or seller). Empty when
    /// not logged in.
    my_deliveries: Vec<Delivery>,
    /// Every delivery in the game — populated only for the host (else empty).
    all_deliveries: Vec<Delivery>,
    /// Bug reports / feature requests — populated only for the host (else empty).
    reports: Vec<Report>,
    /// False when the most recent save failed to reach the disk (the game is
    /// effectively running without persistence); the admin page shows a warning.
    save_ok: bool,
    /// How many rounds have closed in total. `history` only carries the most
    /// recent [`HISTORY_ROUNDS`]; clients use this counter to detect new closes.
    rounds_closed: usize,
}

/// How many closed rounds `/api/state` includes. History grows every round and
/// is refetched by every client on every change, so the payload is capped;
/// the full log stays available to the host via `/api/log`.
const HISTORY_ROUNDS: usize = 20;

/// How many ledger entries `/api/log` returns (newest last). The order log is
/// append-only and unbounded; the admin UI only shows recent activity.
const LOG_ORDERS: usize = 500;

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

pub async fn get_state(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let token = token_of(&headers);

    // Cheap revalidation: the ETag is the change counter plus who's asking
    // (the payload contains per-player private fields). Browsers revalidate
    // automatically under `Cache-Control: no-cache`, so the 30s safety polls
    // answer 304 here without locking or serializing anything when the game
    // hasn't changed.
    let etag = {
        let game = state.lock_game();
        let me = game.player_for_token(&token);
        format!("W/\"g{}-p{}\"", state.version(), me.map_or(-1, |id| id as i64))
    };
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(axum::http::header::ETAG, etag), (axum::http::header::CACHE_CONTROL, "no-cache, private".into())],
        )
            .into_response();
    }

    let game = state.lock_game();

    // Total copies of each card in circulation (public market depth), including
    // the unallocated house inventory.
    let mut supply: HashMap<CardId, u32> = HashMap::new();
    for p in game.players.values() {
        for (&card, &qty) in &p.holdings {
            *supply.entry(card).or_insert(0) += qty;
        }
    }
    for (&card, &qty) in &game.house.holdings {
        *supply.entry(card).or_insert(0) += qty;
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
                colors: c.colors.clone(),
                color_identity: c.color_identity.clone(),
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
                elo: p.elo,
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
    let my_trades = me.map(|id| player_trade_views(&game, id)).unwrap_or_default();
    let my_has_password = me.is_some_and(|id| game.has_password(id));
    let my_deliveries: Vec<Delivery> = match me {
        Some(id) => game.deliveries.iter().filter(|d| d.buyer == id || d.seller == id).cloned().collect(),
        None => Vec::new(),
    };
    let all_deliveries: Vec<Delivery> =
        if game.is_admin(&token) { game.deliveries.clone() } else { Vec::new() };
    let reports: Vec<Report> = if game.is_admin(&token) { game.reports.clone() } else { Vec::new() };
    let (my_committed, my_available) = match me {
        Some(id) => {
            let committed = game.committed(id);
            // Fills only ever lower committed by at least as much as they lower
            // the balance, so this stays >= 0 in practice; clamp defensively so
            // the UI never shows a negative "available to bid".
            let avail = (game.players[&id].balance + game.config.debt_limit - committed).max(0);
            (committed, avail)
        }
        None => (0, 0),
    };

    // Unallocated house inventory (public — these cards exist in the game).
    let mut house: Vec<HoldingView> = game
        .house
        .holdings
        .iter()
        .map(|(&card, &qty)| HoldingView { card, name: game.cards[&card].name.clone(), qty })
        .collect();
    house.sort_by(|a, b| a.name.cmp(&b.name));

    let view = StateView {
        phase: game.phase,
        round: game.round,
        total_rounds: game.phase_rounds(),
        debt_limit: game.config.debt_limit,
        starting_money: game.config.starting_money,
        set_name: game.set_name.clone(),
        cards,
        players,
        history: game.history[game.history.len().saturating_sub(HISTORY_ROUNDS)..].to_vec(),
        me,
        am_admin: game.is_admin(&token),
        my_has_password,
        my_bids,
        my_offers,
        my_trades,
        my_committed,
        my_available,
        house,
        house_balance: game.house.balance,
        round_deadline: game.round_deadline,
        round_seconds: game.round_seconds(),
        server_now: now_epoch(),
        my_deliveries,
        all_deliveries,
        reports,
        save_ok: state.persistence_ok(),
        rounds_closed: game.history.len(),
    };
    (
        [(axum::http::header::ETAG, etag), (axum::http::header::CACHE_CONTROL, "no-cache, private".into())],
        Json(view),
    )
        .into_response()
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
    let game = state.lock_game();
    match game.player_for_token(&req.token) {
        Some(id) => Ok(Json(LoginResponse {
            player: Some(id),
            name: Some(game.players[&id].name.clone()),
            admin: id == game.admin_id,
        })),
        None => Err(ApiError::unauthorized("invalid token")),
    }
}

#[derive(Deserialize)]
pub struct PasswordLoginRequest {
    name: String,
    password: String,
}

/// What a successful password login hands back. `token` is the player's bearer
/// token, which the client then stores and sends as `X-Token` like normal.
#[derive(Serialize)]
pub struct PasswordLoginResponse {
    player: PlayerId,
    name: String,
    admin: bool,
    token: String,
}

/// Log in by name + password, returning the player's token for the session.
pub async fn password_login(State(state): State<AppState>, Json(req): Json<PasswordLoginRequest>) -> Result<Json<PasswordLoginResponse>, ApiError> {
    let game = state.lock_game();
    match game.player_for_name_password(&req.name, &req.password) {
        Some(id) => Ok(Json(PasswordLoginResponse {
            player: id,
            name: game.players[&id].name.clone(),
            admin: id == game.admin_id,
            token: game.tokens[&id].clone(),
        })),
        None => Err(ApiError::unauthorized("wrong name or password")),
    }
}

#[derive(Deserialize)]
pub struct SetPasswordRequest {
    password: String,
}

/// Set (or change) your own login password. Requires a valid token (a magic
/// link or an existing password session).
pub async fn set_password(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<SetPasswordRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.set_password(me, &req.password)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
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
        let guard = state.lock_game();
        if guard.phase != Phase::Setup && !guard.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can start a new game"));
        }
    }

    // The pool sources are mutually exclusive — exactly one is used.
    let pool = match config.pool_source {
        PoolSource::Sample => crate::model::CardPool::sample(),
        PoolSource::Scryfall => {
            let code = config.set.trim().to_lowercase();
            if code.is_empty() || code == "sample" {
                return Err("choose a Scryfall set code (or pick the sample/manual source)".to_string().into());
            }
            scryfall::fetch_pool(&config.set).await?
        }
        PoolSource::Manual => scryfall::fetch_decklist_pool(&config.card_list).await?,
    };
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
    {
        // Re-check auth under the final lock: another host could have created a
        // game while we were fetching the set, and only its host may replace it.
        // (Any orders placed during the fetch are intentionally discarded — this
        // is a deliberate "start a new game" reset.)
        let mut guard = state.lock_game();
        if guard.phase != Phase::Setup && !guard.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can start a new game"));
        }
        // Bug reports / feature requests are about the app, not the game, so they
        // survive a reset.
        let (reports, seq) = guard.take_reports();
        game.restore_reports(reports, seq);
        *guard = game;
    }
    state.save_and_notify().await;
    Ok(Json(SetupResponse { players }))
}

#[derive(Deserialize)]
pub struct SetCardsQuery {
    set: String,
}

#[derive(Serialize)]
pub struct SetCard {
    name: String,
    rarity: Rarity,
    ref_price: Option<Cents>,
    /// Canonical `WUBRG`-ordered colour string (empty = colorless), for the
    /// picker's colour pips.
    colors: String,
    /// Canonical `WUBRG`-ordered colour-identity string (empty = colorless),
    /// for the picker's colour filter.
    color_identity: String,
}

#[derive(Serialize)]
pub struct SetCardsResponse {
    set_name: String,
    cards: Vec<SetCard>,
}

/// List a set's cards (name, rarity, reference price) so the host can build a
/// manual card list by picking from it. Uses the same cached Scryfall fetch as
/// setup; `sample` returns the built-in offline set. Open during initial setup;
/// host-only once a game is in progress (to avoid mid-game Scryfall spam).
pub async fn get_set_cards(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<SetCardsQuery>) -> Result<Json<SetCardsResponse>, ApiError> {
    {
        let game = state.lock_game();
        if game.phase != Phase::Setup && !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can browse sets while a game is on"));
        }
    }
    let pool = scryfall::fetch_pool(&q.set).await?;
    let mut cards: Vec<SetCard> = pool
        .commons
        .iter()
        .chain(&pool.uncommons)
        .chain(&pool.rares)
        .chain(&pool.mythics)
        .map(|pc| SetCard { name: pc.name.clone(), rarity: pc.rarity, ref_price: pc.ref_price, colors: pc.colors.clone(), color_identity: pc.color_identity.clone() })
        .collect();
    cards.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(SetCardsResponse { set_name: pool.set_name, cards }))
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
        let mut game = state.lock_game();
        authorize_player(&game, &headers, req.player)?;
        game.place_bid(req.player, req.card, req.qty, req.price)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn place_offer(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<OrderRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        authorize_player(&game, &headers, req.player)?;
        game.place_offer(req.player, req.card, req.qty, req.price)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn close_round(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<RoundResult>, ApiError> {
    let result = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can close the auction"));
        }
        let result = game.close_round()?;
        game.record_deliveries(&result, now_epoch());
        game.arm_timer(now_epoch());
        result
    };
    state.save_and_notify().await;
    // Snapshot after a close so a catastrophe loses at most one round.
    state.backup_on_close(now_epoch());
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct DeliveryRequest {
    delivery_id: u64,
}

/// The buyer marks one of their deliveries received (settling it).
pub async fn receive_delivery(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<DeliveryRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.mark_delivery_received(me, req.delivery_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Host: reverse a delivery to correct an error (no penalty).
pub async fn reverse_delivery(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<DeliveryRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can reverse a delivery"));
        }
        game.reverse_delivery(req.delivery_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct FeedbackRequest {
    kind: ReportKind,
    text: String,
}

/// Anyone (logged in or not) can file a bug report or feature request.
pub async fn add_report(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<FeedbackRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let reporter = game.player_for_token(&token_of(&headers));
        game.add_report(req.kind, &req.text, reporter, now_epoch())?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ResolveReportRequest {
    report_id: u64,
    resolved: bool,
}

/// Host: mark a report resolved or reopen it.
pub async fn resolve_report(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ResolveReportRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can update reports"));
        }
        game.set_report_resolved(req.report_id, req.resolved)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AmendReportRequest {
    report_id: u64,
    kind: ReportKind,
    text: String,
}

/// Host: amend a report's kind and text.
pub async fn amend_report(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<AmendReportRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can update reports"));
        }
        game.amend_report(req.report_id, req.kind, &req.text)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct DeleteReportRequest {
    report_id: u64,
}

/// Host: delete a report.
pub async fn delete_report(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<DeleteReportRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can delete reports"));
        }
        game.delete_report(req.report_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct LedgerView {
    orders: Vec<OrderEvent>,
    trades: Vec<RoundResult>,
}

/// The full order ledger and trade history — admin only, since it reveals
/// everyone's (otherwise sealed) bids and offers.
pub async fn get_log(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<LedgerView>, ApiError> {
    let game = state.lock_game();
    if !game.is_admin(&token_of(&headers)) {
        return Err(ApiError::unauthorized("only the host can view the order ledger"));
    }
    // Both logs are append-only and unbounded; send only the recent tail the
    // admin UI actually shows (newest entries are last, as before).
    Ok(Json(LedgerView {
        orders: game.order_log[game.order_log.len().saturating_sub(LOG_ORDERS)..].to_vec(),
        trades: game.history[game.history.len().saturating_sub(HISTORY_ROUNDS)..].to_vec(),
    }))
}

// ---- Mid-game host actions ----------------------------------------------

#[derive(Deserialize)]
pub struct AddCardsRequest {
    card_list: String,
}

/// Host: add cards (from a pasted list) to the unallocated house inventory after
/// the game has started.
pub async fn add_cards(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<AddCardsRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    // Authorize before the (slow) metadata fetch, and again before mutating.
    {
        let game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can add cards"));
        }
    }
    let pool = scryfall::fetch_decklist_pool(&req.card_list).await?;
    let added = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can add cards"));
        }
        game.add_cards(pool)?
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "added": added })))
}

#[derive(Deserialize)]
pub struct AddPlayerRequest {
    name: String,
}

#[derive(Serialize)]
pub struct AddPlayerResponse {
    player: PlayerId,
    name: String,
    token: String,
}

/// Host: add a player mid-game, dealing them their allocation from the house.
pub async fn add_player(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<AddPlayerRequest>) -> Result<Json<AddPlayerResponse>, ApiError> {
    let resp = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can add players"));
        }
        let id = game.add_player(req.name)?;
        AddPlayerResponse { player: id, name: game.players[&id].name.clone(), token: game.tokens[&id].clone() }
    };
    state.save_and_notify().await;
    Ok(Json(resp))
}

/// Host: list the house's unallocated cards into the auction at a noisy
/// reference price.
pub async fn offer_house(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let listed = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can offer house cards"));
        }
        // A fresh seed per call so re-listing re-rolls the noise.
        let mut rng = crate::engine::Rng::new(now_epoch() ^ game.config.seed);
        game.offer_house_cards(&mut rng)?
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "listed": listed })))
}

// ---- ELO ladder ---------------------------------------------------------

/// Public ladder view: standings and all matches, plus the calendar shape and,
/// for the request's player, their own availability and weekly target.
#[derive(Serialize)]
pub struct LadderView {
    standings: Vec<Standing>,
    matches: Vec<Match>,
    /// Block start hours (UTC) within each day, e.g. `[9, 13, 18, 21]`.
    blocks: Vec<u32>,
    window_days: u32,
    max_games_per_week: u32,
    server_now: u64,
    me: Option<PlayerId>,
    my_availability: Vec<i64>,
    my_games_per_week: u32,
}

pub async fn get_ladder(State(state): State<AppState>, headers: HeaderMap) -> Json<LadderView> {
    let game = state.lock_game();
    let me = game.player_for_token(&token_of(&headers));
    let (my_availability, my_games_per_week) = match me {
        Some(id) => (
            game.ladder.availability.get(&id).cloned().unwrap_or_default(),
            game.ladder.games_per_week.get(&id).copied().unwrap_or(0),
        ),
        None => (Vec::new(), 0),
    };
    Json(LadderView {
        standings: game.standings(),
        matches: game.ladder.matches.clone(),
        blocks: game.config.ladder_block_hours.clone(),
        window_days: game.config.schedule_window_days,
        max_games_per_week: game.config.max_games_per_week,
        server_now: now_epoch(),
        me,
        my_availability,
        my_games_per_week,
    })
}

/// Resolve the request token to a player, or 401.
fn require_player(game: &Game, headers: &HeaderMap) -> Result<PlayerId, ApiError> {
    game.player_for_token(&token_of(headers)).ok_or_else(|| ApiError::unauthorized("log in first"))
}

#[derive(Deserialize)]
pub struct AvailabilityRequest {
    slots: Vec<i64>,
}

pub async fn set_availability(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<AvailabilityRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.set_availability(me, req.slots)?;
        game.auto_schedule(now_epoch()); // new availability may enable matches
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct GamesRequest {
    games_per_week: u32,
}

pub async fn set_games_per_week(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<GamesRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.set_games_per_week(me, req.games_per_week)?;
        game.auto_schedule(now_epoch()); // a higher target may enable matches
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Host-triggered scheduling pass (the timer also runs this automatically).
pub async fn schedule_matches(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let created = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can run the scheduler"));
        }
        game.expire_stale_matches(now_epoch());
        game.auto_schedule(now_epoch())
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "created": created })))
}

#[derive(Deserialize)]
pub struct ReportRequest {
    match_id: u64,
    a_wins: u32,
    b_wins: u32,
    #[serde(default)]
    draws: u32,
}

/// Enter a match result. A player reports their own match (pending until the
/// opponent confirms); the host reports any match directly as a final override.
pub async fn report_result(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ReportRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let token = token_of(&headers);
        let me = require_player(&game, &headers)?;
        if game.is_admin(&token) {
            game.force_match_result(req.match_id, req.a_wins, req.b_wins, req.draws)?;
        } else {
            game.propose_match_result(me, req.match_id, req.a_wins, req.b_wins, req.draws)?;
        }
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct MatchRequest {
    match_id: u64,
}

/// The opposing player confirms a pending result, making it final.
pub async fn confirm_result(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<MatchRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.confirm_match_result(me, req.match_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// A player cancels a scheduled match, taking the ELO penalty.
pub async fn cancel_match(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<MatchRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.cancel_match(me, req.match_id)?;
        game.auto_schedule(now_epoch()); // freed slot/quota may enable matches
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
