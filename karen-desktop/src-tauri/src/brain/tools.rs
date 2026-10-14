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
    let unsafe_char = url
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | ';' | '&' | '|' | '`' | '$'));
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

fn valid_volume(value: i64) -> bool {
    (0..=100).contains(&value)
}

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
    let path = format!(
        "/tmp/karen-screenshot-{}.png",
        chrono::Utc::now().timestamp_millis()
    );
    let result = std::process::Command::new("gnome-screenshot")
        .args(["-f", &path])
        .status();
    match result {
        Ok(status) if status.success() => json!({ "ok": true, "path": path }),
        Ok(status) => {
            json!({ "ok": false, "error": format!("screenshot command exited with {status}") })
        }
        Err(e) => json!({ "ok": false, "error": format!("screenshot unavailable: {e}") }),
    }
}

fn valid_key(key: &str) -> bool {
    matches!(
        key,
        "Return"
            | "Escape"
            | "Tab"
            | "space"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "ctrl+c"
            | "ctrl+v"
            | "ctrl+x"
            | "ctrl+s"
    )
}

fn press_key(args: Value) -> Value {
    let Some(key) = args.get("key").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "missing 'key' argument" });
    };
    if !valid_key(key) {
        return json!({ "ok": false, "error": "key is not in Karen's safe key allowlist" });
    }
    match std::process::Command::new("xdotool")
        .args(["key", key])
        .status()
    {
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
    match std::process::Command::new("xdotool")
        .args(["type", "--", text])
        .status()
    {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityLifecycle {
    Stateless,
    ProcessScoped,
    ExternalService,
}

impl CapabilityLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stateless => "stateless",
            Self::ProcessScoped => "process_scoped",
            Self::ExternalService => "external_service",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilitySpec {
    pub source: &'static str,
    pub lifecycle: CapabilityLifecycle,
    pub risk: ToolRisk,
    pub confirmation_preview: &'static str,
    pub cancellable: bool,
    pub argument_constraints: &'static str,
    pub input_contract: &'static str,
    pub output_contract: &'static str,
    pub failure_contract: &'static str,
}

impl CapabilitySpec {
    fn as_json(self) -> Value {
        json!({
            "source": self.source,
            "lifecycle": self.lifecycle.as_str(),
            "risk": self.risk.as_str(),
            "confirmation_preview": self.confirmation_preview,
            "cancellable": self.cancellable,
            "argument_constraints": self.argument_constraints,
            "input_contract": self.input_contract,
            "output_contract": self.output_contract,
            "failure_contract": self.failure_contract,
        })
    }
}

const NATIVE_READ_CAPABILITY: CapabilitySpec = CapabilitySpec {
    source: "native",
    lifecycle: CapabilityLifecycle::Stateless,
    risk: ToolRisk::AutoRun,
    confirmation_preview: "No confirmation required for read-only local inspection",
    cancellable: false,
    argument_constraints: "Tool-specific bounded JSON fields",
    input_contract: "JSON object validated by the tool before execution",
    output_contract: "JSON object with ok=true and tool-specific result fields",
    failure_contract: "Return ok=false with an explicit error; do not fabricate success",
};

const NATIVE_MUTATION_CAPABILITY: CapabilitySpec = CapabilitySpec {
    source: "native",
    lifecycle: CapabilityLifecycle::Stateless,
    risk: ToolRisk::ConfirmRequired,
    confirmation_preview: "Show the tool name and validated arguments before execution",
    cancellable: true,
    argument_constraints: "Tool-specific allowlists and bounded values",
    input_contract: "JSON object validated before any desktop mutation",
    output_contract: "JSON object with ok=true only after the OS reports success",
    failure_contract: "Require confirmation first, then return explicit execution failure",
};

const MEMORY_CAPABILITY: CapabilitySpec = CapabilitySpec {
    source: "native",
    lifecycle: CapabilityLifecycle::ProcessScoped,
    risk: ToolRisk::AutoRun,
    confirmation_preview: "No confirmation required for Karen-owned persistence",
    cancellable: false,
    argument_constraints: "Bounded memory and event fields",
    input_contract: "JSON object with bounded memory or event fields",
    output_contract: "JSON object reflecting persisted SQLite state",
    failure_contract: "Return ok=false when persistence or query fails",
};

const TASK_CAPABILITY: CapabilitySpec = CapabilitySpec {
    source: "native",
    lifecycle: CapabilityLifecycle::ProcessScoped,
    risk: ToolRisk::AutoRun,
    confirmation_preview: "No confirmation required for Karen-owned task state",
    cancellable: false,
    argument_constraints: "Bounded task identifiers and descriptions",
    input_contract: "JSON object with bounded task identifiers or descriptions",
    output_contract: "JSON object reflecting persisted task state",
    failure_contract: "Return ok=false when task persistence or lookup fails",
};

const MCP_CAPABILITY: CapabilitySpec = CapabilitySpec {
    source: "mcp",
    lifecycle: CapabilityLifecycle::ExternalService,
    risk: ToolRisk::ConfirmRequired,
    confirmation_preview:
        "Show the external server, tool, and validated arguments before execution",
    cancellable: true,
    argument_constraints: "Must match the external MCP input schema",
    input_contract: "JSON object matching the external MCP server schema",
    output_contract: "JSON object containing server, tool, and bounded result data",
    failure_contract: "Return ok=false with the server/tool failure; prune broken peers",
};

/// Canonical capability metadata for every executable tool.
///
/// Tool schemas remain next to their descriptions in `all_tool_defs_with`, but
/// execution policy and lifecycle contracts live here so the model, dispatcher,
/// UI, and audit trail cannot silently disagree as the registry grows.
pub fn capability_for(name: &str) -> CapabilitySpec {
    if let Some((server, tool)) = crate::mcp::split_qualified(name) {
        let mut capability = MCP_CAPABILITY;
        capability.risk = if crate::mcp::is_read_only_action(&server, &tool) {
            ToolRisk::AutoRun
        } else {
            ToolRisk::ConfirmRequired
        };
        return capability;
    }

    match name {
        "open_app" | "set_volume" | "type_text" | "press_key" => NATIVE_MUTATION_CAPABILITY,
        "save_memory" | "search_events" => MEMORY_CAPABILITY,
        "create_task" | "get_task" | "list_tasks" => TASK_CAPABILITY,
        "open_url" | "get_system_info" | "list_processes" | "take_screenshot" | "screen_ocr"
        | "list_directory" | "read_file" | "list_dev_logs" | "read_dev_logs"
        | "get_current_time" => NATIVE_READ_CAPABILITY,
        _ => CapabilitySpec {
            source: "unknown",
            lifecycle: CapabilityLifecycle::Stateless,
            risk: ToolRisk::ConfirmRequired,
            confirmation_preview: "Unknown capabilities cannot be confirmed",
            cancellable: false,
            argument_constraints: "No arguments accepted",
            input_contract: "Unknown tool arguments are not accepted",
            output_contract: "No successful output contract exists",
            failure_contract: "Reject explicitly as an unknown capability",
        },
    }
}

pub fn risk_for(name: &str) -> ToolRisk {
    capability_for(name).risk
}

pub fn all_tool_defs() -> Vec<Value> {
    all_tool_defs_with(true)
}

/// `include_mcp` gates the MCP (Playwright browser) tool definitions out of
/// the registry sent to the model. Those definitions are large (24 tools'
/// worth of JSON schema) and get re-sent on every single turn regardless of
/// whether that turn could ever use a browser -- a meaningful, recurring
/// token cost on every plain conversational exchange. Native tools are
/// small and always needed, so they're never gated.
pub fn all_tool_defs_with(include_mcp: bool) -> Vec<Value> {
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
    if include_mcp {
        definitions.extend(crate::mcp::tool_defs());
    }
    for definition in &mut definitions {
        if let Some(function) = definition.get_mut("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                let capability = capability_for(name);
                function["x-risk"] = json!(capability.risk.as_str());
                function["x-capability"] = capability.as_json();
            }
        }
    }

    definitions
}

use super::browse_files;
mod memory;
mod system;
mod tasks;

pub async fn dispatch(name: &str, args_json: &str, app: &tauri::AppHandle) -> Value {
    dispatch_inner(name, args_json, app, false).await
}

async fn dispatch_inner(
    name: &str,
    args_json: &str,
    app: &tauri::AppHandle,
    confirmed: bool,
) -> Value {
    let capability = capability_for(name);
    if capability.source == "unknown" {
        return json!({ "ok": false, "error": format!("unknown capability: {name}") });
    }
    if capability.risk == ToolRisk::ConfirmRequired && !confirmed {
        if let Ok(mut pending) = pending_tool().lock() {
            *pending = Some((name.to_string(), args_json.to_string()));
        }
        {
            use tauri::Manager;
            let memory = app.state::<crate::memory::MemoryEngine>();
            let db = memory.db.lock().await;
            let description = format!(
                "Capability: {name} | Source: {} | Lifecycle: {} | Preview: {} | Cancellable: {} | Constraints: {} | Status: confirmation_required | Args: {args_json}",
                capability.source,
                capability.lifecycle.as_str(),
                capability.confirmation_preview,
                capability.cancellable,
                capability.argument_constraints
            );
            let _ = db.record_event(
                "tool_confirmation_requested",
                &description,
                Some("brain::tools"),
            );
        }
        let _ = app.emit(
            "karen-pipeline",
            json!({
                "stage": "confirmation-required",
                "detail": format!(
                    "Approve action: {name} | {} | Args: {args_json}",
                    capability.confirmation_preview
                )
            }),
        );
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
        }
        "search_events" => memory::search_events(parsed.clone(), app).await,
        "create_task" => tasks::create(parsed.clone(), app).await,
        "get_task" => tasks::get(parsed.clone(), app).await,
        "list_tasks" => tasks::list(parsed.clone(), app).await,
        other if other.starts_with("mcp__") => crate::mcp::call_tool(other, parsed.clone()).await,
        other => json!({ "ok": false, "error": format!("unknown tool: {other}") }),
    };

    let duration = start_time.elapsed().as_millis();
    let status = if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(true) {
        "success"
    } else {
        "failure"
    };

    if status == "success" {
        if let Some(path) = result.get("path").and_then(Value::as_str) {
            use tauri::Manager;
            let memory = app.state::<crate::memory::MemoryEngine>();
            let db = memory.db.lock().await;
            let kind = if name.contains("ocr") {
                "ocr_capture"
            } else {
                "screenshot"
            };
            if let Err(error) = db.record_artifact(kind, path, None, Some(&format!("tool={name}")))
            {
                log::warn!("could not record artifact from {name}: {error}");
            }
        }
    }

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

        let desc = format!(
            "Capability: {name} | Source: {} | Lifecycle: {} | Args: {args_json} | Status: {status} | Duration: {duration}ms | Res: {result_preview}",
            capability.source,
            capability.lifecycle.as_str()
        );
        let _ = db.record_event("tool_execution", &desc, Some("brain::tools"));
    }

    result
}

