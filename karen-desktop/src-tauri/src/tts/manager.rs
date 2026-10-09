use super::provider::{TtsProvider, TtsError};
use super::groq::GroqProvider;
use super::piper::PiperProvider;
use std::path::PathBuf;

pub struct TtsManager {
    primary: Box<dyn TtsProvider>,
    fallback: Box<dyn TtsProvider>,
    max_retries: usize,
}

impl TtsManager {
    pub fn new() -> Result<Self, String> {
        let primary = GroqProvider::new()?;
        let fallback = PiperProvider::new()?;
        Ok(Self {
            primary: Box::new(primary),
            fallback: Box::new(fallback),
            max_retries: 2,
        })
    }

    #[cfg(test)]
    pub fn with_providers(primary: Box<dyn TtsProvider>, fallback: Box<dyn TtsProvider>) -> Self {
        Self {
            primary,
            fallback,
            max_retries: 2,
        }
    }

    pub async fn generate_audio(&self, text: &str) -> Result<(PathBuf, &'static str), TtsError> {
        let mut attempts = 0;
        loop {
            match self.primary.generate_audio(text).await {
                Ok(path) => return Ok((path, self.primary.name())),
                Err(e) => {
                    attempts += 1;
                    match e {
                        TtsError::Fatal(msg) => {
                            log::error!("[TTS] Primary provider fatal error: {}", msg);
                            return Err(TtsError::Fatal(msg));
                        }
                        TtsError::Retryable(msg) => {
                            log::warn!("[TTS] Primary provider retryable error (attempt {}): {}", attempts, msg);
                            if attempts >= self.max_retries {
                                log::warn!("[TTS] Max retries reached for primary. Falling back to {}.", self.fallback.name());
                                break;
                            }
                            // Brief backoff
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                }
            }
        }

        // Fallback
        match self.fallback.generate_audio(text).await {
            Ok(path) => Ok((path, self.fallback.name())),
            Err(e) => {
                log::error!("[TTS] Fallback provider also failed: {}", e);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockProvider {
        name: &'static str,
        results: std::sync::Mutex<Vec<Result<PathBuf, TtsError>>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn new(name: &'static str, mut results: Vec<Result<PathBuf, TtsError>>, call_count: Arc<AtomicUsize>) -> Self {
            results.reverse();
            Self { name, results: std::sync::Mutex::new(results), call_count }
        }
    }

    #[async_trait::async_trait]
    impl TtsProvider for MockProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn generate_audio(&self, _text: &str) -> Result<PathBuf, TtsError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            results.pop().unwrap_or(Err(TtsError::Fatal("Unexpected call".to_string())))
        }
    }

    #[tokio::test]
    async fn test_manager_success_primary() {
        let count = Arc::new(AtomicUsize::new(0));
        let p = Box::new(MockProvider::new("Groq", vec![Ok(PathBuf::from("ok.wav"))], count.clone()));
        let f = Box::new(MockProvider::new("Piper", vec![], Arc::new(AtomicUsize::new(0))));
        
        let m = TtsManager::with_providers(p, f);
        let res = m.generate_audio("test").await;
        
        assert!(res.is_ok());
        assert_eq!(res.unwrap().1, "Groq");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_manager_retryable_then_fallback() {
        let pc = Arc::new(AtomicUsize::new(0));
        let p = Box::new(MockProvider::new("Groq", vec![
            Err(TtsError::Retryable("timeout".into())),
            Err(TtsError::Retryable("timeout".into())),
        ], pc.clone()));
        
        let fc = Arc::new(AtomicUsize::new(0));
        let f = Box::new(MockProvider::new("Piper", vec![Ok(PathBuf::from("fallback.wav"))], fc.clone()));
        
        let m = TtsManager::with_providers(p, f);
        let res = m.generate_audio("test").await;
        
        assert!(res.is_ok());
        assert_eq!(res.unwrap().1, "Piper");
        assert_eq!(pc.load(Ordering::SeqCst), 2);
        assert_eq!(fc.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_manager_fatal_no_fallback() {
        let pc = Arc::new(AtomicUsize::new(0));
        let p = Box::new(MockProvider::new("Groq", vec![
            Err(TtsError::Fatal("auth".into())),
        ], pc.clone()));
        
        let fc = Arc::new(AtomicUsize::new(0));
        let f = Box::new(MockProvider::new("Piper", vec![], fc.clone()));
        
        let m = TtsManager::with_providers(p, f);
        let res = m.generate_audio("test").await;
        
        assert!(matches!(res, Err(TtsError::Fatal(_))));
        assert_eq!(pc.load(Ordering::SeqCst), 1);
        assert_eq!(fc.load(Ordering::SeqCst), 0); // Should not fallback
    }
}
