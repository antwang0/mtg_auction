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
        .route("/api/players/remove", post(remove_player))
        .route("/api/ladder/pairings", post(override_pairings))
        .route("/api/tokens", get(get_tokens))
        .route("/api/set-code", post(set_set_code))
        .route("/api/house/offer", post(offer_house))
        .route("/api/log", get(get_log))
        .route("/api/ladder", get(get_ladder))
        .route("/api/ladder/availability", post(set_availability))
        .route("/api/ladder/recurring", post(set_recurring))
        .route("/api/ladder/games", post(set_games_per_week))
        .route("/api/ladder/schedule", post(schedule_matches))
        .route("/api/ladder/report", post(report_result))
        .route("/api/ladder/cancel", post(cancel_match))
        .route("/api/ladder/draw-unreported", post(draw_unreported))
        .route("/api/ladder/delete", post(delete_match))
        .route("/api/league/bid", post(place_league_bid))
        .route("/api/league/bid/cancel", post(cancel_league_bid))
        .route("/api/league/open", post(open_league_auction))
        .route("/api/league/history", get(league_history))
        .route("/api/inventory/add", post(inventory_add))
        .route("/api/inventory/remove", post(inventory_remove))
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

/// A trade as *everyone* may see it: who bought what from whom, and the price
/// it actually cleared at.
///
/// Deliberately not [`Trade`], which also carries the crossing `bid` and
/// `offer` — the two traders' private limit prices. Those are exactly what
/// `/api/log` is admin-gated to protect, so they must not ride along in the
/// state every client polls. A sealed-bid league auction leaks a winner's
/// whole valuation otherwise, and in the standard economy it leaks both
/// parties' reservation prices on every fill.
#[derive(Serialize)]
pub struct TradeView {
    card: CardId,
    card_name: String,
    buyer: PlayerId,
    buyer_name: String,
    seller: PlayerId,
    seller_name: String,
    qty: u32,
    price: Cents,
}

/// A closed round as everyone may see it. `clears` is unchanged: it is the
/// per-card top of book, an aggregate attributed to nobody, and the auction
/// publishes it on purpose.
#[derive(Serialize)]
pub struct RoundResultView {
    round: u32,
    trades: Vec<TradeView>,
    clears: Vec<CardClear>,
}

fn public_round(r: &RoundResult) -> RoundResultView {
    RoundResultView {
        round: r.round,
        trades: r
            .trades
            .iter()
            .map(|t| TradeView {
                card: t.card,
                card_name: t.card_name.clone(),
                buyer: t.buyer,
                buyer_name: t.buyer_name.clone(),
                seller: t.seller,
                seller_name: t.seller_name.clone(),
                qty: t.qty,
                price: t.price,
            })
            .collect(),
        clears: r.clears.clone(),
    }
}

/// Full state for the client. Public info plus, for the player identified by
/// the request token, that player's own (private) resting orders.
/// The logged-in player's resting league bids.
#[derive(Serialize)]
pub struct LeagueBidView {
    id: u64,
    card: CardId,
    name: String,
    price: Cents,
}

