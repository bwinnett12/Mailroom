// src/inference.rs
//
// The inference module handles all communication with LocalAI.
// It has three jobs:
//   classify()   — given an envelope, return a JD address
//   summarise()  — given text content, return a summary string
//   chat()       — forward a chat request, return the response
//
// All three speak the OpenAI-compatible API that LocalAI exposes.
// The only difference between them is which model they use and
// what system prompt they send.

use serde::{Deserialize, Serialize};
use crate::state::InferenceConfig;
use crate::envelope::Envelope;

// ── OpenAI-compatible request/response types ──────────────────────────────────
// LocalAI speaks the OpenAI wire format. These structs represent
// the JSON that goes over the wire.

#[derive(Debug, Serialize)]
// Only Serialize — we send this, we never receive it.
struct ChatRequest {
    model:    String,
    messages: Vec<Message>,

    #[serde(skip_serializing_if = "Option::is_none")]
    // skip_serializing_if: don't include this field in JSON if it's None.
    // Keeps the request clean — LocalAI uses its default if absent.
    temperature: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role:    String,
    // OpenAI roles: "system", "user", "assistant"
    // "system" sets the AI's behaviour for the conversation.
    // "user" is the human turn.
    // "assistant" is the AI's previous responses (for multi-turn).

    content: String,
}

#[derive(Debug, Deserialize)]
// Only Deserialize — we receive this, we never send it.
struct ChatResponse {
    choices: Vec<Choice>,
    // LocalAI can return multiple completions (choices).
    // We always ask for one and take choices[0].
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

// ── InferenceClient ───────────────────────────────────────────────────────────
// Wraps the reqwest::Client and InferenceConfig together.
// Handlers construct one of these when they need to call LocalAI.

pub struct InferenceClient<'a> {
    // Rust concept — lifetimes:
    // The `'a` lifetime parameter says: "this struct borrows data that
    // must live at least as long as 'a". Here we borrow InferenceConfig
    // and reqwest::Client from AppState rather than cloning them.
    // The compiler ensures InferenceClient can't outlive AppState.
    config: &'a InferenceConfig,
    client: &'a reqwest::Client,
}

impl<'a> InferenceClient<'a> {
    /// Construct from references to AppState's fields.
    /// Called inside a handler:
    ///   let ai = InferenceClient::new(&state.inference, &state.http_client);
    pub fn new(config: &'a InferenceConfig, client: &'a reqwest::Client) -> Self {
        Self { config, client }
    }

    /// Ask LocalAI which JD address this envelope belongs to.
    /// Returns a JD address string like "34.2" or "35.3".
    /// Called when envelope.jd_address is None.
    pub async fn classify(&self, envelope: &Envelope) -> anyhow::Result<String> {
        let system_prompt = format!(
            // format! works like println! but returns a String instead of printing.
            // {{}} inside format strings is a literal brace — not a placeholder.
            "You are a routing assistant for a personal knowledge system \
             organised by Johnny Decimal addresses. Given a piece of data, \
             return only the most appropriate JD address (e.g. '34.2'). \
             No explanation. No punctuation. Just the address."
        );

        let user_prompt = format!(
            "Data type: {}\nSource: {:?}\nContent preview: {}",
            envelope.data_type,
            envelope.source,
            // {:?} uses the Debug format — works on any type with #[derive(Debug)]
            self.payload_preview(envelope),
        );

        let response = self.call(
            &self.config.classify_model,
            &system_prompt,
            &user_prompt,
            Some(0.1),
            // Low temperature = more deterministic output.
            // For classification we want consistent, focused answers.
        ).await?;

        // Trim whitespace — model responses sometimes have trailing newlines.
        Ok(response.trim().to_string())
    }

