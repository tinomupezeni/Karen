use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::audio::AudioFormat;
use crate::{brain, resample, stt, tts};

/// Below this, assume the user clicked start/stop without really saying
/// anything, and skip Whisper/Groq entirely.
const MIN_RECORDING_SECS: f32 = 0.3;

/// Whisper emits bracketed/parenthesized markers like `[BLANK_AUDIO]`,
/// `[SILENCE]`, `(wind blowing)` for non-speech audio instead of an empty
/// string -- those should be treated the same as "didn't catch anything",
/// not forwarded to the brain as if they were a real command.
fn is_non_speech_marker(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'))
}

pub async fn run(app: AppHandle, samples: Vec<f32>, format: AudioFormat) {
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
        return;
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
            return;
        }
    };

    let stt_started_at = std::time::Instant::now();
    let transcript = match stt::transcribe(ctx, mono_16k).await {
        Ok(text) if !text.trim().is_empty() && !is_non_speech_marker(&text) => text,
        Ok(_) => {
            emit_if_current("idle", Some("Didn't catch that"));
            return;
        }
        Err(e) => {
            log::error!("whisper transcription failed: {e}");
            emit_if_current("error", Some(&e));
            return;
        }
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
        return;
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
            return;
        }
    };
    let llm_completed_at = std::time::Instant::now();
    log::info!("LLM duration: {:?}", llm_completed_at.duration_since(stt_completed_at));

    let playback_completed_at = std::time::Instant::now(); // Note: This doesn't represent actual playback end anymore since playback is queued async
    log::info!("End-of-speech to complete response: {:?}", playback_completed_at.duration_since(speech_ended_at));

    // REMOVED: emitting `idle` at the end so the answer stays on the screen!
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
}
