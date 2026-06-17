use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Client;
use serde_json::json;
use tracing::debug;

use crate::config::AgentConfig;
use crate::ingest::envelope::{Envelope, Payload, SourceKind};

/// Sends an envelope to a local Ollama instance.
/// Maps source kinds to appropriate Ollama API calls.
pub async fn send(client: Client, agent: &AgentConfig, envelope: &Envelope) -> Result<()> {
    let url = format!("{}/api/generate", agent.base_url.trim_end_matches('/'));

    let body = match &envelope.payload {
        Payload::Text { content, .. } => {
            // Plain text → Ollama generate
            json!({
                "model": "llama3",
                "prompt": content,
                "stream": false,
                "context": {
                    "envelope_id": envelope.id,
                    "jd_address": envelope.jd_address,
                    "source": envelope.source.as_str(),
                }
            })
        }

        Payload::Binary { data, mime_type } => {
            let b64 = STANDARD.encode(data);
            if mime_type.starts_with("image/") || envelope.source == SourceKind::Camera {
                // Vision model path
                json!({
                    "model": "llava",
                    "prompt": "Describe what you see in this image.",
                    "images": [b64],
                    "stream": false,
                })
            } else {
                // Audio / video — forward as base64 with a note
                // (Ollama doesn't natively support audio yet; this is a stub)
                json!({
                    "model": "llama3",
                    "prompt": format!(
                        "[Binary payload received: {} bytes, type: {}. Source: {}.]",
                        data.len(), mime_type, envelope.source.as_str()
                    ),
                    "stream": false,
                })
            }
        }

        Payload::Json(v) => {
            json!({
                "model": "llama3",
                "prompt": format!("Process this JSON payload:\n{}", v),
                "stream": false,
            })
        }
    };

    debug!(url = %url, "Sending to Ollama");
    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned {}: {}", status, text);
    }

    Ok(())
}
