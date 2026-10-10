use serde_json::json;
use tauri::{AppHandle, Emitter};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::AudioFormat;
use crate::{brain, resample, stt, tts};

/// Below this, assume the user clicked start/stop without really saying
/// anything, and skip Whisper/Groq entirely.
const MIN_RECORDING_SECS: f32 = 0.3;
static ACTIVE_PIPELINE: AtomicBool = AtomicBool::new(false);

pub fn is_busy() -> bool {
    ACTIVE_PIPELINE.load(Ordering::SeqCst)
}

/// Whisper emits bracketed/parenthesized markers like `[BLANK_AUDIO]`,
/// `[SILENCE]`, `(wind blowing)` for non-speech audio instead of an empty
/// string -- those should be treated the same as "didn't catch anything",
/// not forwarded to the brain as if they were a real command.
fn is_non_speech_marker(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'))
}

fn clean_transcript(text: &str) -> Option<String> {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() || is_non_speech_marker(&cleaned) {
        return None;
    }

    // These are common Whisper completions for silence or background audio.
    // Keep this list deliberately narrow so short real commands remain valid.
    let normalized = cleaned.to_ascii_lowercase();
    let hallucination = [
        "thank you for watching",
        "thanks for watching",
        "please subscribe",
        "like and subscribe",
        "字幕 by amara.org",
    ];
    if hallucination.iter().any(|phrase| normalized == *phrase) {
        return None;
    }
    Some(cleaned)
}

fn is_cancel_command(text: &str) -> bool {
    matches!(
        normalize_voice_text(text).as_str(),
        "cancel" | "stop" | "stop talking" | "be quiet" | "never mind" | "nevermind"
    )
}

fn normalize_voice_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn wake_word_command(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let wake_word = crate::config::VoiceConfig::from_env().wake_word;
    let configured = normalize_voice_text(&wake_word);
    if configured.is_empty() {
        return None;
    }
    let words = normalize_voice_text(trimmed);
    let mut tokens = words.split_whitespace();
    let first = tokens.next()?;

    // Whisper commonly turns "Karen" into one of these near matches.
    let aliases: Vec<String> = if configured == "karen" {
        ["karen", "karin", "caren", "karrin", "karren"].into_iter().map(String::from).collect()
    } else {
        vec![configured.clone()]
    };
    let mut matched = aliases.iter().any(|alias| alias == first);

    // Accept the natural forms "Hey Karen" and "Okay Karen".
    if !matched && matches!(first, "hey" | "okay" | "ok" | "hello") {
        if let Some(second) = tokens.next() {
            matched = aliases.iter().any(|alias| alias == second);
        }
    }
    if !matched {
        return None;
    }

    let consumed = if first == "hey" || first == "okay" || first == "ok" || first == "hello" { 2 } else { 1 };
    Some(words.split_whitespace().skip(consumed).collect::<Vec<_>>().join(" "))
}

