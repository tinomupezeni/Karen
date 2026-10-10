use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex as AsyncMutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperHandle(pub AsyncMutex<Option<Arc<WhisperContext>>>);

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
const DEFAULT_MODEL: &str = "ggml-tiny.en.bin";

fn model_filename() -> String {
    std::env::var("KAREN_WHISPER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

fn validate_model_override(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("KAREN_WHISPER_MODEL_PATH must be an absolute path".into())
    }
}

fn model_path(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("KAREN_WHISPER_MODEL_PATH") {
        return validate_model_override(&path);
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve app data dir: {e}"))?
        .join("whisper");
    Ok(dir.join(model_filename()))
}

fn is_local_model_override() -> bool {
    std::env::var("KAREN_WHISPER_MODEL_PATH").is_ok()
}

const DOWNLOAD_ATTEMPTS: u32 = 10;
const RETRY_BACKOFF_SECS: u64 = 3;

/// Streams the model to disk in chunks (rather than buffering the whole
/// ~148MB in memory via `.bytes()`), and resumes via HTTP Range on retry
/// instead of restarting from zero -- this connection has been getting cut
/// mid-transfer by something between here and the CDN (not a slow-bandwidth
/// issue alone, since the cutoffs happen at varying transfer sizes), so
/// each retry should only need to cover whatever is still missing.
async fn download_with_retries(url: &str, part_path: &std::path::Path) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut last_err = String::new();

    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_BACKOFF_SECS)).await;
        }

        // A fresh client (and so a fresh connection pool + DNS cache) per
        // attempt, rather than one shared across retries -- on a connection
        // that keeps getting cut mid-transfer, reusing the same pool risks
        // every retry inheriting whatever broke the first one.
        let client = reqwest::Client::new();

        let existing_len = tokio::fs::metadata(part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut request = client.get(url);
        if existing_len > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={existing_len}-"));
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("model download request failed: {e}");
                log::warn!("whisper model download attempt {attempt} failed to start: {e:?}");
                continue;
            }
        };

        // A server that ignores Range and resends the whole file (200, not
        // 206) means our partial data doesn't line up with a fresh body --
        // start that attempt over rather than silently corrupting the file.
        let resuming = existing_len > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if existing_len > 0 && !resuming {
            let _ = tokio::fs::remove_file(part_path).await;
        }

        let mut response = match response.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("model download request failed: {e}");
                log::warn!("whisper model download attempt {attempt} failed to start: {last_err}");
                continue;
            }
        };

        let result: Result<(), String> = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(resuming)
                .truncate(!resuming)
                .open(part_path)
                .await
                .map_err(|e| format!("failed to open model file: {e}"))?;

            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| format!("failed to read model download chunk: {e}"))?
            {
                file.write_all(&chunk)
                    .await
                    .map_err(|e| format!("failed to write downloaded model chunk: {e}"))?;
            }
            file.flush()
                .await
                .map_err(|e| format!("failed to flush downloaded model: {e}"))
        }
        .await;

        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::warn!(
                    "whisper model download attempt {attempt} failed ({} bytes so far): {e}",
                    tokio::fs::metadata(part_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0)
                );
                last_err = e;
            }
        }
    }

    Err(format!(
        "model download failed after {DOWNLOAD_ATTEMPTS} attempts: {last_err}"
    ))
}

async fn ensure_model_downloaded(app: &AppHandle) -> Result<PathBuf, String> {
    let path = model_path(app)?;
    if path.exists() {
        return Ok(path);
    }

    if is_local_model_override() {
        return Err(format!("configured Whisper model does not exist: {}", path.display()));
    }

    let dir = path.parent().ok_or("invalid model path")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("could not create whisper model dir: {e}"))?;

    let _ = app.emit(
        "karen-pipeline",
        serde_json::json!({
            "stage": "model-missing",
            "detail": "Setting up speech recognition for the first time, this may take a minute..."
        }),
    );

    let filename = model_filename();
    let url = format!("{MODEL_BASE_URL}{filename}");
    log::info!("downloading whisper model from {url}");

    let part_path = path.with_extension("part");
    download_with_retries(&url, &part_path).await?;
    tokio::fs::rename(&part_path, &path)
        .await
        .map_err(|e| format!("failed to finalize downloaded model: {e}"))?;

    log::info!("whisper model downloaded to {}", path.display());
    Ok(path)
}

pub async fn ensure_loaded(app: &AppHandle) -> Result<Arc<WhisperContext>, String> {
    let handle = app.state::<WhisperHandle>();
    let mut guard = handle.0.lock().await;
    if let Some(ctx) = guard.as_ref() {
        return Ok(ctx.clone());
    }

    let path = ensure_model_downloaded(app).await?;
    let path_str = path.to_string_lossy().to_string();

    let ctx = tokio::task::spawn_blocking(move || {
        WhisperContext::new_with_params(&path_str, WhisperContextParameters::default())
    })
    .await
    .map_err(|e| format!("whisper model loading task panicked: {e}"))?
    .map_err(|e| format!("failed to load whisper model: {e}"))?;

    let ctx = Arc::new(ctx);
    *guard = Some(ctx.clone());
    Ok(ctx)
}

pub async fn transcribe(ctx: Arc<WhisperContext>, mono_16k: Vec<f32>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let mut state = ctx
            .create_state()
            .map_err(|e| format!("failed to create whisper state: {e}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &mono_16k)
            .map_err(|e| format!("whisper inference failed: {e}"))?;

        let n_segments = state.full_n_segments();

        let mut text = String::new();
        for i in 0..n_segments {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            if let Ok(segment_text) = segment.to_str() {
                text.push_str(segment_text);
            }
        }

        Ok(text.trim().to_string())
    })
    .await
    .map_err(|e| format!("whisper transcription task panicked: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::validate_model_override;

    #[test]
    fn model_override_requires_absolute_path() {
        assert!(validate_model_override("/tmp/ggml-base.en.bin").is_ok());
        assert!(validate_model_override("models/ggml-base.en.bin").is_err());
    }
}
