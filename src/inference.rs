// src/inference.rs
//
// The inference module handles all communication with model backends.
//
// The core of it is one function: `run()`. It takes a task name (a row in
// the InferenceConfig table — "classify", "summarise", "chat", "transcribe",
// or anything else you add to Mailroom.toml), a Payload to act on, and a
// context string. It looks up that task's backend/model/prompt, sends the
// request, and returns the model's output as a String.
//
// Everything else in this file — classify(), summarise(), transcribe() —
// is a thin convenience wrapper around run(), kept around so existing
// call sites in routes/ don't need to change shape. A model *chain*
// (lint the input, then sanitize it, then classify the result) is just
// calling run() several times, feeding each output back in as the next
// call's input/context — there's no separate "chain" API, because you
// don't need one.

use serde::{Deserialize, Serialize};
use crate::machines;

use crate::envelope::{Envelope, Payload};
use crate::state::{InferenceConfig, TaskConfig, TaskKind};

// ── OpenAI-compatible chat request/response types ─────────────────────────────
// Used for every Chat-kind task (classify, summarise, chat, and any new
// chain step you add — lint, sanitize, compartmentalize, ...).

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,

    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

// ── OpenAI-compatible audio transcription response ────────────────────────────
// Whisper.cpp's server and faster-whisper's server both return this shape
// from POST /v1/audio/transcriptions.

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    text: String,
}

// ── InferenceClient ───────────────────────────────────────────────────────────

pub struct InferenceClient<'a> {
    config: &'a InferenceConfig,
    client: &'a reqwest::Client,
}

impl<'a> InferenceClient<'a> {
    pub fn new(config: &'a InferenceConfig, client: &'a reqwest::Client) -> Self {
        Self { config, client }
    }

    // ── The one function ──────────────────────────────────────────────────
    //
    /// Run a named task against a Payload, with a context string substituted
    /// into the task's system prompt (or, for Audio tasks, used as Whisper's
    /// vocabulary hint). Every other method on this struct calls through
    /// here — this is the only place that actually knows how to reach a
    /// model backend.
    ///
    /// task_name must match a `name` in Mailroom.toml's [[tasks]] table
    /// (or one of the four built-in defaults if no Mailroom.toml is
    /// present — see InferenceConfig::from_env_defaults).
    pub async fn run(
        &self,
        task_name: &str,
        input: &Payload,
        context: &str,
    ) -> anyhow::Result<String> {
        let task = self.config.tasks.get(task_name).ok_or_else(|| {
            anyhow::anyhow!(
                "no task named '{task_name}' configured — check Mailroom.toml's [[tasks]]"
            )
        })?;

        match task.kind {
            TaskKind::Chat => self.run_chat(task, input, context).await,
            TaskKind::Audio => self.run_audio(task, input, context).await,
        }
    }

