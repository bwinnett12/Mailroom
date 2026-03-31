use async_trait::async_trait;
use loco_rs::prelude::*;
use loco_rs::boot::{create_app, BootResult, StartMode};
use loco_rs::environment::Environment;
// use loco_rs::controller::middleware::Middleware;
use loco_rs::controller::middleware::Middleware;

use loco_rs::controller::middleware::cors;

use loco_rs::controller::AppRoutes;
use loco_rs::app::Hooks;
use loco_rs::config::Config;

pub use axum::middleware::from_fn;


use crate::{
    controllers, initializers, models, tasks, workers,
};

pub struct App;
#[async_trait]
impl Hooks for App {
    fn app_name() -> &'static str {
        "mailroom"
    }

    fn routes(_ctx: &AppContext) -> AppRoutes {
        AppRoutes::with_default_api()
            .add_route(controllers::auth::routes())
            //.add_route(controllers::notes::routes())
    }

    async fn boot(
        mode: StartMode, 
        environment: &Environment,
        config: Config
    ) -> Result<BootResult> {
        create_app::<Self>(mode, environment, config).await
    }

    fn middlewares(_ctx: &AppContext) -> Result<Vec<Box<dyn loco_rs::controller::middleware::Handler>>> {
        Ok(vec![])
    }
}