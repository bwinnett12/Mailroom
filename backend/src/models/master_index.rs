use serde::{Deserialize, Serialize};
use sea_orm::entity::prelude::*;
use loco_rs::model::ModelResult;    
use loco_rs::model::ModelError;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "master_index")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub cid: String,
    pub title: String,
    pub parent_cid: Option<String>,
    pub level: i32,
    pub path: Option<String>,
    pub content: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Find a specific decimal by its CID (e.g., "10.01")
    pub async fn find_by_cid(db: &DatabaseConnection, cid: &str) -> Result<Self, ModelError> {
        let res = Entity::find_by_id(cid.to_owned()).one(db).await?;
        res.ok_or(ModelError::EntityNotFound)
    }
}