    /// Summarise text content into a single short paragraph.
    /// Used when a node's .mailroom has ai_classify = true,
    /// or when the front-end requests a summary.
    pub async fn summarise(
        &self,
        content:  &str,
        context:  &str,
        // context is the .about file for the destination JD node —
        // tells the model what this area is for.
    ) -> anyhow::Result<String> {
        let system_prompt = format!(
            "You are a summarisation assistant for a personal knowledge system. \
             Summarise the following content concisely. \
             Context about where this will be stored: {context}"
        );

        let response = self.call(
            &self.config.summarise_model,
            &system_prompt,
            content,
            Some(0.3),
        ).await?;

        Ok(response.trim().to_string())
    }

    /// Forward a chat message to LocalAI and return the response text.
    /// Used by the /v1/chat/completions route.
    /// `messages` is the full conversation history from the request.
    /// `model_override` lets the JD router select a specific model.
    pub async fn chat(
        &self,
        messages:       Vec<(String, String)>,
        // Vec of (role, content) pairs — the conversation so far.
        model_override: Option<&str>,
        // If the JD routing table selected a specific model, use it.
        // Otherwise fall back to self.config.chat_model.
    ) -> anyhow::Result<String> {
        let model = model_override
            .unwrap_or(&self.config.chat_model)
            .to_string();
        // Option::unwrap_or returns the inner value if Some,
        // or the provided default if None.

        let msgs: Vec<Message> = messages
            .into_iter()
            // into_iter() consumes the Vec, giving us owned values.
            // (vs iter() which gives references)
            .map(|(role, content)| Message { role, content })
            // map transforms each (role, content) tuple into a Message struct.
            .collect();

        let system = "You are a helpful assistant integrated into a personal \
                      knowledge system organised by Johnny Decimal addresses.";

        let mut all_messages = vec![Message {
            role:    "system".to_string(),
            content: system.to_string(),
        }];
        all_messages.extend(msgs);
        // extend appends all items from an iterator onto a Vec.

        let request = ChatRequest {
            model,
            messages:    all_messages,
            temperature: Some(0.7),
        };

        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let resp = self.client
            .post(&url)
            .json(&request)
            // .json() serializes our ChatRequest to JSON and sets
            // Content-Type: application/json automatically.
            .send()
            .await?
            .json::<ChatResponse>()
            // .json::<ChatResponse>() deserializes the response body
            // into our ChatResponse struct.
            // The ::<ChatResponse> is a "turbofish" — it tells Rust
            // which type to deserialize into when it can't infer it.
            .await?;

        let content = resp.choices
            .into_iter()
            .next()
            // .next() takes the first item from the iterator — Option<Choice>.
            .map(|c| c.message.content)
            // .map extracts the content string if Some.
            .unwrap_or_default();
            // unwrap_or_default() returns String::default() (empty string)
            // if there were no choices. Better than panicking.

        Ok(content)
    }

    // ── Private helper ────────────────────────────────────────────────────────

    /// Core HTTP call to LocalAI. All three public methods go through here.
    async fn call(
        &self,
        model:       &str,
        system:      &str,
        user:        &str,
        temperature: Option<f32>,
    ) -> anyhow::Result<String> {
        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![
                Message { role: "system".to_string(), content: system.to_string() },
                Message { role: "user".to_string(),   content: user.to_string()   },
            ],
            temperature,
        };

        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let resp = self.client
            .post(&url)
            .json(&request)
            .send()
            .await?
            .json::<ChatResponse>()
            .await?;

        Ok(resp.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default())
    }

    /// Extract a short preview of the envelope's payload for classification.
    /// We don't send the full payload — just enough for the model to classify.
    fn payload_preview(&self, envelope: &Envelope) -> String {
        use crate::envelope::Payload;
        match &envelope.payload {
            Payload::Text(s) => s.chars().take(500).collect(),
            // .chars() iterates over Unicode characters (not bytes).
            // .take(500) limits to 500 characters.
            // .collect() gathers them back into a String.

            Payload::Json(v) => v.to_string().chars().take(500).collect(),
            Payload::Url(u)  => u.clone(),
            Payload::FilePath(p) => format!("file: {}", p.display()),
            Payload::Bytes(_)    => "[binary data]".to_string(),
        }
    }
}