//! D&D draft auction game — web server.

use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use mtg_auction::{api, app};
use std::path::PathBuf;

const INDEX_HTML: &str = include_str!("../static/index.html");
const UTIL_JS: &str = include_str!("../static/util.js");
const APP_CORE_JS: &str = include_str!("../static/app-core.js");
const APP_HOME_JS: &str = include_str!("../static/app-home.js");
const APP_MARKET_JS: &str = include_str!("../static/app-market.js");
const APP_LADDER_JS: &str = include_str!("../static/app-ladder.js");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");
const ADMIN_HTML: &str = include_str!("../static/admin.html");
const ADMIN_JS: &str = include_str!("../static/admin.js");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn admin() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

fn js(body: &'static str) -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "application/javascript")], body)
}

async fn util_js() -> impl IntoResponse {
    js(UTIL_JS)
}

async fn app_core_js() -> impl IntoResponse {
    js(APP_CORE_JS)
}

async fn app_home_js() -> impl IntoResponse {
    js(APP_HOME_JS)
}

async fn app_market_js() -> impl IntoResponse {
    js(APP_MARKET_JS)
}

async fn app_ladder_js() -> impl IntoResponse {
    js(APP_LADDER_JS)
}

async fn app_js() -> impl IntoResponse {
    js(APP_JS)
}

async fn admin_js() -> impl IntoResponse {
    js(ADMIN_JS)
}

async fn style_css() -> impl IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/css")], STYLE_CSS)
}

#[tokio::main]
async fn main() {
    // Structured logging: RUST_LOG controls the filter (default: info for us,
    // warn for dependencies' request noise).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    // Persist the game to disk so a session survives a restart. Set STATE_FILE
    // to a path, or to an empty string to disable persistence.
    let state_file = match std::env::var("STATE_FILE") {
        Ok(s) if s.is_empty() => None,
        Ok(s) => Some(PathBuf::from(s)),
        Err(_) => Some(PathBuf::from("game_state.json")),
    };
    let state = app::App::new(state_file);

    // Background task: auto-close rounds whose timer has expired.
    tokio::spawn(app::timer_loop(state.clone()));

    let app = Router::new()
        .route("/", get(index))
        .route("/admin", get(admin))
        .route("/util.js", get(util_js))
        .route("/app-core.js", get(app_core_js))
        .route("/app-home.js", get(app_home_js))
        .route("/app-market.js", get(app_market_js))
        .route("/app-ladder.js", get(app_ladder_js))
        .route("/app.js", get(app_js))
        .route("/admin.js", get(admin_js))
        .route("/style.css", get(style_css))
        .merge(api::api_router())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("could not bind {addr}: {e}");
            match e.kind() {
                std::io::ErrorKind::AddrInUse => tracing::error!(
                    "that port is already in use. Pick another, e.g. `BIND=127.0.0.1:8080 cargo run`."
                ),
                std::io::ErrorKind::PermissionDenied => tracing::error!(
                    "ports below 1024 need root. Use a higher port, e.g. `BIND=127.0.0.1:8080 cargo run`."
                ),
                _ => {}
            }
            std::process::exit(1);
        }
    };
    tracing::info!("auction house open at http://{addr}");

    // Race the server against Ctrl-C rather than using `with_graceful_shutdown`:
    // the `/api/events` SSE stream holds connections open indefinitely, so a
    // graceful drain would never finish — the server would print "Shutting down"
    // and hang. Game state is persisted after every change, so dropping live
    // connections on exit loses nothing.
    tokio::select! {
        r = axum::serve(listener, app) => r.expect("server"),
        _ = shutdown_signal() => tracing::info!("shutting down"),
    }
    // Flush a final save in case a background write was still in flight.
    state.save();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
