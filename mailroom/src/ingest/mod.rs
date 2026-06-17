pub mod envelope;
mod camera;
mod microphone;
mod video;
mod text;
mod file_upload;
mod webhook;

pub use envelope::{Envelope, Payload, SourceKind};

use axum::{Router, routing::post};
use std::sync::Arc;
use crate::router::AppState;

/// Mount all ingest routes under /ingest
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        // Still-image frames from a camera source
        .route("/ingest/camera",      post(camera::handler))
        // Raw audio chunks / streams
        .route("/ingest/microphone",  post(microphone::handler))
        // Video clips or streams
        .route("/ingest/video",       post(video::handler))
        // Plain text / transcripts / prompts
        .route("/ingest/text",        post(text::handler))
        // Generic file upload (auto-detects MIME)
        .route("/ingest/file",        post(file_upload::handler))
        // Arbitrary JSON webhooks
        .route("/ingest/webhook",     post(webhook::handler))
}
