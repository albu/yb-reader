//! The plugin log, in the same format as mirror.koplugin's `plog()`.
//! Defaults to the device path; overridable for desktop testing.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: Mutex<Option<String>> = Mutex::new(None);

pub fn set_path(path: &str) {
    *LOG_PATH.lock().unwrap() = Some(path.to_string());
}

pub fn path() -> String {
    LOG_PATH
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "/mnt/us/extensions/mirror/plugin.log".to_string())
}

pub fn plog(msg: &str) {
    let p = path();
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (h, m, s) = ((now / 3600) % 24, (now / 60) % 60, now % 60);
    let _ = writeln!(f, "[{:02}:{:02}:{:02}] {}", h, m, s, msg);
    let _ = f.flush();
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
