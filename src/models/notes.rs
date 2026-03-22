pub use super::_entities::notes::ActiveModel;
pub use super::_entities::notes::Entity;
pub use super::_entities::notes::Model;
use loco_rs::prelude::*; // <--- Add this import

impl Entity {
    // Custom logic goes here
}

// Change the 'super::_entities' path to just 'ActiveModelBehavior'
impl ActiveModelBehavior for ActiveModel {}
