// src/controllers/record.rs
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};
use crate::services::decimal_record::DecimalService;
use mongodb::Database;
// use ax_extract::Extension;
use axum::Extension;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordRequest {
    pub code: String,
    pub title: String,
    pub content: String,
}

pub async fn create(
    State(_ctx): State<AppContext>,
    Extension(db): Extension<Database>,
    Json(params): Json<RecordRequest>,
) -> Result<Response> {
    // 1. Parse
let record = DecimalService::parse_code(&params.code, &params.content, &params.title)
        .map_err(Box::from)?; // Box<dyn Error> is automatically converted to loco_rs::Error

    // 2. Save
    DecimalService::save(&db, record.clone())
        .await
        .map_err(|e| Error::InternalServerError(e.to_string()))?; 

    // 3. Return JSON
    format::json(record)
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/records")
        .add("/", post(create))
}