#[derive(Serialize)]
pub struct StateView {
    /// Which game mode is running (league games behave very differently).
    mode: GameMode,
    phase: Phase,
    round: u32,
    total_rounds: u32,
    debt_limit: Cents,
    starting_money: Cents,
    set_name: String,
    cards: Vec<CardView>,
    players: Vec<PlayerView>,
    history: Vec<RoundResultView>,
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
    /// League mode: whether an auction is currently taking bids (its close
    /// instant is `round_deadline`), the per-close stipend, the informational
    /// packs-per-player count, and the logged-in player's resting bids.
    league_open: bool,
    weekly_stipend: Cents,
    league_packs_per_player: u32,
    my_league_bids: Vec<LeagueBidView>,
    /// League timezone (minutes east of UTC) that auction/matchmaking days are
    /// expressed in, so clients can render the schedule in the league's time.
    league_tz_offset_mins: i32,
    /// True once the last auction date has passed — no more auctions can open.
    league_ended: bool,
    /// The next scheduled auction close (epoch second), even while no auction is
    /// open, so the UI can show when the next one lands. `None` once ended.
    league_next_close: Option<u64>,
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
        Some(id) if game.phase == Phase::League => {
            // League bids are uncapped: any amount may rest across bids, and
            // over-committed bids are amended down at the close. "Available"
            // is therefore just the balance.
            (game.league_committed(id), game.players[&id].balance)
        }
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
    let my_league_bids: Vec<LeagueBidView> = match me {
        Some(id) => {
            let mut v: Vec<LeagueBidView> = game
                .league_bids
                .iter()
                .filter(|b| b.player == id)
                .map(|b| LeagueBidView {
                    id: b.id,
                    card: b.card,
                    name: game.cards[&b.card].name.clone(),
                    price: b.price,
                })
                .collect();
            v.sort_by(|a, b| a.name.cmp(&b.name).then(b.price.cmp(&a.price)));
            v
        }
        None => Vec::new(),
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
        mode: game.config.mode,
        phase: game.phase,
        round: game.round,
        total_rounds: game.phase_rounds(),
        debt_limit: game.config.debt_limit,
        starting_money: game.config.starting_money,
        set_name: game.set_name.clone(),
        cards,
        players,
        history: game.history[game.history.len().saturating_sub(HISTORY_ROUNDS)..]
            .iter()
            .map(public_round)
            .collect(),
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
        league_open: game.league_open(),
        weekly_stipend: game.config.weekly_stipend,
        league_packs_per_player: game.config.league_packs_per_player,
        my_league_bids,
        league_tz_offset_mins: game.config.league_tz_offset_mins,
        league_ended: game.league_ended(now_epoch()),
        league_next_close: if game.config.mode == GameMode::League {
            game.next_league_close_at(now_epoch())
        } else {
            None
        },
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

/// The match schedule and standings as machine-readable plain text (one
/// tab-separated record per line, `#`-prefixed header/comment lines), for
/// programmatic use — e.g. `curl /matches | cut -f2`.
pub async fn matches_text(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    use std::fmt::Write;
    let game = state.lock_game();
    let league = game.is_league();
    let mut out = String::new();
    let _ = writeln!(out, "# generated {}", now_epoch());
    let _ = writeln!(out, "# mode\t{}", if league { "league" } else { "standard" });
    let _ = writeln!(out, "# standings");
    let _ = writeln!(out, "# rank\tname\tpoints\telo\twins\tlosses\tdraws\tgame_wins\tgame_losses\tomw");
    for s in game.standings() {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}",
            s.rank, s.name, s.points, s.elo, s.wins, s.losses, s.draws, s.game_wins, s.game_losses, s.omw
        );
    }
    let _ = writeln!(out, "# matches");
    let _ = writeln!(out, "# id\tstatus\t{}\ta\tb\ta_wins\tb_wins\tdraws", if league { "play_by_epoch" } else { "slot_epoch" });
    let mut ms: Vec<&Match> = game.ladder.matches.iter().collect();
    ms.sort_by_key(|m| (m.slot_start, m.id));
    for m in ms {
        let status = match m.status {
            MatchStatus::Scheduled => "scheduled",
            MatchStatus::Completed => "completed",
            MatchStatus::Cancelled => "cancelled",
            MatchStatus::Expired => "unreported",
        };
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            m.id, status, m.slot_start, m.a_name, m.b_name, m.a_wins, m.b_wins, m.draws
        );
    }
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], out)
}

/// Case-insensitive player lookup by name.
fn player_by_name(game: &Game, name: &str) -> Result<PlayerId, ApiError> {
    let name = name.trim();
    game.players
        .values()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.id)
        .ok_or_else(|| format!("no player named \"{name}\"").into())
}

#[derive(Deserialize)]
pub struct RemovePlayerRequest {
    name: String,
}

/// Host: remove a player (holdings return to the house; upcoming matches are
/// dropped and their opponents re-paired by the scheduler).
pub async fn remove_player(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<RemovePlayerRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    let name = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can remove players"));
        }
        let id = player_by_name(&game, &req.name)?;
        let name = game.players[&id].name.clone();
        game.remove_player(id)?;
        game.auto_schedule(now_epoch()); // re-pair anyone their removal freed up
        name
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "removed": name })))
}