pub async fn confirm_pending(app: &tauri::AppHandle) -> Value {
    let pending = pending_tool()
        .lock()
        .ok()
        .and_then(|mut value| value.take());
    match pending {
        Some((name, args)) => dispatch_inner(&name, &args, app, true).await,
        None => json!({ "ok": false, "error": "no pending action to confirm" }),
    }
}

pub fn cancel_pending() -> Value {
    let had_pending = pending_tool()
        .lock()
        .ok()
        .and_then(|mut value| value.take())
        .is_some();
    if had_pending {
        json!({ "ok": true, "cancelled": true })
    } else {
        json!({ "ok": false, "error": "no pending action to cancel" })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        all_tool_defs, capability_for, risk_for, valid_key, valid_volume, CapabilityLifecycle,
        ToolRisk,
    };
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
            assert!(function["x-capability"]["source"].is_string());
            assert!(function["x-capability"]["lifecycle"].is_string());
            assert!(function["x-capability"]["confirmation_preview"].is_string());
            assert!(function["x-capability"]["cancellable"].is_boolean());
            assert!(function["x-capability"]["argument_constraints"].is_string());
            assert!(function["x-capability"]["input_contract"].is_string());
            assert!(function["x-capability"]["output_contract"].is_string());
            assert!(function["x-capability"]["failure_contract"].is_string());
        }
    }

    #[test]
    fn desktop_mutation_is_marked_for_confirmation() {
        assert_eq!(risk_for("open_app"), ToolRisk::ConfirmRequired);
        assert_eq!(risk_for("read_file"), ToolRisk::AutoRun);
        assert_eq!(risk_for("unknown"), ToolRisk::ConfirmRequired);
    }

    #[test]
    fn capability_registry_describes_native_and_mcp_lifecycles() {
        let native = capability_for("read_file");
        assert_eq!(native.source, "native");
        assert_eq!(native.lifecycle, CapabilityLifecycle::Stateless);
        assert_eq!(native.risk, ToolRisk::AutoRun);

        let mcp = capability_for("mcp__playwright__browser_click");
        assert_eq!(mcp.source, "mcp");
        assert_eq!(mcp.lifecycle, CapabilityLifecycle::ExternalService);
        assert_eq!(mcp.risk, ToolRisk::ConfirmRequired);
    }

    #[test]
    fn unknown_capabilities_have_rejecting_contracts() {
        let unknown = capability_for("not_registered");
        assert_eq!(unknown.source, "unknown");
        assert!(unknown.failure_contract.contains("Reject"));
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
