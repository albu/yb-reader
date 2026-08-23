//! Awake policy: the single place that decides whether the device may
//! auto-suspend, and what happens when it wakes up anyway.
//!
//! powerd (framework-free in takeover) is *an* input-idle suspender —
//! its t1/t2 timers do fire mid-conversation — but it cannot be the
//! only one: the t1 path leans on the frozen framework, and a wedged
//! wlan stack resets its timer forever via wmt t1TimerReset spam
//! (measured 2026-08-21: 27 min untouched and awake at 121mA). yui's
//! App therefore enforces the stock 10-min idle sleep itself
//! (`holds_awake`/vbus are the opt-outs); this thread's job is the
//! `preventScreenSaver` hold for live sessions (mirror streaming,
//! receive server) and USB power, so powerd stays out of the way while
//! a session runs. Reading a book deliberately holds nothing: page
//! turns are input, and stillness means the reader put the device
//! down — the Kindle behavior, battery-honest.
//!
//! The 30 s policy thread re-asserts the hold (a lost lipc call gets
//! ~30 retries inside powerd's ≥15 min window) and heals Wi-Fi for
//! screens that need it (never against a manual user off). The resume
//! hook (App's wall-gap detection) covers everything suspend breaks in
//! one place: repaint, our frontlight levels (powerd restores its own
//! over ours), and the Wi-Fi link — restored when a reason wants it
//! (ybdev::wifi::wifi_wanted_on_wake), powered down when nothing does
//! (suspend kills the association; an up-but-scanning radio drains).

use std::time::Duration;

use ybdev::log::plog;
use ybdev::sysinfo;

use crate::wifi;

/// Pure decision, host-testable: a screen's live reason, USB power, or
/// the Wi-Fi link being up (reachable ⇒ awake — a sleeping device
/// can't be deployed to, and it shouldn't silently drop off the network
/// mid-session; turn Wi-Fi off in the curtain to let it sleep).
pub fn desired_awake(screen_wants: bool, vbus: bool, wifi_up: bool) -> bool {
    screen_wants || vbus || wifi_up
}

/// Screens with a live reason not to suspend call this on enter/leave
/// (mirror streaming, receive server). The library reader does not —
/// input-idle suspend while reading is the approved policy. The flag
/// lives in ybdev::wifi: it is also the session half of the wake
/// policy (wifi_wanted_on_wake), visible to yui's sleep screen.
pub fn screen_wants_awake(on: bool) {
    ybdev::wifi::set_session_wants(on);
}

/// The resume hook App calls with the measured suspend gap.
pub fn on_resume(gap: Duration) {
    plog(&format!("resume after {}s (suspended)", gap.as_secs()));
    // Our frontlight levels over powerd's restore.
    reassert_frontlight();
    // Wi-Fi: bring it back only when something wants it (live session
    // or the user's persisted Wi-Fi/SSH choice — pre-fix, a None from a
    // slow wifid meant "restore", so every wake re-associated). When
    // nothing wants it, power an unwanted radio down: suspend kills the
    // association, and an up-but-unassociated radio scans at ~3× the
    // idle drain (ybdev::wifi::verify_or_power_down has the measured
    // numbers and the drain guard). Stock mode keeps the historic
    // restore-if-not-explicitly-off: the framework owns the radio and
    // nothing of ours sets intents there, so the policy would only ever
    // fire its power-down half — fighting powerd for its own Wi-Fi.
    // Radio restore is subprocess-bound (ifconfig + lipc round-trips,
    // ~1s on this CPU) — synchronous, that's a full second before the
    // first post-wake repaint, which is exactly the wake latency the
    // user feels. Same decision tree as before, off the UI thread.
    let takeover = ybdev::sysinfo::takeover();
    let _ = std::thread::Builder::new()
        .name("wifi-resume".to_string())
        .spawn(move || {
            if !takeover {
                match wifi::wifi_state() {
                    Some(false) => {}
                    _ => {
                        wifi::turn_on_wifi();
                        ybdev::wifi::verify_or_power_down();
                    }
                }
            } else if ybdev::wifi::wifi_wanted_on_wake() {
                wifi::turn_on_wifi();
                ybdev::wifi::verify_or_power_down();
            } else if wifi::wifi_state() != Some(false) {
                plog("resume: wifi not wanted — radio down");
                ybdev::wifi::turn_off();
            }
        });
}

