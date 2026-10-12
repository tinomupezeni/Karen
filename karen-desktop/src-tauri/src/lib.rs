mod audio;
mod brain;
mod context_engine;
mod config;
mod gesture;
mod groq_keys;
mod mcp;
mod memory;
mod pipeline;
mod resample;
mod stt;
mod tts;

use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;

use tauri::{AppHandle, Emitter, Manager};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::Mutex as AsyncMutex;
use chrono::Timelike;

use audio::{AudioBuffer, AudioCommand};
use context_engine::ContextEngine;
use memory::MemoryEngine;
use stt::WhisperHandle;

struct AudioHandle(Mutex<Sender<AudioCommand>>);
struct ListeningState(Mutex<bool>);
static INSTANCE_LOCK: OnceLock<File> = OnceLock::new();

fn acquire_single_instance() -> bool {
    let mut dir = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.push("karen-assistant");
    if std::fs::create_dir_all(&dir).is_err() {
        log::error!("could not create Karen data directory");
        return false;
    }
    let path = dir.join("karen.lock");

    let try_create = || {
        OpenOptions::new().write(true).create_new(true).open(&path)
    };

    let file = match try_create() {
        Ok(file) => file,
        Err(_) => {
            // On Linux, recover a lock left by a crashed process when its PID
            // no longer exists. A live PID always wins and prevents startup.
            let stale = std::fs::read_to_string(&path)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .map(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists())
                .unwrap_or(false);
            if !stale || std::fs::remove_file(&path).is_err() {
                log::error!("another Karen instance is already running");
                return false;
            }
            match try_create() {
                Ok(file) => file,
                Err(_) => {
                    log::error!("another Karen instance claimed the startup lock");
                    return false;
                }
            }
        }
    };

    let mut file = file;
    let _ = writeln!(file, "{}", std::process::id());
    let _ = INSTANCE_LOCK.set(file);
    true
}

// Shared by the global-shortcut handler and the toggle_mic command, so a
// keyboard toggle and a button click can never drift out of sync with each
// other or with the actual mic capture state.
fn toggle_mic(app: &AppHandle) -> bool {
    let audio = app.state::<AudioHandle>();
    let listening_state = app.state::<ListeningState>();

    let mut listening = listening_state.0.lock().unwrap();
    *listening = !*listening;
    let cmd = if *listening {
        AudioCommand::Start
    } else {
        AudioCommand::Stop
    };
    let _ = audio.0.lock().unwrap().send(cmd);
    let _ = app.emit("toggle-mic", ());
    log::info!("mic toggled -> listening = {}", *listening);

    if !*listening {
        let buffer = app.state::<AudioBuffer>();
        if let Some((samples, format)) = buffer.drain() {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                pipeline::run(app_handle, samples, format).await;
            });
        }
    }

    *listening
}

fn push_to_talk_start(app: &AppHandle) {
    let listening_state = app.state::<ListeningState>();
    let mut listening = listening_state.0.lock().unwrap();
    if *listening {
        return;
    }
    *listening = true;
    let audio = app.state::<AudioHandle>();
    let _ = audio.0.lock().unwrap().send(AudioCommand::Start);
    let _ = app.emit("push-to-talk", serde_json::json!({ "active": true }));
    log::info!("push-to-talk pressed: mic started");
}

fn push_to_talk_stop(app: &AppHandle) {
    let listening_state = app.state::<ListeningState>();
    let mut listening = listening_state.0.lock().unwrap();
    if !*listening {
        return;
    }
    *listening = false;
    let audio = app.state::<AudioHandle>();
    let _ = audio.0.lock().unwrap().send(AudioCommand::Stop);
    let _ = app.emit("push-to-talk", serde_json::json!({ "active": false }));
    let buffer = app.state::<AudioBuffer>();
    if let Some((samples, format)) = buffer.drain() {
        let app_handle = app.clone();
        tauri::async_runtime::spawn(async move {
            pipeline::run(app_handle.clone(), samples, format).await;
            let _ = app_handle.emit("karen-pipeline", serde_json::json!({ "stage": "speaking" }));
            crate::tts::wait_all().await;
        });
    }
    log::info!("push-to-talk released: mic stopped");
}

fn wake(app: &AppHandle) {
    crate::tts::cancel();
    let audio = app.state::<AudioHandle>();
    let listening_state = app.state::<ListeningState>();

    let mut listening = listening_state.0.lock().unwrap();
    if !*listening {
        *listening = true;
    }
    // A response pipeline stops capture while Karen speaks. Request Start on
    // every wake so Ctrl+Space can interrupt playback and begin a new turn
    // even when the logical listening flag is already true.
    let _ = audio.0.lock().unwrap().send(AudioCommand::Start);
    let _ = app.emit("wake-karen", ());
    log::info!("wake triggered");
}

#[tauri::command]
fn toggle_mic_command(app: AppHandle) -> bool {
    toggle_mic(&app)
}

