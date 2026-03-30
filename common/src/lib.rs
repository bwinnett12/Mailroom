use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DecimalRecord {
    pub code: String,
    pub title: String,
    pub content: String,
    pub depth: u32,
    pub parent_code: Option<String>,
    pub created_at: DateTime<Utc>,
}