use axum::{
    extract::{Multipart, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::envelope::{Envelope, Payload, SourceKind};
use super::camera::IngestResponse;
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct MicQuery {
    pub agent: Option<String>,
    pub jd: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// POST /ingest/microphone
///
/// Accepts raw audio bytes (`audio` field in multipart).
/// Typical MIME types: audio/wav, audio/webm, audio/ogg.
pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MicQuery>,
    mut multipart: Multipart,
) -> Result<Json<IngestResponse>, StatusCode> {
    let mut audio_data: Option<Vec<u8>> = None;
    let mut mime_type = "audio/wav".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if field.name() == Some("audio") {
            if let Some(ct) = field.content_type() {
                mime_type = ct.to_string();
            }
            audio_data = Some(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_vec());
        }
    }

    let data = audio_data.ok_or(StatusCode::BAD_REQUEST)?;
    info!(bytes = data.len(), sample_rate = ?q.sample_rate, "Audio chunk received");

    let mut envelope = Envelope::new(
        SourceKind::Microphone,
        Payload::Binary { mime_type, data },
    );

    if let Some(jd) = q.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &q.agent { envelope = envelope.for_agent(agent); }
    if let Some(sr) = q.sample_rate {
        envelope.meta.insert("sample_rate".into(), sr.into());
    }
    if let Some(ch) = q.channels {
        envelope.meta.insert("channels".into(), ch.into());
    }

    let routed_to = state.agents.dispatch(&envelope).await;

    Ok(Json(IngestResponse {
        envelope_id: envelope.id.to_string(),
        routed_to,
    }))
}
