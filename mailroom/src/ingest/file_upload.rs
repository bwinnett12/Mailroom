// file_upload.rs
use axum::{extract::{Multipart, Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;
use mime_guess::from_path;

use crate::ingest::envelope::{Envelope, Payload, SourceKind};
use crate::ingest::camera::IngestResponse;
use crate::router::AppState;

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub agent: Option<String>,
    pub jd: Option<String>,
}

/// POST /ingest/file
/// Accepts any file in a `file` multipart field, auto-detects MIME from extension.
pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FileQuery>,
    mut multipart: Multipart,
) -> Result<Json<IngestResponse>, StatusCode> {
    let mut raw: Option<Vec<u8>> = None;
    let mut mime_type = "application/octet-stream".to_string();
    let mut filename: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if field.name() == Some("file") {
            filename = field.file_name().map(|s| s.to_string());
            if let Some(ref fname) = filename {
                let guessed = from_path(fname).first_or_octet_stream();
                mime_type = guessed.to_string();
            }
            if let Some(ct) = field.content_type() { mime_type = ct.to_string(); }
            raw = Some(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_vec());
        }
    }

    let data = raw.ok_or(StatusCode::BAD_REQUEST)?;
    info!(bytes = data.len(), mime = %mime_type, filename = ?filename, "File received");

    let mut envelope = Envelope::new(SourceKind::File, Payload::Binary { mime_type, data });
    if let Some(jd) = q.jd { envelope = envelope.with_jd(jd); }
    if let Some(agent) = &q.agent { envelope = envelope.for_agent(agent); }
    if let Some(fname) = filename {
        envelope.meta.insert("filename".into(), serde_json::Value::String(fname));
    }

    let routed_to = state.agents.dispatch(&envelope).await;
    Ok(Json(IngestResponse { envelope_id: envelope.id.to_string(), routed_to }))
}
