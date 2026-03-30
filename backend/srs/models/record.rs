// src/models/record.rs
use serde::{Deserialize, Serialize};
use mongodb::bson::oid::ObjectId;
use std::collections::HashMap;

// Based on the Johnny Decimal System
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecimalRecord {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<ObjectId>,

    /// The unique J.D code (e.g., "13.4-A-SPECIFIC-TERM")
    pub code: String,

    /// The human-readable title of the folder/process
    pub title: String,

    /// The actual content (Markdown, instructions, or system paths)
    pub content: String,

    /// Defined depth level (0 = Category, 1 = ID, 2+ = Sub-item)
    pub depth: u32,

    /// Parent code for easy tree traversal (e.g., "13.4" is parent of "13.4-A")
    pub parent_code: Option<String>,

    /// Specific tags for filtering (e.g., ["health", "automation", "nixos"])
    pub tags: Vec<String>,

    /// For RAG: This is where we store the ID of the vector in Qdrant
    pub vector_id: Option<String>,

    /// Extensible metadata for "Real World" or "System" links
    /// e.g., {"location": "Physical Filing Cabinet", "script_path": "/bin/sync"}
    pub external_context: HashMap<String, String>,

    /// Timestamps for scheduling and tracking
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}



impl DecimalRecord {
    /// Helper to determine if this record is a "Leaf" (a specific process)
    /// vs a "Branch" (a folder/category)
    pub fn is_process(&self) -> bool {
        self.depth >= 2
    }
}