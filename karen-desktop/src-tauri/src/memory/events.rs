use crate::memory::Database;
use rusqlite::Result;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: String,
    pub event_type: String,
    pub description: String,
    pub source: Option<String>,
    pub recorded_at: String,
}

impl Database {
    pub fn record_event(&self, event_type: &str, description: &str, source: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO events (id, event_type, description, source) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, event_type, description, source],
        )?;
        Ok(id)
    }

    pub fn get_recent_events(&self, limit: usize) -> Result<Vec<EventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, description, source, recorded_at 
             FROM events 
             ORDER BY recorded_at DESC 
             LIMIT ?1"
        )?;
        
        let evt_iter = stmt.query_map([limit as i64], |row| {
            Ok(EventRecord {
                id: row.get(0)?,
                event_type: row.get(1)?,
                description: row.get(2)?,
                source: row.get(3)?,
                recorded_at: row.get(4)?,
            })
        })?;

        let mut events = Vec::new();
        for e in evt_iter {
            events.push(e?);
        }
        
        // Reverse so they are chronological if we want to inject them in order
        events.reverse();
        Ok(events)
    }
}
