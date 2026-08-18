//! Kindle WiFi control, matching KOReader's kindleEnableWifi: lipc props.

use std::process::Command;
use std::time::{Duration, Instant};

/// Wi-Fi state per wifid, or None when lipc can't answer. The curtain
/// toggle must treat None as off: the old assume-on fallback routed the
/// tap into the turn-OFF branch exactly when the radio was already
/// unreachable — a "dead tile" in takeover mode (2026-08-19).
pub fn wifi_state() -> Option<bool> {
    let out = Command::new("lipc-get-prop")
        .args(["-i", "com.lab126.wifid", "enable"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8(out.stdout).ok()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub fn is_wifi_on() -> bool {
    // Optimistic default for the startup paths — the app retries anyway.
    wifi_state().unwrap_or(true)
}

pub fn turn_on_wifi() {
    // The interface must be administratively up before wifid can
    // associate: the off path (and a suspend) can leave wlan0 down, and
    // `wifid enable 1` alone never raises it — the curtain tile turned
    // Wi-Fi off fine and then could never turn it back on (2026-08-19).
    // Mirrors guard's emergency_wake_restore, the proven wake path.
    // wifid before com.lab126.cmd: cmd is framework-owned and never
    // answers in takeover mode.
    let _ = Command::new("/sbin/ifconfig").args(["wlan0", "up"]).status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.wifid", "enable", "1"])
        .status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.cmd", "wirelessEnable", "1"])
        .status();
}

pub fn ensure_wifi() {
    if !is_wifi_on() {
        ybdev::log::plog("wifi down — turning it back on");
        turn_on_wifi();
        wait_for_wifi(Duration::from_secs(20));
    }
}

/// Poll `com.lab126.wifid cmState` until CONNECTED (KOReader's
/// kindleGetScanList uses the same property). Association takes seconds
/// after `enable`; without this wait a cold radio would produce spurious
/// "Mac not found" on the very first tap.
pub fn wait_for_wifi(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(out) = Command::new("lipc-get-prop")
            .args(["com.lab126.wifid", "cmState"])
            .output()
        {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    if s.trim() == "CONNECTED" {
                        return true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Keep the stock screensaver/suspend from interrupting a reading or
/// mirroring session (KOReader's KeepAlive plugin uses exactly this).
pub fn keep_awake(on: bool) {
    let v = if on { "1" } else { "0" };
    let _ = Command::new("lipc-set-prop")
        .args(["com.lab126.powerd", "preventScreenSaver", v])
        .status();
}
