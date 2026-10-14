use serde_json::{json, Value};
use tauri::Manager;

pub async fn create(args: Value, app: &tauri::AppHandle) -> Value {
    let Some(description) = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return json!({"ok":false,"error":"missing description"});
    };
    let memory = app.state::<crate::memory::MemoryEngine>();
    let db = memory.db.lock().await;
    match db.create_task(description) {
        Ok(id) => json!({"ok":true,"task_id":id,"status":"queued"}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    }
}

pub async fn get(args: Value, app: &tauri::AppHandle) -> Value {
    let Some(id) = args.get("task_id").and_then(Value::as_str) else {
        return json!({"ok":false,"error":"missing task_id"});
    };
    let memory = app.state::<crate::memory::MemoryEngine>();
    let db = memory.db.lock().await;
    match db.get_task(id) {
        Ok(Some(task)) => json!({"ok":true,"task":task}),
        Ok(None) => json!({"ok":false,"error":"task not found"}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    }
}

pub async fn list(args: Value, app: &tauri::AppHandle) -> Value {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(50) as usize;
    let memory = app.state::<crate::memory::MemoryEngine>();
    let db = memory.db.lock().await;
    match db.list_tasks(limit) {
        Ok(tasks) => json!({"ok":true,"tasks":tasks}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    }
}
