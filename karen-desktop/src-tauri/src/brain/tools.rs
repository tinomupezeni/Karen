use serde_json::{json, Value};
use tauri::Emitter;

use super::devlogs;
use super::open_app;
use std::sync::{Mutex, OnceLock};

static PENDING_TOOL: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

fn pending_tool() -> &'static Mutex<Option<(String, String)>> {
    PENDING_TOOL.get_or_init(|| Mutex::new(None))
}

fn valid_url(url: &str) -> bool {
    let valid_scheme = url.starts_with("https://") || url.starts_with("http://");
    let unsafe_char = url.chars().any(|c| matches!(c, '\n' | '\r' | ';' | '&' | '|' | '`' | '$'));
    valid_scheme && !unsafe_char && url.len() <= 2048
}

fn open_url(args: Value) -> Value {
    let Some(url) = args.get("url").and_then(Value::as_str).map(str::trim) else {
        return json!({ "ok": false, "error": "missing 'url' argument" });
    };

    if !valid_url(url) {
        return json!({ "ok": false, "error": "only safe http(s) URLs are allowed" });
    }

    match std::process::Command::new("xdg-open").arg(url).spawn() {
        Ok(_) => json!({ "ok": true, "opened": url }),
        Err(e) => json!({ "ok": false, "error": format!("failed to open URL: {e}") }),
    }
}

fn get_system_info() -> Value {
    let os = std::process::Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    json!({ "ok": true, "platform": os, "hostname": std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()) })
}

fn valid_volume(value: i64) -> bool { (0..=100).contains(&value) }

fn set_volume(args: Value) -> Value {
    let Some(value) = args.get("percent").and_then(Value::as_i64) else {
        return json!({ "ok": false, "error": "missing 'percent' argument" });
    };
    if !valid_volume(value) {
        return json!({ "ok": false, "error": "volume must be between 0 and 100" });
    }
    match std::process::Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{value}%")])
        .status()
    {
        Ok(status) if status.success() => json!({ "ok": true, "percent": value }),
        Ok(status) => json!({ "ok": false, "error": format!("pactl exited with {status}") }),
        Err(e) => json!({ "ok": false, "error": format!("failed to set volume: {e}") }),
    }
}

fn screenshot() -> Value {
    let path = format!("/tmp/karen-screenshot-{}.png", chrono::Utc::now().timestamp_millis());
    let result = std::process::Command::new("gnome-screenshot").args(["-f", &path]).status();
    match result {
        Ok(status) if status.success() => json!({ "ok": true, "path": path }),
        Ok(status) => json!({ "ok": false, "error": format!("screenshot command exited with {status}") }),
        Err(e) => json!({ "ok": false, "error": format!("screenshot unavailable: {e}") }),
    }
}

fn valid_key(key: &str) -> bool {
    matches!(key, "Return" | "Escape" | "Tab" | "space" | "Up" | "Down" | "Left" | "Right" | "ctrl+c" | "ctrl+v" | "ctrl+x" | "ctrl+s")
}

fn press_key(args: Value) -> Value {
    let Some(key) = args.get("key").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "missing 'key' argument" });
    };
    if !valid_key(key) {
        return json!({ "ok": false, "error": "key is not in Karen's safe key allowlist" });
    }
    match std::process::Command::new("xdotool").args(["key", key]).status() {
        Ok(status) if status.success() => json!({ "ok": true, "key": key }),
        Ok(status) => json!({ "ok": false, "error": format!("xdotool exited with {status}") }),
        Err(e) => json!({ "ok": false, "error": format!("keyboard control unavailable: {e}") }),
    }
}

