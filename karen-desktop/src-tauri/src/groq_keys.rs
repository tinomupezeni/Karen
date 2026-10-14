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

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

/// Last-known rate-limit state for one key, read from Groq's own
/// `x-ratelimit-*` response headers (present on both success and 429
/// responses).
#[derive(Clone, Copy, Debug)]
struct KeyBudget {
    remaining_tokens: u32,
    recorded_at: Instant,
    /// How long Groq said it would be until this key's token budget resets
    /// to its full limit, measured from `recorded_at`.
    reset_after: Duration,
}

static BUDGETS: OnceLock<Mutex<HashMap<String, KeyBudget>>> = OnceLock::new();
static GLOBAL_COOLDOWN_UNTIL: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn budgets() -> &'static Mutex<HashMap<String, KeyBudget>> {
    BUDGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn global_cooldown_until() -> &'static Mutex<Option<Instant>> {
    GLOBAL_COOLDOWN_UNTIL.get_or_init(|| Mutex::new(None))
}

/// Parses the Go-style duration strings Groq sends in `x-ratelimit-reset-*`
/// headers (e.g. "36.547s", "23m2.4s", "1h2m3s"). No existing dependency
/// handles this format, and it's narrow enough not to warrant adding one.
fn parse_duration(s: &str) -> Option<Duration> {
    if s.is_empty() {
        return None;
    }
    let mut total_secs = 0f64;
    let mut current = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            current.push(c);
        } else if matches!(c, 'h' | 'm' | 's') {
            let value: f64 = current.parse().ok()?;
            let multiplier = match c {
                'h' => 3600.0,
                'm' => 60.0,
                _ => 1.0,
            };
            total_secs += value * multiplier;
            current.clear();
        } else {
            return None;
        }
    }
    if !current.is_empty() {
        return None;
    }
    Some(Duration::from_secs_f64(total_secs))
}

fn parse_retry_after(s: &str) -> Option<Duration> {
    s.parse::<f64>()
        .ok()
        .map(Duration::from_secs_f64)
        .or_else(|| parse_duration(s))
}

/// Records one key's rate-limit state from a response's headers, so the
/// next call can prefer whichever configured key actually has headroom
/// instead of guessing via blind rotation. Safe to call on every response,
/// success or failure -- Groq sends these headers either way.
pub fn record_rate_limit_headers(key: &str, headers: &reqwest::header::HeaderMap) {
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let Some(remaining_tokens) =
        header_str("x-ratelimit-remaining-tokens").and_then(|s| s.parse::<u32>().ok())
    else {
        return;
    };
    let Some(reset_after) = header_str("x-ratelimit-reset-tokens").and_then(parse_duration) else {
        return;
    };
    if let Ok(mut map) = budgets().lock() {
        map.insert(
            key.to_string(),
            KeyBudget {
                remaining_tokens,
                recorded_at: Instant::now(),
                reset_after,
            },
        );
    }
}

/// Records a provider-wide cooldown after a rate limit response. Groq quotas
/// are organization-scoped, so rotating credentials cannot bypass a shared
/// exhausted window.
pub fn record_rate_limit_response(key: &str, status: u16, headers: &reqwest::header::HeaderMap) {
    record_rate_limit_headers(key, headers);
    if status != 429 {
        return;
    }

    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let retry_after = header_str("retry-after")
        .and_then(parse_retry_after)
        .or_else(|| header_str("x-ratelimit-reset-requests").and_then(parse_duration))
        .or_else(|| header_str("x-ratelimit-reset-tokens").and_then(parse_duration))
        .unwrap_or(Duration::from_secs(60));
    if let Ok(mut cooldown) = global_cooldown_until().lock() {
        let until = Instant::now() + retry_after;
        if cooldown.map(|current| current < until).unwrap_or(true) {
            *cooldown = Some(until);
        }
    }
}

/// Returns the remaining organization-wide cooldown, if one is active.
pub fn cooldown_remaining() -> Option<Duration> {
    let mut cooldown = global_cooldown_until().lock().ok()?;
    let until = (*cooldown)?;
    let remaining = until.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        *cooldown = None;
        None
    } else {
        Some(remaining)
    }
}

