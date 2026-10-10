use crate::memory::Database;
use rusqlite::Result;
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use chrono::TimeZone;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub recorded_at: String,
}

impl Database {
    pub fn create_conversation(&self) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO conversations (id) VALUES (?1)",
            [&id],
        )?;
        Ok(id)
    }

    pub fn get_active_conversation(&self) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM conversations WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1"
        )?;
        let mut rows = stmt.query([])?;
        
        if let Some(row) = rows.next()? {
            let conv_id: String = row.get(0)?;
            
            // Check last message time
            let mut msg_stmt = self.conn.prepare(
                "SELECT recorded_at FROM messages WHERE conversation_id = ?1 ORDER BY recorded_at DESC LIMIT 1"
            )?;
            let mut msg_rows = msg_stmt.query([&conv_id])?;
            if let Some(msg_row) = msg_rows.next()? {
                let recorded_at: String = msg_row.get(0)?;
                let naive = chrono::NaiveDateTime::parse_from_str(&recorded_at, "%Y-%m-%d %H:%M:%S").unwrap_or_default();
                let utc = chrono::Utc.from_utc_datetime(&naive);
                let duration = chrono::Utc::now().signed_duration_since(utc);
                
                // If it's been more than 12 hours since the last message, close the session
                if duration.num_hours() > 12 {
                    self.conn.execute("UPDATE conversations SET ended_at = CURRENT_TIMESTAMP WHERE id = ?1", [&conv_id])?;
                    return Ok(None);
                }
            }
            Ok(Some(conv_id))
        } else {
            Ok(None)
        }
    }

    pub fn append_message(&self, conversation_id: &str, role: &str, content: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content) VALUES (?1, ?2, ?3, ?4)",
            &[&id, conversation_id, role, content],
        )?;
        Ok(id)
    }

    pub fn get_recent_messages(&self, conversation_id: &str, limit: usize) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, conversation_id, role, content, recorded_at 
             FROM messages 
             WHERE conversation_id = ?1 
             ORDER BY recorded_at DESC
             LIMIT ?2"
        )?;
        
        let msg_iter = stmt.query_map(rusqlite::params![conversation_id, limit as i64], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                recorded_at: row.get(4)?,
            })
        })?;

        let mut messages = Vec::new();
        for msg in msg_iter {
            messages.push(msg?);
        }
        messages.reverse();
        Ok(messages)
    }
}
