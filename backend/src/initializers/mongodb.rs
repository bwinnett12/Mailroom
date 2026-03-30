// src/initializers/mongodb.rs
use async_trait::async_trait;
use loco_rs::{
    app::{AppContext, Initializer},
    Result,
};
use mongodb::Client;
use axum::Extension;

pub struct MongoInitializer;

#[async_trait]
impl Initializer for MongoInitializer {
    fn name(&self) -> String {
        "mongodb".into()
    }

    async fn after_routes(&self, router: axum::Router, ctx: &AppContext) -> Result<axum::Router> {
        // 1. Get Config
        let mongo_config = ctx.config.initializers
            .as_ref()
            .and_then(|i| i.get("mongodb"))
            .ok_or_else(|| loco_rs::Error::Message("mongodb config missing".into()))?;

        let uri = mongo_config.get("uri").and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb uri missing".into()))?;
        
        let db_name = mongo_config.get("database").and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb database name missing".into()))?;

        // 2. Connect
        let client = Client::with_uri_str(uri).await.map_err(|e| {
            loco_rs::Error::Message(format!("failed to connect to mongodb: {e}"))
        })?;
        
        let db = client.database(db_name);

        // 3. Inject into Axum as an Extension
        // This makes 'Database' available to any controller via 'Extension<Database>'
        Ok(router.layer(Extension(db)))
    }
}