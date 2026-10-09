use super::provider::{TtsProvider, TtsError};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct PiperProvider {
    model_path: PathBuf,
}

impl PiperProvider {
    pub fn new() -> Result<Self, String> {
        let mut model_path = std::env::var("PIPER_MODEL_PATH")
            .unwrap_or_else(|_| "/usr/share/piper-voices/en/en_US/lessac/medium/en_US-lessac-medium.onnx".to_string());
        
        let mut path = PathBuf::from(&model_path);
        if (!path.exists()) {
            let local_path = "/home/shadowe/.local/share/piper-voices/en/en_US/lessac/medium/en_US-lessac-medium.onnx";
            if PathBuf::from(local_path).exists() {
                path = PathBuf::from(local_path);
            }
        }
        
        Ok(Self { model_path: path })
    }

    fn unique_temp_wav_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("karen-tts-piper-{nanos}.wav"))
    }
}

#[async_trait::async_trait]
impl TtsProvider for PiperProvider {
    fn name(&self) -> &'static str {
        "Piper"
    }

    async fn generate_audio(&self, text: &str) -> Result<PathBuf, TtsError> {
        if !self.model_path.exists() {
            return Err(TtsError::Fatal(format!("Piper model not found at {}", self.model_path.display())));
        }

        let out_path = Self::unique_temp_wav_path();
        
        use std::process::Stdio;
        
        let mut child = tokio::process::Command::new("piper")
            .arg("--model")
            .arg(&self.model_path)
            .arg("--output_file")
            .arg(&out_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TtsError::Fatal(format!("Failed to spawn piper: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(text.as_bytes()).await {
                return Err(TtsError::Retryable(format!("Failed to write to piper stdin: {e}")));
            }
        }

        let output = child.wait_with_output().await
            .map_err(|e| TtsError::Retryable(format!("Failed to wait for piper: {e}")))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(TtsError::Fatal(format!("Piper failed with status {}: {}", output.status, err)));
        }

        Ok(out_path)
    }
}
