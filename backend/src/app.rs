use async_trait::async_trait;
use loco_rs::prelude::*;
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
            .add_route(controllers::notes::routes())
    }

    async fn boot(mode: StartMode, environment: &Environment) -> Result<BootResult> {
        create_app::<Self>(mode, environment).await
    }

    fn middlewares(_ctx: &AppContext) -> Result<Vec<Box<dyn Middleware>>> {
        Ok(vec![
            Box::new(axum::middleware::from_fn(loco_rs::boot::middleware::cors)),
        ])
    }
}