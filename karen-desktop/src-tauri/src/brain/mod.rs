mod devlogs;
mod open_app;
mod browse_files;
mod tools;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

const GROQ_CHAT_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const DEFAULT_MODEL: &str = "openai/gpt-oss-120b";
const MAX_ITERS: usize = 5;

const PERSONA_PATH_ENV: &str = "KAREN_PERSONA_PATH";
const DEFAULT_PERSONA_PATH: &str = "/home/shadowe/Projects/Karen/Persona.md";
const FALLBACK_PERSONA: &str = "You are Karen, a voice-activated desktop assistant.";

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

pub async fn run_agentic(transcript: &str, app: &AppHandle, speech_ended_at: Option<std::time::Instant>, current_gen_id: usize) -> Result<String, String> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| "GROQ_API_KEY is not set".to_string())?;
    let model = std::env::var("GROQ_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let persona = load_persona().await;

    let context_engine = app.state::<crate::context_engine::ContextEngine>();
    let ctx = context_engine.get_context();
    let (relevant_memories, recent_events) = {
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = memory.db.lock().await;

        let mut relevant_memories = String::new();
        if let Ok(memories) = db.search_memories(transcript) {
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
                    recent_events.push_str(&format!("- [{time_str}] ({}): {}\n", e.event_type, e.description));
                }
            }
        }

        (relevant_memories, recent_events)
    };

    let ctx_str = format!("Current inferred activity: {}. Top active processes: {:?}{}{}", ctx.activity_inference, ctx.current_processes, relevant_memories, recent_events);

    let mut messages = vec![
        Message::system(format!(
            "{persona}\n\n---\n\
             Operational context: today's date is {today}. {ctx_str}\n\
             You have tools available to launch \
             desktop applications, browse the local filesystem (read files and list directories), \
             and read dev-log entries from the user's engineering standards repo. Use them when \
             the request calls for it. Only answer directly with plain text once you have \
             everything you need. Despite the persona above, keep answers concise -- they may \
             be read aloud via text-to-speech or shown in a small status pill; save depth for \
             when elaboration is specifically asked for."
        )),
    ];

    let conv_id = {
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = memory.db.lock().await;
        
        let id = match db.get_active_conversation().unwrap_or(None) {
            Some(id) => id,
            None => db.create_conversation().unwrap_or_else(|_| "default-conv".to_string()),
        };

        if let Ok(recent) = db.get_recent_messages(&id, 10) {
            for msg in recent {
                let time_str = crate::memory::temporal::format_relative_time(&msg.recorded_at);
                if msg.role == "user" {
                    let content_with_time = format!("[{time_str}]: {}", msg.content);
                    messages.push(Message::user(content_with_time));
                } else if msg.role == "assistant" {
                    messages.push(Message::assistant(msg.content));
                }
            }
        }

        let _ = db.append_message(&id, "user", transcript);
        id
    };

    messages.push(Message::user(transcript.to_string()));

    let tools = tools::all_tool_defs();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    for _ in 0..MAX_ITERS {
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

        let body = serde_json::to_vec(&request).map_err(|e| format!("failed to encode request: {e}"))?;
        log::debug!("Groq request body: {} bytes", body.len());

        let mut response = client
            .post(GROQ_CHAT_URL)
            .bearer_auth(&api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Groq request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Groq returned {status}: {text}"));
        }

        let mut accumulated_content = String::new();
        let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
        let mut buffer = String::new();
        let mut first_token = true;
        let mut last_spoken_idx = 0;
        let mut seq_num = 0;
        let utterance_id = format!("u-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());

        while let Some(chunk) = response.chunk().await.map_err(|e| format!("stream error: {e}"))? {
            // Check if cancelled
            if current_gen_id != crate::tts::get_current_generation() {
                log::info!("LLM generation cancelled by user interruption.");
                return Ok(accumulated_content);
            }

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(idx) = buffer.find("\n\n") {
                let event_str = buffer[..idx].to_string();
                buffer = buffer[idx+2..].to_string();

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
                                            log::info!("LLM First Token received (TTFT): {:?}", std::time::Instant::now().duration_since(start));
                                        } else {
                                            log::info!("LLM First Token received.");
                                        }
                                        first_token = false;
                                    }
                                    accumulated_content.push_str(text);
                                    if current_gen_id == crate::tts::get_current_generation() {
                                        let _ = app.emit("karen-pipeline", json!({ "stage": "answer", "detail": accumulated_content }));
                                    }
                                    
                                    while let Some(idx) = accumulated_content[last_spoken_idx..].find(|c| c == '.' || c == '!' || c == '?') {
                                        let end_idx = last_spoken_idx + idx + 1;
                                        let sentence = accumulated_content[last_spoken_idx..end_idx].trim().to_string();
                                        
                                        if sentence.chars().any(|c| c.is_alphanumeric()) {
                                            crate::tts::speak_chunk(sentence, if last_spoken_idx == 0 { speech_ended_at } else { None }, utterance_id.clone(), seq_num, current_gen_id);
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
                                                function: ToolCallFn { name: String::new(), arguments: String::new() },
                                            });
                                        }
                                        let active_tc = &mut accumulated_tool_calls[tc.index];
                                        if let Some(ref id) = tc.id { active_tc.id.push_str(id); }
                                        if let Some(ref kind) = tc.kind { active_tc.kind.push_str(kind); }
                                        if let Some(ref f) = tc.function {
                                            if let Some(ref n) = f.name { active_tc.function.name.push_str(n); }
                                            if let Some(ref a) = f.arguments { active_tc.function.arguments.push_str(a); }
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
            content: if accumulated_content.is_empty() { None } else { Some(accumulated_content.clone()) },
            tool_calls: if accumulated_tool_calls.is_empty() { None } else { Some(accumulated_tool_calls.clone()) },
            tool_call_id: None,
            name: None,
        };

        if last_spoken_idx < accumulated_content.len() {
            let remainder = accumulated_content[last_spoken_idx..].trim().to_string();
            if remainder.chars().any(|c| c.is_alphanumeric()) {
                crate::tts::speak_chunk(remainder, if last_spoken_idx == 0 { speech_ended_at } else { None }, utterance_id.clone(), seq_num, current_gen_id);
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
            emit_running_tool(app, &call.function.name, current_gen_id);
            let result = tools::dispatch(&call.function.name, &call.function.arguments, app).await;
            messages.push(Message::tool_result(call.id, call.function.name.clone(), result));
        }
    }

    Err("gave up after several tool-calling iterations without a final answer".to_string())
}
