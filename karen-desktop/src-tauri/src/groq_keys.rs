//! Shared Groq API key rotation.
//!
//! Every Groq call starts at the next round-robin slot and fails over to the
//! following key when the service reports expired/invalid credentials
//! (401/403) or exhausted per-key quota (429). A single configured key keeps
//! the previous behaviour exactly.
//!
//! Supported env shapes (combined in this order, deduped):
//! - `GROQ_API_KEYS`: comma- or newline-separated list
//! - `GROQ_API_KEY`: primary key
//! - `GROQ_API_KEY_2` … `GROQ_API_KEY_9`: fallback keys
//!
//! Key values are never logged; only key counts and 1-based positions.

use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

fn push_unique(keys: &mut Vec<String>, value: &str) {
    let value = value.trim().to_string();
    if !value.is_empty() && !keys.iter().any(|existing| existing == &value) {
        keys.push(value);
    }
}

/// All configured Groq keys in priority order, deduped.
pub fn configured_keys() -> Vec<String> {
    let mut keys = Vec::new();
    if let Ok(list) = std::env::var("GROQ_API_KEYS") {
        for part in list.split([',', '\n']) {
            push_unique(&mut keys, part);
        }
    }
    if let Ok(key) = std::env::var("GROQ_API_KEY") {
        push_unique(&mut keys, &key);
    }
    for slot in 2..=9 {
        if let Ok(key) = std::env::var(format!("GROQ_API_KEY_{slot}")) {
            push_unique(&mut keys, &key);
        }
    }
    keys
}

/// Keys in call order: rotated so consecutive calls start at successive slots
/// (spreads per-key quota usage), wrapping around to cover every key.
pub fn ordered_keys() -> Vec<String> {
    let keys = configured_keys();
    if keys.len() <= 1 {
        return keys;
    }
    let start = NEXT_SLOT.fetch_add(1, Ordering::SeqCst) % keys.len();
    keys.iter()
        .cycle()
        .skip(start)
        .take(keys.len())
        .cloned()
        .collect()
}

/// Whether this status means "try the next key": expired/invalid credentials
/// (401/403) or per-key quota/rate limit (429). Anything else (400/404/5xx)
/// is about the request or the service, not the key.
pub fn should_try_next_key(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

#[cfg(test)]
mod tests {
    use super::should_try_next_key;

    #[test]
    fn rotates_on_expired_key_or_quota() {
        assert!(should_try_next_key(401));
        assert!(should_try_next_key(403));
        assert!(should_try_next_key(429));
    }

    #[test]
    fn does_not_rotate_on_request_or_server_errors() {
        assert!(!should_try_next_key(200));
        assert!(!should_try_next_key(400));
        assert!(!should_try_next_key(404));
        assert!(!should_try_next_key(500));
    }
}
