mod browse_files;
mod devlogs;
mod open_app;
mod router;
pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager};

const GROQ_CHAT_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const DEFAULT_OLLAMA_URL: &str = "http://10.50.101.11:11434/v1/chat/completions";
const DEFAULT_MODEL: &str = "openai/gpt-oss-120b";
const DEFAULT_CONTEXT_MESSAGES: usize = 10;
const MAX_CONTEXT_MESSAGE_CHARS: usize = 4000;
const MAX_TTS_CHUNK_CHARS: usize = 160;
const DEFAULT_LATENCY_BUDGET_MS: u64 = 8_000;

fn latency_budget_ms() -> u64 {
    std::env::var("KAREN_LATENCY_BUDGET_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (500..=120_000).contains(value))
        .unwrap_or(DEFAULT_LATENCY_BUDGET_MS)
}

fn context_message_limit() -> usize {
    std::env::var("KAREN_CONTEXT_MESSAGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (2..=40).contains(value))
        .unwrap_or(DEFAULT_CONTEXT_MESSAGES)
}

fn compact_context_message(content: &str) -> String {
    if content.chars().count() <= MAX_CONTEXT_MESSAGE_CHARS {
        return content.to_string();
    }
    let prefix: String = content.chars().take(MAX_CONTEXT_MESSAGE_CHARS).collect();
    format!("{prefix}… [earlier detail compacted]")
}

fn needs_semantic_memory(transcript: &str) -> bool {
    let text = transcript.to_ascii_lowercase();
    [
        "remember",
        "memory",
        "memories",
        "recall",
        "forgot",
        "preference",
        "yesterday",
        "last time",
        "previous",
        "we worked",
        "we discussed",
        "what did we",
        "my project",
        "my setup",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// MCP (Playwright browser) tool definitions are ~24 tools' worth of JSON
/// schema, resent on every turn that includes them. Background/research
/// tasks always need them (that's their whole purpose, handled by the
/// caller passing `is_background_task: true`), but an ordinary live
/// conversational turn almost never touches a browser -- gating this out
/// for plain conversation is the single largest lever on per-turn token
/// cost, which is what actually determines how long Groq's per-minute
/// quota lasts across a session, not how many keys are configured.
fn needs_browser_tools(transcript: &str, is_background_task: bool) -> bool {
    if is_background_task {
        return true;
    }
    let text = transcript.to_ascii_lowercase();
    [
        "browse",
        "website",
        "web page",
        "webpage",
        "http://",
        "https://",
        "search the web",
        "navigate to",
        "click on the",
        "screenshot the page",
        ".com",
        ".org",
        ".dev",
        ".io",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

fn next_tts_boundary(text: &str, start: usize) -> Option<usize> {
    if let Some(offset) = text[start..].find(|c| c == '.' || c == '!' || c == '?') {
        return Some(start + offset + 1);
    }
    if text[start..].chars().count() >= MAX_TTS_CHUNK_CHARS {
        let boundary = text[start..]
            .char_indices()
            .take(MAX_TTS_CHUNK_CHARS)
            .filter(|(_, c)| c.is_whitespace() || *c == ',' || *c == ';' || *c == ':')
            .map(|(idx, _)| start + idx)
            .last();
        return boundary.map(|idx| idx + 1);
    }
    None
}

/// Explicit "do this later" phrasing, for anyone who wants to name the
/// mechanism directly rather than just asking a research-shaped question.
const EXPLICIT_BACKGROUND_PREFIXES: &[&str] = &[
    "run this in the background: ",
    "work on this in the background: ",
    "keep working on: ",
];

/// Natural research phrasing that should become a background task without
/// the user ever needing to say "in the background" -- "research X",
/// "look into X", "can you look up X" should all just work.
const RESEARCH_PREFIXES: &[&str] = &[
    "research ",
    "research into ",
    "do some research on ",
    "do some research into ",
    "look into ",
    "can you look into ",
    "could you look into ",
    "look up ",
    "can you look up ",
    "could you look up ",
    "find out about ",
    "can you find out about ",
    "dig into ",
    "investigate ",
    "can you investigate ",
];

fn background_request(text: &str) -> Option<&str> {
    let text = text.trim();
    EXPLICIT_BACKGROUND_PREFIXES
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .or_else(|| {
            let lower = text.to_ascii_lowercase();
            RESEARCH_PREFIXES.iter().find_map(|prefix| {
                lower
                    .starts_with(prefix)
                    .then(|| text[prefix.len()..].trim_start())
            })
        })
        .filter(|rest| !rest.trim().is_empty())
}

/// Background jobs (research, multi-step digging) default to the local
/// Ollama VM so a long-running job never competes with the live
/// conversation for Groq's per-minute token budget. Overridable in case
/// the local model isn't reliable enough for a particular job's tool use.
fn background_llm_provider() -> String {
    std::env::var("KAREN_BACKGROUND_LLM_PROVIDER").unwrap_or_else(|_| "ollama".into())
}

/// A short, in-character line read out loud the moment research actually
/// finishes -- not the findings themselves (those can be long and the user
/// may be mid-conversation about something else), just an invitation to
/// come back to it when ready, the way a colleague would say "I've got
/// that for you whenever."
fn research_ready_announcement(topic: &str, ok: bool) -> String {
    if ok {
        format!("I've got what I found on {topic} whenever you want to hear it.")
    } else {
        format!(
            "I hit a wall researching {topic}. Ask me about it and I'll tell you what went wrong."
        )
    }
}

async fn start_background_task(
    request: &str,
    app: &AppHandle,
    _current_gen_id: usize,
) -> Option<String> {
    let memory = app.state::<crate::memory::MemoryEngine>();
    let task_id = {
        let db = memory.db.lock().await;
        db.create_task(request).ok()?
    };
    let task_for_worker = task_id.clone();
    let worker_app = app.clone();
    let worker_request = request.to_string();
    let provider = background_llm_provider();
    let _ = app.emit(
        "karen-task",
        json!({"task_id": task_id, "status": "queued", "description": request}),
    );
    std::thread::spawn(move || {
        tauri::async_runtime::block_on(async move {
            {
                let memory = worker_app.state::<crate::memory::MemoryEngine>();
                let db = memory.db.lock().await;
                let _ = db.update_task(&task_for_worker, "running", None);
            }
            let _ = worker_app.emit("karen-task", json!({"task_id": task_for_worker, "status": "running", "description": worker_request}));

            // Background work never speaks through the live turn's generation --
            // by the time a research job finishes, the user has likely moved on
            // to other turns entirely, so this must interrupt with whatever
            // generation is current *now*, not the one captured at kickoff.
            let announce_gen_id = crate::tts::get_current_generation();
            let result = run_agentic_inner(
                &worker_request,
                &worker_app,
                None,
                announce_gen_id,
                Some(&provider),
            )
            .await;
            let (status, output, ok) = match result {
                Ok(answer) => ("completed", answer, true),
                Err(error) => ("failed", error.clone(), false),
            };
            let memory = worker_app.state::<crate::memory::MemoryEngine>();
            let db = memory.db.lock().await;
            let _ = db.update_task(&task_for_worker, status, Some(&output));
            drop(db);

            let announce_gen_id = crate::tts::get_current_generation();
            speak_now(
                research_ready_announcement(&worker_request, ok),
                None,
                announce_gen_id,
            );
            let _ = worker_app.emit("karen-task", json!({"task_id": task_for_worker, "status": status, "description": worker_request, "result": output}));
        })
    });
    Some(task_id)
}
const MAX_ITERS: usize = 5;

const PERSONA_PATH_ENV: &str = "KAREN_PERSONA_PATH";
const DEFAULT_PERSONA_PATH: &str = "/home/shadowe/Projects/Karen/Persona.md";
const FALLBACK_PERSONA: &str = "You are Friday, a voice-activated desktop assistant.";
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> Result<&'static reqwest::Client, String> {
    if let Some(client) = HTTP_CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    let _ = HTTP_CLIENT.set(client);
    Ok(HTTP_CLIENT.get().expect("HTTP client was just initialized"))
    /*
        reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))
    */
}

/// Reads fresh each call rather than cached, so editing Persona.md takes
/// effect on the next spoken command without an app restart. It's a small
/// file read against a multi-second STT+LLM round trip, so the cost is
/// negligible.
async fn load_persona() -> String {
    let path = std::env::var(PERSONA_PATH_ENV).unwrap_or_else(|_| DEFAULT_PERSONA_PATH.to_string());
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(e) => {
            log::warn!("could not read persona file at {path}, using fallback persona: {e}");
            FALLBACK_PERSONA.to_string()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl Message {
    fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn tool_result(tool_call_id: String, name: String, result: Value) -> Self {
        Self {
            role: "tool".into(),
            content: Some(result.to_string()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            name: Some(name),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ToolCallFn,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ToolCallFn {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    tools: &'a [Value],
    tool_choice: &'a str,
    stream: bool,
    max_tokens: u32,
    reasoning_effort: &'a str,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Message,
}

#[derive(Deserialize)]
struct ChatChunk {
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<ChunkFunction>,
}

#[derive(Deserialize)]
struct ChunkFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn emit_running_tool(app: &AppHandle, name: &str, gen_id: usize) {
    if crate::tts::get_current_generation() == gen_id {
        let _ = app.emit(
            "karen-pipeline",
            json!({ "stage": "running-tool", "detail": format!("Running: {name}") }),
        );
    }
}

/// The system prompt tells the model not to use markdown, but that's a
/// request, not a guarantee -- a model will still occasionally slip in
/// `**bold**`, `- bullet` lines, or a `#` heading. TTS speaks whatever
/// string it's given literally, so an unstripped `**` reads out loud as
/// "star star", which is exactly the "sounds robotic" complaint this
/// fixes. Applied right before every call to `speak_chunk` so no path can
/// forget it.
fn strip_markdown_for_speech(text: &str) -> String {
    // Turn list-style line breaks into spoken pauses before stripping
    // bullet markers, so "- First\n- Second" reads as "First. Second."
    // instead of running the words together with no pause at all.
    let mut normalized = String::with_capacity(text.len());
    for line in text.replace("\r\n", "\n").split('\n') {
        let trimmed = line.trim_start();
        let without_bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("• "))
            .unwrap_or(trimmed);
        if !normalized.is_empty() && !without_bullet.is_empty() {
            normalized.push_str(". ");
        }
        normalized.push_str(without_bullet);
    }

    // Drop emphasis/heading/code markers outright -- nobody says "asterisk
    // asterisk" or "hash" out loud, so there is no spoken equivalent to
    // preserve; the enclosed words carry the meaning on their own.
    let stripped: String = normalized
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '#' | '`'))
        .collect();

    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .", ".")
}

/// Every fast, non-streamed reply path (deterministic routing, canned
/// greetings, background-task acknowledgements) must still reach TTS the
/// same way a streamed LLM answer does -- otherwise the HUD shows text with
/// no audio, which looks identical to Friday being broken.
fn speak_now(text: String, speech_ended_at: Option<std::time::Instant>, gen_id: usize) -> String {
    let utterance_id = format!(
        "u-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    crate::tts::speak_chunk(
        strip_markdown_for_speech(&text),
        speech_ended_at,
        utterance_id,
        0,
        gen_id,
    );
    text
}

pub async fn run_agentic(
    transcript: &str,
    app: &AppHandle,
    speech_ended_at: Option<std::time::Instant>,
    current_gen_id: usize,
) -> Result<String, String> {
    run_agentic_inner(transcript, app, speech_ended_at, current_gen_id, None).await
}

/// `forced_provider` lets a caller pin this one call to a specific LLM
/// provider ("groq"/"ollama") regardless of the process-wide
/// `KAREN_LLM_PROVIDER` setting. Used by background research tasks so a
/// long-running job doesn't contend with the live conversation's Groq
/// token budget -- the setting is process-wide and mutable (see
/// ARCHITECTURE.md's known limitations), so overriding it for the live
/// turn would also wrongly affect the background job and vice versa.
pub async fn run_agentic_inner(
    transcript: &str,
    app: &AppHandle,
    speech_ended_at: Option<std::time::Instant>,
    current_gen_id: usize,
    forced_provider: Option<&str>,
) -> Result<String, String> {
    if let Some(reply) = router::fast_reply(transcript) {
        return Ok(speak_now(reply, speech_ended_at, current_gen_id));
    }
    if let Some(request) = background_request(transcript) {
        if let Some(_task_id) = start_background_task(request, app, current_gen_id).await {
            let reply = format!("On it — I'll dig into {request} in the background and let you know when I've got something.");
            return Ok(speak_now(reply, speech_ended_at, current_gen_id));
        }
        return Err("I couldn’t create a background task record.".into());
    }
    if let Some(route) = router::route(transcript) {
        match route {
            router::Route::StopPlayback => {
                // Mirrors pipeline.rs's own cancel handling: stay silent so
                // Friday doesn't talk over the "stop" the user just gave.
                crate::tts::cancel();
                return Ok("Playback stopped. I’m listening.".into());
            }
            router::Route::SystemInfo => {
                let result = tools::dispatch("get_system_info", "{}", app).await;
                let reply = format_routed_result("System status", result);
                return Ok(speak_now(reply, speech_ended_at, current_gen_id));
            }
            router::Route::SetVolume(percent) => {
                let result =
                    tools::dispatch("set_volume", &json!({"percent": percent}).to_string(), app)
                        .await;
                let reply = format_routed_result("Volume", result);
                return Ok(speak_now(reply, speech_ended_at, current_gen_id));
            }
            router::Route::OpenApp(name) => {
                let result =
                    tools::dispatch("open_app", &json!({"name": name}).to_string(), app).await;
                let reply = format_routed_result("Application", result);
                return Ok(speak_now(reply, speech_ended_at, current_gen_id));
            }
        }
    }

    let budget = std::time::Duration::from_millis(latency_budget_ms());
    let deadline = std::time::Instant::now() + budget;
    let configured_provider = forced_provider
        .map(|p| p.to_string())
        .unwrap_or_else(|| std::env::var("KAREN_LLM_PROVIDER").unwrap_or_else(|_| "groq".into()));
    let use_ollama = configured_provider.eq_ignore_ascii_case("ollama")
        || (forced_provider.is_none() && crate::groq_keys::cooldown_remaining().is_some());
    if use_ollama && configured_provider.eq_ignore_ascii_case("groq") {
        log::warn!("Groq is in a shared rate-limit cooldown; using Ollama for this turn");
    }
    let groq_keys = if use_ollama {
        Vec::new()
    } else {
        let keys = crate::groq_keys::ordered_keys();
        if keys.is_empty() {
            return Err("GROQ_API_KEY is not set".to_string());
        }
        keys
    };
    let model = if use_ollama {
        std::env::var("KAREN_OLLAMA_MODEL").unwrap_or_else(|_| "qwen2.5:3b".into())
    } else {
        std::env::var("GROQ_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
    };
    let endpoint = if use_ollama {
        std::env::var("KAREN_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.into())
    } else {
        GROQ_CHAT_URL.into()
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let persona = load_persona().await;
    let proactive_mode = std::env::var("KAREN_PROACTIVE_MODE")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "on"))
        .unwrap_or(false);

    let context_engine = app.state::<crate::context_engine::ContextEngine>();
    let ctx = context_engine.get_context();
    let query_embedding = if use_ollama && needs_semantic_memory(transcript) {
        crate::memory::semantic::ollama_embedding(transcript)
            .await
            .ok()
    } else {
        None
    };
    let (relevant_memories, recent_events) = {
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = memory.db.lock().await;

        let mut relevant_memories = String::new();
        let memories = if let Some(ref embedding) = query_embedding {
            db.search_semantic_memories(embedding, 5)
                .or_else(|_| db.search_memories(transcript))
        } else {
            db.search_memories(transcript)
        };
        if let Ok(memories) = memories {
            if !memories.is_empty() {
                relevant_memories.push_str("\n\nRelevant past memories:\n");
                for m in memories {
                    let time_str = crate::memory::temporal::format_relative_time(&m.recorded_at);
                    relevant_memories.push_str(&format!("- [{time_str}]: {}\n", m.content));
                }
            }
        }

        let mut recent_events = String::new();
        if let Ok(events) = db.get_recent_events(10) {
            if !events.is_empty() {
                recent_events.push_str("\n\nRecent activity timeline:\n");
                for e in events {
                    let time_str = crate::memory::temporal::format_relative_time(&e.recorded_at);
                    recent_events.push_str(&format!(
                        "- [{time_str}] ({}): {}\n",
                        e.event_type, e.description
                    ));
                }
            }
        }

        (relevant_memories, recent_events)
    };

    let ctx_str = format!(
        "Current inferred activity: {}. Top active processes: {:?}{}{}",
        ctx.activity_inference, ctx.current_processes, relevant_memories, recent_events
    );

    let mut messages = vec![Message::system(format!(
            "{persona}\n\n---\n\
             Operational context: today's date is {today}. {ctx_str}\n\
             You have tools available to launch \
             desktop applications, browse the local filesystem (read files and list directories), \
             and read dev-log entries from the user's engineering standards repo. Use them when \
             the request calls for it. Only answer directly with plain text once you have \
             everything you need. Despite the persona above, keep answers concise -- they may \
             be read aloud via text-to-speech or shown in a small status pill; save depth for \
             when elaboration is specifically asked for. Every reply is spoken aloud, never read \
             as text, so never use markdown, bullet points, numbered lists, or headers -- no \
             asterisks, dashes-as-bullets, or hash symbols. Say lists as a spoken sentence \
             (\"first, ... then, ... finally, ...\") instead. Proactive mode is {proactive_mode}; \
             when it is enabled, offer one relevant next step or useful observation after \
             completing the request, but never invent activity or interrupt without a clear reason."
        ))];

    let conv_id = {
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = memory.db.lock().await;

        let id = match db.get_active_conversation().unwrap_or(None) {
            Some(id) => id,
            None => db
                .create_conversation()
                .unwrap_or_else(|_| "default-conv".to_string()),
        };

        if let Ok(recent) = db.get_recent_messages(&id, context_message_limit()) {
            for msg in recent {
                let time_str = crate::memory::temporal::format_relative_time(&msg.recorded_at);
                if msg.role == "user" {
                    let content_with_time =
                        format!("[{time_str}]: {}", compact_context_message(&msg.content));
                    messages.push(Message::user(content_with_time));
                } else if msg.role == "assistant" {
                    messages.push(Message::assistant(compact_context_message(&msg.content)));
                }
            }
        }

        let _ = db.append_message(&id, "user", transcript);
        id
    };

    messages.push(Message::user(transcript.to_string()));

    let include_mcp = needs_browser_tools(transcript, forced_provider.is_some());
    let tools = tools::all_tool_defs_with(include_mcp);
    log::info!(
        "tool definitions: {} tools ({}), {} bytes serialized",
        tools.len(),
        if include_mcp {
            "incl. browser tools"
        } else {
            "browser tools excluded"
        },
        serde_json::to_vec(&tools).map(|v| v.len()).unwrap_or(0)
    );
    let client = http_client()?;

    for _ in 0..MAX_ITERS {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "request exceeded its {}ms latency budget",
                budget.as_millis()
            ));
        }
        let request = ChatRequest {
            model: &model,
            messages: &messages,
            tools: &tools,
            tool_choice: "auto",
            stream: true,
            max_tokens: 1024,
            reasoning_effort: "low",
            temperature: 0.2,
        };

        let body =
            serde_json::to_vec(&request).map_err(|e| format!("failed to encode request: {e}"))?;
        log::debug!(
            "{} request body: {} bytes",
            if use_ollama { "Ollama" } else { "Groq" },
            body.len()
        );

        // Groq keys rotate round-robin with failover on 401/403/429; Ollama
        // needs no key and always makes a single attempt.
        let attempts: Vec<Option<String>> = if use_ollama {
            vec![None]
        } else {
            groq_keys.iter().cloned().map(Some).collect()
        };
        let mut response = None;
        for (attempt_idx, key) in attempts.iter().enumerate() {
            let mut request_builder = client
                .post(&endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            if let Some(key) = key {
                request_builder = request_builder.bearer_auth(key);
            }
            let attempt = tokio::time::timeout(remaining, request_builder.send())
                .await
                .map_err(|_| {
                    format!(
                        "request exceeded its {}ms latency budget",
                        budget.as_millis()
                    )
                })?
                .map_err(|e| format!("LLM request failed: {e}"))?;

            let status = attempt.status();
            if !use_ollama {
                if let Some(key) = key {
                    crate::groq_keys::record_rate_limit_response(
                        key,
                        status.as_u16(),
                        attempt.headers(),
                    );
                }
            }
            if status.is_success() {
                response = Some(attempt);
                break;
            }
            let text = attempt.text().await.unwrap_or_default();
            let more_keys = attempt_idx + 1 < attempts.len();
            if !use_ollama && more_keys && crate::groq_keys::should_try_next_key(status.as_u16()) {
                log::warn!(
                    "Groq LLM key {}/{} rejected ({}), rotating to next key",
                    attempt_idx + 1,
                    attempts.len(),
                    status
                );
                continue;
            }
            return Err(format!("LLM provider returned {status}: {text}"));
        }
        let mut response = match response {
            Some(response) => response,
            None => return Err("LLM provider returned no usable response".to_string()),
        };

        let mut accumulated_content = String::new();
        let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
        let mut buffer = String::new();
        let mut first_token = true;
        let mut last_spoken_idx = 0;
        let mut seq_num = 0;
        let mut acknowledgement_spoken = false;
        let utterance_id = format!(
            "u-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        while let Some(chunk) = tokio::time::timeout(
            deadline.saturating_duration_since(std::time::Instant::now()),
            response.chunk(),
        )
        .await
        .map_err(|_| {
            format!(
                "request exceeded its {}ms latency budget",
                budget.as_millis()
            )
        })?
        .map_err(|e| format!("stream error: {e}"))?
        {
            // Check if cancelled
            if current_gen_id != crate::tts::get_current_generation() {
                log::info!("LLM generation cancelled by user interruption.");
                return Ok(accumulated_content);
            }

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(idx) = buffer.find("\n\n") {
                let event_str = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();

                for line in event_str.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data.trim() == "[DONE]" {
                            continue;
                        }
                        if let Ok(parsed) = serde_json::from_str::<ChatChunk>(data) {
                            if let Some(choice) = parsed.choices.first() {
                                if let Some(ref text) = choice.delta.content {
                                    if first_token {
                                        if let Some(start) = speech_ended_at {
                                            log::info!(
                                                "LLM First Token received (TTFT): {:?}",
                                                std::time::Instant::now().duration_since(start)
                                            );
                                        } else {
                                            log::info!("LLM First Token received.");
                                        }
                                        first_token = false;
                                    }
                                    accumulated_content.push_str(text);
                                    if current_gen_id == crate::tts::get_current_generation() {
                                        let _ = app.emit("karen-pipeline", json!({ "stage": "answer", "detail": accumulated_content }));
                                    }

                                    while let Some(end_idx) =
                                        next_tts_boundary(&accumulated_content, last_spoken_idx)
                                    {
                                        let sentence = accumulated_content
                                            [last_spoken_idx..end_idx]
                                            .trim()
                                            .to_string();

                                        if sentence.chars().any(|c| c.is_alphanumeric()) {
                                            crate::tts::speak_chunk(
                                                strip_markdown_for_speech(&sentence),
                                                if last_spoken_idx == 0 {
                                                    speech_ended_at
                                                } else {
                                                    None
                                                },
                                                utterance_id.clone(),
                                                seq_num,
                                                current_gen_id,
                                            );
                                            seq_num += 1;
                                        }
                                        last_spoken_idx = end_idx;
                                    }
                                }
                                if let Some(ref tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        while accumulated_tool_calls.len() <= tc.index {
                                            accumulated_tool_calls.push(ToolCall {
                                                id: String::new(),
                                                kind: "function".to_string(),
                                                function: ToolCallFn {
                                                    name: String::new(),
                                                    arguments: String::new(),
                                                },
                                            });
                                        }
                                        let active_tc = &mut accumulated_tool_calls[tc.index];
                                        if let Some(ref id) = tc.id {
                                            active_tc.id.push_str(id);
                                        }
                                        // Unlike `name`/`arguments`, which genuinely arrive as
                                        // string fragments to be concatenated, `type` is always
                                        // sent as a complete value ("function") -- some providers
                                        // resend it on every delta for the same tool call, not
                                        // just the first. `push_str`-ing it like a fragment turned
                                        // repeated "function" deltas into "functionfunction", which
                                        // Groq's strict enum validation on `tool_calls[].type`
                                        // rejected with a 400 on the next request that replayed
                                        // this message as history. Assign, don't accumulate.
                                        if let Some(ref kind) = tc.kind {
                                            active_tc.kind = kind.clone();
                                        }
                                        if let Some(ref f) = tc.function {
                                            if let Some(ref n) = f.name {
                                                active_tc.function.name.push_str(n);
                                            }
                                            if let Some(ref a) = f.arguments {
                                                active_tc.function.arguments.push_str(a);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let message = Message {
            role: "assistant".to_string(),
            content: if accumulated_content.is_empty() {
                None
            } else {
                Some(accumulated_content.clone())
            },
            tool_calls: if accumulated_tool_calls.is_empty() {
                None
            } else {
                Some(accumulated_tool_calls.clone())
            },
            tool_call_id: None,
            name: None,
        };

        if last_spoken_idx < accumulated_content.len() {
            let remainder = accumulated_content[last_spoken_idx..].trim().to_string();
            if remainder.chars().any(|c| c.is_alphanumeric()) {
                crate::tts::speak_chunk(
                    strip_markdown_for_speech(&remainder),
                    if last_spoken_idx == 0 {
                        speech_ended_at
                    } else {
                        None
                    },
                    utterance_id.clone(),
                    seq_num,
                    current_gen_id,
                );
            }
        }

        let tool_calls = accumulated_tool_calls;
        messages.push(message.clone());

        if tool_calls.is_empty() {
            let answer = accumulated_content;
            {
                let memory = app.state::<crate::memory::MemoryEngine>();
                let db = memory.db.lock().await;
                let _ = db.append_message(&conv_id, "assistant", &answer);
            }
            return Ok(answer);
        }

        for call in tool_calls {
            if !tool_call_allowed(transcript, &call.function.name) {
                log::warn!(
                    "blocked tool call without explicit user intent: {}",
                    call.function.name
                );
                messages.push(Message::tool_result(call.id, call.function.name.clone(), json!({"ok": false, "error": "tool call blocked because the original request did not explicitly ask for this action"})));
                continue;
            }
            emit_running_tool(app, &call.function.name, current_gen_id);
            if !acknowledgement_spoken && accumulated_content.trim().is_empty() {
                crate::tts::speak_chunk(
                    tool_acknowledgement(&call.function.name).to_string(),
                    speech_ended_at,
                    utterance_id.clone(),
                    seq_num,
                    current_gen_id,
                );
                seq_num += 1;
                acknowledgement_spoken = true;
            }
            let result = tools::dispatch(&call.function.name, &call.function.arguments, app).await;
            messages.push(Message::tool_result(
                call.id,
                call.function.name.clone(),
                result,
            ));
        }
    }

    Err("gave up after several tool-calling iterations without a final answer".to_string())
}

fn format_routed_result(label: &str, result: Value) -> String {
    if result
        .get("confirmation_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Confirmation required.")
            .to_string();
    }
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        match label {
            "Volume" => format!(
                "Volume set to {} percent.",
                result.get("percent").and_then(Value::as_i64).unwrap_or(0)
            ),
            "Application" => format!(
                "Opened {}.",
                result
                    .get("opened")
                    .and_then(Value::as_str)
                    .unwrap_or("the application")
            ),
            _ => format!("{label} checked."),
        }
    } else {
        result
            .get("error")
            .and_then(Value::as_str)
            .map(|e| format!("{label} failed: {e}"))
            .unwrap_or_else(|| format!("{label} failed."))
    }
}

fn tool_acknowledgement(name: &str) -> &'static str {
    match name {
        "open_app" | "open_url" => "Opening that now.",
        "read_file" | "list_directory" | "list_dev_logs" | "read_dev_logs" => {
            "I’m checking that now."
        }
        "screen_ocr" | "take_screenshot" => "I’m looking at the screen now.",
        "list_processes" | "get_system_info" => "I’m checking the system now.",
        "save_memory" => "I’ll remember that.",
        _ => "Working on that now.",
    }
}

fn tool_call_allowed(original_request: &str, name: &str) -> bool {
    if matches!(
        name,
        "read_file"
            | "list_directory"
            | "get_system_info"
            | "list_processes"
            | "get_current_time"
            | "search_events"
            | "list_tasks"
            | "get_task"
            | "screen_ocr"
            | "take_screenshot"
    ) {
        return true;
    }
    let text = original_request.to_ascii_lowercase();
    // Namespaced MCP tools (browser operate/read/research): allowed when the
    // request is observably a look-something-up errand.
    if name.starts_with("mcp__") {
        return [
            "research", "search", "look up", "lookup", "browse", "open", "visit", "go to", "check",
            "read", "find", "compare", "look at", "what is", "what are", "what was", "how to",
        ]
        .iter()
        .any(|v| text.contains(v));
    }
    match name {
        "open_app" => ["open", "launch", "start"].iter().any(|v| text.contains(v)),
        "open_url" => text.contains("open") || text.contains("visit") || text.contains("go to"),
        "set_volume" => text.contains("volume") || text.contains("sound") || text.contains("mute"),
        "press_key" | "type_text" => {
            text.contains("type") || text.contains("press") || text.contains("write")
        }
        "save_memory" => {
            text.contains("remember") || text.contains("save this") || text.contains("note that")
        }
        "create_task" => {
            text.contains("task") || text.contains("background") || text.contains("work on")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        background_request, needs_browser_tools, needs_semantic_memory,
        research_ready_announcement, strip_markdown_for_speech, tool_acknowledgement,
        tool_call_allowed,
    };

    #[test]
    fn semantic_lookup_is_reserved_for_memory_questions() {
        assert!(needs_semantic_memory("What did we work on yesterday?"));
        assert!(needs_semantic_memory(
            "remember that my VM is on the lab network"
        ));
        assert!(!needs_semantic_memory("open the browser"));
        assert!(!needs_semantic_memory("set the volume to 40"));
    }

    #[test]
    fn tool_acknowledgements_are_short() {
        assert_eq!(tool_acknowledgement("open_app"), "Opening that now.");
        assert!(tool_acknowledgement("unknown").chars().count() < 40);
    }

    #[test]
    fn blocks_implicit_mutations_but_allows_explicit_actions() {
        assert!(!tool_call_allowed("tell me about my system", "type_text"));
        assert!(tool_call_allowed("open VS Code", "open_app"));
        assert!(tool_call_allowed(
            "what is my system status",
            "get_system_info"
        ));
    }

    #[test]
    fn recognizes_explicit_background_requests_only() {
        assert_eq!(
            background_request("run this in the background: inspect the logs"),
            Some("inspect the logs")
        );
        assert_eq!(background_request("inspect the logs"), None);
    }

    #[test]
    fn recognizes_natural_research_phrasing_without_needing_background_wording() {
        assert_eq!(
            background_request("Research clean architecture in SDLC"),
            Some("clean architecture in SDLC")
        );
        assert_eq!(
            background_request("can you look into the new Rust async traits feature"),
            Some("the new Rust async traits feature")
        );
        assert_eq!(
            background_request("look up the latest whisper.cpp release notes"),
            Some("the latest whisper.cpp release notes")
        );
        assert_eq!(
            background_request("investigate why the build is slow"),
            Some("why the build is slow")
        );
    }

    #[test]
    fn leaves_ordinary_conversation_alone() {
        assert_eq!(background_request("what's the weather like"), None);
        assert_eq!(background_request("open the browser"), None);
    }

    #[test]
    fn research_announcement_is_short_and_in_character_for_both_outcomes() {
        let ok = research_ready_announcement("clean architecture", true);
        let failed = research_ready_announcement("clean architecture", false);
        assert!(ok.contains("clean architecture"));
        assert!(failed.contains("clean architecture"));
        assert_ne!(ok, failed);
    }

    #[test]
    fn strips_markdown_emphasis_and_headings_so_tts_never_speaks_symbols() {
        assert_eq!(
            strip_markdown_for_speech("**Multi-cloud** workloads"),
            "Multi-cloud workloads"
        );
        assert_eq!(strip_markdown_for_speech("# Heading"), "Heading");
        assert_eq!(
            strip_markdown_for_speech("use `cargo test`"),
            "use cargo test"
        );
        assert_eq!(strip_markdown_for_speech("_italic_ text"), "italic text");
    }

    #[test]
    fn turns_bullet_lines_into_spoken_pauses_not_run_together_words() {
        let spoken = strip_markdown_for_speech("- First point\n- Second point");
        assert_eq!(spoken, "First point. Second point");
    }

    #[test]
    fn leaves_ordinary_hyphenated_words_alone() {
        assert_eq!(
            strip_markdown_for_speech("zero-trust networking"),
            "zero-trust networking"
        );
    }

    #[test]
    fn background_tasks_always_get_browser_tools() {
        assert!(needs_browser_tools("what's the weather", true));
    }

    #[test]
    fn ordinary_conversation_excludes_browser_tools_unless_it_names_the_web() {
        assert!(!needs_browser_tools("what time is it", false));
        assert!(!needs_browser_tools("set the volume to 50", false));
        assert!(needs_browser_tools(
            "go to github.com and check the issues",
            false
        ));
        assert!(needs_browser_tools("can you browse to that website", false));
    }
}
