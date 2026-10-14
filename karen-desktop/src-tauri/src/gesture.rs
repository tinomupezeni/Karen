//! Friday-side consumer for the friday-gesture sidecar.
//!
//! Split of responsibilities: the sidecar is a dumb sensor (always spawned
//! with `--dry-run`, so it only emits detection events and never acts).
//! Policy and actuation live here, in Friday, next to the existing tool
//! risk model.
//!
//! Gesture actions bypass the confirmation gate on purpose: a deliberate
//! physical gesture is a direct user command, exactly like pressing
//! Ctrl+Space or a keyboard media key -- neither of which pops a
//! confirmation either. Only inherently safe, reversible media/volume
//! actions are mapped; anything beyond that must go through the normal
//! LLM tool + confirmation path.
//!
//! Event protocol (NDJSON on the sidecar's stdout, see friday-gesture/):
//!   {"type": "gesture", "gesture": "fist", "hand": "Right", ...}
//!
//! Configuration (read fresh each loop so flips apply within seconds):
//! - `FRIDAY_GESTURE_ENABLED=true` to run the sidecar (default: off).
//! - `FRIDAY_GESTURE_DIR` sidecar directory (default: repo checkout path).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Emitter};

static SIDECAR_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn child_slot() -> &'static Mutex<Option<Child>> {
    SIDECAR_CHILD.get_or_init(|| Mutex::new(None))
}

pub fn is_enabled() -> bool {
    std::env::var("FRIDAY_GESTURE_ENABLED")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "on"))
        .unwrap_or(false)
}

/// Runtime on/off switch for the UI toggle. `supervise`'s loop re-reads
/// `is_enabled()` on its own, so flipping to enabled just lets the next
/// loop iteration spawn the sidecar; flipping to disabled kills whatever
/// is running right now instead of waiting for it to exit on its own.
pub fn set_enabled(enabled: bool) {
    unsafe {
        std::env::set_var(
            "FRIDAY_GESTURE_ENABLED",
            if enabled { "true" } else { "false" },
        );
    }
    if !enabled {
        stop();
    }
}

fn sidecar_dir() -> String {
    std::env::var("FRIDAY_GESTURE_DIR")
        .unwrap_or_else(|_| "/home/shadowe/Projects/Karen/friday-gesture".into())
}

/// Gesture name -> safe action. Unknown gestures are ignored.
pub fn action_for_gesture(gesture: &str) -> Option<&'static str> {
    match gesture {
        "fist" => Some("play_pause"),
        "peace" => Some("next_track"),
        "open_palm" => Some("prev_track"),
        "pinch" => Some("toggle_mute"),
        "thumbs_up" => Some("volume_up"),
        _ => None,
    }
}

/// Extract (gesture, hand) from one NDJSON event line.
pub fn parse_gesture_event(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "gesture" {
        return None;
    }
    Some((
        value.get("gesture")?.as_str()?.to_string(),
        value.get("hand")?.as_str().unwrap_or("?").to_string(),
    ))
}

fn shell(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn {cmd}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn mpris_players() -> Vec<String> {
    let Ok(out) = shell(
        "dbus-send",
        &[
            "--session",
            "--print-reply=literal",
            "--dest=org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus.ListNames",
        ],
    ) else {
        return Vec::new();
    };
    out.split('"')
        .filter(|s| s.contains("org.mpris.MediaPlayer2"))
        .map(str::to_string)
        .collect()
}

fn mpris(method: &str) -> String {
    let players = mpris_players();
    if players.is_empty() {
        return "no media players found".into();
    }
    let mut done = 0;
    for player in &players {
        if shell(
            "dbus-send",
            &[
                "--session",
                "--type=method_call",
                &format!("--dest={player}"),
                "/org/mpris/MediaPlayer2",
                &format!("org.mpris.MediaPlayer2.Player.{method}"),
            ],
        )
        .is_ok()
        {
            done += 1;
        }
    }
    format!("{method} sent to {done}/{} player(s)", players.len())
}

/// Execute a safe gesture action. Best-effort: always returns a human
/// message for the HUD, never fails the caller.
pub fn run_action(action: &str) -> String {
    match action {
        "play_pause" => mpris("PlayPause"),
        "next_track" => mpris("Next"),
        "prev_track" => mpris("Previous"),
        "toggle_mute" => match shell("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "toggle"]) {
            Ok(_) => "speaker mute toggled".into(),
            Err(e) => e,
        },
        "volume_up" => match shell("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+5%"]) {
            Ok(_) => "volume up".into(),
            Err(e) => e,
        },
        _ => format!("unknown gesture action: {action}"),
    }
}

