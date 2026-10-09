use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn resolve_path(path_str: &str) -> PathBuf {
    let path = Path::new(path_str);
    if path.has_root() {
        path.to_path_buf()
    } else {
        // Resolve relative to user's home directory if possible, fallback to current dir
        if let Some(home) = dirs::home_dir() {
            if path_str == "~" || path_str.starts_with("~/") {
                let stripped = path_str.strip_prefix("~/").unwrap_or("");
                home.join(stripped)
            } else {
                home.join(path)
            }
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    }
}

pub async fn list_directory(args: Value) -> Value {
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return json!({ "ok": false, "error": "missing 'path' parameter" }),
    };

    let target_dir = resolve_path(path_str);
    if !target_dir.exists() || !target_dir.is_dir() {
        return json!({ "ok": false, "error": format!("Directory not found: {}", target_dir.display()) });
    }

    let mut entries = Vec::new();
    let mut read_dir = match tokio::fs::read_dir(&target_dir).await {
        Ok(rd) => rd,
        Err(e) => return json!({ "ok": false, "error": format!("Failed to read dir: {}", e) }),
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        // Skip hidden files to save token space unless specifically needed
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
        let kind = if is_dir { "directory" } else { "file" };
        entries.push(json!({ "name": name, "type": kind }));
    }

    entries.sort_by(|a, b| {
        let a_type = a["type"].as_str().unwrap();
        let b_type = b["type"].as_str().unwrap();
        if a_type == b_type {
            a["name"].as_str().unwrap().cmp(b["name"].as_str().unwrap())
        } else if a_type == "directory" {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });

    let total = entries.len();
    if total > 50 {
        entries.truncate(50);
        entries.push(json!({ "name": format!("... and {} more items", total - 50), "type": "info" }));
    }

    json!({
        "ok": true,
        "path": target_dir.display().to_string(),
        "contents": entries
    })
}

pub async fn read_file(args: Value) -> Value {
    let path_str = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return json!({ "ok": false, "error": "missing 'path' parameter" }),
    };

    let target_file = resolve_path(path_str);
    if !target_file.exists() || !target_file.is_file() {
        return json!({ "ok": false, "error": format!("File not found: {}", target_file.display()) });
    }

    match tokio::fs::read_to_string(&target_file).await {
        Ok(content) => {
            // Truncate if too long to avoid blowing up the context window
            let max_chars = 4000;
            let display_content = if content.len() > max_chars {
                format!("{}... (truncated, {} bytes total)", &content[..max_chars], content.len())
            } else {
                content
            };
            json!({ "ok": true, "path": target_file.display().to_string(), "content": display_content })
        }
        Err(e) => json!({ "ok": false, "error": format!("Failed to read file: {}", e) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolve_path("~/"), home);
        assert_eq!(resolve_path("~/Projects"), home.join("Projects"));
    }

    #[tokio::test]
    async fn test_list_directory() {
        let args = json!({ "path": "~/" });
        let result = list_directory(args).await;
        assert_eq!(result["ok"], true);
        assert!(result["contents"].as_array().unwrap().len() > 0);
    }
}
