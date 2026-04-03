pub mod _entities;
pub mod users;
pub mod notes;
pub mod movies;
pub mod record;

// backend/src/models/mod.rs
pub mod _entities;
pub mod users;
pub mod notes;

// Re-export for easier access in controllers and services
pub use _entities::decimal_ledger;
pub use _entities::decimal_ledger::Entity as DecimalLedger;
pub use _entities::decimal_ledger::Model as DecimalLedgerModel;