#[derive(Deserialize)]
pub struct PairingsRequest {
    text: String,
}

/// Host: manually override pairings from a pasted text — one match per line,
/// two player names separated by a comma, tab, " vs ", or (for single-word
/// names) whitespace. Listed players' upcoming matches are replaced.
pub async fn override_pairings(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<PairingsRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    let created = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can set pairings"));
        }
        let mut pairs: Vec<(PlayerId, PlayerId)> = Vec::new();
        for line in req.text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let two: Vec<&str> = if let Some((a, b)) = line.split_once(['\t', ',']) {
                vec![a, b]
            } else if let Some((a, b)) = line.split_once(" vs ") {
                vec![a, b]
            } else {
                line.split_whitespace().collect()
            };
            if two.len() != 2 {
                return Err(format!(
                    "couldn't read the line \"{line}\" — use two names per line, separated by a comma (needed when names contain spaces)"
                ).into());
            }
            pairs.push((player_by_name(&game, two[0])?, player_by_name(&game, two[1])?));
        }
        game.override_pairings(&pairs, now_epoch())?;
        game.auto_schedule(now_epoch()); // re-pair any opponents the override freed
        pairs.len()
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "created": created })))
}

#[derive(Deserialize)]
pub struct SetCodeRequest {
    set: String,
}

/// Host: change the Scryfall set code(s) that pin card-name lookups, mid-game
/// (e.g. to add a companion set like a Mystical Archive). Re-adding a card
/// afterwards refreshes its image and rarity from the right printing.
pub async fn set_set_code(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<SetCodeRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can change the set code"));
        }
        let set = req.set.trim();
        if set.len() > 60 {
            return Err("set code list is too long".to_string().into());
        }
        game.config.set = if set.is_empty() { "sample".to_string() } else { set.to_string() };
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Host: every player's login token, for re-sharing lost magic links.
pub async fn get_tokens(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<SetupResponse>, ApiError> {
    let game = state.lock_game();
    if !game.is_admin(&token_of(&headers)) {
        return Err(ApiError::unauthorized("only the host can view login tokens"));
    }
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
    Ok(Json(SetupResponse { players }))
}

pub async fn setup(State(state): State<AppState>, headers: HeaderMap, Json(mut config): Json<Config>) -> Result<Json<SetupResponse>, ApiError> {
    // League games: fill any unset (0) schedule days from the current time,
    // aligning the auctions with the match rounds. Matchmaking opens
    // immediately (each player's first N matches are assigned as soon as the
    // game starts); an auction closes at the end of each round except the
    // last, i.e. every N weeks, `rounds − 1` times.
    if config.mode == GameMode::League {
        let n = config.league_pending_per_player.clamp(1, 20) as i64;
        if config.league_matchmaking_start_day == 0 {
            config.league_matchmaking_start_day =
                crate::engine::league_day_of(now_epoch(), config.league_tz_offset_mins);
        }
        if config.league_first_auction_day == 0 {
            config.league_first_auction_day = config.league_matchmaking_start_day + 7 * n;
            // The derived schedule also sets the cadence and end to match the
            // rounds (a host-supplied first-auction date leaves them alone).
            config.league_period_weeks = n.clamp(1, 8) as u32;
            if config.league_last_auction_day == 0 {
                let auctions = config.league_rounds.saturating_sub(1).max(1) as i64;
                config.league_last_auction_day =
                    config.league_first_auction_day + (auctions - 1) * 7 * n;
            }
        }
    }
    // A fresh server has no game and anyone may start the first one. Once a
    // game exists, only its host may replace it. Check auth, then release the
    // lock so we don't hold it across the (slow) Scryfall fetch.
    {
        let guard = state.lock_game();
        if guard.phase != Phase::Setup && !guard.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can start a new game"));
        }
    }

    // The pool sources are mutually exclusive — exactly one is used. League
    // games start with no pool at all (the host stocks the auction weekly).
    let pool = if config.mode == GameMode::League {
        CardPool::default()
    } else {
        match config.pool_source {
        PoolSource::Sample => crate::model::CardPool::sample(),
        PoolSource::Scryfall => {
            let code = config.set.trim().to_lowercase();
            if code.is_empty() || code == "sample" {
                return Err("choose a Scryfall set code (or pick the sample/manual source)".to_string().into());
            }
            scryfall::fetch_pool(&config.set).await?
        }
        PoolSource::Manual => scryfall::fetch_decklist_pool(&config.card_list, Some(&config.set)).await?,
        }
    };
    let mut game = Game::setup(config, pool)?;
    game.arm_timer(now_epoch());
    // League: assign every player's first matches immediately.
    game.auto_schedule(now_epoch());
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
        let result = if game.phase == Phase::League {
            let mut rng = crate::engine::Rng::new(now_epoch() ^ game.config.seed);
            game.close_league_auction(&mut rng)?
        } else {
            game.close_round()?
        };
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

/// The buyer (or the host) marks a delivery received (settling it).
pub async fn receive_delivery(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<DeliveryRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        let is_admin = game.is_admin(&token_of(&headers));
        game.mark_delivery_received(me, req.delivery_id, is_admin)?;
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
    let set = {
        let game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can add cards"));
        }
        game.config.set.clone()
    };
    let pool = scryfall::fetch_decklist_pool(&req.card_list, Some(&set)).await?;
    let added = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can add cards"));
        }
        let added = game.add_cards(pool)?;
        // League: stocking the pool opens the next auction automatically.
        if game.phase == Phase::League && !game.league_open() {
            let _ = game.open_league_auction(now_epoch());
        }
        added
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
    /// The request player's recurring weekly availability (weekly-slot indices).
    my_recurring: Vec<u32>,
    my_games_per_week: u32,
    /// Whether this is a league game (deadline-based matches, swiss standings),
    /// so match views can label times as play-by deadlines.
    league: bool,
}

