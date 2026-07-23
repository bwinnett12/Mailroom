// src/state.rs

use std::{
    path::PathBuf,
    sync::Arc,
};

use reqwest::Client;
use crate::registry::Registry;

// ── InferenceConfig ───────────────────────────────────────────────────────────
// Describes how the Mailroom talks to LocalAI.
// Each task type gets its own model — small/fast for classification,
// larger for summarisation and chat.

#[derive(Clone, Debug)]
pub struct InferenceConfig {
    /// Base URL of the LocalAI instance on Island.
    /// Example: http://100.x.x.x:8080
    /// Set via MAILROOM_LLM_URL env var.
    pub base_url: String,

    /// Model used for envelope classification.
    /// Task: "where does this data belong in the JD system?"
    /// Should be fast — this runs on every unaddressed envelope.
    /// Example: "qwen_qwen3.5-0.8b"
    pub classify_model: String,

    /// Model used for summarisation.
    /// Task: "summarise this journal entry / health record / note"
    /// Can be the same as classify_model or larger.
    /// Example: "qwen_qwen3.5-0.8b"
    pub summarise_model: String,

    /// Model used for chat completions via /v1/chat/completions.
    /// Task: general conversation, JD-routed queries.
    /// This is what your existing routes.rs proxy used.
    /// Example: "codellama" for 11.* (NixOS), "meditron" for 35.* (Health)
    pub chat_model: String,
}

impl InferenceConfig {
    pub fn from_env() -> Self {
        // Group all inference config into one constructor that reads
        // env vars with sensible defaults.
        // Called once in main.rs — keeps main.rs clean.
        Self {
            base_url: std::env::var("MAILROOM_LLM_URL")
                .unwrap_or_else(|_| "http://localhost:8090".to_string()),

            classify_model: std::env::var("MAILROOM_CLASSIFY_MODEL")
                .unwrap_or_else(|_| "qwen_qwen3.5-0.8b".to_string()),

            summarise_model: std::env::var("MAILROOM_SUMMARISE_MODEL")
                .unwrap_or_else(|_| "qwen_qwen3.5-0.8b".to_string()),

            chat_model: std::env::var("MAILROOM_CHAT_MODEL")
                .unwrap_or_else(|_| "qwen_qwen3.5-0.8b".to_string()),
        }
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct AppState {
    /// Absolute path to the local vault clone.
    pub vault_root: PathBuf,

    pub library_root:  PathBuf,

    /// Live registry built from .mailroom files at startup.
    pub registry: Arc<Registry>,

    /// Shared HTTP client for all outbound requests.
    /// One instance, shared across all handlers via Arc inside Client.
    pub http_client: Client,

    /// LocalAI connection config and model assignments.
    pub inference: InferenceConfig,
}

impl AppState {
    pub fn new(
        vault_root:   impl Into<PathBuf>,
        library_root: impl Into<PathBuf>,   // ← add this
        registry:     Registry,
        inference:    InferenceConfig,
    ) -> Self {
        Self {
            vault_root:   vault_root.into(),
            library_root: library_root.into(),   // ← add this
            registry:     Arc::new(registry),
            http_client:  Client::new(),
            inference,
        }
    }
}