// src/routes/mod.rs
//
// This module does two things:
//   1. Declares all route submodules
//   2. Exports a single router() function that assembles them
//
// main.rs calls routes::router(state) and gets back one fully
// configured Router ready to hand to Axum.

pub mod api;
pub mod envelope;
// Declare the envelope submodule.
// Rust will look for src/routes/envelope.rs
pub mod entries;
pub mod journal;
pub mod web;

use std::sync::Arc;
use axum::{extract::DefaultBodyLimit, routing::{get, post, get_service}, Router, response::IntoResponse};
use crate::state::AppState;

use tower_http::services::{ServeDir, ServeFile};
use tower::ServiceBuilder;
// use tower_http::set_header::SetResponseHeaderLayer;
use axum::http::HeaderValue;

/// Build and return the complete application router.
/// Called once in main.rs at startup.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // ── Health ────────────────────────────────────────────────────
        .route("/health", get(health))

        // ── Envelope API ──────────────────────────────────────────────
        .route("/envelope", axum::routing::post(envelope::receive))
        .route("/envelopes", get(entries::list))
        .route("/envelope/upload", post(envelope::upload))
        .layer(DefaultBodyLimit::max(200 * 1024 * 1024))

        // ── Registry Graph API ────────────────────────────────────────
        .route("/api/registry/{id}", get(api::get_manifest))
        .route("/api/registry/graph", get(api::registry_graph))
        .route("/api/registry/{id}/readme", get(api::get_node_readme))
        .route("/api/registry/{id}/files", get(api::list_node_files))

        // ── Journal ───────────────────────────────────────────────────
        .route("/journal", post(journal::write))
        .route("/journal/summary", post(journal::summary))

        // ── Web Forms ─────────────────────────────────────────────────
        .route("/", get(web::new_envelope_form))
        .route("/submit", post(web::submit))

        // ── Leptos UI (explicit route) ────────────────────────────────
        .route("/ui", get(serve_ui_index))

        // ── Fallback: serve dist/ at root (catches /mailroom-ui-*.js, etc) ──
        .fallback_service(ServeDir::new("mailroom-ui/dist").precompressed_gzip())

        // ── Attach state ──────────────────────────────────────────────
        .with_state(state)
}

// Serve the Leptos HTML
async fn serve_ui_index() -> impl IntoResponse {
    let html = std::fs::read_to_string("mailroom-ui/dist/index.html")
        .unwrap_or_else(|_| "<h1>Mailroom</h1>".into());
    ([(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

async fn health() -> &'static str {
    "ok"
}