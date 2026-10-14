use crate::memory::Database;
use crate::memory::semantic;
use rusqlite::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn build_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter_map(|word| {
            let cleaned: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!cleaned.is_empty()).then(|| format!("{cleaned}*"))
        })
        .take(12)
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub recorded_at: String,
}

impl Database {
    pub fn save_memory(&self, content: &str, source_id: Option<&str>) -> Result<String> {
        self.save_memory_with_embedding(content, source_id, None)
    }

    pub fn save_memory_with_embedding(
        &self,
        content: &str,
        source_id: Option<&str>,
        embedding: Option<&[f32]>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO memories (id, content, source_id, embedding) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                id,
                content,
                source_id,
                embedding.map(|v| serde_json::to_string(v).unwrap_or_default())
            ],
        )?;
        Ok(id)
    }

    pub fn search_semantic_memories(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, recorded_at, embedding FROM memories WHERE embedding IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let embedding: String = row.get(3)?;
            let vector = serde_json::from_str::<Vec<f32>>(&embedding).unwrap_or_default();
            Ok((
                MemoryRecord {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    recorded_at: row.get(2)?,
                },
                vector,
            ))
        })?;
        let candidates: Vec<_> = rows.filter_map(Result::ok).collect();
        Ok(semantic::rank(
            query,
            &candidates
                .iter()
                .map(|(record, vector)| (record.clone(), vector.clone()))
                .collect::<Vec<_>>(),
            limit,
        )
        .into_iter()
        .map(|(record, _)| record)
        .collect())
    }

    pub fn search_memories(&self, query: &str) -> Result<Vec<MemoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.content, m.recorded_at 
             FROM memories m
             JOIN memories_fts f ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1 
             ORDER BY rank LIMIT 5",
        )?;

        let fts_query = build_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }

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
            "SELECT id, content, recorded_at FROM memories ORDER BY recorded_at DESC LIMIT ?1",
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

#[cfg(test)]
mod tests {
    use super::build_fts_query;

    #[test]
    fn builds_word_search_query() {
        assert_eq!(
            build_fts_query("Karen architecture debate"),
            "Karen* OR architecture* OR debate*"
        );
        assert_eq!(build_fts_query("   "), "");
    }
}
