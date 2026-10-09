use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const DEFAULT_ROOT: &str = "/home/shadowe/Projects/SOFTWARE-ENGINEER";
const ROOT_ENV: &str = "KAREN_DEVLOG_ROOT";

const TECHNICAL_DIRS: &[&str] = &[
    "Architecture_and_Design",
    "Backend_and_API",
    "Database_and_State",
    "DevOps_and_Infrastructure",
    "Frontend_and_UI",
    "Integrations_and_Auth",
    "Mobile_Apps",
];
const REPORTS_DIR: &str = "reports";

const MAX_LISTED_ENTRIES: usize = 200;
const MAX_READ_PATHS: usize = 15;
const MAX_FILE_CHARS: usize = 6000;

fn devlog_root() -> PathBuf {
    std::env::var(ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_ROOT))
}

fn is_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_digit())
}

/// Dev-log filenames are `[PROJECT]-YYYY-MM-DD-description.md`, but PROJECT
/// itself can contain hyphens (e.g. "tese-marketplace"), so a naive split
/// can't assume a fixed field count. Instead, scan the hyphen-separated
/// parts for the first run of three that look like a year/month/day and
/// treat everything before that as the project name.
fn parse_filename(fname: &str) -> Option<(String, String)> {
    let stem = fname.strip_suffix(".md")?;
    let parts: Vec<&str> = stem.split('-').collect();

    for i in 0..parts.len() {
        if i + 2 >= parts.len() {
            break;
        }
        if is_digits(parts[i], 4) && is_digits(parts[i + 1], 2) && is_digits(parts[i + 2], 2) {
            if i == 0 {
                return None; // no project name before the date
            }
            let project = parts[..i].join("-");
            let date = format!("{}-{}-{}", parts[i], parts[i + 1], parts[i + 2]);
            return Some((project, date));
        }
    }
    None
}

pub async fn list(args: Value) -> Value {
    let days = args.get("days").and_then(Value::as_i64).unwrap_or(1).max(1);
    let cutoff = (chrono::Local::now().date_naive() - chrono::Duration::days(days - 1))
        .format("%Y-%m-%d")
        .to_string();

    let root = devlog_root();
    let mut entries = Vec::new();

    for dir in TECHNICAL_DIRS.iter().chain(std::iter::once(&REPORTS_DIR)) {
        let full_dir = root.join(dir);
        let Ok(mut read_dir) = tokio::fs::read_dir(&full_dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let fname = entry.file_name().to_string_lossy().into_owned();
            let Some((project, date)) = parse_filename(&fname) else {
                continue;
            };
            if date.as_str() >= cutoff.as_str() {
                entries.push(json!({
                    "path": format!("{dir}/{fname}"),
                    "project": project,
                    "date": date,
                }));
            }
        }
    }

    entries.sort_by(|a, b| b["date"].as_str().cmp(&a["date"].as_str()));
    entries.truncate(MAX_LISTED_ENTRIES);

    json!({ "ok": true, "entries": entries })
}

async fn read_one_confined(root: &Path, rel: &str) -> Value {
    let Some((dir, _)) = rel.split_once('/') else {
        return json!({ "path": rel, "ok": false, "error": "invalid path shape" });
    };
    if !TECHNICAL_DIRS.contains(&dir) && dir != REPORTS_DIR {
        return json!({ "path": rel, "ok": false, "error": "path outside allowed dev-log directories" });
    }
    if !rel.ends_with(".md") || rel.contains("..") {
        return json!({ "path": rel, "ok": false, "error": "invalid path" });
    }

    let candidate = root.join(rel);
    let Ok(canon) = candidate.canonicalize() else {
        return json!({ "path": rel, "ok": false, "error": "file not found" });
    };
    if !canon.starts_with(root) {
        return json!({ "path": rel, "ok": false, "error": "path escapes dev-log root" });
    }

    match tokio::fs::read_to_string(&canon).await {
        Ok(content) => {
            let truncated = content.chars().count() > MAX_FILE_CHARS;
            let content: String = if truncated {
                content.chars().take(MAX_FILE_CHARS).collect()
            } else {
                content
            };
            json!({ "path": rel, "ok": true, "content": content, "truncated": truncated })
        }
        Err(e) => json!({ "path": rel, "ok": false, "error": e.to_string() }),
    }
}

