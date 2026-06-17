// src/main.rs

mod envelope;
mod inference;
mod manifest;
mod registry;
mod routes;
mod state;
mod store;

use std::path::PathBuf;
use axum::Router;
use registry::Registry;
use state::{AppState, InferenceConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let vault_root = std::env::var("MAILROOM_VAULT")
        .unwrap_or_else(|_| "/home/user/vault".to_string());

    let addr = std::env::var("MAILROOM_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let inference = InferenceConfig::from_env();

    let library_root = std::env::var("MAILROOM_LIBRARY_ROOT")
        .unwrap_or_else(|_| {
            // Default: a Library/ directory next to the vault
            format!("{}/Library", vault_root)
        });

    tracing::info!(
        classify_model  = %inference.classify_model,
        summarise_model = %inference.summarise_model,
        chat_model      = %inference.chat_model,
        llm_url         = %inference.base_url,
        "inference config loaded"
    );

    let vault_path = PathBuf::from(&vault_root);

    let registry = if vault_path.exists() {
        tracing::info!(vault = %vault_root, "building registry");
        Registry::load(&vault_path)?
    } else {
        tracing::warn!(
            vault = %vault_root,
            "vault path does not exist — starting with empty registry"
        );
        Registry::empty()
    };

    tracing::info!(nodes = registry.node_count, "registry ready");

    // ── AppState ──────────────────────────────────────────────────────────────
    let state = std::sync::Arc::new(
        AppState::new(vault_path, library_root, registry, inference)
    );

    // ── Router ────────────────────────────────────────────────────────────────
    let app = routes::router(state);
    // One call — routes/mod.rs assembles everything.
    // The health check now lives there too.

    // ── Serve ─────────────────────────────────────────────────────────────────
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(addr = %addr, "Mailroom listening");
    axum::serve(listener, app).await?;

    Ok(())
}