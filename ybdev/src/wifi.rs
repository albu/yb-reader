//! Wi-Fi radio control, shared by yui's sleep screen and the reader app.
//! The sequences mirror KOReader's kindleEnableWifi (and the reader's
//! turn_on_wifi, which they now back): wifid before the framework-owned
//! com.lab126.cmd property, and the interface raised first — `wifid
//! enable 1` alone never brings wlan0 up after it was downed.

use std::process::Command;
use std::time::{Duration, Instant};

use crate::log::plog;

/// Bring the interface up and ask wifid to associate (idempotent).
pub fn turn_on() {
    let _ = Command::new("/sbin/ifconfig").args(["wlan0", "up"]).status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.wifid", "enable", "1"])
        .status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.cmd", "wirelessEnable", "1"])
        .status();
}

/// Power the radio down (the sleep-screen sequence, reversed).
pub fn turn_off() {
    let _ = Command::new("/sbin/ifconfig").args(["wlan0", "down"]).status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.wifid", "enable", "0"])
        .status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.cmd", "wirelessEnable", "0"])
        .status();
}

/// Poll `com.lab126.wifid cmState` until CONNECTED (KOReader's
/// kindleGetScanList reads the same property). Association takes
/// seconds after `enable`; a dead wifid just fails the poll, and the
/// deadline bounds the wall time regardless of how slow lipc answers.
pub fn wait_connected(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
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
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Drain guard for the wake path. A restored-but-unassociated radio
/// scans forever well above the awake baseline: measured 121mA average
/// over a 27-min idle on 2026-08-21 (60%→57% battery) after a 34h
/// suspend woke with wpa_supplicant dead — the radio ran unassociated
/// until the NEXT power press restarted wifid and current fell to
/// ~40mA. Give the restore 10s to associate, retry the idempotent
/// sequence once (the field case healed on the second restore), and if
/// the radio still has nothing to talk to, power it down. A live
/// session that needs Wi-Fi re-asserts it (ensure_wifi on mirror and
/// receive entry; the awake policy thread heals within 30s), so this
/// guard can cost latency, never connectivity. Spawns its own thread:
/// the wake path must never block input on lipc round-trips.
pub fn verify_or_power_down() {
    let _ = std::thread::Builder::new()
        .name("wifi-verify".to_string())
        .spawn(|| {
            if wait_connected(Duration::from_secs(10)) {
                return;
            }
            plog("wifi: not associated 10s after wake — retrying restore");
            turn_on();
            if wait_connected(Duration::from_secs(10)) {
                return;
            }
            plog("wifi: still unassociated — radio down (idle drain guard)");
            turn_off();
        });
}
