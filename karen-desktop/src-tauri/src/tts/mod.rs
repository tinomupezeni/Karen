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

fn wav_summary(path: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return "unreadable".into() };
    let Ok(meta) = file.metadata() else { return "metadata unavailable".into() };
    let mut riff = [0u8; 12];
    if file.read_exact(&mut riff).is_err() || &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return format!("{} bytes; unrecognized WAV header", meta.len());
    }
    // Walk chunks instead of assuming `data` lives at byte 40: Groq's WAVs
    // carry a LIST/INFO chunk between `fmt ` and `data`, and streaming-style
    // headers use 0xFFFFFFFF placeholder sizes (real size derived from file).
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut bits = 0u16;
    let mut data_len: Option<u64> = None;
    loop {
        let mut chunk_hdr = [0u8; 8];
        if file.read_exact(&mut chunk_hdr).is_err() {
            break;
        }
        let tag = &chunk_hdr[0..4];
        let size = u32::from_le_bytes([chunk_hdr[4], chunk_hdr[5], chunk_hdr[6], chunk_hdr[7]]) as u64;
        if tag == b"fmt " {
            let take = size.min(16) as usize;
            let mut fmt = vec![0u8; take];
            if file.read_exact(&mut fmt).is_err() || fmt.len() < 16 {
                break;
            }
            channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            if size > 16 && file.seek(SeekFrom::Current((size - 16) as i64)).is_err() {
                break;
            }
        } else if tag == b"data" {
            let pos = file.stream_position().unwrap_or(meta.len());
            data_len = Some(if size == 0xFFFF_FFFF {
                meta.len().saturating_sub(pos)
            } else {
                size
            });
            break;
        } else {
            // Chunks are word-aligned; a trailing pad byte follows odd sizes.
            let skip = size + (size & 1);
            if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                break;
            }
        }
    }
    let Some(data_len) = data_len else {
        return format!("{} bytes; no data chunk found", meta.len());
    };
    let byte_rate = rate.saturating_mul(channels as u32).saturating_mul(bits as u32 / 8);
    let secs = if byte_rate == 0 { 0.0 } else { data_len as f64 / byte_rate as f64 };
    format!("{} bytes; {channels}ch {rate}Hz {bits}-bit; {secs:.2}s", meta.len())
}

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
                    let custom_player = std::env::var("KAREN_AUDIO_PLAYER").ok().filter(|p| !p.trim().is_empty());
                    let player = custom_player.or_else(|| which::which("pw-play").ok().map(|p| p.display().to_string())).unwrap_or_else(|| "aplay".to_string());
                    log::info!("[TTS] [{utterance_id}:{seq_num}] Playback starting: player={player:?}, wav={}", wav_summary(&path));
                    log::debug!("[TTS] [{utterance_id}:{seq_num}] Audio env: XDG_RUNTIME_DIR={:?}, PIPEWIRE_REMOTE_set={}, PULSE_SERVER_set={}, ALSA_CONFIG_PATH_set={}", std::env::var("XDG_RUNTIME_DIR").ok(), std::env::var_os("PIPEWIRE_REMOTE").is_some(), std::env::var_os("PULSE_SERVER").is_some(), std::env::var_os("ALSA_CONFIG_PATH").is_some());
                    match tokio::process::Command::new(&player).arg(&path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn() {
                        Ok(mut child) => {
                            let stderr = child.stderr.take();
                            *child_clone.lock().await = Some(child);
                            let mut c = child_clone.lock().await.take().unwrap();
                            let status = c.wait().await;
                            let stderr_text = if let Some(mut stderr) = stderr {
                                let mut output = String::new();
                                use tokio::io::AsyncReadExt;
                                let _ = stderr.read_to_string(&mut output).await;
                                output.trim().to_string()
                            } else { String::new() };
                            match status {
                                Ok(status) if status.success() => log::info!("[TTS] [{utterance_id}:{seq_num}] Player succeeded (code={:?}); stderr={stderr_text:?}", status.code()),
                                Ok(status) => log::error!("[TTS] [{utterance_id}:{seq_num}] Player failed (status={status}); stderr={stderr_text:?}"),
                                Err(error) => log::error!("[TTS] [{utterance_id}:{seq_num}] Player wait failed: {error}; stderr={stderr_text:?}"),
                            }
                        }
                        Err(error) => log::error!("[TTS] [{utterance_id}:{seq_num}] Player spawn failed: player={player:?}, found_in_path={}, error={error}", which::which(&player).is_ok()),
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
