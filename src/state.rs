// src/state.rs
 
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
 
use reqwest::Client;
use serde::Deserialize;
 
use crate::registry::Registry;
 
// ── TaskKind ──────────────────────────────────────────────────────────────────
// What shape of request a task sends over the wire.
// Chat tasks (classify, summarise, chat, lint, sanitize, ...) all speak the
// same OpenAI-compatible /v1/chat/completions JSON.
// Audio tasks (transcribe) speak the OpenAI-compatible
// /v1/audio/transcriptions multipart form instead — Whisper.cpp and
// faster-whisper's server both implement this, so it slots into the same
// table as everything else rather than needing its own code path.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Chat,
    Audio,
}
 
// ── TaskConfig ────────────────────────────────────────────────────────────────
// One "situation" the model layer can be attuned to. classify, summarise,
// chat, transcribe, lint, sanitize — each is just a row in this table:
// a backend URL, a model name, a system prompt template, and a couple of
// dials (temperature, how much input to keep). Swapping LocalAI for Ollama,
// llama.cpp, or a whisper.cpp server for any one task is a config edit, not
// a code change — and adding a new step in a model chain (e.g. "sanitize")
// is a new [[tasks]] entry in Mailroom.toml, not a new Rust function.
#[derive(Clone, Debug, Deserialize)]
pub struct TaskConfig {
    /// Base URL of the backend serving this task.
    /// Example: "http://localhost:8090" (LocalAI), "http://localhost:11434" (Ollama)
    pub base_url: String,
 
    /// Model name as the backend expects it.
    pub model: String,
 
    /// Sampling temperature. Ignored for Audio tasks.
    #[serde(default)]
    pub temperature: Option<f32>,
 
    /// System prompt template for this task. The literal substring
    /// "{context}" is replaced with whatever context string the caller
    /// passes to `InferenceClient::run()` at call time — the same template
    /// gets reused across every envelope, only the context changes.
    /// For Audio tasks this doubles as Whisper's optional vocabulary/style
    /// hint (its "prompt" field), so context still does something useful
    /// there even though there's no "system" role in the audio API.
    #[serde(default)]
    pub system_prompt: String,
 
    /// Chat or Audio. Determines which wire format `run()` uses.
    #[serde(default)]
    pub kind: TaskKind,
 
    /// Optional cap on input length in characters, applied before sending.
    /// classify wants a cheap preview (a few hundred chars); summarise
    /// usually wants the full text. None = no truncation.
    #[serde(default)]
    pub max_input_chars: Option<usize>,
}
 
// ── InferenceConfig ───────────────────────────────────────────────────────────
// A table of tasks, keyed by name. Loaded from Mailroom.toml's [[tasks]]
// array at startup. This replaces the old fixed
// classify_model/summarise_model/chat_model fields — those were really
// just three hardcoded rows of the table this now is.
#[derive(Clone, Debug, Default)]
pub struct InferenceConfig {
    pub tasks: HashMap<String, TaskConfig>,
}
 
// Shape of the [[tasks]] array as it appears in Mailroom.toml.
// `name` is the array key we pull out into the HashMap; everything else
// maps straight onto TaskConfig.
#[derive(Deserialize)]
struct RawTask {
    name: String,
    #[serde(flatten)]
    config: TaskConfig,
}
 
#[derive(Deserialize)]
struct RawInferenceFile {
    #[serde(default, rename = "tasks")]
    tasks: Vec<RawTask>,
}
 
impl InferenceConfig {
    /// Load task definitions from a Mailroom.toml-shaped file.
    /// Called once in main.rs at startup, same as Registry::load.
    pub fn load(config_path: &Path) -> anyhow::Result<Self> {
        let raw_text = std::fs::read_to_string(config_path)?;
        let raw: RawInferenceFile = toml::from_str(&raw_text)?;
 
        let tasks = raw
            .tasks
            .into_iter()
            .map(|t| (t.name, t.config))
            .collect();
 
        Ok(Self { tasks })
    }
 
    /// Zero-config fallback so the server still starts if Mailroom.toml is
    /// missing or has no [[tasks]] — reproduces the four tasks the old
    /// env-var-only config used to provide, reading the same env vars.
    /// This is what runs today, before you've written a [[tasks]] table;
    /// once one exists on disk, `load()` takes over and this is unused.
    pub fn from_env_defaults() -> Self {
        let base_url = std::env::var("MAILROOM_LLM_URL")
            .unwrap_or_else(|_| "http://localhost:8090".to_string());
        let model = std::env::var("MAILROOM_CHAT_MODEL")
            .unwrap_or_else(|_| "qwen_qwen3.5-0.8b".to_string());
 
        let mut tasks = HashMap::new();
 
        tasks.insert(
            "classify".to_string(),
            TaskConfig {
                base_url: base_url.clone(),
                model: std::env::var("MAILROOM_CLASSIFY_MODEL").unwrap_or_else(|_| model.clone()),
                temperature: Some(0.1),
                system_prompt: "You are a routing assistant for a personal knowledge system \
                    organised by Johnny Decimal addresses. Given a piece of data, return only \
                    the most appropriate JD address (e.g. '34.2'). No explanation. No \
                    punctuation. Just the address."
                    .to_string(),
                kind: TaskKind::Chat,
                max_input_chars: Some(500),
            },
        );
 
        tasks.insert(
            "summarise".to_string(),
            TaskConfig {
                base_url: base_url.clone(),
                model: std::env::var("MAILROOM_SUMMARISE_MODEL").unwrap_or_else(|_| model.clone()),
                temperature: Some(0.3),
                system_prompt: "You are a summarisation assistant for a personal knowledge \
                    system. Summarise the following content concisely. Context about where \
                    this will be stored: {context}"
                    .to_string(),
                kind: TaskKind::Chat,
                max_input_chars: None,
            },
        );
 
        tasks.insert(
            "chat".to_string(),
            TaskConfig {
                base_url: base_url.clone(),
                model,
                temperature: Some(0.7),
                system_prompt: "You are a helpful assistant integrated into a personal \
                    knowledge system organised by Johnny Decimal addresses."
                    .to_string(),
                kind: TaskKind::Chat,
                max_input_chars: None,
            },
        );
 
        Self { tasks }
    }
}
 
// ── AppState ──────────────────────────────────────────────────────────────────
 
#[derive(Clone, Debug)]
pub struct AppState {
    /// Absolute path to the local vault clone.
    pub vault_root: PathBuf,
 
    pub library_root: PathBuf,
 
    /// Live registry built from .mailroom files at startup.
    pub registry: std::sync::Arc<Registry>,
 
    /// Shared HTTP client for all outbound requests.
    /// One instance, shared across all handlers via Arc inside Client.
    pub http_client: Client,
 
    /// Task table and model routing config.
    pub inference: InferenceConfig,
}
 
impl AppState {
    pub fn new(
        vault_root: impl Into<PathBuf>,
        library_root: impl Into<PathBuf>,
        registry: Registry,
        inference: InferenceConfig,
    ) -> Self {
        Self {
            vault_root: vault_root.into(),
            library_root: library_root.into(),
            registry: std::sync::Arc::new(registry),
            http_client: Client::new(),
            inference,
        }
    }
}
 

