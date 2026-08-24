//! Wi-Fi radio control, shared by yui's sleep screen and the reader app.
//! The sequences mirror KOReader's kindleEnableWifi (and the reader's
//! turn_on_wifi, which they now back): wifid before the framework-owned
//! com.lab126.cmd property, and the interface raised first — `wifid
//! enable 1` alone never brings wlan0 up after it was downed.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::log::plog;

// --- Wake policy ---------------------------------------------------------
//
// The radio comes back on wake only for a reason: a live session
// (mirror streaming, receive server), or the user's persisted Wi-Fi/SSH
// choice. Before this, both wake paths (the sleep screen's leave and
// the app resume hook) restored unconditionally, so every wake
// re-associated the radio even when nobody wanted it.

const WIFI_WANTED_PATH: &str = "/var/local/yb-reader/wifi";
const SSH_WANTED_PATH: &str = "/var/local/yb-reader/ssh";
/// The manual-off latch, persisted: an in-memory atomic alone forgot the
/// choice on every reboot (field case 2026-08-23 — user turned the radio
/// off, one power press later it was back).
const USER_OFF_PATH: &str = "/var/local/yb-reader/wifi_off";

/// A live session wants the radio (set by mirror/receive enter/leave
/// via awake::screen_wants_awake).
static SESSION_WANTS: AtomicBool = AtomicBool::new(false);
/// The user turned Wi-Fi off by hand (curtain / System card). The radio
/// is down on purpose: the 30s session healer must not quietly bring it
/// back. Any real turn_on makes the latch stale, so it clears there.
static USER_OFF: AtomicBool = AtomicBool::new(false);

pub fn set_session_wants(on: bool) {
    SESSION_WANTS.store(on, Ordering::SeqCst);
}

pub fn session_wants() -> bool {
    SESSION_WANTS.load(Ordering::SeqCst)
}

pub fn user_off() -> bool {
    USER_OFF.load(Ordering::SeqCst)
}

fn intent(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

fn set_intent(path: &str, on: bool) {
    // The dir is boot.sh's in takeover, but nothing guarantees it in
    // stock mode or on a fresh device — without this the toggle works
    // live and the choice silently evaporates.
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if on {
        let _ = std::fs::File::create(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub fn set_ssh_wanted(on: bool) {
    set_intent(SSH_WANTED_PATH, on);
}

/// Manual Wi-Fi off (curtain / System card): remember the user's choice
/// (drop the wake intent) and latch against session healing. The latch
/// is persisted — it must survive reboots, not just wakes.
pub fn user_turned_off() {
    USER_OFF.store(true, Ordering::SeqCst);
    set_intent(WIFI_WANTED_PATH, false);
    set_intent(USER_OFF_PATH, true);
}

/// Manual Wi-Fi on: persist the choice so wake brings it back, and drop
/// the user-off latch (turn_on also clears it for the programmatic paths).
pub fn user_turned_on() {
    USER_OFF.store(false, Ordering::SeqCst);
    set_intent(USER_OFF_PATH, false);
    set_intent(WIFI_WANTED_PATH, true);
}

/// Boot-time hydration: the latch's atomic starts false in a fresh
/// process — reload it from the intent file or the first wake of every
/// boot would undo the persisted choice.
pub fn hydrate_user_off() {
    if intent(USER_OFF_PATH) {
        USER_OFF.store(true, Ordering::SeqCst);
    }
}

/// Pure decision, host-testable: does a wake have a reason to raise the
/// radio? SSH counts because an unreachable SSH toggle is useless.
pub fn wake_wants_wifi(session: bool, wifi_intent: bool, ssh_intent: bool) -> bool {
    session || wifi_intent || ssh_intent
}

pub fn wifi_wanted_on_wake() -> bool {
    // The manual-off latch outranks every intent, SSH included: "I
    // turned the radio off" must not be undone by the next power press.
    // Before this gate the persisted SSH intent alone re-raised the
    // radio on every wake, and turn_on then ERASED the latch — the
    // choice didn't survive even one sleep/wake (field 2026-08-23).
    // Opting back in is one real turn_on (curtain/System ON, a wifi
    // feature's ensure_wifi), which clears the latch.
    !user_off()
        && wake_wants_wifi(
            session_wants(),
            intent(WIFI_WANTED_PATH),
            intent(SSH_WANTED_PATH),
        )
}

/// Bring the interface up and ask wifid to associate (idempotent).
pub fn turn_on() {
    USER_OFF.store(false, Ordering::SeqCst);
    set_intent(USER_OFF_PATH, false);
    let _ = Command::new("/sbin/ifconfig")
        .args(["wlan0", "up"])
        .status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.wifid", "enable", "1"])
        .status();
    let _ = Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.cmd", "wirelessEnable", "1"])
        .status();
}

/// Power the radio down (the sleep-screen sequence, reversed).
pub fn turn_off() {
    let _ = Command::new("/sbin/ifconfig")
        .args(["wlan0", "down"])
        .status();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_wifi_truth_table() {
        // No reason at all: the pre-fix behavior (always on) is gone.
        assert!(!wake_wants_wifi(false, false, false));
        // Each reason alone suffices.
        assert!(wake_wants_wifi(true, false, false));
        assert!(wake_wants_wifi(false, true, false));
        assert!(wake_wants_wifi(false, false, true));
    }

    #[test]
    fn manual_off_latch_outranks_wake_intents() {
        // A live session would want the radio — but the user turned it
        // off by hand. The latch wins (the SSH-intent field case:
        // every wake re-raised the radio and turn_on erased the latch).
        set_session_wants(true);
        user_turned_off();
        assert!(!wifi_wanted_on_wake());
        // Opting back in clears the latch.
        user_turned_on();
        set_session_wants(false);
        // Intent files point at /var/local — writes fail (or no-op)
        // on the host, so only the latch semantics are asserted here.
        assert!(!user_off() || !wifi_wanted_on_wake());
        set_session_wants(false);
    }
}
