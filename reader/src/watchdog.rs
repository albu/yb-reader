//! Hang watchdog: the one safety net that must live OUTSIDE the process
//! it protects. The reader's own guard.rs handles panics and TERM — the
//! events where the process can still run code. A hang (deadlocked loop,
//! wedged render, stuck syscall) is precisely the case where it can't,
//! and the crash counter in boot.sh never fires because the process
//! never exits. This module runs as a second copy of the same binary
//! (`reader --watchdog <pid>`), reads the main loop's heartbeat on
//! tmpfs, and kills the hung instance so upstart respawns a fresh one.
//!
//! CPU frugality — this is an e-reader and the net must be quiet:
//! - Passes every 30 s. A hung UI needs "the device fixes itself
//!   eventually", not "fixes itself in 183 s" — detection lands within
//!   180–210 s of the hang, for at most 2,880 wakeups per fully-awake
//!   day (the awake-policy thread's own 5 s tick is 17,280; input
//!   polling runs at 25 Hz while awake).
//! - The heartbeat touch is rate-limited to one utimensat per 5 s —
//!   one syscall, no allocation, tmpfs only (never flash).
//! - Suspended cost is exactly zero: suspend-to-RAM freezes this
//!   process with everything else, and a userspace timer holds nothing
//!   awake (powerd suspends even while the app's threads run — the
//!   measured behavior awake.rs documents).
//!
//! Suspend is the classic false positive and the reason both clocks
//! here are wall clocks: the sleep screen blocks the main loop inside
//! suspend_to_mem() for hours, freezing the heartbeat with it. But it
//! freezes this loop too — so a large *self* gap (time between this
//! loop's passes) means the SoC slept and staleness means nothing. Only
//! a stale heartbeat with a healthy self gap is a hang. Wall, not
//! monotonic, because monotonic stops in suspend (app.rs documents the
//! same property for the resume detector).
//!
//! Second duty, riding the same 5 s gate: mount self-integrity. USB is
//! fully stock (plug = drive, always — the user's absolute recovery
//! floor), and the export of /mnt/us unmounts the very partition this
//! binary, its books and its logs live on. A plug while AWAKE kills the
//! process outright (mapping eviction); a plug while SUSPENDED is the
//! dangerous case — eviction is invisible until the first page fault,
//! and a resumed process holding stale FUSE handles can silently
//! corrupt positions. The yui resume hook can't be the checkpoint (it
//! deliberately stays silent while the sleep screen is on top), so the
//! check lives here, at the one point every path crosses. Any real
//! suspend widens the wall gap past the rate gate, so the first
//! post-resume pass always inspects. On mismatch: exit through the
//! guard — boot.sh sees the plug, waits out the unplug, respawns fresh.
//!
//! On a hang: append a decaying strike (bootaudit's ledger — persistent
//! hangs accumulate to the stock fallback) and SIGKILL by pid. The kill
//! skips guard::restore by nature of SIGKILL; the respawned instance
//! re-derives usb/wifi state itself, and a leaked firewall ACCEPT with
//! no listener is inert. USB is never armed or disarmed from here — it
//! is always armed, stock semantics, and that is the point.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use ybdev::log::plog;

/// The heartbeat file. tmpfs: created by the app, read (mtime only) by
/// the watchdog, removed by boot.sh before each session so a stale file
/// from a previous boot can never satisfy this one.
pub const HEARTBEAT: &str = "/tmp/yb-heartbeat";