    async fn run_chat(
        &self,
        task: &TaskConfig,
        input: &Payload,
        context: &str,
    ) -> anyhow::Result<String> {
        let system_prompt = task.system_prompt.replace("{context}", context);
        let user_content = Self::payload_as_text(input, task.max_input_chars);

        let request = ChatRequest {
            model: task.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: system_prompt,
                },
                Message {
                    role: "user".to_string(),
                    content: user_content,
                },
            ],
            temperature: task.temperature,
        };

        let url = format!("{}/v1/chat/completions", task.base_url);

        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await?
            .json::<ChatResponse>()
            .await?;

        Ok(resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    async fn run_audio(
        &self,
        task: &TaskConfig,
        input: &Payload,
        context: &str,
    ) -> anyhow::Result<String> {
        // Audio tasks only make sense against Bytes or FilePath payloads —
        // anything else is a config/call-site mistake, so bail loudly
        // rather than silently transcribing an empty clip.
        let bytes = match input {
            Payload::Bytes(b) => b.clone(),
            Payload::FilePath(p) => std::fs::read(p)?,
            other => anyhow::bail!(
                "transcribe task requires a Bytes or FilePath payload, got {other:?}"
            ),
        };

        let hint = task.system_prompt.replace("{context}", context);

        let part = reqwest::multipart::Part::bytes(bytes).file_name("audio.wav");
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", task.model.clone());

        if !hint.trim().is_empty() {
            // Whisper's "prompt" field biases vocabulary/spelling —
            // e.g. hand it JD terms or proper nouns likely to come up.
            form = form.text("prompt", hint);
        }

        let url = format!("{}/v1/audio/transcriptions", task.base_url);

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await?
            .json::<TranscriptionResponse>()
            .await?;

        Ok(resp.text.trim().to_string())
    }

    // ── Convenience wrappers ──────────────────────────────────────────────
    // These exist so routes/*.rs doesn't need to change. Each is a one-line
    // call into run() with the payload shaped the way that task expects.

    /// Ask the model which JD address this envelope belongs to.
    pub async fn classify(&self, envelope: &Envelope) -> anyhow::Result<String> {
        let combined = format!(
            "Data type: {}\nSource: {:?}\nContent preview: {}",
            envelope.data_type,
            envelope.source,
            Self::payload_as_text(&envelope.payload, Some(500)),
        );

        self.run("classify", &Payload::Text(combined), "").await
    }

    /// Summarise text content into a single short paragraph.
    pub async fn summarise(&self, content: &str, context: &str) -> anyhow::Result<String> {
        self.run("summarise", &Payload::Text(content.to_string()), context)
            .await
    }

    /// Transcribe an audio payload (Bytes or FilePath). `context` is passed
    /// through as Whisper's vocabulary hint — e.g. JD terms or names that
    /// are likely to show up and easy for a small model to mis-hear.
    pub async fn transcribe(&self, audio: &Payload, context: &str) -> anyhow::Result<String> {
        self.run("transcribe", audio, context).await
    }

    /// Forward a full multi-turn chat message to the model, return its
    /// response text. Kept separate from run() rather than folded in —
    /// conversation history doesn't fit the single-input/single-context
    /// shape the rest of this file uses, and forcing it to would just
    /// make both harder to read.
    pub async fn chat(
        &self,
        messages: Vec<(String, String)>,
        model_override: Option<&str>,
    ) -> anyhow::Result<String> {
        let task = self.config.tasks.get("chat");

        let model = model_override
            .map(|m| m.to_string())
            .or_else(|| task.map(|t| t.model.clone()))
            .unwrap_or_else(|| "qwen_qwen3.5-0.8b".to_string());

        let base_url = task
            .map(|t| t.base_url.clone())
            .unwrap_or_else(|| "http://localhost:8090".to_string());

        let system = task
            .map(|t| t.system_prompt.clone())
            .unwrap_or_else(|| {
                "You are a helpful assistant integrated into a personal knowledge system \
                 organised by Johnny Decimal addresses."
                    .to_string()
            });

        let msgs: Vec<Message> = messages
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect();

        let mut all_messages = vec![Message {
            role: "system".to_string(),
            content: system,
        }];
        all_messages.extend(msgs);

        let request = ChatRequest {
            model,
            messages: all_messages,
            temperature: task.and_then(|t| t.temperature).or(Some(0.7)),
        };

        let url = format!("{}/v1/chat/completions", base_url);

        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await?
            .json::<ChatResponse>()
            .await?;

        // machines::resolve_available(&registry, &priority_list, port, "/health").await;

        Ok(resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default())
    
    }

    // ── Private helper ────────────────────────────────────────────────────

    /// Turn a Payload into text suitable for a chat "user" message.
    /// `max_chars`, if set, truncates — cheap tasks like classify only
    /// need a preview; leave it None for tasks that want the full text.
    fn payload_as_text(payload: &Payload, max_chars: Option<usize>) -> String {
        let full = match payload {
            Payload::Text(s) => s.clone(),
            Payload::Json(v) => v.to_string(),
            Payload::Url(u) => u.clone(),
            Payload::FilePath(p) => format!("file: {}", p.display()),
            Payload::Bytes(_) => "[binary data]".to_string(),
        };

        match max_chars {
            Some(n) => full.chars().take(n).collect(),
            None => full,
        }
    }
}