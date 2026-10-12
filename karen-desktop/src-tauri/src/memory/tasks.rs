use crate::memory::Database;
use rusqlite::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub description: String,
    pub status: String,
    pub result: Option<String>,
    pub recorded_at: String,
}

impl Database {
    pub fn create_task(&self, description: &str) -> Result<String> {
        let id = format!("task-{}", Uuid::new_v4());
        self.conn.execute(
            "INSERT INTO tasks (id, description, status) VALUES (?1, ?2, 'queued')",
            rusqlite::params![id, description],
        )?;
        Ok(id)
    }

    pub fn update_task(&self, id: &str, status: &str, result: Option<&str>) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE tasks SET status = ?1, result = ?2 WHERE id = ?3",
            rusqlite::params![status, result, id],
        )?;
        Ok(changed == 1)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<TaskRecord>> {
        let mut stmt = self.conn.prepare("SELECT id, description, status, result, recorded_at FROM tasks WHERE id = ?1")?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(TaskRecord { id: row.get(0)?, description: row.get(1)?, status: row.get(2)?, result: row.get(3)?, recorded_at: row.get(4)? })),
            None => Ok(None),
        }
    }

    pub fn list_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let mut stmt = self.conn.prepare("SELECT id, description, status, result, recorded_at FROM tasks ORDER BY recorded_at DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| Ok(TaskRecord { id: row.get(0)?, description: row.get(1)?, status: row.get(2)?, result: row.get(3)?, recorded_at: row.get(4)? }))?;
        rows.collect()
    }
}
