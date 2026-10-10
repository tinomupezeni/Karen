pub mod provider;
pub mod groq;
pub mod piper;
pub mod manager;

use manager::TtsManager;

use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use std::sync::OnceLock;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub enum TtsCommand {
    Speak {
        text: String,
        speech_ended_at: Option<std::time::Instant>,
        utterance_id: String,
        seq_num: usize,
        gen_id: usize,
    },
    Wait(tokio::sync::oneshot::Sender<()>),
    Cancel,
}

static TTS_TX: OnceLock<mpsc::UnboundedSender<TtsCommand>> = OnceLock::new();
static PLAY_TX: OnceLock<mpsc::Sender<PlaybackCommand>> = OnceLock::new();
static CURRENT_GENERATION: AtomicUsize = AtomicUsize::new(0);

pub enum PlaybackCommand {
    Play {
        path: std::path::PathBuf,
        utterance_id: String,
        seq_num: usize,
    },
    Wait(tokio::sync::oneshot::Sender<()>),
    Cancel,
}

pub fn init() {
    let (tx, mut rx) = mpsc::unbounded_channel::<TtsCommand>();
    TTS_TX.set(tx).ok();

    let (play_tx, mut play_rx) = mpsc::channel::<PlaybackCommand>(1);
    PLAY_TX.set(play_tx.clone()).ok();
    
    // Playback worker
    let active_child: Arc<Mutex<Option<tokio::process::Child>>> = Arc::new(Mutex::new(None));
    let child_clone = active_child.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(cmd) = play_rx.recv().await {
            match cmd {
                PlaybackCommand::Cancel => {
                    log::info!("[TTS] Playback Cancel received, killing active audio");
                    if let Some(mut child) = child_clone.lock().await.take() {
                        let _ = child.kill().await;
                    }
                    // clear the queue
                    while let Ok(discarded) = play_rx.try_recv() {
                        if let PlaybackCommand::Play { path, utterance_id, seq_num } = discarded {
                            log::info!("[TTS] [{utterance_id}:{seq_num}] Discarded from playback queue due to cancellation");
                            let _ = tokio::fs::remove_file(&path).await;
                        }
                    }
                }
                PlaybackCommand::Wait(tx) => {
                    let _ = tx.send(());
                }
                PlaybackCommand::Play { path, utterance_id, seq_num } => {
                    let play_started = std::time::Instant::now();
                    log::info!("[TTS] [{utterance_id}:{seq_num}] Playback started");
                    if let Ok(mut child) = tokio::process::Command::new("aplay").arg(&path).spawn() {
                        *child_clone.lock().await = Some(child);
                        let mut c = child_clone.lock().await.take().unwrap();
                        let _ = c.wait().await;
                    }
                    let _ = tokio::fs::remove_file(&path).await;
                    log::info!("[TTS] [{utterance_id}:{seq_num}] Playback ended (duration: {:?})", std::time::Instant::now().duration_since(play_started));
                }
            }
        }
    });

    // Generation worker
    tauri::async_runtime::spawn(async move {
        let manager = TtsManager::new().expect("Failed to initialize TtsManager");
        
        while let Some(cmd) = rx.recv().await {
            match cmd {
                TtsCommand::Speak { text, speech_ended_at, utterance_id, seq_num, gen_id } => {
                    if gen_id != CURRENT_GENERATION.load(Ordering::SeqCst) {
                        log::info!("[TTS] [{utterance_id}:{seq_num}] Synthesis discarded before start (stale generation)");
                        continue;
                    }

                    let gen_started = std::time::Instant::now();
                    log::info!("[TTS] [{utterance_id}:{seq_num}] Synthesis started");
                    
                    match manager.generate_audio(&text).await {
                        Ok((path, provider_name)) => {
                            // only send if generation hasn't been cancelled
                            if gen_id == CURRENT_GENERATION.load(Ordering::SeqCst) {
                                log::info!("[TTS] [{utterance_id}:{seq_num}] {} synthesis duration: {:?}", provider_name, std::time::Instant::now().duration_since(gen_started));
                                if let Some(start) = speech_ended_at {
                                    log::info!("End-of-speech to first audible response: {:?}", std::time::Instant::now().duration_since(start));
                                }
                                let _ = play_tx.send(PlaybackCommand::Play { path, utterance_id, seq_num }).await;
                            } else {
                                log::info!("[TTS] [{utterance_id}:{seq_num}] Synthesis discarded (cancelled)");
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                        }
                        Err(e) => {
                            log::error!("[TTS] [{utterance_id}:{seq_num}] Synthesis failed: {}", e);
                        }
                    }
                }
                TtsCommand::Wait(tx) => {
                    let _ = play_tx.send(PlaybackCommand::Wait(tx)).await;
                }
                TtsCommand::Cancel => {
                    // The generation is advanced synchronously in `cancel`.
                    // Keeping this arm as a queue drain signal avoids changing
                    // the playback worker protocol without incrementing twice.
                }
            }
        }
    });
}

pub fn get_current_generation() -> usize {
    CURRENT_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn speak_chunk(text: String, speech_ended_at: Option<std::time::Instant>, utterance_id: String, seq_num: usize, gen_id: usize) {
    if let Some(tx) = TTS_TX.get() {
        let _ = tx.send(TtsCommand::Speak { text, speech_ended_at, utterance_id, seq_num, gen_id });
    }
}

pub fn cancel() {
    // Advance before returning so callers that immediately capture the new
    // generation cannot race the asynchronous TTS worker.
    CURRENT_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(tx) = TTS_TX.get() {
        let _ = tx.send(TtsCommand::Cancel);
    }
    if let Some(play_tx) = PLAY_TX.get() {
        let _ = play_tx.try_send(PlaybackCommand::Cancel);
    }
}

pub async fn wait_all() {
    if let Some(tx) = TTS_TX.get() {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(TtsCommand::Wait(reply_tx));
        let _ = reply_rx.await;
    }
}
