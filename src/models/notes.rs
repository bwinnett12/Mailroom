pub use super::_entities::notes::ActiveModel;
pub use super::_entities::notes::Entity;
pub use super::_entities::notes::Model;
use loco_rs::prelude::*;
use sea_orm::prelude::*;
 

impl Entity {
    // Helper to find the most recent journal entry for a user
    pub async fn find_latest_journal(_db: &DatabaseConnection, _user_id: i32) -> Result<Option<Model>> {
        /*let journal = Entity::find()
            .filter(crate::models::_entities::notes::Column::UserId.eq(user_id))
            .filter(crate::models::_entities::notes::Column::Title.contains("Journal")) // Or a specific 'kind' column if you added one
            .order_by_desc(crate::models::_entities::notes::Column::CreatedAt)
            .one(db)
            .await?;
        */
        Ok(None)
    }
}

#[async_trait::async_trait]
impl ActiveModelBehavior for ActiveModel {
    // This runs automatically before every INSERT or UPDATE
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> std::result::Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert {
            // Set defaults or perform logic only on first creation
        }
        Ok(self)
    }
}