/// One pass per this many seconds (see the frugality note above).
const PASS: Duration = Duration::from_secs(30);
/// A gap between this loop's own passes larger than this means the SoC
/// suspended under us: rebase and judge nothing. 3× the pass period —
/// scheduler jitter is nowhere near this, suspend is far beyond it.
const SELF_GAP_SUSPEND: Duration = Duration::from_secs(90);
/// A heartbeat this stale (with a healthy self gap) is a hang. The
/// awake loop's worst legal silence is the 20 s Home tick plus one
/// bounded blocking operation (Wi-Fi waits cap at 20 s); 180 s is a
/// 4× margin over anything legitimate.
const HANG_AFTER: Duration = Duration::from_secs(180);
/// The heartbeat must exist by this age or the app never reached its
/// loop (stuck in setup) — also a hang.
const START_GRACE: Duration = Duration::from_secs(60);
/// The loop touches the heartbeat at most this often; the watchdog's
/// threshold needs 180 s of resolution, not loop-rate resolution.
const TOUCH_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    /// Heartbeat fresh (or too early to tell) — nothing to do.
    Normal,
    /// The SoC slept under this loop; staleness proves nothing.
    Suspended,
    /// Awake and silent past every legitimate bound.
    Hung,
}

/// Pure, host-testable. `self_gap`: wall time between this loop's
/// passes. `hb_age`: heartbeat mtime age, or None if the file hasn't
/// appeared yet. `since_start`: wall time since the watchdog spawned.
pub fn verdict(
    self_gap: Duration,
    hb_age: Option<Duration>,
    since_start: Duration,
) -> Verdict {
    if self_gap > SELF_GAP_SUSPEND {
        return Verdict::Suspended;
    }
    match hb_age {
        Some(age) if age > HANG_AFTER => Verdict::Hung,
        // No heartbeat yet: only a hang once the start grace has passed.
        None if since_start > START_GRACE => Verdict::Hung,
        _ => Verdict::Normal,
    }
}

/// Pure: heartbeat age, with a backward clock step treated as *fresh*.
/// An mtime in the future means the clock moved back under a recent
/// touch (post-wake NTP corrections are real on this firmware), not
/// that the file never appeared — mapping it to None would read as
/// "app never reached its loop" and kill a healthy reader.
fn mtime_age(now: SystemTime, mtime: SystemTime) -> Duration {
    now.duration_since(mtime).unwrap_or_default()
}

/// Pure: may the loop touch the heartbeat yet? A `now` *behind* the
/// last touch is a backward clock step — touch anyway (re-anchoring on
/// the new clock), never strand the heartbeat until the wall clock
/// re-passes a pre-step timestamp.
fn should_touch(now_ms: u64, last_ms: u64) -> bool {
    now_ms < last_ms || now_ms - last_ms >= TOUCH_EVERY.as_millis() as u64
}

/// Pure: the /proc/mounts entry for /mnt/us (the whole line — device,
/// mount root and fstype together identify the serving instance).
pub fn mount_line(proc_mounts: &str) -> Option<&str> {
    proc_mounts
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(MOUNT_POINT))
}

/// Pure: has the mount changed under us? A `None` snapshot means the
/// check is disabled (we started without /mnt/us mounted — nothing to
/// protect). Missing-now and changed-both count: an export unmounts,
/// and a re-served mount is a *different* instance even if it looks
/// similar.
pub fn mount_changed(snapshot: Option<&str>, current: Option<&str>) -> bool {
    match snapshot {
        Some(expected) => current != Some(expected),
        None => false,
    }
}

const MOUNT_POINT: &str = "/mnt/us";

