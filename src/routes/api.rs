use std::path::{Path, PathBuf};
use std::fs;
use axum::{extract::State, Json, response::IntoResponse};
use serde_json::json;
use std::sync::Arc;
use crate::state::AppState;

// ── Registry Graph endpoint (keep your existing one) ────────────────────

pub async fn registry_graph(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let registry = state.registry.read().await;
    let nodes: Vec<_> = registry
        .all()
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.name,
                "kind": m.kind,
                "path": m.path,
                "store": m.store,
            })
        })
        .collect();

    Json(json!({"nodes": nodes, "count": nodes.len()}))
}

// ── Get manifest by node ID ────────────────────────────────────────────

pub async fn get_manifest(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(node_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let registry = state.registry.read().await;
    
    // Try exact match first
    if let Some(manifest) = registry.get(&node_id) {
        return Json(serde_json::to_value(manifest).unwrap_or(json!({})));
    }
    
    // Fall back to parent by stripping last segment
    let parent_id = get_parent_id(&node_id);
    if !parent_id.is_empty() {
        if let Some(manifest) = registry.get(parent_id) {
            return Json(serde_json::to_value(manifest).unwrap_or(json!({})));
        }
    }
    
    Json(json!({"error": "Not found"}))
}

fn get_parent_id(node_id: &str) -> &str {
    if let Some(pos) = node_id.rfind('-') {
        return &node_id[..pos];
    }
    if let Some(pos) = node_id.rfind('.') {
        return &node_id[..pos];
    }
    ""
}

// ── Get README for a node ──────────────────────────────────────────────

pub async fn get_node_readme(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(node_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let registry = state.registry.read().await;
    
    // Find the actual directory for this node
    if let Some(dir) = find_node_directory(&state.vault_root, &node_id) {
        // Try README.md first
        if let Ok(content) = fs::read_to_string(dir.join("README.md")) {
            return Json(json!({"content": content, "filename": "README.md"}));
        }
        
        // Try {id}.md
        if let Ok(content) = fs::read_to_string(dir.join(&format!("{}.md", node_id))) {
            return Json(json!({"content": content, "filename": format!("{}.md", node_id)}));
        }
        
        return Json(json!({"content": "No README found", "filename": "—"}));
    }
    
    Json(json!({"error": "Node directory not found"}))
}

// ── List files in a node ───────────────────────────────────────────────

pub async fn list_node_files(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(node_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let registry = state.registry.read().await;
    
    if let Some(dir) = find_node_directory(&state.vault_root, &node_id) {
        match fs::read_dir(&dir) {
            Ok(entries) => {
                let files: Vec<String> = entries
                    .filter_map(|e| {
                        e.ok().and_then(|entry| {
                            let path = entry.path();
                            if path.is_file() {
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                
                return Json(json!({"files": files}));
            }
            Err(_) => return Json(json!({"files": []})),
        }
    }
    
    Json(json!({"error": "Node directory not found"}))
}

// ── Helper: find node directory by walking the vault ───────────────────

fn find_node_directory(vault_root: &Path, node_id: &str) -> Option<PathBuf> {
    // Use walkdir (which is already a dependency for registry.rs)
    use walkdir::WalkDir;
    
    for entry in WalkDir::new(vault_root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if let Some(name) = path.file_name() {
            if let Some(name_str) = name.to_str() {
                // Match "39.2-3C_notes-personal" against "39.2-3C"
                if name_str.starts_with(&format!("{}_", node_id)) || name_str == node_id {
                    return Some(path.to_path_buf());
                }
            }
        }
    }
    None
}