use loco_rs::schema::*;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Notes::Table)
                .add_column(ColumnDef::new(Notes::Tags).string().null())
                .to_owned(),
        )
        .await
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(
            Table::alter()
                .table(Notes::Table)
                .drop_column(Notes::Tags)
                .to_owned(),
        )
        .await
    }
}

#[derive(DeriveIden)]
enum Notes {
    Table,
    Tags, // This maps to the new column
}