fn type_text(args: Value) -> Value {
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "missing 'text' argument" });
    };
    if text.is_empty() || text.len() > 4000 || text.chars().any(|c| c == '\n' || c == '\r') {
        return json!({ "ok": false, "error": "text must be 1-4000 characters without newlines" });
    }
    match std::process::Command::new("xdotool").args(["type", "--", text]).status() {
        Ok(status) if status.success() => json!({ "ok": true, "characters": text.chars().count() }),
        Ok(status) => json!({ "ok": false, "error": format!("xdotool exited with {status}") }),
        Err(e) => json!({ "ok": false, "error": format!("keyboard control unavailable: {e}") }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRisk {
    AutoRun,
    ConfirmRequired,
}

impl ToolRisk {
    fn as_str(self) -> &'static str {
        match self {
            Self::AutoRun => "auto_run",
            Self::ConfirmRequired => "confirm_required",
        }
    }
}

pub fn risk_for(name: &str) -> ToolRisk {
    // Namespaced MCP tools: read-only observations auto-run, everything
    // else confirms through the normal gate. See mcp.rs for the policy.
    if let Some((server, tool)) = crate::mcp::split_qualified(name) {
        return if crate::mcp::is_read_only_action(&server, &tool) {
            ToolRisk::AutoRun
        } else {
            ToolRisk::ConfirmRequired
        };
    }
    match name {
        // These tools only inspect local state or update Karen's own memory.
        "list_directory" | "read_file" | "list_dev_logs" | "read_dev_logs"
        | "get_current_time" | "search_events" | "save_memory" | "create_task" | "get_task" | "list_tasks" => ToolRisk::AutoRun,
        // Launching an application changes desktop state and will become
        // confirmation-aware when the UI confirmation flow is added.
        "open_app" => ToolRisk::ConfirmRequired,
        "open_url" => ToolRisk::AutoRun,
        "get_system_info" | "take_screenshot" | "screen_ocr" => ToolRisk::AutoRun,
        "list_processes" => ToolRisk::AutoRun,
        "set_volume" | "type_text" | "press_key" => ToolRisk::ConfirmRequired,
        _ => ToolRisk::ConfirmRequired,
    }
}

pub fn all_tool_defs() -> Vec<Value> {
    let mut definitions = vec![
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
                "name": "open_url",
                "description": "Open a safe HTTP or HTTPS URL in the default browser.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "An http:// or https:// URL." }
                    },
                    "required": ["url"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "get_system_info",
                "description": "Report basic operating system and hostname information.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "take_screenshot",
                "description": "Capture the current desktop to a temporary PNG file.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "screen_ocr",
                "description": "Capture the current desktop and extract visible English text with OCR.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_processes",
                "description": "List the top currently running processes sorted by CPU usage.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "set_volume",
                "description": "Set the system output volume from 0 to 100 percent.",
                "parameters": { "type": "object", "properties": { "percent": { "type": "integer" } }, "required": ["percent"] }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "type_text",
                "description": "Type text into the currently focused application.",
                "parameters": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "press_key",
                "description": "Press one key or safe keyboard shortcut.",
                "parameters": { "type": "object", "properties": { "key": { "type": "string" } }, "required": ["key"] }
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
        json!({"type":"function","function":{"name":"create_task","description":"Create a persistent background task record for work that cannot finish in the current voice turn.","parameters":{"type":"object","properties":{"description":{"type":"string"}},"required":["description"]}}}),
        json!({"type":"function","function":{"name":"get_task","description":"Retrieve the status and result of a background task by ID.","parameters":{"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"]}}}),
        json!({"type":"function","function":{"name":"list_tasks","description":"List recent background tasks and their statuses.","parameters":{"type":"object","properties":{"limit":{"type":"integer"}}}}}),
    ];

    // Keep risk metadata beside the canonical tool definitions so the model,
    // UI, and executor can share one registry as the action surface grows.
    // MCP tools are appended first so they receive x-risk too.
    definitions.extend(crate::mcp::tool_defs());
    for definition in &mut definitions {
        if let Some(function) = definition.get_mut("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                function["x-risk"] = json!(risk_for(name).as_str());
            }
        }
    }

    definitions
}

use super::browse_files;
mod system;
mod memory;
mod tasks;

pub async fn dispatch(name: &str, args_json: &str, app: &tauri::AppHandle) -> Value {
    dispatch_inner(name, args_json, app, false).await
}

