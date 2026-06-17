pub mod registry;
mod ollama;

pub use registry::AgentRegistry;

use axum::{Router, routing::get};
use std::sync::Arc;
use crate::router::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/agents", get(registry::list_agents))
}
