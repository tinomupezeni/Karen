use std::path::PathBuf;

use serde_json::{json, Value};

const ALIASES: &[(&str, &str)] = &[
    ("vs code", "code"),
    ("vscode", "code"),
    ("visual studio code", "code"),
    ("code editor", "code"),
    ("terminal", "gnome-terminal"),
    ("command line", "gnome-terminal"),
    ("file manager", "nautilus"),
    ("files", "nautilus"),
    ("browser", "firefox"),
    ("web browser", "firefox"),
    ("firefox", "firefox"),
    ("text editor", "gedit"),
    ("calculator", "gnome-calculator"),
];

const REJECTED_CHARS: &[char] = &['/', '\\', ';', '&', '|', '$', '`', '\n', '\r', ' '];

fn resolve(name: &str) -> Option<PathBuf> {
    let normalized = name.trim().to_lowercase();
    let candidate = ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, bin)| bin.to_string())
        .unwrap_or(normalized);

    if candidate.is_empty() || candidate.contains(REJECTED_CHARS) {
        return None;
    }

    which::which(&candidate).ok()
}

pub async fn run(args: Value) -> Value {
    let Some(name) = args.get("name").and_then(Value::as_str) else {
        return json!({ "ok": false, "error": "missing 'name' argument" });
    };

    match resolve(name) {
        Some(path) => match std::process::Command::new(&path).spawn() {
            Ok(_) => json!({ "ok": true, "launched": path.to_string_lossy() }),
            Err(e) => json!({ "ok": false, "error": format!("failed to launch {name}: {e}") }),
        },
        None => json!({ "ok": false, "error": format!("no application matching '{name}' found") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_alias() {
        // `code` is confirmed present at /usr/bin/code on dev machines this
        // was built on; if that's ever not true, this test's intent still
        // holds (alias -> bare binary name -> PATH lookup), just skip it.
        if which::which("code").is_ok() {
            assert!(resolve("vs code").is_some());
            assert!(resolve("VS Code").is_some());
        }
    }

    #[test]
    fn rejects_unknown_app() {
        assert!(resolve("definitely-not-a-real-app-xyz123").is_none());
    }

    #[test]
    fn rejects_shell_injection_attempts() {
        assert!(resolve("code; rm -rf ~").is_none());
        assert!(resolve("code && echo pwned").is_none());
        assert!(resolve("$(whoami)").is_none());
        assert!(resolve("/bin/sh").is_none());
    }
}
