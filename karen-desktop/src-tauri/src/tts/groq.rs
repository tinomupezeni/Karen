use super::provider::{TtsProvider, TtsError};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const TTS_URL: &str = "https://api.groq.com/openai/v1/audio/speech";
const DEFAULT_MODEL: &str = "canopylabs/orpheus-v1-english";
const DEFAULT_VOICE: &str = "hannah";

pub struct GroqProvider {
    client: reqwest::Client,
}

impl GroqProvider {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
            
        Ok(Self { client })
    }
    
    fn unique_temp_wav_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("karen-tts-{nanos}.wav"))
    }
}

#[async_trait::async_trait]
impl TtsProvider for GroqProvider {
    fn name(&self) -> &'static str {
        "Groq"
    }

    async fn generate_audio(&self, text: &str) -> Result<PathBuf, TtsError> {
        let keys = crate::groq_keys::ordered_keys();
        if keys.is_empty() {
            return Err(TtsError::Fatal("GROQ_API_KEY is not set".to_string()));
        }
        let model = std::env::var("GROQ_TTS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let voice = std::env::var("GROQ_TTS_VOICE").unwrap_or_else(|_| DEFAULT_VOICE.to_string());

        let mut last_detail = String::new();
        for (key_idx, key) in keys.iter().enumerate() {
            let body = serde_json::json!({
                "model": model,
                "input": text,
                "voice": voice,
                "response_format": "wav",
            });

            let response = self.client
                .post(TTS_URL)
                .bearer_auth(key)
                .json(&body)
                .send()
                .await
                .map_err(|e| TtsError::Retryable(format!("TTS request failed: {e}")))?;

            let status = response.status();
            if status.is_success() {
                let audio = response
                    .bytes()
                    .await
                    .map_err(|e| TtsError::Retryable(format!("failed to read TTS audio: {e}")))?;

                let path = Self::unique_temp_wav_path();
                tokio::fs::write(&path, &audio)
                    .await
                    .map_err(|e| TtsError::Fatal(format!("failed to write TTS audio to disk: {e}")))?;

                return Ok(path);
            }

            let detail = response.text().await.unwrap_or_default();
            let more_keys = key_idx + 1 < keys.len();
            if more_keys && crate::groq_keys::should_try_next_key(status.as_u16()) {
                log::warn!("[TTS] Groq key {}/{} rejected ({}), rotating to next key", key_idx + 1, keys.len(), status);
                last_detail = format!("{status}: {detail}");
                continue;
            }
            if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 400 || status.as_u16() == 404 {
                return Err(TtsError::Fatal(format!("TTS returned {status}: {detail}")));
            }
            return Err(TtsError::Retryable(format!("TTS returned {status}: {detail}")));
        }

        // Every key was rejected as expired or rate-limited. Report it as
        // retryable so the TTS manager falls back to Piper instead of leaving
        // the turn silent.
        Err(TtsError::Retryable(format!(
            "all {} Groq keys rejected (last: {last_detail})",
            keys.len()
        )))
    }
}
