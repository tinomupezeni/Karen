//! MCP client: curated external tool servers for Friday.
//!
//! Friday's native tools cannot drive a real browser or do web research, and
//! the Wayland desktop breaks `xdotool`-style Oscraping. Instead of
//! hand-rolling a browser stack, Friday embeds a Model Context Protocol
//! client (the official `rmcp` SDK) and borrows whole capabilities from
//! maintained MCP servers -- starting with Playwright for browser operate,
//! type-in-page, research, and read-back.
//!
//! Design rules:
//! - Curated servers only: `FRIDAY_MCP_SERVERS_JSON` (or the Playwright
//!   default when `FRIDAY_MCP_ENABLED=true`). This is not an open registry.
//! - Least privilege: Playwright runs `--isolated` (fresh browser profile,
//!   no access to the user's logged-in Chrome) and headless.
//! - `browser_evaluate` (arbitrary JS) is never advertised and refused if
//!   called -- too much power for a voice assistant's tool loop.
//! - MCP results are truncated before entering LLM context (snapshots can
//!   be enormous).
//! - Tool names are namespaced `mcp__<server>__<tool>` so they can never
//!   collide with native tools. Risk policy lives in
//!   `brain::tools::risk_for`, which consults `is_read_only_action` here.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, Tool as RmcpTool};
use rmcp::service::{Peer, RoleClient, ServiceExt};
use rmcp::transport::TokioChildProcess;

const TOOL_PREFIX: &str = "mcp__";
/// Cap MCP results before they enter LLM context (browser snapshots are big).
const MAX_RESULT_CHARS: usize = 12_000;
/// Never exposed to the model, even on confirmation: arbitrary JS execution.
const BLOCKED_TOOLS: &[&str] = &["browser_evaluate"];
/// Playwright tools that only observe (safe to auto-run, like open_url).
const READ_ONLY_PLAYWRIGHT_TOOLS: &[&str] = &[
    "browser_snapshot",
    "browser_take_screenshot",
    "browser_console_messages",
    "browser_network_requests",
    "browser_tab_list",
    "browser_wait_for",
];

#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug)]
struct CachedTool {
    qualified: String,
    server: String,
    tool: String,
    description: String,
    parameters: serde_json::Value,
}

static TOOL_CACHE: OnceLock<RwLock<Vec<CachedTool>>> = OnceLock::new();
static PEERS: OnceLock<Mutex<HashMap<String, Peer<RoleClient>>>> = OnceLock::new();

fn tool_cache() -> &'static RwLock<Vec<CachedTool>> {
    TOOL_CACHE.get_or_init(|| RwLock::new(Vec::new()))
}

