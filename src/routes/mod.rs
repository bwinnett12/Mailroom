// src/routes/mod.rs
//
// This module does two things:
//   1. Declares all route submodules
//   2. Exports a single router() function that assembles them
//
// main.rs calls routes::router(state) and gets back one fully
// configured Router ready to hand to Axum.

pub mod envelope;
// Declare the envelope submodule.
// Rust will look for src/routes/envelope.rs
pub mod entries;

use std::sync::Arc;
use axum::{routing::get, Router};
use crate::state::AppState;

/// Build and return the complete application router.
/// Called once in main.rs at startup.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // ── Health ────────────────────────────────────────────────────
        .route("/health", get(health))

        // ── Envelope API ──────────────────────────────────────────────
        .route("/envelope", axum::routing::post(envelope::receive))

		// -- Multiple Entries
		.route("/envelopes", get(entries::list))

        // ── Attach state ──────────────────────────────────────────────
        .with_state(state)
        // .with_state() makes Arc<AppState> available to any handler
        // that declares State(s): State<Arc<AppState>> as an argument.
}

async fn health() -> &'static str {
    "ok"
}