#[tauri::command]
fn set_tts_provider(provider: String) -> Result<String, String> {
    if !matches!(provider.to_ascii_lowercase().as_str(), "auto" | "groq" | "piper") {
        return Err("TTS provider must be auto, groq, or piper".into());
    }
    // This process-wide setting is read by TtsManager for each utterance.
    unsafe { std::env::set_var("KAREN_TTS_PROVIDER", provider.to_ascii_lowercase()) };
    Ok(provider.to_ascii_lowercase())
}

#[tauri::command]
fn set_llm_provider(provider: String, model: String) -> Result<String, String> {
    if !matches!(provider.to_ascii_lowercase().as_str(), "groq" | "ollama") {
        return Err("LLM provider must be groq or ollama".into());
    }
    if model.trim().is_empty() || model.len() > 120 || model.contains(['\n', '\r']) {
        return Err("LLM model name is invalid".into());
    }
    let provider = provider.to_ascii_lowercase();
    unsafe {
        std::env::set_var("KAREN_LLM_PROVIDER", &provider);
        if provider == "ollama" {
            std::env::set_var("KAREN_OLLAMA_MODEL", model.trim());
        } else {
            std::env::set_var("GROQ_MODEL", model.trim());
        }
    }
    Ok(provider)
}

#[tauri::command]
async fn check_ollama() -> Result<serde_json::Value, String> {
    let url = std::env::var("KAREN_OLLAMA_URL")
        .unwrap_or_else(|_| "http://10.50.101.11:11434/v1/chat/completions".into())
        .replace("/v1/chat/completions", "/api/tags");
    let response = reqwest::Client::new().get(url).send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "ok": status.is_success(), "status": status.as_u16(), "models": body.get("models").cloned().unwrap_or(serde_json::Value::Null) }))
}

#[tauri::command]
async fn confirm_pending_tool(app: AppHandle) -> serde_json::Value {
    brain::tools::confirm_pending(&app).await
}

#[tauri::command]
fn cancel_pending_tool() -> serde_json::Value {
    brain::tools::cancel_pending()
}

#[tauri::command]
fn set_gesture_enabled(enabled: bool) -> bool {
    crate::gesture::set_enabled(enabled);
    enabled
}

