//! Awake policy: the single place that decides whether the device may
//! auto-suspend, and what happens when it wakes up anyway.
//!
//! powerd (framework-free in takeover) stays the suspender — its t1/t2
//! input-idle timers fire, we merely hold `preventScreenSaver` when a
//! screen has a live reason to stay up (mirror streaming, receive
//! server) or when USB power is present (file-transfer window, tethered
//! dev loop). Reading a book deliberately does NOT hold: page turns are
//! input and reset the timer, and ~20 min of stillness means the reader
//! put the device down — the Kindle behavior, battery-honest.
//!
//! The 30 s policy thread re-asserts the hold (a lost lipc call gets
//! ~30 retries inside powerd's ≥15 min window) and heals Wi-Fi for
//! screens that need it. The resume hook (App's wall-gap detection)
//! covers everything suspend breaks in one place: repaint, our
//! frontlight levels (powerd restores its own over ours), and the Wi-Fi
//! link (wlan0 stays administratively up across suspend, association
//! dies — wifid's enable prop surviving as 1 is exactly the "was on"
//! signal).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ybdev::log::plog;
use ybdev::sysinfo;

use crate::wifi;

static AWAKE_WANTED: AtomicBool = AtomicBool::new(false);

/// Pure decision, host-testable: a screen's live reason plus USB power.
pub fn desired_awake(screen_wants: bool, vbus: bool) -> bool {
    screen_wants || vbus
}

/// Screens with a live reason not to suspend call this on enter/leave
/// (mirror streaming, receive server). The library reader does not —
/// input-idle suspend while reading is the approved policy.
pub fn screen_wants_awake(on: bool) {
    AWAKE_WANTED.store(on, Ordering::SeqCst);
}

/// The resume hook App calls with the measured suspend gap.
pub fn on_resume(gap: Duration) {
    plog(&format!("resume after {}s (suspended)", gap.as_secs()));
    // Our frontlight levels over powerd's restore.
    let (b, t) = (
        ybdev::frontlight::last_bright(),
        ybdev::frontlight::last_tone(),
    );
    if b >= 0 || t >= 0 {
        if let Ok(fl) = ybdev::frontlight::Frontlight::open() {
            if b >= 0 {
                fl.set(b);
            }
            if t >= 0 {
                fl.tone_set(t);
            }
        }
    }
    // Wi-Fi: restore if it was on (or wifid can't answer — err toward
    // connectivity; the sequence is idempotent).
    match wifi::wifi_state() {
        Some(false) => {}
        _ => wifi::turn_on_wifi(),
    }
}

/// Start the policy thread (runs for process lifetime; dies with it).
pub fn spawn() {
    let _ = std::thread::Builder::new()
        .name("awake".to_string())
        .spawn(loop_fn);
}

fn loop_fn() {
    loop {
        let want = desired_awake(AWAKE_WANTED.load(Ordering::SeqCst), sysinfo::vbus());
        wifi::keep_awake(want);
        // Wi-Fi healing is gated to live-session screens only: the sleep
        // screen turns the radio off deliberately, and healing there
        // would fight it. Post-suspend restoration in general is the
        // resume hook's job.
        if AWAKE_WANTED.load(Ordering::SeqCst) && wifi::wifi_state() != Some(true) {
            plog("awake: wifi down during active session, healing");
            wifi::turn_on_wifi();
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_truth_table() {
        assert!(!desired_awake(false, false));
        assert!(desired_awake(true, false));
        assert!(desired_awake(false, true));
        assert!(desired_awake(true, true));
    }
}
