use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_iso() -> String {
    // Keep this dependency-free and stable enough for logs/state.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}