#[tauri::command]
fn get_gesture_enabled() -> bool {
    crate::gesture::is_enabled()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Ok(path) = dotenvy::dotenv() {
        println!("loaded env from {}", path.display());
    }

    if !acquire_single_instance() {
        return;
    }

    let (speech_tx, speech_rx) = std::sync::mpsc::channel();
    let audio_buffer = AudioBuffer::new();
    let audio_tx = audio::spawn_audio_worker(audio_buffer.clone(), speech_tx);

    let mut db_path = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    db_path.push("karen-assistant");
    std::fs::create_dir_all(&db_path).unwrap();
    db_path.push("memory.db");
    
    let memory_engine = MemoryEngine::new(db_path).expect("Failed to initialize memory database");
    let context_engine = ContextEngine::new(memory_engine.db.clone());

    tauri::Builder::default()
        .manage(AudioHandle(Mutex::new(audio_tx)))
        .manage(ListeningState(Mutex::new(false)))
        .manage(audio_buffer)
        .manage(WhisperHandle(AsyncMutex::new(None)))
        .manage(context_engine)
        .manage(memory_engine)
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Debug)
                .build(),
        )
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    let ctrl_space = Shortcut::new(Some(Modifiers::CONTROL), Code::Space);
                    let push_to_talk =
                        Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);

                    if shortcut == &ctrl_space && event.state == ShortcutState::Pressed {
                            wake(app);
                    } else if shortcut == &push_to_talk {
                        match event.state {
                            ShortcutState::Pressed => push_to_talk_start(app),
                            ShortcutState::Released => push_to_talk_stop(app),
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![toggle_mic_command, set_tts_provider, set_llm_provider, check_ollama, confirm_pending_tool, cancel_pending_tool, set_gesture_enabled, get_gesture_enabled])
        .setup(move |app| {
            crate::tts::init();
            let show = MenuItemBuilder::with_id("show", "Show Karen").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit Karen").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;
            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Karen")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        crate::tts::cancel();
                        crate::gesture::stop();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            let ctrl_space = Shortcut::new(Some(Modifiers::CONTROL), Code::Space);
            let mute_shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);

            if let Err(e) = app.global_shortcut().register(ctrl_space) {
                log::error!("failed to register Ctrl+Space shortcut: {e}");
            }
            if let Err(e) = app.global_shortcut().register(mute_shortcut) {
                log::error!("failed to register Ctrl+Alt+M shortcut: {e}");
            }

            // Warm Whisper during boot so the first user command does not pay
            // model loading or download latency. The greeting and microphone
            // remain usable while this task runs; ensure_loaded is shared and
            // will await the same initialization if needed.
            let warm_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let started = std::time::Instant::now();
                match stt::ensure_loaded(&warm_app).await {
                    Ok(_) => log::info!("Whisper warmed during startup in {:?}", started.elapsed()),
                    Err(e) => log::warn!("Whisper warm-up failed; it will retry on first command: {e}"),
                }
            });

            // Gesture sidecar supervisor (no-op unless FRIDAY_GESTURE_ENABLED).
            // The sidecar is a sensor only (--dry-run); Friday maps gestures
            // to safe actions here. See gesture.rs.
            let gesture_app = app.handle().clone();
            std::thread::spawn(move || crate::gesture::supervise(gesture_app));

            // MCP tool servers (no-op unless configured). Curated external
            // capabilities such as the Playwright browser. See mcp.rs.
            tauri::async_runtime::spawn(async move {
                crate::mcp::supervise().await;
            });

            wake(app.handle());

            let app_handle_for_speech = app.handle().clone();
            std::thread::spawn(move || {
                for _ in speech_rx {
                    let listening_state = app_handle_for_speech.state::<ListeningState>();
                    let is_listening = *listening_state.0.lock().unwrap();
                    if is_listening {
                        let audio_handle = app_handle_for_speech.state::<AudioHandle>();
                        let _ = audio_handle.0.lock().unwrap().send(AudioCommand::Stop);

                        let buffer = app_handle_for_speech.state::<AudioBuffer>();
                        if let Some((samples, format)) = buffer.drain() {
                            let app_handle = app_handle_for_speech.clone();
                            tauri::async_runtime::spawn(async move {
                                let follow_up = pipeline::run(app_handle.clone(), samples, format).await;
                                if follow_up {
                                    let _ = app_handle.emit("karen-pipeline", serde_json::json!({ "stage": "listening" }));
                                } else {
                                    let _ = app_handle.emit("karen-pipeline", serde_json::json!({ "stage": "speaking" }));
                                    crate::tts::wait_all().await;

                                    let listening_state = app_handle.state::<ListeningState>();
                                    if *listening_state.0.lock().unwrap() {
                                        let audio = app_handle.state::<AudioHandle>();
                                        let _ = audio.0.lock().unwrap().send(AudioCommand::Start);
                                    }
                                }
                            });
                        }
                    }
                }
            });

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let msg = "The system just booted up and you are coming online. Generate a short, deadpan 1-sentence greeting letting me know you are online and listening. Be perfectly in character.";
                let _ = app_handle.emit("karen-pipeline", serde_json::json!({ "stage": "thinking", "detail": "Waking up..." }));
                
                let audio_handle = app_handle.state::<AudioHandle>();
                let _ = audio_handle.0.lock().unwrap().send(AudioCommand::Stop);

                let gen_id = crate::tts::get_current_generation();
                match brain::run_agentic(msg, &app_handle, Some(std::time::Instant::now()), gen_id).await {
                    Ok(answer) => {
                        let _ = app_handle.emit("karen-pipeline", serde_json::json!({ "stage": "answer", "detail": &answer }));
                        
                        crate::tts::wait_all().await;

                        let listening_state = app_handle.state::<ListeningState>();
                        if *listening_state.0.lock().unwrap() {
                            let _ = audio_handle.0.lock().unwrap().send(AudioCommand::Start);
                        }
                    }
                    Err(e) => {
                        log::error!("failed to generate startup greeting: {e}");
                    }
                }
            });

            if std::env::var("KAREN_PROACTIVE_MODE").map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "on")).unwrap_or(false) {
                let proactive_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let interval_minutes = std::env::var("KAREN_PROACTIVE_INTERVAL_MINUTES")
                        .ok().and_then(|v| v.parse::<u64>().ok()).filter(|v| *v >= 5).unwrap_or(30);
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_minutes * 60));
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        let hour = chrono::Local::now().hour();
                        let quiet_start = std::env::var("KAREN_PROACTIVE_QUIET_START").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(22);
                        let quiet_end = std::env::var("KAREN_PROACTIVE_QUIET_END").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(7);
                        let quiet = if quiet_start > quiet_end { hour >= quiet_start || hour < quiet_end } else { hour >= quiet_start && hour < quiet_end };
                        if quiet || pipeline::is_busy() {
                            continue;
                        }
                        let _ = proactive_app.emit("karen-pipeline", serde_json::json!({ "stage": "thinking", "detail": "Checking whether I can be useful…" }));
                        let prompt = "You are running a quiet proactive check for Tino. Based only on verified current context, recent activity, and remembered unresolved work, produce either one concise useful suggestion or exactly the word SKIP if there is nothing worth interrupting him about. Never invent an issue, never claim to have acted, and do not ask a broad question.";
                        let gen_id = crate::tts::get_current_generation();
                        if let Ok(answer) = brain::run_agentic(prompt, &proactive_app, None, gen_id).await {
                            if answer.trim().eq_ignore_ascii_case("SKIP") { continue; }
                            let _ = proactive_app.emit("karen-pipeline", serde_json::json!({ "stage": "answer", "detail": &answer }));
                            crate::tts::wait_all().await;
                        }
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