pub async fn get_ladder(State(state): State<AppState>, headers: HeaderMap) -> Json<LadderView> {
    let game = state.lock_game();
    let me = game.player_for_token(&token_of(&headers));
    let (my_availability, my_recurring, my_games_per_week) = match me {
        Some(id) => (
            game.ladder.availability.get(&id).cloned().unwrap_or_default(),
            game.ladder.recurring.get(&id).cloned().unwrap_or_default(),
            game.quota(id),
        ),
        None => (Vec::new(), Vec::new(), 0),
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
        my_recurring,
        my_games_per_week,
        league: game.is_league(),
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
pub struct RecurringRequest {
    slots: Vec<u32>,
}

pub async fn set_recurring(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<RecurringRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.set_recurring(me, req.slots)?;
        game.auto_schedule(now_epoch()); // new recurring availability may enable matches
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

/// Host: delete a match from the record entirely (reverting any applied ELO;
/// swiss standings recompute from the remaining matches).
pub async fn delete_match(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<MatchRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can delete matches"));
        }
        game.delete_match(req.match_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Host: record every match whose play-by deadline has passed without a
/// result as a 1-1 draw (league housekeeping, e.g. at the end of the season).
pub async fn draw_unreported(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let recorded = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can record unreported matches"));
        }
        game.record_unreported_as_draws(now_epoch())
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "recorded": recorded })))
}

/// Host-triggered scheduling pass (the timer also runs this automatically).
pub async fn schedule_matches(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let created = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can run the scheduler"));
        }
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

/// Enter a match result. Either participant reports it and it's final
/// immediately (no opponent confirmation). The host may report any match too,
/// and can re-report a completed one to correct a mistake.
pub async fn report_result(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ReportRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let token = token_of(&headers);
        let me = require_player(&game, &headers)?;
        if game.is_admin(&token) {
            game.force_match_result(req.match_id, req.a_wins, req.b_wins, req.draws)?;
        } else {
            game.submit_match_result(me, req.match_id, req.a_wins, req.b_wins, req.draws)?;
        }
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct MatchRequest {
    match_id: u64,
}

// ---- League mode --------------------------------------------------------

#[derive(Deserialize)]
pub struct LeagueBidRequest {
    card: CardId,
    price: Cents,
}

/// Place one single-copy bid in the open league auction.
pub async fn place_league_bid(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<LeagueBidRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    let id = {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.place_league_bid(me, req.card, req.price)?
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "bid_id": id })))
}

