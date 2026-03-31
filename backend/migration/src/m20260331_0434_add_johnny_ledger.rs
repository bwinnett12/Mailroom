use loco_rs::schema::table_auto_tz;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                table_auto_tz(DecimalLedger::Table)
                    .col(ColumnDef::new(DecimalLedger::Cid).string().not_null().primary_key())
                    .col(ColumnDef::new(DecimalLedger::Title).string().not_null())
                    .col(ColumnDef::new(DecimalLedger::ParentCid).string())
                    .col(ColumnDef::new(DecimalLedger::Level).integer().not_null())
                    .col(ColumnDef::new(DecimalLedger::Path).string()) // Nullable if no file exists yet
                    .col(ColumnDef::new(DecimalLedger::Content).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DecimalLedger::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DecimalLedger {
    Table,
    Cid,
    Title,
    ParentCid,
    Level,
    Path,
    Content
}