fn handle_event_line(app: &AppHandle, line: &str) {
    let Some((gesture, hand)) = parse_gesture_event(line) else {
        return;
    };
    let Some(action) = action_for_gesture(&gesture) else {
        log::info!("[gesture] unmapped gesture ignored: {gesture} ({hand})");
        return;
    };
    let result = run_action(action);
    log::info!("[gesture] {hand} {gesture} -> {action}: {result}");
    {
        use tauri::Manager;
        let memory = app.state::<crate::memory::MemoryEngine>();
        let db = tauri::async_runtime::block_on(memory.db.lock());
        let description =
            format!("Gesture: {gesture} | Hand: {hand} | Action: {action} | Result: {result}");
        if let Err(error) =
            db.record_event("gesture_action", &description, Some("gesture::sidecar"))
        {
            log::warn!("[gesture] could not record action: {error}");
        }
    }
    let _ = app.emit(
        "karen-pipeline",
        serde_json::json!({ "stage": "running-tool", "detail": format!("Gesture: {gesture} → {result}") }),
    );
}

fn spawn_sidecar() -> Result<Child, String> {
    let dir = sidecar_dir();
    let python = format!("{dir}/.venv/bin/python");
    Command::new(&python)
        .arg("main.py")
        .arg("--dry-run")
        .arg("--no-preview")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn gesture sidecar ({python}): {e}"))
}

/// Supervise the sidecar forever: (re)spawn while enabled, stream its
/// events, back off on crashes. Runs on its own thread from Tauri setup.
pub fn supervise(app: AppHandle) {
    loop {
        if !is_enabled() {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        // Reap any previous child before (re)spawning.
        if let Some(mut child) = child_slot().lock().unwrap().take() {
            let _ = child.kill();
        }
        let mut child = match spawn_sidecar() {
            Ok(child) => child,
            Err(e) => {
                log::warn!("[gesture] {e}; retrying in 10s");
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
        };
        log::info!("[gesture] sidecar started");
        let stdout = child.stdout.take();
        *child_slot().lock().unwrap() = Some(child);
        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if line.contains("\"type\": \"status\"") || line.contains("\"type\": \"warn\"") {
                    log::info!("[gesture] sidecar: {line}");
                    continue;
                }
                handle_event_line(&app, &line);
            }
        }
        log::warn!("[gesture] sidecar exited; restarting in 5s");
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Best-effort stop (app quit). Leaves nothing holding the camera.
pub fn stop() {
    if let Some(mut child) = child_slot().lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::{action_for_gesture, parse_gesture_event};

    #[test]
    fn maps_known_gestures_to_safe_actions() {
        assert_eq!(action_for_gesture("fist"), Some("play_pause"));
        assert_eq!(action_for_gesture("peace"), Some("next_track"));
        assert_eq!(action_for_gesture("open_palm"), Some("prev_track"));
        assert_eq!(action_for_gesture("pinch"), Some("toggle_mute"));
        assert_eq!(action_for_gesture("thumbs_up"), Some("volume_up"));
    }

    #[test]
    fn ignores_unknown_gestures() {
        assert_eq!(action_for_gesture("jazz_hands"), None);
        assert_eq!(action_for_gesture(""), None);
    }

    #[test]
    fn parses_gesture_events() {
        let line = r#"{"ts": 1.0, "type": "gesture", "gesture": "fist", "hand": "Right"}"#;
        assert_eq!(
            parse_gesture_event(line),
            Some(("fist".to_string(), "Right".to_string()))
        );
    }

    #[test]
    fn ignores_non_gesture_lines() {
        assert_eq!(
            parse_gesture_event(r#"{"type": "status", "status": "started"}"#),
            None
        );
        assert_eq!(parse_gesture_event("not json"), None);
        assert_eq!(parse_gesture_event(r#"{"type": "gesture"}"#), None);
    }
}
