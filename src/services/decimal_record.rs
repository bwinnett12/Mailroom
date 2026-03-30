// src/services/decimal_record.rs
use crate::models::record::DecimalRecord;
use anyhow::{Result, anyhow};
use chrono::Utc;

// use mongodb::Database;
// use loco_rs::prelude::AppContext;

pub struct DecimalService;

impl DecimalService {
    /// Parses a string like "13.4-A-SOIL" into a DecimalRecord
    pub fn parse_code(input: &str, content: &str, title: &str) -> Result<DecimalRecord> {
        let parts: Vec<&str> = input.split(|c| c == '.' || c == '-').collect();
        
        // Basic validation: Must at least have a top-level category (e.g., "13")
        if parts.is_empty() {
            return Err(anyhow!("Invalid Johnny.Decimal format for DecimalRecord"));
        }

        // Calculate depth based on number of separators
        // 13 -> depth 0
        // 13.4 -> depth 1
        // 13.4-A -> depth 2
        let depth = (parts.len() - 1) as u32;

        // Determine parent (everything except the last part)
        let parent_code = if depth > 0 {
            let last_separator_idx = input.rfind(|c| c == '.' || c == '-').unwrap();
            Some(input[..last_separator_idx].to_string())
        } else {
            None
        };

        Ok(DecimalRecord {
            id: None,
            code: input.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            depth,
            parent_code,
            external_context: std::collections::HashMap::new(),
            vector_id: None,
            tags: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

pub async fn save(db: &mongodb::Database, record: DecimalRecord) -> Result<()> {
        // 1. Get the MongoDB client from AppContext
        // We'll use a helper to get the DB handle we set up in app.rs
    //    let db = ctx.extra.get("mongodb_handle")
    //        .and_then(|v| v.downcast_ref::<Database>())
    //        .ok_or_else(|| anyhow!("MongoDB handle not found in AppContext"))?;

        let collection: mongodb::Collection<DecimalRecord> = db.collection::<DecimalRecord>("records");

        // 2. Insert the record
        collection
            .insert_one(record)
            .await
            .map_err(|e| anyhow!("Failed to insert into MongoDB: {}", e))?;

        Ok(())
    }

}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parsing_logic() {
        let code = "13.4-A-SOIL";
        let record = DecimalService::parse_code(code, "test content", "Soil Project").unwrap();
        
        assert_eq!(record.depth, 3);
        assert_eq!(record.parent_code, Some("13.4-A".to_string()));
    }

    #[test]
    fn test_top_level_parent() {
        let code = "13";
        let record = DecimalService::parse_code(code, "test", "Top").unwrap();
        
        assert_eq!(record.depth, 0);
        assert_eq!(record.parent_code, None);
    }
}