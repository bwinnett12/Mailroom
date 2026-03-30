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
use crate::initializers::mongodb::MongoInitializer;
use crate::components::record_list::RecordList;

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
        Ok(vec![
            Box::new(MongoInitializer),
        ])
    }

    async fn after_context(ctx: AppContext) -> Result<AppContext> {
        // 1. Get config safely
        let mongo_config = ctx.config.initializers
            .as_ref()
            .and_then(|i| i.get("mongodb"))
            .ok_or_else(|| loco_rs::Error::Message("mongodb config missing".into()))?;

        let uri_str = mongo_config.get("uri").and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb uri missing".into()))?;
        
        let db_name_str = mongo_config.get("database").and_then(|v| v.as_str())
            .ok_or_else(|| loco_rs::Error::Message("mongodb db_name missing".into()))?;

        // 2. Establish connection
        let client = Client::with_uri_str(uri_str).await.map_err(|e| {
            loco_rs::Error::Message(format!("failed to connect to mongodb: {e}"))
        })?;
        
        // We will verify the connection here
        let _db = client.database(db_name_str);

        // Since your AppContext has no 'extra' field, we will skip trying to 
        // force it in here and instead pass it directly to our services.
        Ok(ctx)
    }

    fn routes(_ctx: &AppContext) -> AppRoutes {
        AppRoutes::with_default_routes()
            .add_route(controllers::movie::routes())
            .add_route(controllers::note::routes())
            .add_route(controllers::auth::routes())
            .add_route(controllers::record::routes())
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

    fn middleware(_ctx: &AppContext) -> Result<Vec<Box<dyn Middleware>>> {
        Ok(vec![
            // This allows your frontend (wherever it lives) to talk to the API
            Box::new(CorsLayer::permissive()),
        ])
    }

    // This function lives in your Leptos frontend
    async fn fetch_records() -> Vec<DecimalRecord> {
        let res = reqwest::get("http://10.0.1.10:5150/api/records")
            .await
            .expect("Failed to fetch")
            .json::<Vec<DecimalRecord>>()
            .await
            .expect("Failed to parse JSON");
        res
    }
}



#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router>
            <main class="bg-slate-50 min-h-screen">
                <Routes>
                    // When the user hits the root URL, show the list
                    <Route path="" view=move || view! { 
                        <div class="container mx-auto py-8">
                            <h1 class="text-3xl font-bold mb-6">"Mailroom Dashboard"</h1>
                            <RecordList /> // <--- It goes here!
                        </div>
                    }/>
                </Routes>
            </main>
        </Router>
    }
}