pub async fn read(args: Value) -> Value {
    let Some(paths) = args.get("paths").and_then(Value::as_array) else {
        return json!({ "ok": false, "error": "missing 'paths' array argument" });
    };

    let root = devlog_root();
    let Ok(root) = root.canonicalize() else {
        return json!({ "ok": false, "error": "dev-log root does not exist" });
    };

    let mut results = Vec::new();
    for p in paths.iter().take(MAX_READ_PATHS) {
        if let Some(rel) = p.as_str() {
            results.push(read_one_confined(&root, rel).await);
        }
    }

    json!({ "ok": true, "files": results })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_project_name() {
        let (project, date) =
            parse_filename("HBEC-2026-09-13-groq-model-retired.md").unwrap();
        assert_eq!(project, "HBEC");
        assert_eq!(date, "2026-09-13");
    }

    #[test]
    fn parses_hyphenated_project_name() {
        let (project, date) = parse_filename(
            "tese-marketplace-2026-05-26-production-issues-products-images.md",
        )
        .unwrap();
        assert_eq!(project, "tese-marketplace");
        assert_eq!(date, "2026-05-26");
    }

    #[test]
    fn rejects_filenames_without_a_date() {
        assert!(parse_filename("theme.md").is_none());
        assert!(parse_filename("Mattermost App-lessons-engineering-principles.md").is_none());
    }

    #[test]
    fn rejects_non_md_files() {
        assert!(parse_filename("HBEC-2026-09-13-notes.txt").is_none());
    }

    #[tokio::test]
    async fn list_real_repo_includes_known_recent_entries() {
        // Uses the real SOFTWARE-ENGINEER checkout on this machine rather
        // than a fixture, since the goal here is confirming the scan logic
        // against real, already-reviewed filenames.
        let result = list(json!({ "days": 400 })).await;
        assert_eq!(result["ok"], true);
        let entries = result["entries"].as_array().unwrap();
        let paths: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
        assert!(
            paths
                .iter()
                .any(|p| p.contains("KAREN-2026-10-08-global-shortcut-unsuited-to-wayland.md")),
            "expected to find today's KAREN entry, got: {paths:?}"
        );
        // Must never surface non-dev-log directories.
        assert!(paths.iter().all(|p| !p.starts_with("Principal_Engineer/")));
        assert!(paths.iter().all(|p| !p.starts_with("Lessons/")));
        assert!(paths.iter().all(|p| !p.starts_with("References/")));
    }

    #[tokio::test]
    async fn read_rejects_path_traversal() {
        let result = read(json!({
            "paths": ["../../../etc/passwd", "Architecture_and_Design/../../../etc/passwd"]
        }))
        .await;
        let files = result["files"].as_array().unwrap();
        for f in files {
            assert_eq!(f["ok"], false, "traversal attempt should be rejected: {f:?}");
        }
    }

    #[tokio::test]
    async fn read_rejects_path_outside_allowed_dirs() {
        let result = read(json!({ "paths": ["templates/issue-template.md"] })).await;
        let files = result["files"].as_array().unwrap();
        assert_eq!(files[0]["ok"], false);
    }

    #[tokio::test]
    async fn read_real_known_file_succeeds() {
        let result = read(json!({
            "paths": ["Architecture_and_Design/KAREN-2026-10-08-global-shortcut-unsuited-to-wayland.md"]
        }))
        .await;
        let files = result["files"].as_array().unwrap();
        assert_eq!(files[0]["ok"], true, "{:?}", files[0]);
        assert!(files[0]["content"].as_str().unwrap().contains("Wayland"));
    }
}
