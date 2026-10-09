use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum TtsError {
    Retryable(String),
    Fatal(String),
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TtsError::Retryable(msg) => write!(f, "Retryable: {}", msg),
            TtsError::Fatal(msg) => write!(f, "Fatal: {}", msg),
        }
    }
}

#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync {
    /// Generates audio for a given text snippet and returns the path to the temporary .wav file.
    async fn generate_audio(&self, text: &str) -> Result<PathBuf, TtsError>;
    
    /// A human-readable name of the provider (e.g. "Groq", "Piper")
    fn name(&self) -> &'static str;
}
