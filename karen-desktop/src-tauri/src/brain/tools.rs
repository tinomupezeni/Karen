use serde_json::{json, Value};

use super::devlogs;
use super::open_app;

pub fn all_tool_defs() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "open_app",
                "description": "Launch a desktop application by name (e.g. \"vs code\", \"terminal\", \"browser\").",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "The application the user wants opened, in their own words."
                        }
                    },
                    "required": ["name"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_dev_logs",
                "description": "List dev-log entries (bug/issue logs and reports) from the last N days, across all projects and technical areas. Use days:1 for \"today\".",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "days": {
                            "type": "integer",
                            "description": "How many days back to look, inclusive of today."
                        }
                    },
                    "required": ["days"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_dev_logs",
                "description": "Read the full content of specific dev-log files, by the paths returned from list_dev_logs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Relative paths as returned by list_dev_logs, e.g. \"Backend_and_API/HBEC-2026-09-13-foo.md\"."
                        }
                    },
                    "required": ["paths"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "List the contents of a directory on the local filesystem.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The directory path to list. Supports absolute paths, relative paths, or '~' for home directory."
                        }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the text content of a file on the local filesystem.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path to read."
                        }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "save_memory",
                "description": "Extract and save a durable fact, decision, preference, or lesson about the user, project, or system.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The concise, factual memory to store (e.g. 'Project Karen is built using Tauri and React')."
                        }
                    },
                    "required": ["content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "get_current_time",
                "description": "Get the current system clock time in both UTC and the user's local timezone.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "search_events",
                "description": "Retrieve recent system events (e.g., activity changes, tool executions) from the audit log.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "The maximum number of recent events to retrieve."
                        }
                    },
                    "required": ["limit"]
                }
            }
        }),
    ]
}

use super::browse_files;

pub async fn dispatch(name: &str, args_json: &str, app: &tauri::AppHandle) -> Value {
    let parsed: Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("bad tool arguments: {e}") }),
    };

    let start_time = std::time::Instant::now();

    let result = match name {
        "open_app" => open_app::run(parsed.clone()).await,
        "list_dev_logs" => devlogs::list(parsed.clone()).await,
        "read_dev_logs" => devlogs::read(parsed.clone()).await,
        "list_directory" => browse_files::list_directory(parsed.clone()).await,
        "read_file" => browse_files::read_file(parsed.clone()).await,
        "save_memory" => {
            if let Some(content) = parsed.get("content").and_then(|v| v.as_str()) {
                use tauri::Manager;
                let memory = app.state::<crate::memory::MemoryEngine>();
                let db = memory.db.lock().await;
                match db.save_memory(content, None) {
                    Ok(id) => json!({ "ok": true, "id": id }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                }
            } else {
                json!({ "ok": false, "error": "missing content parameter" })
            }
        },
        "get_current_time" => {
            let utc = chrono::Utc::now();
            let local = chrono::Local::now();
            json!({
                "ok": true,
                "utc": utc.to_rfc3339(),
                "local": local.to_rfc3339(),
                "timezone": local.offset().to_string()
            })
        },
        "search_events" => {
            let limit = parsed.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            use tauri::Manager;
            let memory = app.state::<crate::memory::MemoryEngine>();
            let db = memory.db.lock().await;
            match db.get_recent_events(limit) {
                Ok(events) => json!({ "ok": true, "events": events }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            }
        },
        other => json!({ "ok": false, "error": format!("unknown tool: {other}") }),
    };

    let duration = start_time.elapsed().as_millis();
    let status = if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(true) { "success" } else { "failure" };
    
    // Log tool execution
    {
        use tauri::Manager;
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = memory.db.lock().await;
        
        // Truncate result for logs to avoid blowing up SQLite with massive file reads
        let mut result_preview = result.to_string();
        if result_preview.len() > 200 {
            result_preview.truncate(200);
            result_preview.push_str("...[truncated]");
        }
        
        let desc = format!("Tool: {name} | Args: {args_json} | Status: {status} | Duration: {duration}ms | Res: {result_preview}");
        let _ = db.record_event("tool_execution", &desc, Some("brain::tools"));
    }

    result
}
