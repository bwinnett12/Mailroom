// src/main.rs

mod attendant;
mod cleanup;
mod clean_file;
mod cli;
mod dodo;
mod envelope;
mod inference;
mod machines;
mod move_branch;
mod manifest;
mod nest;
mod orchard;
mod registry;
mod route;
mod routes;
mod schema;
mod state;
mod store;

use std::path::{PathBuf, Path};
use axum::Router;
use ron;
use registry::Registry;
use state::{AppState, InferenceConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli_args: Vec<String> = std::env::args().collect();
    if cli_args.get(1).map(String::as_str) == Some("add-nest") {
        return cli::add_nest(&cli_args[2..]);
    }
   if cli_args.get(1).map(String::as_str) == Some("move-branch") {
       return cli::move_branch(&cli_args[2..]);
   }

    let vault_root = std::env::var("MAILROOM_VAULT")
        .unwrap_or_else(|_| "/home/user/vault".to_string());

    let addr = std::env::var("MAILROOM_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string());


    if std::env::var("MAILROOM_GENERATE_SCHEMA").is_ok() {
        let schema_path = std::env::var("MAILROOM_SCHEMA_PATH")
            .unwrap_or_else(|_| format!("{}/schema.ron", vault_root));

        schema::generate_fresh_tree(
            Path::new(&schema_path),
            Path::new(&vault_root),
        )?;
    }
    if cli_args.get(1).map(String::as_str) == Some("refresh-schema") {
        return cli::refresh_schema(&cli_args[2..]);
    }

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

    // ── Title cleanup list (optional) ────────────────────────────────────────
    let title_cleanup = match std::env::var("MAILROOM_TITLE_CLEANUP") {
        Ok(path) => match cleanup::load(Path::new(&path)) {
            Ok(phrases) => {
                tracing::info!(path = %path, count = phrases.len(), "loaded title cleanup list");
                phrases
            }
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "failed to load title cleanup list — proceeding with none");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    };

    // ── AppState ──────────────────────────────────────────────────────────────
    let state = std::sync::Arc::new(
        AppState::new(vault_path, library_root, registry, inference, title_cleanup)
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