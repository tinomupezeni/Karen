use crate::memory::Database;
use rusqlite::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub source_event_id: Option<String>,
    pub metadata: Option<String>,
    pub recorded_at: String,
}

impl Database {
    pub fn record_artifact(
        &self,
        kind: &str,
        path: &str,
        source_event_id: Option<&str>,
        metadata: Option<&str>,
    ) -> Result<String> {
        let id = format!("artifact-{}", Uuid::new_v4());
        self.conn.execute(
            "INSERT INTO artifacts (id, kind, path, source_event_id, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, kind, path, source_event_id, metadata],
        )?;
        Ok(id)
    }

    pub fn list_artifacts(&self, limit: usize) -> Result<Vec<ArtifactRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, source_event_id, metadata, recorded_at
             FROM artifacts ORDER BY recorded_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(ArtifactRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                source_event_id: row.get(3)?,
                metadata: row.get(4)?,
                recorded_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }
}