fn peers() -> &'static Mutex<HashMap<String, Peer<RoleClient>>> {
    PEERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn is_enabled() -> bool {
    std::env::var("FRIDAY_MCP_ENABLED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "on"))
        .unwrap_or(false)
}

fn default_playwright() -> McpServerConfig {
    McpServerConfig {
        name: "playwright".into(),
        command: "npx".into(),
        // --isolated: fresh browser profile, no user logins. --browser chrome:
        // reuse the installed Chrome binary (no multi-hundred-MB download).
        args: vec![
            "-y".into(),
            "@playwright/mcp@latest".into(),
            "--isolated".into(),
            "--headless".into(),
            "--browser".into(),
            "chrome".into(),
        ],
    }
}

/// Configured servers: explicit JSON list wins, else the Playwright default
/// when the master switch is on, else none.
pub fn server_configs() -> Vec<McpServerConfig> {
    if let Ok(json) = std::env::var("FRIDAY_MCP_SERVERS_JSON") {
        let parsed: Result<Vec<serde_json::Value>, _> = serde_json::from_str(&json);
        match parsed {
            Ok(list) => list
                .iter()
                .filter_map(|entry| {
                    Some(McpServerConfig {
                        name: entry.get("name")?.as_str()?.to_string(),
                        command: entry.get("command")?.as_str()?.to_string(),
                        args: entry
                            .get("args")?
                            .as_array()?
                            .iter()
                            .filter_map(|a| a.as_str().map(str::to_string))
                            .collect(),
                    })
                })
                .collect(),
            Err(e) => {
                log::warn!("[mcp] ignoring invalid FRIDAY_MCP_SERVERS_JSON: {e}");
                Vec::new()
            }
        }
    } else if is_enabled() {
        vec![default_playwright()]
    } else {
        Vec::new()
    }
}

/// Split `mcp__<server>__<tool>` into (server, tool).
pub fn split_qualified(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(TOOL_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() || tool.contains("__") {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// Read-only (auto-runnable) MCP actions. Everything else confirms first;
/// blocked tools are refused outright in `call_tool`.
pub fn is_read_only_action(server: &str, tool: &str) -> bool {
    server == "playwright" && READ_ONLY_PLAYWRIGHT_TOOLS.contains(&tool)
}

fn prune_server(name: &str) {
    peers().lock().unwrap().remove(name);
    if let Ok(mut cache) = tool_cache().write() {
        cache.retain(|t| t.server != name);
    }
}

/// Translate one MCP tool into an OpenAI-style function definition.
pub(crate) fn translate_tool(server: &str, tool: &RmcpTool) -> Option<CachedTool> {
    let name = tool.name.to_string();
    if BLOCKED_TOOLS.contains(&name.as_str()) {
        return None;
    }
    let parameters = serde_json::Value::Object(tool.input_schema.as_ref().clone());
    Some(CachedTool {
        qualified: format!("{TOOL_PREFIX}{server}__{name}"),
        server: server.to_string(),
        tool: name,
        description: format!(
            "{} (via MCP server '{}')",
            tool.description.as_deref().unwrap_or("No description."),
            server
        ),
        parameters,
    })
}

/// Snapshot of currently advertised MCP tools as LLM function definitions.
pub fn tool_defs() -> Vec<serde_json::Value> {
    tool_cache()
        .read()
        .map(|cache| {
            cache
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.qualified,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn connect_one(
    config: &McpServerConfig,
) -> Result<(Peer<RoleClient>, Vec<RmcpTool>), String> {
    let mut cmd = tokio::process::Command::new(&config.command);
    cmd.args(&config.args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let transport = TokioChildProcess::new(cmd)
        .map_err(|e| format!("MCP '{}': spawn failed: {e}", config.name))?;
    let running = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .map_err(|_| format!("MCP '{}': handshake timed out", config.name))?
        .map_err(|e| format!("MCP '{}': handshake failed: {e}", config.name))?;
    let peer = running.peer().clone();
    let tools = tokio::time::timeout(Duration::from_secs(30), peer.list_tools(Default::default()))
        .await
        .map_err(|_| format!("MCP '{}': tools/list timed out", config.name))?
        .map_err(|e| format!("MCP '{}': tools/list failed: {e}", config.name))?;
    Ok((peer, tools.tools))
}

/// Connect every configured server missing from the registry. Idempotent.
pub async fn ensure_started() {
    for config in server_configs() {
        if peers().lock().unwrap().contains_key(&config.name) {
            continue;
        }
        match connect_one(&config).await {
            Ok((peer, tools)) => {
                let kept: Vec<CachedTool> = tools
                    .iter()
                    .filter_map(|t| translate_tool(&config.name, t))
                    .collect();
                log::info!(
                    "[mcp] '{}': {} tools ({} advertised)",
                    config.name,
                    tools.len(),
                    kept.len()
                );
                peers().lock().unwrap().insert(config.name.clone(), peer);
                if let Ok(mut cache) = tool_cache().write() {
                    cache.retain(|t| t.server != config.name);
                    cache.extend(kept);
                }
            }
            Err(e) => log::warn!("[mcp] {e}; will retry"),
        }
    }
}

fn truncate_result(text: String) -> String {
    if text.len() <= MAX_RESULT_CHARS {
        return text;
    }
    format!(
        "{}...[truncated, {} chars total]",
        &text[..MAX_RESULT_CHARS],
        text.len()
    )
}

/// Call an MCP tool by qualified name. Returns a Friday-style result value.
pub async fn call_tool(qualified: &str, args: serde_json::Value) -> serde_json::Value {
    let Some((server, tool)) = split_qualified(qualified) else {
        return serde_json::json!({ "ok": false, "error": format!("not an MCP tool: {qualified}") });
    };
    if BLOCKED_TOOLS.contains(&tool.as_str()) {
        return serde_json::json!({ "ok": false, "error": format!("MCP tool '{tool}' is disabled") });
    }
    let args_map = match args {
        serde_json::Value::Object(map) => map,
        _ => {
            return serde_json::json!({ "ok": false, "error": "MCP tool arguments must be an object" });
        }
    };
    let peer = peers().lock().unwrap().get(&server).cloned();
    let Some(peer) = peer else {
        return serde_json::json!({ "ok": false, "error": format!("MCP server '{server}' is not connected") });
    };
    let started = std::time::Instant::now();
    let mut param = CallToolRequestParams::default();
    param.name = tool.clone().into();
    param.arguments = Some(args_map);
    let result = tokio::time::timeout(Duration::from_secs(90), peer.call_tool(param)).await;
    log::info!("[mcp] {server}/{tool} finished in {:?}", started.elapsed());
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => {
            prune_server(&server);
            return serde_json::json!({ "ok": false, "error": format!("MCP {server}/{tool} failed: {e}") });
        }
        Err(_) => {
            return serde_json::json!({ "ok": false, "error": format!("MCP {server}/{tool} timed out") });
        }
    };
    let mut texts = Vec::new();
    let mut non_text = 0;
    for content in &result.content {
        // Content variants differ by SDK version; lean on its JSON shape.
        let value = serde_json::to_value(content).unwrap_or(serde_json::Value::Null);
        if value.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                texts.push(text.to_string());
            }
        } else {
            non_text += 1;
        }
    }
    if non_text > 0 {
        texts.push(format!("[{non_text} non-text content part(s) omitted]"));
    }
    let is_error = result.is_error.unwrap_or(false);
    serde_json::json!({
        "ok": !is_error,
        "server": server,
        "tool": tool,
        "provenance": format!("mcp://{server}/{tool}"),
        "untrusted_content": true,
        "result": truncate_result(texts.join("\n")),
    })
}

/// Supervisor: keep configured servers connected, forever. Spawn once.
pub async fn supervise() {
    loop {
        ensure_started().await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{is_read_only_action, split_qualified, translate_tool};
    use rmcp::model::Tool as RmcpTool;
    use std::sync::Arc;

    fn rmcp_tool(name: &'static str) -> RmcpTool {
        RmcpTool::new(
            name,
            "Does a thing.",
            Arc::new(serde_json::Map::from_iter([(
                "type".to_string(),
                serde_json::Value::String("object".into()),
            )])),
        )
    }

    #[test]
    fn splits_qualified_names() {
        assert_eq!(
            split_qualified("mcp__playwright__browser_navigate"),
            Some(("playwright".into(), "browser_navigate".into()))
        );
        assert_eq!(split_qualified("open_app"), None);
        assert_eq!(split_qualified("mcp__playwright"), None);
        assert_eq!(split_qualified("mcp____browser_x"), None);
    }

    #[test]
    fn translates_tools_with_namespace() {
        let cached = translate_tool("playwright", &rmcp_tool("browser_navigate")).unwrap();
        assert_eq!(cached.qualified, "mcp__playwright__browser_navigate");
        assert!(cached.description.contains("playwright"));
        assert_eq!(cached.parameters["type"], "object");
    }

    #[test]
    fn blocks_evaluate_from_advertisement() {
        assert!(translate_tool("playwright", &rmcp_tool("browser_evaluate")).is_none());
    }

    #[test]
    fn read_only_policy() {
        assert!(is_read_only_action("playwright", "browser_snapshot"));
        assert!(!is_read_only_action("playwright", "browser_navigate"));
        assert!(!is_read_only_action("playwright", "browser_click"));
        assert!(!is_read_only_action("playwright", "browser_evaluate"));
        assert!(!is_read_only_action("other", "browser_snapshot"));
    }
}
