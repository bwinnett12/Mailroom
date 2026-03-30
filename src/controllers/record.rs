// src/controllers/record.rs
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};
use crate::services::decimal_record::DecimalService;
use mongodb::Database;
use ax_extract::Extension;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordRequest {
    pub code: String,
    pub title: String,
    pub content: String,
}

/// Handler to create a new Johnny.Decimal record
pub async fn create(
    State(_ctx): State<AppContext>, // We still need the base context
    Json(params): Json<RecordRequest>,
) -> Result<Response> {
    // 1. Logic: Parse the input using your verified service
    let record = DecimalService::parse_code(&params.code, &params.content, &params.title)
        .map_err(|e| format_err!(BadRequest, e.to_string()))?;

    // 2. Database: Since we can't use AppContext.extra, we'll initialize 
    // the connection here (or pull from a global state if we set that up next)

	crate::services::decimal_record::DecimalService::save(&db, record)
        .await
        .map_err(|e| format_err!(InternalServerError, e.to_string()))?;

    format::json("Record created successfully")

    format::json(record)
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/records")
        .add("/", post(create))
}