/// Estimated remaining token budget for a key right now, or `None` if it's
/// never been observed or its last-known reset window has already
/// elapsed (Groq would have refilled it back toward the full limit by
/// then, so a stale low reading must not keep penalizing it forever).
fn estimated_remaining(key: &str) -> Option<u32> {
    let map = budgets().lock().ok()?;
    let budget = map.get(key)?;
    if budget.recorded_at.elapsed() >= budget.reset_after {
        None
    } else {
        Some(budget.remaining_tokens)
    }
}

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
/// (spreads per-key quota usage), wrapping around to cover every key, then
/// reordered so any key we *know* is low on token budget right now sinks to
/// the back instead of being tried first and immediately 429ing. A key with
/// no recent reading (never used, or its window already rolled over) is
/// treated as having a full/unknown budget -- never penalized below a key
/// we know is actually low, but round-robin still breaks ties between
/// unknowns so load keeps spreading across all configured keys over time.
pub fn ordered_keys() -> Vec<String> {
    let keys = configured_keys();
    if keys.len() <= 1 {
        return keys;
    }
    let start = NEXT_SLOT.fetch_add(1, Ordering::SeqCst) % keys.len();
    let mut rotated: Vec<String> = keys
        .iter()
        .cycle()
        .skip(start)
        .take(keys.len())
        .cloned()
        .collect();
    rotated.sort_by_key(|key| std::cmp::Reverse(estimated_remaining(key).unwrap_or(u32::MAX)));
    rotated
}

/// Whether this status means "try the next key": expired/invalid credentials
/// (401/403) or per-key quota/rate limit (429). Anything else (400/404/5xx)
/// is about the request or the service, not the key.
pub fn should_try_next_key(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

#[cfg(test)]
mod tests {
    use super::{
        cooldown_remaining, estimated_remaining, parse_duration, parse_retry_after,
        record_rate_limit_headers, record_rate_limit_response, should_try_next_key,
    };

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

    #[test]
    fn parses_groq_style_duration_strings() {
        assert_eq!(
            parse_duration("36.547s"),
            Some(std::time::Duration::from_secs_f64(36.547))
        );
        assert_eq!(
            parse_duration("23m2.4s"),
            Some(std::time::Duration::from_secs_f64(23.0 * 60.0 + 2.4))
        );
        assert_eq!(
            parse_duration("1h2m3s"),
            Some(std::time::Duration::from_secs_f64(3600.0 + 120.0 + 3.0))
        );
    }

    #[test]
    fn rejects_unparseable_duration_strings() {
        assert_eq!(parse_duration("not a duration"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("5"), None);
    }

    #[test]
    fn parses_numeric_retry_after_seconds() {
        assert_eq!(
            parse_retry_after("5"),
            Some(std::time::Duration::from_secs(5))
        );
    }

    fn headers_with(remaining_tokens: &str, reset_tokens: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-ratelimit-remaining-tokens",
            remaining_tokens.parse().unwrap(),
        );
        headers.insert("x-ratelimit-reset-tokens", reset_tokens.parse().unwrap());
        headers
    }

    #[test]
    fn remembers_remaining_budget_until_its_reset_window_elapses() {
        let key = "test-key-remembers-budget";
        record_rate_limit_headers(key, &headers_with("1234", "5s"));
        assert_eq!(estimated_remaining(key), Some(1234));
    }

    #[test]
    fn treats_budget_as_unknown_once_its_reset_window_has_passed() {
        let key = "test-key-expires-budget";
        record_rate_limit_headers(key, &headers_with("10", "0.01s"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(estimated_remaining(key), None);
    }

    #[test]
    fn ignores_responses_missing_rate_limit_headers() {
        let key = "test-key-no-headers";
        record_rate_limit_headers(key, &reqwest::header::HeaderMap::new());
        assert_eq!(estimated_remaining(key), None);
    }

    #[test]
    fn applies_global_cooldown_from_retry_after() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "5s".parse().unwrap());
        record_rate_limit_response("test-key-global-cooldown", 429, &headers);
        assert!(cooldown_remaining().is_some_and(|remaining| remaining > std::time::Duration::ZERO));
    }
}
