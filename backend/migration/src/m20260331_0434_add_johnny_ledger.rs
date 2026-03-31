pub use sea_orm_migration::prelude::*;

mod m20260331_0434_add_johnny_ledger; // Replace with your actual filename

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260331_0434_add_johnny_ledger::Migration),
        ]
    }
}