/// The loop's liveness proof and self-integrity check, sharing one 5 s
/// rate gate (one budget, one wakeup cadence). Touches the heartbeat's
/// mtime, then verifies /mnt/us is still the mount we started on.
pub fn heartbeat_touch() {
    static LAST_MS: AtomicU64 = AtomicU64::new(0);
    static SNAPSHOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let now_ms = epoch_ms();
    let last = LAST_MS.load(Ordering::Relaxed);
    if !should_touch(now_ms, last) {
        return;
    }
    LAST_MS.store(now_ms, Ordering::Relaxed);
    let c = HEARTBEAT.as_bytes().as_ptr() as *const libc::c_char;
    // NULL times = "now": one syscall, no data written, tmpfs only.
    // ENOENT (first call, or tmpfs swept) falls through to a create.
    if unsafe { libc::utimensat(libc::AT_FDCWD, c, std::ptr::null(), 0) } != 0 {
        let _ = std::fs::File::create(HEARTBEAT);
    }
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let snap = SNAPSHOT.get_or_init(|| mount_line(&mounts).map(str::to_string));
    if mount_changed(snap.as_deref(), mount_line(&mounts)) {
        plog("watchdog: /mnt/us changed under us (USB export?) — exiting for clean respawn");
        crate::guard::graceful_exit(1);
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn heartbeat_age() -> Option<Duration> {
    let mtime = std::fs::metadata(HEARTBEAT).ok()?.modified().ok()?;
    Some(mtime_age(SystemTime::now(), mtime))
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A reaped-pending zombie still answers kill(pid, 0); striking a
/// corpse would double-count a death boot.sh already observed. /proc is
/// checked only on the kill path, never per pass.
fn pid_is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            // The state letter follows the parenthesized comm; comm can
            // contain spaces and parens, so parse after the LAST ')'.
            let tail = s.rsplit(')').next()?;
            tail.trim_start().chars().next()
        })
        .is_some_and(|c| c == 'Z')
}

/// The `reader --watchdog <pid>` entry point. Watches that exact pid —
/// never a name — so a deploy's new generation can never be killed by
/// the previous generation's watchdog. Exits when the pid dies (or is
/// killed here); the exit code is informational, nobody waits on it.
pub fn run(rpid: i32) -> i32 {
    let start = SystemTime::now();
    let mut last_pass = start;
    loop {
        std::thread::sleep(PASS);
        let now = SystemTime::now();
        let self_gap = now.duration_since(last_pass).unwrap_or_default();
        last_pass = now;
        if !pid_alive(rpid) {
            return 0;
        }
        let v = verdict(
            self_gap,
            heartbeat_age(),
            now.duration_since(start).unwrap_or_default(),
        );
        if v != Verdict::Hung {
            continue;
        }
        if pid_is_zombie(rpid) {
            return 0;
        }
        let age = heartbeat_age()
            .map(|d| d.as_secs())
            .unwrap_or(0);
        plog(&format!(
            "watchdog: reader hung (heartbeat {age}s stale, self gap {}s) — killing pid {rpid}",
            self_gap.as_secs()
        ));
        let ledger = PathBuf::from(crate::bootaudit::STATE_DIR).join("strikes");
        if let Err(e) = crate::bootaudit::append_strike(&ledger, epoch_secs()) {
            plog(&format!("watchdog: strike append FAILED: {e}"));
        }
        unsafe {
            libc::kill(rpid, libc::SIGKILL);
        }
        return 9;
    }
}

