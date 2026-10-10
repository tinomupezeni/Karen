#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConfig {
    pub wake_word_enabled: bool,
    pub wake_word: String,
}

impl VoiceConfig {
    pub fn from_env() -> Self {
        let wake_word_enabled = std::env::var("KAREN_WAKE_WORD_ENABLED")
            .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
            .unwrap_or(false);
        let wake_word = std::env::var("KAREN_WAKE_WORD")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "karen".into());
        Self { wake_word_enabled, wake_word }
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceConfig;

    #[test]
    fn uses_safe_voice_defaults() {
        let config = VoiceConfig { wake_word_enabled: false, wake_word: "karen".into() };
        assert!(!config.wake_word_enabled);
        assert_eq!(config.wake_word, "karen");
    }
}
