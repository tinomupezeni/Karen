use crate::memory::Database;
use rusqlite::Result;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub recorded_at: String,
}

impl Database {
    pub fn save_memory(&self, content: &str, source_id: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO memories (id, content, source_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, content, source_id],
        )?;
        Ok(id)
    }

    pub fn search_memories(&self, query: &str) -> Result<Vec<MemoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.content, m.recorded_at 
             FROM memories m
             JOIN memories_fts f ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1 
             ORDER BY rank LIMIT 5"
        )?;

        // FTS5 requires matching syntax, a simple word match works
        let fts_query = format!("\"{}\"*", query.replace("\"", "\"\""));
        
        let mem_iter = stmt.query_map([fts_query], |row| {
            Ok(MemoryRecord {
                id: row.get(0)?,
                content: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        })?;

        let mut memories = Vec::new();
        for m in mem_iter {
            memories.push(m?);
        }
        Ok(memories)
    }

    pub fn get_recent_memories(&self, limit: usize) -> Result<Vec<MemoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, recorded_at FROM memories ORDER BY recorded_at DESC LIMIT ?1"
        )?;
        let mem_iter = stmt.query_map([limit as i64], |row| {
            Ok(MemoryRecord {
                id: row.get(0)?,
                content: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        })?;
        let mut memories = Vec::new();
        for m in mem_iter {
            memories.push(m?);
        }
        Ok(memories)
    }
}
