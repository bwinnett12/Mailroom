// src/main.rs

mod adopt;
mod attendant;
mod blobs;
mod clean_file;
mod coffer;
mod coffer_push;
mod cleanup;
mod cli;
mod dodo;
mod envelope;
mod event;
mod graph;
mod inference;
mod ledger;
mod machines;
mod manifest;
mod migrate;
mod move_branch;
mod nest;
mod orchard;
mod registry;
mod route;
mod routes;
mod routing;
mod schema;
mod state;
mod store;

#[cfg(test)]
mod route_ledger_tests;

use axum::Router;
use registry::Registry;
use ron;
use state::{AppState, InferenceConfig};
use std::path::{Path, PathBuf};

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
    if cli_args.get(1).map(String::as_str) == Some("adopt") {
        return adopt::cli(&cli_args[2..]).await;
    }
    if cli_args.get(1).map(String::as_str) == Some("migrate") {
        return migrate::run(&cli_args[2..]);
    }
    if cli_args.get(1).map(String::as_str) == Some("coffer") {
        if matches!(cli_args.get(2).map(String::as_str), Some("status") | Some("push")) {
            return coffer_push::cli(&cli_args[2..]);
        }
        if cli_args.get(2).map(String::as_str) != Some("init") {
            return Err("usage: mailroom coffer init | status --to DEST | push --to DEST [--dry-run] [--max-size 100M]".into());
        }
        let vault = std::env::var("MAILROOM_VAULT").map_err(|_| "set MAILROOM_VAULT")?;
        let written = coffer::init(Path::new(&vault))?;
        if written.is_empty() {
            println!("the coffer nests already exist");
        }
        for f in written {
            println!("created {}", f.display());
        }
        return Ok(());
    }

    let vault_root =
        std::env::var("MAILROOM_VAULT").unwrap_or_else(|_| "/home/user/vault".to_string());

    let addr = std::env::var("MAILROOM_LISTEN").unwrap_or_else(|_| "127.0.0.1:3000".to_string()); // loopback: see flake.nix listenAddr

    if std::env::var("MAILROOM_GENERATE_SCHEMA").is_ok() {
        let schema_path = std::env::var("MAILROOM_SCHEMA_PATH")
            .unwrap_or_else(|_| format!("{}/schema.ron", vault_root));

        schema::generate_fresh_tree(Path::new(&schema_path), Path::new(&vault_root))?;
    }
    if cli_args.get(1).map(String::as_str) == Some("refresh-schema") {
        return cli::refresh_schema(&cli_args[2..]);
    }

    let inference_config_path =
        std::env::var("MAILROOM_CONFIG").unwrap_or_else(|_| "./Mailroom.toml".to_string());
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

    // There is no separate Library any more: entries live in their nests
    // and file bytes in the coffer (see coffer.rs).
    if std::env::var("MAILROOM_LIBRARY_ROOT").is_ok() {
        tracing::warn!("MAILROOM_LIBRARY_ROOT is set but no longer used; `mailroom migrate` moves an old library into the vault");
    }

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
    let routing =
        routing::RoutingTable::load(&vault_path.join("routes.toml"), &vault_path, &registry)?;
    tracing::info!(routes = routing.route_count(), "routing table ready");

    // ── Ledger ────────────────────────────────────────────────────────────────
    // Opened before serving: this finds the coffer (a vault without one, or
    // still in the old layout, stops here with what to do), takes the
    // ledger's lock (a second Mailroom on the same vault stops here),
    // repairs a torn final line, and records any stop a crash cut off.
    // Nothing is in flight yet, so the scan is exact.
    if vault_path.exists() {
        let ledger = ledger::for_vault(&vault_path)?;
        let health = ledger.recover()?;
        tracing::info!(
            ledger = %ledger.path().display(),
            records = health.records,
            interrupted = health.interrupted.len(),
            dangling_causes = health.dangling_causes.len(),
            dangling_parents = health.dangling_parents.len(),
            "ledger ready"
        );
    }

    let state = std::sync::Arc::new(AppState::new(
        vault_path,
        registry,
        inference,
        title_cleanup,
        routing,
    ));

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
