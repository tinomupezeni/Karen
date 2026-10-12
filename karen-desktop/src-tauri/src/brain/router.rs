//! High-confidence routing for commands that do not need an LLM.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    OpenApp(String),
    SetVolume(i64),
    SystemInfo,
    StopPlayback,
}

// Small reply pools so a canned conversational answer doesn't feel like the
// exact same recording playing on every greeting. Index rotates rather than
// being random so behavior stays deterministic and testable.
const GREETINGS: &[&str] = &[
    "Hi. I'm listening.",
    "Hey. Go ahead.",
    "Hello. What do you need?",
];
const STATUS_CHECKS: &[&str] = &[
    "Still here, still listening.",
    "Right here. Go ahead.",
];
const IDENTITY: &[&str] = &["I'm Friday, your local desktop assistant."];

static GREETING_IDX: AtomicUsize = AtomicUsize::new(0);
static STATUS_IDX: AtomicUsize = AtomicUsize::new(0);

fn pick(options: &[&'static str], counter: &AtomicUsize) -> String {
    let idx = counter.fetch_add(1, Ordering::Relaxed) % options.len();
    options[idx].to_string()
}

/// Plain conversational turns that don't need an LLM round-trip at all --
/// greetings, "are you there" checks, and identity questions. These bypass
/// `run_agentic`'s HTTP call entirely, so the reply reaches TTS as fast as
/// STT finishes instead of waiting on a model response.
pub fn fast_reply(input: &str) -> Option<String> {
    let text = input
        .trim()
        .trim_matches(|c: char| c == '.' || c == '!' || c == '?')
        .to_ascii_lowercase();
    match text.as_str() {
        "hi" | "hey" | "hello" | "yo" | "hiya" | "sup" | "hey there" | "hi there"
        | "hello there" | "good morning" | "good afternoon" | "good evening" => {
            Some(pick(GREETINGS, &GREETING_IDX))
        }
        "are you there" | "you there" | "are you listening" | "you listening"
        | "are you awake" | "can you hear me" => Some(pick(STATUS_CHECKS, &STATUS_IDX)),
        "who are you" | "what are you" | "what's your name" | "whats your name" => {
            Some(IDENTITY[0].to_string())
        }
        _ => None,
    }
}

pub fn route(input: &str) -> Option<Route> {
    let text = input.trim().to_ascii_lowercase();
    if text.is_empty() { return None; }

    if matches!(text.as_str(), "stop" | "cancel" | "stop talking" | "stop playback" | "stop the music" | "be quiet" | "never mind" | "nevermind") {
        return Some(Route::StopPlayback);
    }
    if matches!(text.as_str(), "system status" | "system info" | "system information" | "computer status" | "what is my system status") {
        return Some(Route::SystemInfo);
    }

    for prefix in ["set volume to ", "set the volume to ", "volume to ", "turn volume to "] {
        if let Some(value) = text.strip_prefix(prefix).and_then(|v| v.strip_suffix('%').unwrap_or(v).trim().parse::<i64>().ok()) {
            if (0..=100).contains(&value) { return Some(Route::SetVolume(value)); }
        }
    }

    for prefix in ["open ", "launch ", "start "] {
        if let Some(app) = text.strip_prefix(prefix).map(str::trim).filter(|v| !v.is_empty()) {
            if !app.contains(' ') || app.split_whitespace().count() <= 4 {
                return Some(Route::OpenApp(app.to_string()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{fast_reply, route, Route, GREETINGS};

    #[test]
    fn routes_obvious_commands_without_guessing() {
        assert_eq!(route("Set the volume to 40%"), Some(Route::SetVolume(40)));
        assert_eq!(route("open VS Code"), Some(Route::OpenApp("vs code".into())));
        assert_eq!(route("system status"), Some(Route::SystemInfo));
        assert_eq!(route("stop the music"), Some(Route::StopPlayback));
        assert_eq!(route("make Karen more sarcastic"), None);
    }

    #[test]
    fn rejects_ambiguous_or_invalid_volume_commands() {
        assert_eq!(route("set volume to 140"), None);
        assert_eq!(route("set volume to forty"), None);
    }

    #[test]
    fn greets_without_needing_an_llm() {
        assert!(fast_reply("Hi").is_some());
        assert!(fast_reply("hey there!").is_some());
        assert!(fast_reply("Hello?").is_some());
    }

    #[test]
    fn answers_presence_and_identity_checks() {
        assert!(fast_reply("are you there").is_some());
        assert!(fast_reply("who are you").is_some());
    }

    #[test]
    fn leaves_real_requests_to_the_llm() {
        assert_eq!(fast_reply("open the browser"), None);
        assert_eq!(fast_reply("what's on my calendar today"), None);
    }

    #[test]
    fn rotates_through_greeting_variants_instead_of_repeating() {
        let replies: std::collections::HashSet<_> = (0..GREETINGS.len() * 2)
            .map(|_| fast_reply("hi").unwrap())
            .collect();
        assert!(replies.len() > 1, "expected more than one distinct greeting to show up");
    }
}
