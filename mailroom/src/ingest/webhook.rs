use axum::{extract::{Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

use crate::ingest::envelope::{Envelope, Payload, SourceKind};
use crate::ingest::camera::IngestResponse;
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct WebhookQuery {
    pub agent: Option<String>,
    pub jd: Option<String>,
    pub source: Option<String>,  // e.g. "github", "stripe", "n8n"
}

/// POST /ingest/webhook
/// Accepts arbitrary JSON. Source name comes from query param.
pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WebhookQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<IngestResponse>, StatusCode> {
    let source_name = q.source.unwrap_or_else(|| "unknown".into());
    info!(source = %source_name, "Webhook received");

    let mut envelope = Envelope::new(
        SourceKind::Custom(source_name.clone()),
        Payload::Json(body),
    );
    if let Some(jd) = q.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &q.agent { envelope = envelope.for_agent(agent); }
    envelope.meta.insert("webhook_source".into(), serde_json::Value::String(source_name));

    let routed_to = state.agents.dispatch(&envelope).await;
    Ok(Json(IngestResponse { envelope_id: envelope.id.to_string(), routed_to }))
}