pub async fn run(app: AppHandle, samples: Vec<f32>, format: AudioFormat) -> bool {
    struct BusyGuard;
    impl Drop for BusyGuard {
        fn drop(&mut self) { ACTIVE_PIPELINE.store(false, Ordering::SeqCst); }
    }
    ACTIVE_PIPELINE.store(true, Ordering::SeqCst);
    let _busy_guard = BusyGuard;
    let gen_id = crate::tts::get_current_generation();

    let emit_if_current = |stage: &str, detail: Option<&str>| {
        if crate::tts::get_current_generation() == gen_id {
            if let Some(d) = detail {
                let _ = app.emit("karen-pipeline", json!({ "stage": stage, "detail": d }));
            } else {
                let _ = app.emit("karen-pipeline", json!({ "stage": stage }));
            }
        }
    };

    let min_samples =
        (format.sample_rate as f32 * format.channels as f32 * MIN_RECORDING_SECS) as usize;
    if samples.len() < min_samples {
        emit_if_current("idle", Some("Didn't catch anything"));
        return false;
    }

    let audio_duration = samples.len() as f32 / (format.sample_rate as f32 * format.channels as f32);
    log::info!("================ METRICS ================");
    log::info!("Audio captured: {:.2}s (including VAD silence)", audio_duration);

    let speech_ended_at = std::time::Instant::now();

    emit_if_current("transcribing", None);

    let mono = resample::downmix_to_mono(&samples, format.channels);
    let mono_16k = resample::resample_linear(&mono, format.sample_rate, 16_000);

    let ctx = match stt::ensure_loaded(&app).await {
        Ok(ctx) => ctx,
        Err(e) => {
            log::error!("failed to load whisper model: {e}");
            emit_if_current("error", Some(&e));
            return false;
        }
    };

    let stt_started_at = std::time::Instant::now();
    let transcript = match stt::transcribe(ctx, mono_16k).await {
        Ok(text) => match clean_transcript(&text) {
            Some(text) => text,
            None => {
                emit_if_current("idle", Some("Didn't catch that"));
                return false;
            }
        },
        Err(e) => {
            log::error!("whisper transcription failed: {e}");
            emit_if_current("error", Some(&e));
            return false;
        }
    };

    if is_cancel_command(&transcript) {
        crate::tts::cancel();
        let _ = app.emit("karen-pipeline", json!({ "stage": "idle", "detail": "Stopped. I’m listening." }));
        return false;
    }

    let transcript = if crate::config::VoiceConfig::from_env().wake_word_enabled {
        match wake_word_command(&transcript) {
            Some(command) if !command.trim().is_empty() => command,
            Some(_) => {
                emit_if_current("wake-followup", Some("I’m listening for your command"));
                return true;
            }
            _ => {
                emit_if_current("idle", Some("Say Karen followed by a command"));
                return false;
            }
        }
    } else {
        transcript
    };
    let stt_completed_at = std::time::Instant::now();
    let stt_duration = stt_completed_at.duration_since(stt_started_at);
    let rtf = stt_duration.as_secs_f32() / audio_duration;
    
    log::info!("STT duration: {:?}", stt_duration);
    log::info!("STT Real-Time Factor (RTF): {:.2} (lower is better, < 1.0 means faster than real-time)", rtf);

    log::info!("heard: {transcript}");
    emit_if_current("thinking", Some(&transcript));

    if crate::tts::get_current_generation() != gen_id {
        log::info!("Pipeline cancelled during STT, aborting.");
        return false;
    }

    // Notice we do NOT pass speech_ended_at down to run_agentic anymore?
    // Wait, run_agentic emits TTFT. We can still pass it.
    let answer = match brain::run_agentic(&transcript, &app, Some(speech_ended_at), gen_id).await {
        Ok(answer) => {
            // "Final text is committed even if speech fails."
            // We should only emit answer if not canceled.
            emit_if_current("answer", Some(&answer));
            answer
        }
        Err(e) => {
            log::error!("brain loop failed: {e}");
            emit_if_current("error", Some(&e));
            return false;
        }
    };
    let llm_completed_at = std::time::Instant::now();
    let stt_ms = stt_completed_at.duration_since(stt_started_at).as_millis();
    let llm_ms = llm_completed_at.duration_since(stt_completed_at).as_millis();
    let total_ms = llm_completed_at.duration_since(speech_ended_at).as_millis();
    log::info!("Latency: STT={}ms LLM={}ms total={}ms", stt_ms, llm_ms, total_ms);
    emit_if_current(
        "latency",
        Some(&format!("STT {stt_ms}ms · LLM {llm_ms}ms · total {total_ms}ms")),
    );

    let playback_completed_at = std::time::Instant::now(); // Note: This doesn't represent actual playback end anymore since playback is queued async
    log::info!("End-of-speech to complete response: {:?}", playback_completed_at.duration_since(speech_ended_at));

    // REMOVED: emitting `idle` at the end so the answer stays on the screen!
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_whisper_non_speech_markers() {
        assert!(is_non_speech_marker("[BLANK_AUDIO]"));
        assert!(is_non_speech_marker("  [SILENCE]  "));
        assert!(is_non_speech_marker("(wind blowing)"));
    }

    #[test]
    fn real_speech_is_not_flagged() {
        assert!(!is_non_speech_marker("open vs code"));
        assert!(!is_non_speech_marker("what did we do today"));
    }

    #[test]
    fn cleans_whisper_spacing_and_rejects_common_hallucinations() {
        assert_eq!(clean_transcript("  open   the browser  "), Some("open the browser".into()));
        assert_eq!(clean_transcript("thank you for watching"), None);
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), None);
        assert_eq!(clean_transcript("stop"), Some("stop".into()));
    }

    #[test]
    fn recognizes_barge_in_cancel_phrases() {
        assert!(is_cancel_command("Stop talking"));
        assert!(is_cancel_command("never-mind"));
        assert!(is_cancel_command("CANCEL"));
        assert!(!is_cancel_command("stop the music"));
    }

    #[test]
    fn extracts_wake_word_command() {
        assert_eq!(wake_word_command("Karen, open the browser"), Some("open the browser".into()));
        assert_eq!(wake_word_command("karen: what time is it"), Some("what time is it".into()));
        assert_eq!(wake_word_command("Hey Karen, open the browser"), Some("open the browser".into()));
        assert_eq!(wake_word_command("okay karin what time is it"), Some("what time is it".into()));
        assert_eq!(wake_word_command("hello caren"), Some("".into()));
        assert_eq!(wake_word_command("open the browser"), None);
    }
}
