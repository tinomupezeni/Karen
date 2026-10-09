use chrono::{DateTime, Utc, TimeZone, Local};

pub fn format_relative_time(timestamp: &str) -> String {
    // timestamp is like "2026-10-09 13:58:20" (UTC from sqlite)
    // We need to parse it as UTC and convert to Local
    let naive = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S");
    if let Ok(naive) = naive {
        let utc = Utc.from_utc_datetime(&naive);
        let local: DateTime<Local> = utc.with_timezone(&Local);
        let now = Local::now();
        let duration = now.signed_duration_since(local);

        if duration.num_seconds() < 60 {
            "just now".to_string()
        } else if duration.num_minutes() < 60 {
            format!("{} minutes ago", duration.num_minutes())
        } else if duration.num_hours() < 24 {
            if now.date_naive() == local.date_naive() {
                format!("today at {}", local.format("%H:%M"))
            } else {
                format!("{} hours ago", duration.num_hours())
            }
        } else if duration.num_days() == 1 || (now.date_naive() - local.date_naive()).num_days() == 1 {
            format!("yesterday at {}", local.format("%H:%M"))
        } else {
            format!("{} days ago", duration.num_days())
        }
    } else {
        timestamp.to_string() // fallback
    }
}
