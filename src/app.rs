use async_trait::async_trait;
use loco_rs::{
    app::{AppContext, Hooks, Initializer},
    bgworker::{BackgroundWorker, Queue},
    boot::{create_app, BootResult, StartMode},
    config::Config,
    controller::AppRoutes,
    db::{self, truncate_table},
    environment::Environment,
    task::Tasks,
    Result,
};
use mongodb::{Client, Database};
use migration::Migrator;
use std::path::Path;

#[allow(unused_imports)]
use crate::{
    controllers, 
    models::_entities, 
    models::_entities::users,
    tasks, 
    initializers,
    workers::downloader::DownloadWorker
};

pub struct App;
#[async_trait]
impl Hooks for App {
    fn app_name() -> &'static str {
        env!("CARGO_CRATE_NAME")
    }

    fn app_version() -> String {
        format!(
            "{} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("BUILD_SHA")
                .or(option_env!("GITHUB_SHA"))
                .unwrap_or("dev")
        )
    }

    async fn boot(
        mode: StartMode,
        environment: &Environment,
        config: Config,
    ) -> Result<BootResult> {
        create_app::<Self, Migrator>(mode, environment, config).await
    }

    async fn initializers(_ctx: &AppContext) -> Result<Vec<Box<dyn Initializer>>> {
        Ok(vec![])
    }

/// This is where we inject MongoDB into the AppContext
    async fn after_context(ctx: AppContext) -> Result<AppContext> {
        // Pulling from your config/development.yaml 'initializers' section
        let mongo_config = ctx.config.initializers
            .as_ref()
            .and_then(|i| i.get("mongodb"))
            .ok_or_else(|| loco_rs::Error::Message("mongodb config not found in yaml".into()))?;

        let uri = mongo_config.get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb uri missing".into()))?;

        let db_name = mongo_config.get("database")
            .and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb database name missing".into()))?;

        // Connect to Mongo
        let client = Client::with_uri_str(uri).await.map_err(|e| {
            loco_rs::Error::Message(format!("failed to connect to mongodb: {e}"))
        })?;
        
        let db = client.database(db_name);

        // Inject the Database client into the 'extra' state of AppContext
        // Note: Since 'extra' stores Any, we can put the Database handle there.
        let mut ctx = ctx;
        ctx.extra.insert("mongodb".to_string(), serde_json::to_value(uri).unwrap()); 
        // Tip: For complex types, many users wrap them in an Arc/State struct.
        
        Ok(ctx)
    }

    fn routes(_ctx: &AppContext) -> AppRoutes {
        AppRoutes::with_default_routes()
            .add_route(controllers::movie::routes())
            .add_route(controllers::note::routes())
            .add_route(controllers::auth::routes())
            // You'll add your decimal_record routes here soon
    }


    async fn connect_workers(ctx: &AppContext, queue: &Queue) -> Result<()> {
        queue.register(crate::workers::note_tagger::Worker::build(ctx)).await?;
        queue.register(DownloadWorker::build(ctx)).await?;
        Ok(())
    }

    #[allow(unused_variables)]
    fn register_tasks(tasks: &mut Tasks) {
        // tasks-inject (do not remove)
    }
    async fn truncate(ctx: &AppContext) -> Result<()> {
        truncate_table(&ctx.db, users::Entity).await?;
        Ok(())
    }
    async fn seed(ctx: &AppContext, base: &Path) -> Result<()> {
        db::seed::<users::ActiveModel>(&ctx.db, &base.join("users.yaml").display().to_string())
            .await?;
        Ok(())
    }
}