#[derive(Deserialize)]
pub struct LeagueCancelRequest {
    bid_id: u64,
}

/// Cancel one of your own resting league bids.
pub async fn cancel_league_bid(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<LeagueCancelRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.cancel_league_bid(me, req.bid_id)?;
    }
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Host: (re)open the league auction over the current pool. Stocking the pool
/// via `/api/cards/add` opens it automatically; this covers the carried-over
/// unsold pool when nothing new is added.
pub async fn open_league_auction(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let closes = {
        let mut game = state.lock_game();
        if !game.is_admin(&token_of(&headers)) {
            return Err(ApiError::unauthorized("only the host can open the auction"));
        }
        game.open_league_auction(now_epoch())?
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "closes": closes })))
}

/// One card's result in one closed league auction, as the caller may see it:
/// the public aggregates, plus their own bid and whether they took a copy.
#[derive(Serialize)]
pub struct LeagueHistoryRow {
    round: u32,
    card: CardId,
    card_name: String,
    rarity: Rarity,
    copies: u32,
    cleared: Cents,
    high: Option<Cents>,
    cover: Option<Cents>,
    /// The caller's own bid, or `None` if they didn't bid on this card. Another
    /// player's bid is never served here — the auction is sealed.
    my_bid: Option<Cents>,
    /// Whether the caller took a copy (possibly a free leftover, with no bid).
    won: bool,
}

#[derive(Serialize)]
pub struct LeagueHistoryResponse {
    rows: Vec<LeagueHistoryRow>,
}

/// The per-card history of every closed league auction. Served on its own
/// rather than folded into `/api/state` because it is a cold, bulky payload
/// that only matters when a player opens the History tab, and `/api/state` is
/// polled by everyone.
pub async fn league_history(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<LeagueHistoryResponse>, ApiError> {
    let game = state.lock_game();
    if !game.is_league() {
        return Err("auction history is league-only".to_string().into());
    }
    let me = game.player_for_token(&token_of(&headers));
    let rows = game
        .league_clears
        .iter()
        .map(|c| LeagueHistoryRow {
            round: c.round,
            card: c.card,
            card_name: c.card_name.clone(),
            rarity: game.cards.get(&c.card).map_or(Rarity::Common, |card| card.rarity),
            copies: c.copies,
            cleared: c.cleared,
            high: c.high,
            cover: c.cover,
            my_bid: me.and_then(|id| c.bids.iter().find(|(p, _)| *p == id).map(|(_, price)| *price)),
            won: me.is_some_and(|id| c.winners.contains(&id)),
        })
        .collect();
    Ok(Json(LeagueHistoryResponse { rows }))
}

#[derive(Deserialize)]
pub struct InventoryAddRequest {
    card_list: String,
}

/// League mode: a player adds cards (e.g. their opened packs) to their own
/// inventory, for planning. Purely manual and optional.
pub async fn inventory_add(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<InventoryAddRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    // Authorize before the (slow) metadata fetch, and again before mutating.
    let set = {
        let game = state.lock_game();
        require_player(&game, &headers)?;
        if game.phase != Phase::League {
            return Err("manual inventory edits are only for league games".to_string().into());
        }
        game.config.set.clone()
    };
    let pool = scryfall::fetch_decklist_pool(&req.card_list, Some(&set)).await?;
    let added = {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.inventory_add(me, pool)?
    };
    state.save_and_notify().await;
    Ok(Json(serde_json::json!({ "added": added })))
}

#[derive(Deserialize)]
pub struct InventoryRemoveRequest {
    card: CardId,
    qty: u32,
}

/// League mode: a player removes copies from their own inventory.
pub async fn inventory_remove(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<InventoryRemoveRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    {
        let mut game = state.lock_game();
        let me = require_player(&game, &headers)?;
        game.inventory_remove(me, req.card, req.qty)?;
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
