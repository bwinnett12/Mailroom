use axum::{extract::{Query, State}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::envelope::{Envelope, Payload, SourceKind};
use super::camera::IngestResponse;
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct TextBody {
    pub content: String,
    pub agent: Option<String>,
    pub jd: Option<String>,
    pub role: Option<String>,  // "user" | "system" | "tool_result" etc.
}

/// POST /ingest/text   (JSON body)
pub async fn handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TextBody>,
) -> Result<Json<IngestResponse>, StatusCode> {
    info!(chars = body.content.len(), role = ?body.role, "Text message received");

    let mut envelope = Envelope::new(
        SourceKind::Text,
        Payload::Text {
            content: body.content,
            mime_type: "text/plain".into(),
        },
    );
    if let Some(jd) = body.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &body.agent { envelope = envelope.for_agent(agent); }
    if let Some(role) = body.role {
        envelope.meta.insert("role".into(), serde_json::Value::String(role));
    }

    let routed_to = state.agents.dispatch(&envelope).await;
    Ok(Json(IngestResponse { envelope_id: envelope.id.to_string(), routed_to }))
}