/// Our remembered frontlight levels over powerd's idea of them. Used
/// after suspend (powerd restores its own levels over ours) and on USB
/// plug (powerd's charge policy kills the light — stock shows a USB
/// drive-mode screen instead; our charge-and-read keeps the light
/// where the user left it). No-op unless the user has set a level this
/// session (-1/-1 = never touched ⇒ powerd's call stands).
pub fn reassert_frontlight() {
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
}

/// Start the policy thread (runs for process lifetime; dies with it).
pub fn spawn() {
    let _ = std::thread::Builder::new()
        .name("awake".to_string())
        .spawn(loop_fn);
}

/// Boot-time radio restore. The persisted Wi-Fi/SSH intents are the
/// user's standing choice, but nothing applied them on a fresh boot:
/// the healer is gated to live-session screens, so after any reboot
/// the radio sat down until something was opened — an unreachable
/// device for as long as nobody touched it (2026-08-23: the whole
/// corner-back incident window). Takeover only: in stock mode the
/// framework owns the radio and our policy must not fight it. Off the
/// UI thread — boot's first paint must not wait on lipc round-trips.
pub fn boot_restore() {
    if !ybdev::sysinfo::takeover()
        || ybdev::wifi::user_off()
        || !ybdev::wifi::wifi_wanted_on_wake()
        || wifi::wifi_state() == Some(true)
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("wifi-boot".to_string())
        .spawn(|| {
            plog("awake: boot — restoring wifi per saved intent");
            wifi::turn_on_wifi();
            ybdev::wifi::verify_or_power_down();
        });
}

/// Pure edge, host-testable.
fn vbus_plugged(prev: bool, cur: bool) -> bool {
    cur && !prev
}

fn loop_fn() {
    // 5 s tick: fast enough that the charge-and-read light re-assert
    // lands within one blink of powerd's plug-time light-off, while the
    // hold/heal policy work keeps its 30 s cadence (every 6th tick).
    let mut prev_vbus = sysinfo::vbus();
    let mut tick: u32 = 0;
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let vbus = sysinfo::vbus();
        if vbus_plugged(prev_vbus, vbus) {
            plog("awake: usb power — re-asserting frontlight (charge-and-read)");
            reassert_frontlight();
        }
        prev_vbus = vbus;
        tick = tick.wrapping_add(1);
        if tick % 6 != 0 {
            continue;
        }
        let want = desired_awake(
            ybdev::wifi::session_wants(),
            sysinfo::vbus(),
            sysinfo::wifi_up(),
        );
        wifi::keep_awake(want);
        // Wi-Fi healing is gated to live-session screens only: the sleep
        // screen turns the radio off deliberately, and healing there
        // would fight it. A manual user off (curtain / System card)
        // latches in ybdev::wifi and stops the healer too — the user's
        // choice outranks the session until something real turns the
        // radio back on. Post-suspend restoration in general is the
        // resume hook's job.
        if ybdev::wifi::session_wants()
            && !ybdev::wifi::user_off()
            && wifi::wifi_state() != Some(true)
        {
            plog("awake: wifi down during active session, healing");
            wifi::turn_on_wifi();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_truth_table() {
        assert!(!desired_awake(false, false, false));
        assert!(desired_awake(true, false, false));
        assert!(desired_awake(false, true, false));
        assert!(desired_awake(false, false, true));
        assert!(desired_awake(true, true, true));
    }

    #[test]
    fn vbus_edge_fires_only_on_plug() {
        assert!(vbus_plugged(false, true)); // plug
        assert!(!vbus_plugged(true, true)); // staying plugged
        assert!(!vbus_plugged(true, false)); // unplug
        assert!(!vbus_plugged(false, false)); // battery the whole time
    }
}
