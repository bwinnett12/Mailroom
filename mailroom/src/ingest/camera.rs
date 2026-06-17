use axum::{
    extract::{Multipart, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::envelope::{Envelope, Payload, SourceKind};
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct CameraQuery {
    /// Optional agent to route to; defaults to config routing
    pub agent: Option<String>,
    /// Optional Johnny Decimal address
    pub jd: Option<String>,
    /// Camera device identifier
    pub device_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub envelope_id: String,
    pub routed_to: Vec<String>,
}

/// POST /ingest/camera
///
/// Accepts a multipart body with one `frame` field containing raw image bytes.
/// Headers or query params carry metadata (device, JD address, target agent).
pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CameraQuery>,
    mut multipart: Multipart,
) -> Result<Json<IngestResponse>, StatusCode> {
    let mut frame_data: Option<Vec<u8>> = None;
    let mut mime_type = "image/jpeg".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if field.name() == Some("frame") {
            if let Some(ct) = field.content_type() {
                mime_type = ct.to_string();
            }
            frame_data = Some(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_vec());
        }
    }

    let data = frame_data.ok_or(StatusCode::BAD_REQUEST)?;
    info!(bytes = data.len(), device = ?q.device_id, "Camera frame received");

    let mut envelope = Envelope::new(
        SourceKind::Camera,
        Payload::Binary { mime_type, data },
    );

    if let Some(jd) = q.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &q.agent { envelope = envelope.for_agent(agent); }
    if let Some(dev) = q.device_id {
        envelope.meta.insert("device_id".into(), serde_json::Value::String(dev));
    }

    let routed_to = state.agents.dispatch(&envelope).await;

    Ok(Json(IngestResponse {
        envelope_id: envelope.id.to_string(),
        routed_to,
    }))
}
