//! D&D draft auction game — web server.

use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use mtg_auction::{api, app};
use std::path::PathBuf;

const INDEX_HTML: &str = include_str!("../static/index.html");
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
        .route("/app.js", get(app_js))
        .route("/admin.js", get(admin_js))
        .route("/style.css", get(style_css))
        .merge(api::api_router())
        .with_state(state);

    let addr = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("Could not bind {addr}: {e}");
            match e.kind() {
                std::io::ErrorKind::AddrInUse => eprintln!(
                    "That port is already in use. Pick another, e.g. `BIND=127.0.0.1:8080 cargo run`."
                ),
                std::io::ErrorKind::PermissionDenied => eprintln!(
                    "Ports below 1024 need root. Use a higher port, e.g. `BIND=127.0.0.1:8080 cargo run`."
                ),
                _ => {}
            }
            std::process::exit(1);
        }
    };
    println!("Auction house open at http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nShutting down.");
}
