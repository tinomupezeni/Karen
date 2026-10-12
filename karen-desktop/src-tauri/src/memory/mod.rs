pub mod database;
pub mod conversations;
pub mod temporal;
pub mod semantic;
pub mod memories;
pub mod events;
pub mod tasks;

use std::sync::Arc;
use tokio::sync::Mutex;
pub use database::Database;

pub struct MemoryEngine {
    pub db: Arc<Mutex<Database>>,
}

impl MemoryEngine {
    pub fn new(db_path: std::path::PathBuf) -> Result<Self, rusqlite::Error> {
        let db = Database::new(db_path)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }
}
