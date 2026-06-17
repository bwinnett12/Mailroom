// video.rs — identical shape to camera/mic but for video blobs
use axum::{extract::{Multipart, Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

use super::envelope::{Envelope, Payload, SourceKind};
use super::camera::IngestResponse;
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct VideoQuery {
    pub agent: Option<String>,
    pub jd: Option<String>,
    pub fps: Option<f32>,
}

pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VideoQuery>,
    mut multipart: Multipart,
) -> Result<Json<IngestResponse>, StatusCode> {
    let mut video_data: Option<Vec<u8>> = None;
    let mut mime_type = "video/mp4".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if field.name() == Some("video") {
            if let Some(ct) = field.content_type() { mime_type = ct.to_string(); }
            video_data = Some(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_vec());
        }
    }

    let data = video_data.ok_or(StatusCode::BAD_REQUEST)?;
    info!(bytes = data.len(), fps = ?q.fps, "Video chunk received");

    let mut envelope = Envelope::new(SourceKind::Video, Payload::Binary { mime_type, data });
    if let Some(jd) = q.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &q.agent { envelope = envelope.for_agent(agent); }
    if let Some(fps) = q.fps {
        envelope.meta.insert("fps".into(), serde_json::json!(fps));
    }

    let routed_to = state.agents.dispatch(&envelope).await;
    Ok(Json(IngestResponse { envelope_id: envelope.id.to_string(), routed_to }))
}
