use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        create_table(m, "entries",
            &[
            
            ("id", ColType::PkAuto),
            
            ("title", ColType::StringNull),
            ("content", ColType::TextNull),
            ("kind", ColType::StringNull),
            ("is_private", ColType::BooleanNull),
            ("tags", ColType::TextNull),
            ],
            &[
            ]
        ).await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        drop_table(m, "entries").await
    }
}
