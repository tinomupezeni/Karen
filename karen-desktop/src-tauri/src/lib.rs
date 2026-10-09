mod audio;
mod brain;
mod context_engine;
mod memory;
mod pipeline;
mod resample;
mod stt;
mod tts;

use std::sync::mpsc::Sender;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::Mutex as AsyncMutex;

use audio::{AudioBuffer, AudioCommand};
use context_engine::ContextEngine;
use memory::MemoryEngine;
use stt::WhisperHandle;

struct AudioHandle(Mutex<Sender<AudioCommand>>);
struct ListeningState(Mutex<bool>);

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

fn wake(app: &AppHandle) {
    crate::tts::cancel();
    let audio = app.state::<AudioHandle>();
    let listening_state = app.state::<ListeningState>();

    let mut listening = listening_state.0.lock().unwrap();
    if !*listening {
        *listening = true;
        let _ = audio.0.lock().unwrap().send(AudioCommand::Start);
    }
    let _ = app.emit("wake-karen", ());
    log::info!("wake triggered");
}

#[tauri::command]
fn toggle_mic_command(app: AppHandle) -> bool {
    toggle_mic(&app)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Ok(path) = dotenvy::dotenv() {
        println!("loaded env from {}", path.display());
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
                    if event.state == ShortcutState::Pressed {
                        let ctrl_space = Shortcut::new(Some(Modifiers::CONTROL), Code::Space);
                        let mute_shortcut =
                            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);

                        if shortcut == &ctrl_space {
                            wake(app);
                        } else if shortcut == &mute_shortcut {
                            toggle_mic(app);
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![toggle_mic_command])
        .setup(move |app| {
            crate::tts::init();
            let ctrl_space = Shortcut::new(Some(Modifiers::CONTROL), Code::Space);
            let mute_shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);

            if let Err(e) = app.global_shortcut().register(ctrl_space) {
                log::error!("failed to register Ctrl+Space shortcut: {e}");
            }
            if let Err(e) = app.global_shortcut().register(mute_shortcut) {
                log::error!("failed to register Ctrl+Alt+M shortcut: {e}");
            }

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
                                pipeline::run(app_handle.clone(), samples, format).await;
                                
                                crate::tts::wait_all().await;

                                let listening_state = app_handle.state::<ListeningState>();
                                if *listening_state.0.lock().unwrap() {
                                    let audio = app_handle.state::<AudioHandle>();
                                    let _ = audio.0.lock().unwrap().send(AudioCommand::Start);
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

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while building tauri application");
}
