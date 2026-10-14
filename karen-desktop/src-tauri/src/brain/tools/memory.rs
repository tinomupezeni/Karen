use serde_json::{json, Value};
use tauri::Manager;

pub async fn save_memory(args: Value, app: &tauri::AppHandle) -> Value {
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "missing content parameter" });
    };
    let memory = app.state::<crate::memory::MemoryEngine>();
    let embedding = if std::env::var("KAREN_LLM_PROVIDER")
        .map(|v| v.eq_ignore_ascii_case("ollama"))
        .unwrap_or(false)
    {
        crate::memory::semantic::ollama_embedding(content)
            .await
            .ok()
    } else {
        None
    };
    let db = memory.db.lock().await;
    match db.save_memory_with_embedding(content, None, embedding.as_deref()) {
        Ok(id) => json!({ "ok": true, "id": id }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

pub async fn search_events(args: Value, app: &tauri::AppHandle) -> Value {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let memory = app.state::<crate::memory::MemoryEngine>();
    let db = memory.db.lock().await;
    match db.get_recent_events(limit) {
        Ok(events) => json!({ "ok": true, "events": events }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}
