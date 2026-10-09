use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::{ProcessesToUpdate, System};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextState {
    pub current_processes: Vec<String>,
    pub activity_inference: String,
}

pub struct ContextEngine {
    state: Arc<Mutex<ContextState>>,
}

impl ContextEngine {
    pub fn new(db: Arc<tokio::sync::Mutex<crate::memory::Database>>) -> Self {
        let state = Arc::new(Mutex::new(ContextState {
            current_processes: Vec::new(),
            activity_inference: "Idle".to_string(),
        }));

        let state_clone = Arc::clone(&state);

        tauri::async_runtime::spawn(async move {
            let mut sys = System::new_all();
            let mut last_activity = "Idle".to_string();
            let mut idle_ticks = 0;

            loop {
                sys.refresh_processes(ProcessesToUpdate::All, true);
                
                let mut processes: Vec<_> = sys.processes().values().collect();
                processes.sort_by(|a, b| b.cpu_usage().partial_cmp(&a.cpu_usage()).unwrap_or(std::cmp::Ordering::Equal));
                
                let top_procs: Vec<String> = processes.iter()
                    .take(5)
                    .filter(|p| p.cpu_usage() > 0.0)
                    .map(|p| p.name().to_string_lossy().to_string())
                    .collect();

                let mut current_active = None;
                if top_procs.iter().any(|name| name.contains("code") || name.contains("rust-analyzer") || name.contains("cargo")) {
                    current_active = Some("Coding".to_string());
                } else if top_procs.iter().any(|name| name.contains("firefox") || name.contains("chrome")) {
                    current_active = Some("Browsing".to_string());
                }

                let activity = match current_active {
                    Some(act) => {
                        idle_ticks = 0;
                        act
                    }
                    None => {
                        idle_ticks += 1;
                        if idle_ticks >= 12 { // 60 seconds of no recognized CPU activity
                            "Idle".to_string()
                        } else {
                            last_activity.clone()
                        }
                    }
                };

                if activity != last_activity {
                    let db_lock = db.lock().await;
                    let desc = format!("User is now {}", activity);
                    let _ = db_lock.record_event("activity_change", &desc, Some("ContextEngine"));
                    last_activity = activity.clone();
                }

                if let Ok(mut s) = state_clone.lock() {
                    s.current_processes = top_procs;
                    s.activity_inference = activity;
                }

                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        Self { state }
    }

    pub fn get_context(&self) -> ContextState {
        self.state.lock().unwrap().clone()
    }
}
