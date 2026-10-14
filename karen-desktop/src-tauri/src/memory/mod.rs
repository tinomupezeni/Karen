pub mod artifacts;
pub mod conversations;
pub mod database;
pub mod events;
pub mod memories;
pub mod semantic;
pub mod tasks;
pub mod temporal;

pub use database::Database;
use std::sync::Arc;
use tokio::sync::Mutex;

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