fn epoch_secs() -> u64 {
    (epoch_ms() / 1000).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn suspend_outranks_staleness() {
        // The sleep screen freezes the heartbeat for hours — but it
        // freezes the watchdog too, so the self gap gives it away.
        assert_eq!(verdict(s(3600), Some(s(3600)), s(4000)), Verdict::Suspended);
        // Just over the self-gap bound: judged as suspended, not hung.
        assert_eq!(verdict(s(91), Some(s(400)), s(500)), Verdict::Suspended);
        // At and under the bound, staleness is judged on its merits.
        assert_eq!(verdict(s(90), Some(s(400)), s(500)), Verdict::Hung);
        assert_eq!(verdict(s(45), Some(s(400)), s(500)), Verdict::Hung);
        // A modest self gap (one late pass) with a fresh heartbeat.
        assert_eq!(verdict(s(45), Some(s(20)), s(300)), Verdict::Normal);
    }

    #[test]
    fn hang_threshold() {
        // Fresh heartbeat, healthy gap: fine.
        assert_eq!(verdict(s(30), Some(s(10)), s(600)), Verdict::Normal);
        // Right at the worst legal silence (20 s Home tick + slack).
        assert_eq!(verdict(s(30), Some(s(60)), s(600)), Verdict::Normal);
        // Stale past every legitimate bound.
        assert_eq!(verdict(s(30), Some(s(180)), s(600)), Verdict::Normal);
        assert_eq!(verdict(s(30), Some(s(181)), s(600)), Verdict::Hung);
        assert_eq!(verdict(s(30), Some(s(1800)), s(2000)), Verdict::Hung);
    }

    #[test]
    fn missing_heartbeat_is_a_hang_only_after_grace() {
        // Boot in progress: the app hasn't reached its loop yet.
        assert_eq!(verdict(s(30), None, s(30)), Verdict::Normal);
        assert_eq!(verdict(s(30), None, s(60)), Verdict::Normal);
        // It never got there.
        assert_eq!(verdict(s(30), None, s(61)), Verdict::Hung);
        // But a suspend during early boot must not misread as one.
        assert_eq!(verdict(s(120), None, s(3600)), Verdict::Suspended);
    }

    #[test]
    fn touch_rate_limits_to_five_seconds() {
        // Two immediate touches: the second must be a no-op (same
        // millisecond window). Verifiable only by absence of panic and
        // by the file existing after the first — the rate cap itself is
        // the clock check inside; assert the invariant we can see.
        // (On the host /mnt/us does not exist: the mount check arms
        // with a None snapshot and stays silent — itself a test of the
        // disabled-check path.)
        std::fs::remove_file(HEARTBEAT).ok();
        heartbeat_touch();
        heartbeat_touch();
        assert!(Path::new(HEARTBEAT).exists());
        std::fs::remove_file(HEARTBEAT).ok();
    }

    #[test]
    fn backward_clock_steps_read_as_fresh() {
        let now = SystemTime::now();
        // Normal: 30 s ago is 30 s old.
        assert_eq!(mtime_age(now, now - s(30)), s(30));
        // Clock stepped back under a fresh touch: mtime is in the
        // "future" — that is a live app on a corrected clock, not a
        // missing heartbeat (which maps to None upstream and Hung).
        assert_eq!(mtime_age(now, now + s(3600)), Duration::ZERO);
        // The rate limiter re-anchors instead of stranding the touch
        // until the wall clock re-passes the pre-step timestamp.
        assert!(!should_touch(10_500, 10_000)); // 500 ms since: no
        assert!(should_touch(16_000, 10_000)); // 6 s since: yes
        assert!(should_touch(9_000, 10_000)); // clock went back: yes
    }

    #[test]
    fn mount_line_finds_the_us_entry() {
        let mounts = "rootfs / rootfs rw 0 0\n\
                      /dev/mmcblk0p1 /var/local ext4 rw 0 0\n\
                      fsp /mnt/us fuse.fsp rw 0 0\n\
                      tmpfs /tmp tmpfs rw 0 0\n";
        assert_eq!(mount_line(mounts), Some("fsp /mnt/us fuse.fsp rw 0 0"));
        assert_eq!(mount_line(""), None);
        // A different mount point whose path merely CONTAINS ours must
        // not match (field-exact comparison, not substring).
        assert_eq!(mount_line("fsp /mnt/usr-docs fuse.fsp rw 0 0"), None);
    }

    #[test]
    fn mount_change_detection() {
        let a = Some("fsp /mnt/us fuse.fsp rw 0 0");
        // Healthy: same serving instance.
        assert!(!mount_changed(a, a));
        // Export: unmounted entirely.
        assert!(mount_changed(a, None));
        // Re-served after unplug: textually different instance (device
        // id, root or options) is a change even though /mnt/us exists.
        assert!(mount_changed(
            a,
            Some("fsp1 /mnt/us fuse.fsp rw,nosuid 0 0")
        ));
        // Check disabled: we started without the mount (nothing the
        // guard could snapshot) — never fires.
        assert!(!mount_changed(None, None));
        assert!(!mount_changed(None, Some("fsp /mnt/us fuse.fsp rw 0 0")));
    }
}
