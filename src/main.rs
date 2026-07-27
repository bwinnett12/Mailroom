// src/main.rs

mod envelope;
mod inference;
mod manifest;
mod orchard;
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

    let inference_config_path = std::env::var("MAILROOM_CONFIG")
        .unwrap_or_else(|_| "./Mailroom.toml".to_string());
    let inference_config_path = PathBuf::from(inference_config_path);

    let inference = if inference_config_path.exists() {
        match InferenceConfig::load(&inference_config_path) {
            Ok(cfg) if !cfg.tasks.is_empty() => {
                tracing::info!(
                    path = %inference_config_path.display(),
                    tasks = ?cfg.tasks.keys().collect::<Vec<_>>(),
                    "loaded task config"
                );
                cfg
            }
            Ok(_) => {
                tracing::warn!(
                    path = %inference_config_path.display(),
                    "config file has no [[tasks]] entries — falling back to env defaults"
                );
                InferenceConfig::from_env_defaults()
            }
            Err(e) => {
                tracing::warn!(
                    path = %inference_config_path.display(),
                    error = %e,
                    "failed to parse task config — falling back to env defaults"
                );
                InferenceConfig::from_env_defaults()
            }
        }
    } else {
        tracing::info!("no Mailroom.toml found — using env-var task defaults");
        InferenceConfig::from_env_defaults()
    };

    let library_root = std::env::var("MAILROOM_LIBRARY_ROOT")
        .unwrap_or_else(|_| {
            // Default: a Library/ directory next to the vault
            format!("{}/Library", vault_root)
        });

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