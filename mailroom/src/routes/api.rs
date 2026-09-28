use axum::{
    extract::State,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use crate::state::AppState;
use serde_json::json;

pub async fn registry_graph(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let registry = state.registry.read().await;
    let nodes = registry.all();
    
    // Debug log
    eprintln!("DEBUG: registry.all() returned {} items", nodes.len());
    
    let node_values: Vec<_> = nodes
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or(json!({})))
        .collect();
    
    Json(json!({"nodes": node_values, "count": nodes.len()}))
}
