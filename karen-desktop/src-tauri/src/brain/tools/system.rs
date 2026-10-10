use serde_json::{json, Value};

pub fn open_url(args: Value) -> Value {
    let Some(url) = args.get("url").and_then(Value::as_str).map(str::trim) else { return json!({"ok":false,"error":"missing 'url' argument"}); };
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.chars().any(|c| matches!(c, '\n'|'\r'|';'|'&'|'|'|'`'|'$')) { return json!({"ok":false,"error":"only safe http(s) URLs are allowed"}); }
    match std::process::Command::new("xdg-open").arg(url).spawn() { Ok(_) => json!({"ok":true,"opened":url}), Err(e) => json!({"ok":false,"error":e.to_string()}) }
}

pub fn get_system_info() -> Value {
    let os = std::process::Command::new("uname").arg("-a").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "unknown".into());
    json!({"ok":true,"platform":os,"hostname":std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())})
}

pub fn list_processes() -> Value {
    match std::process::Command::new("ps").args(["-eo", "pid,comm,%cpu,%mem", "--sort=-%cpu"]).output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().take(21).collect();
            json!({ "ok": true, "processes": lines.join("\n") })
        }
        Ok(status) => json!({ "ok": false, "error": format!("ps exited with {}", status.status) }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

pub fn set_volume(args: Value) -> Value {
    let Some(value) = args.get("percent").and_then(Value::as_i64) else { return json!({"ok":false,"error":"missing 'percent' argument"}); };
    if !(0..=100).contains(&value) { return json!({"ok":false,"error":"volume must be between 0 and 100"}); }
    match std::process::Command::new("pactl").args(["set-sink-volume","@DEFAULT_SINK@",&format!("{value}%")]).status() { Ok(s) if s.success()=>json!({"ok":true,"percent":value}), Ok(s)=>json!({"ok":false,"error":s.to_string()}), Err(e)=>json!({"ok":false,"error":e.to_string()}) }
}

pub fn screenshot() -> Value {
    let path = format!("/tmp/karen-screenshot-{}.png", chrono::Utc::now().timestamp_millis());
    match std::process::Command::new("gnome-screenshot").args(["-f",&path]).status() { Ok(s) if s.success()=>json!({"ok":true,"path":path}), Ok(s)=>json!({"ok":false,"error":s.to_string()}), Err(e)=>json!({"ok":false,"error":e.to_string()}) }
}

pub fn screen_ocr() -> Value {
    let path = format!("/tmp/karen-ocr-{}.png", chrono::Utc::now().timestamp_millis());
    let capture = std::process::Command::new("gnome-screenshot").args(["-f", &path]).status();
    if !matches!(capture, Ok(status) if status.success()) {
        return json!({ "ok": false, "error": "desktop screenshot is unavailable" });
    }
    match std::process::Command::new("tesseract").args([&path, "stdout", "-l", "eng"]).output() {
        Ok(output) if output.status.success() => json!({
            "ok": true,
            "text": String::from_utf8_lossy(&output.stdout).trim().to_string(),
            "screenshot": path
        }),
        Ok(_) => json!({ "ok": false, "error": "tesseract OCR is unavailable or failed" }),
        Err(e) => json!({ "ok": false, "error": format!("tesseract unavailable: {e}") }),
    }
}

pub fn press_key(args: Value) -> Value {
    let Some(key) = args.get("key").and_then(Value::as_str).map(str::to_uppercase) else { return json!({"ok":false,"error":"missing 'key' argument"}); };
    if !matches!(key.as_str(), "ENTER"|"ESC"|"TAB"|"SPACE"|"CTRL+C"|"CTRL+L"|"ALT+F4"|"CTRL+ALT+T") { return json!({"ok":false,"error":"key is not allowlisted"}); }
    match std::process::Command::new("xdotool").args(["key",&key]).status() { Ok(s) if s.success()=>json!({"ok":true,"key":key}), Ok(s)=>json!({"ok":false,"error":s.to_string()}), Err(e)=>json!({"ok":false,"error":e.to_string()}) }
}

pub fn type_text(args: Value) -> Value {
    let Some(text) = args.get("text").and_then(Value::as_str) else { return json!({"ok":false,"error":"missing 'text' argument"}); };
    if text.is_empty() || text.len()>4000 || text.contains(['\n','\r']) { return json!({"ok":false,"error":"text must be 1-4000 characters without newlines"}); }
    match std::process::Command::new("xdotool").args(["type","--clearmodifiers",text]).status() { Ok(s) if s.success()=>json!({"ok":true,"characters":text.chars().count()}), Ok(s)=>json!({"ok":false,"error":s.to_string()}), Err(e)=>json!({"ok":false,"error":e.to_string()}) }
}