async fn dispatch_inner(name: &str, args_json: &str, app: &tauri::AppHandle, confirmed: bool) -> Value {
    if risk_for(name) == ToolRisk::ConfirmRequired && !confirmed {
        if let Ok(mut pending) = pending_tool().lock() {
            *pending = Some((name.to_string(), args_json.to_string()));
        }
        {
            use tauri::Manager;
            let memory = app.state::<crate::memory::MemoryEngine>();
            let db = memory.db.lock().await;
            let description = format!("Tool: {name} | Status: confirmation_required | Args: {args_json}");
            let _ = db.record_event("tool_confirmation_requested", &description, Some("brain::tools"));
        }
        let _ = app.emit("karen-pipeline", json!({
            "stage": "confirmation-required",
            "detail": format!("Approve action: {name}")
        }));
        return json!({
            "ok": false,
            "confirmation_required": true,
            "tool": name,
            "message": format!("Confirmation required before running {name}.")
        });
    }

    let parsed: Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": format!("bad tool arguments: {e}") }),
    };

    let start_time = std::time::Instant::now();

    let result = match name {
        "open_app" => open_app::run(parsed.clone()).await,
        "open_url" => system::open_url(parsed.clone()),
        "get_system_info" => system::get_system_info(),
        "list_processes" => system::list_processes(),
        "take_screenshot" => system::screenshot(),
        "screen_ocr" => system::screen_ocr(),
        "set_volume" => system::set_volume(parsed.clone()),
        "type_text" => system::type_text(parsed.clone()),
        "press_key" => system::press_key(parsed.clone()),
        "list_dev_logs" => devlogs::list(parsed.clone()).await,
        "read_dev_logs" => devlogs::read(parsed.clone()).await,
        "list_directory" => browse_files::list_directory(parsed.clone()).await,
        "read_file" => browse_files::read_file(parsed.clone()).await,
        "save_memory" => memory::save_memory(parsed.clone(), app).await,
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
        "search_events" => memory::search_events(parsed.clone(), app).await,
        "create_task" => tasks::create(parsed.clone(), app).await,
        "get_task" => tasks::get(parsed.clone(), app).await,
        "list_tasks" => tasks::list(parsed.clone(), app).await,
        other if other.starts_with("mcp__") => crate::mcp::call_tool(other, parsed.clone()).await,
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

pub async fn confirm_pending(app: &tauri::AppHandle) -> Value {
    let pending = pending_tool().lock().ok().and_then(|mut value| value.take());
    match pending {
        Some((name, args)) => dispatch_inner(&name, &args, app, true).await,
        None => json!({ "ok": false, "error": "no pending action to confirm" }),
    }
}

pub fn cancel_pending() -> Value {
    let had_pending = pending_tool().lock().ok().and_then(|mut value| value.take()).is_some();
    if had_pending {
        json!({ "ok": true, "cancelled": true })
    } else {
        json!({ "ok": false, "error": "no pending action to cancel" })
    }
}

#[cfg(test)]
mod tests {
    use super::{all_tool_defs, risk_for, valid_key, valid_volume, ToolRisk};
    use std::collections::HashSet;

    #[test]
    fn registry_has_unique_names_and_risk_metadata() {
        let definitions = all_tool_defs();
        let names: HashSet<&str> = definitions
            .iter()
            .filter_map(|definition| definition["function"]["name"].as_str())
            .collect();

        assert_eq!(names.len(), definitions.len());
        for definition in definitions {
            let function = &definition["function"];
            assert!(function["x-risk"].is_string());
        }
    }

    #[test]
    fn desktop_mutation_is_marked_for_confirmation() {
        assert_eq!(risk_for("open_app"), ToolRisk::ConfirmRequired);
        assert_eq!(risk_for("read_file"), ToolRisk::AutoRun);
        assert_eq!(risk_for("unknown"), ToolRisk::ConfirmRequired);
    }

    #[test]
    fn rejects_unsafe_urls() {
        assert!(super::valid_url("https://example.com"));
        assert!(!super::valid_url("file:///etc/passwd"));
        assert!(!super::valid_url("https://example.com;whoami"));
    }

    #[test]
    fn validates_desktop_action_arguments() {
        assert!(valid_volume(0));
        assert!(valid_volume(100));
        assert!(!valid_volume(101));
        assert!(valid_key("ctrl+s"));
        assert!(!valid_key("rm -rf /"));
    }
}
