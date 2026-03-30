use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {

    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
    // First column
    m.alter_table(
        Table::alter()
            .table(Alias::new("notes"))
            .add_column(ColumnDef::new(Alias::new("user_id")).integer().not_null().default(1))
            .to_owned(),
    ).await?;

    // Second column
    m.alter_table(
        Table::alter()
            .table(Alias::new("notes"))
            .add_column(ColumnDef::new(Alias::new("parent_id")).integer().null())
            .to_owned(),
    ).await
}

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Alias::new("notes"))
                .drop_column(Alias::new("user_id"))
                .drop_column(Alias::new("parent_id"))
                .to_owned(),
        )
        .await
    }
}
