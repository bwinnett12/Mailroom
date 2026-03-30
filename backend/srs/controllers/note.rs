#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
use loco_rs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::models::_entities::notes::{ActiveModel, Entity, Model};
use crate::workers::note_tagger;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Params {
    pub title: String,
    pub content: Option<String>,
    pub is_research: Option<bool>,
    // pub user_id: i32,
    }

impl Params {
    fn update(&self, item: &mut ActiveModel) {
      item.title = Set(self.title.clone());
      item.content = Set(self.content.clone());
      item.is_research = Set(self.is_research);
      // item.user_id = Set(self.user_id);
      }
}

async fn load_item(ctx: &AppContext, id: i32) -> Result<Model> {
    let item = Entity::find_by_id(id).one(&ctx.db).await?;
    item.ok_or_else(|| Error::NotFound)
}

#[debug_handler]
pub async fn list(State(ctx): State<AppContext>) -> Result<Response> {
    format::json(Entity::find().all(&ctx.db).await?)
}

#[debug_handler]
pub async fn add(State(ctx): State<AppContext>, Json(params): Json<Params>) -> Result<Response> {
    // 1. Create a blank "ActiveModel" (the database worker)
    let mut item = ActiveModel {
        ..Default::default()
    };

    // 2. Fill that blank model with the data from your 'curl' (params)
    params.update(&mut item);
    /*
    if params.is_research.unwrap_or(false) {
        if let Ok(Some(latest_journal)) = Entity::find_latest_journal(&ctx.db, params.user_id).await {
            item.parent_id = Set(Some(latest_journal.id));
        }
    }
    */

    // 3. Save it to SQLite. This returns the final saved 'item' (with its new ID)
    let item = item.insert(&ctx.db).await?;

    // 4. NOW trigger the AI worker using the ID we just got
    // This runs in the background while the user gets their response
    note_tagger::Worker::perform_later(&ctx, note_tagger::NoteTaggerArgs {
        note_id: item.id, 
    }).await?;

    // 5. Tell the user "Success!"
    format::json(item)
}

#[debug_handler]
pub async fn update(
    Path(id): Path<i32>,
    State(ctx): State<AppContext>,
    Json(params): Json<Params>,
) -> Result<Response> {
    let item = load_item(&ctx, id).await?;
    let mut item = item.into_active_model();
    params.update(&mut item);
    let item = item.update(&ctx.db).await?;
    format::json(item)
}

#[debug_handler]
pub async fn remove(Path(id): Path<i32>, State(ctx): State<AppContext>) -> Result<Response> {
    load_item(&ctx, id).await?.delete(&ctx.db).await?;
    format::empty()
}

#[debug_handler]
pub async fn get_one(Path(id): Path<i32>, State(ctx): State<AppContext>) -> Result<Response> {
    format::json(load_item(&ctx, id).await?)
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix("api/notes/")
        .add("/", get(list))
        .add("/", post(add))
        .add("{id}", get(get_one))
        .add("{id}", delete(remove))
        .add("{id}", put(update))
        .add("{id}", patch(update))
}
