//! Boot-time audit: account for how the previous session ended before
//! starting the next one. The crash counter in boot.sh only sees
//! *observed* exits; the one death class it can never see is the SoC
//! dying under a running reader — a power-button-hold reboot, a battery
//! pull, a kernel panic. That class leaves the `running` marker behind
//! (boot.sh removes it after every observed exit, but a hard reset kills
//! boot.sh too), and this module turns the leftover into policy:
//!
//! - died AWAKE  → a user-declared emergency or an awake hang. Append a
//!   decaying strike; enough of them hand the device to the stock
//!   framework. (USB needs no arming — the drive mounts on every plug,
//!   fully stock, so the recovery floor is always available.)
//! - died SUSPENDED (`sleeping` marker present) → an overnight battery
//!   death or a panic in sleep. Nothing was wrong; no strike.
//! - clean (`running` absent) → nothing to do.
//!
//! The strike ledger (`strikes`, one epoch per line, 1 h decay window) is
//! shared with the hang watchdog (watchdog.rs), which appends one per
//! hang kill. It exists because boot.sh's 60 s self-clearing `fails`
//! counter can never accumulate slow failures: a hang that develops after
//! a minute of healthy runtime wipes its own evidence every cycle, which
//! is exactly the infinite hang→hard-reboot→hang loop this ladder ends.
//! Ledger split, explicitly: `fails` = the binary won't run (shell-owned,
//! the pre-Rust rung); `strikes` = runtime misbehavior (hangs and SoC
//! deaths, decayed by time instead of runtime).

use std::path::{Path, PathBuf};

use ybdev::log::plog;

/// State directory shared with boot.sh (marker + ledger files).
pub const STATE_DIR: &str = "/var/local/yb-reader";
/// "A reader session was in progress" — written here at every audit,
/// removed by boot.sh after any observed exit. Only SoC-level death
/// (which kills boot.sh too) leaves it for the next audit to find.
const RUNNING: &str = "running";
/// "The reader was on the sleep screen" when the SoC died — written by
/// the app on sleep-screen entry, removed on wake/exit (yui's
/// sleep_state hook). Consumed (removed) by every audit: it describes
/// the previous session only, and a stale one would misclassify a later
/// awake death as a battery-in-sleep event.
const SLEEPING: &str = "sleeping";
/// One epoch-seconds per strike, oldest first. Pruned to the decay
/// window on every read and append so the file stays tiny.
const STRIKES: &str = "strikes";

/// Strikes older than this no longer count: three hangs across three
/// weeks is three anecdotes, three hangs in an hour is a pattern.
pub const STRIKE_WINDOW_SECS: u64 = 3600;
/// This many recent strikes (including one just appended) hand the
/// device to the stock framework. Deliberately below upstart's respawn
/// limit (10 per 600 s) so the Rust ladder acts first.
pub const STRIKE_FALLBACK: usize = 4;

/// Exit codes boot.sh switches on.
pub const EXIT_NORMAL: i32 = 0;
pub const EXIT_FALLBACK: i32 = 5;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ShutdownKind {
    /// Previous session ended through boot.sh (any observed exit).
    Clean,
    /// The SoC died with the reader on the sleep screen.
    DiedSuspended,
    /// The SoC died with the reader awake — the class that needs a rung.
    DiedAwake,
}

/// Pure, host-testable. `sleeping_exists` only matters when the session
/// died unclean: a clean session's markers are boot.sh's business.
pub fn shutdown_kind(running_exists: bool, sleeping_exists: bool) -> ShutdownKind {
    match (running_exists, sleeping_exists) {
        (false, _) => ShutdownKind::Clean,
        (true, true) => ShutdownKind::DiedSuspended,
        (true, false) => ShutdownKind::DiedAwake,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BootDecision {
    /// Proceed into takeover as usual.
    Normal,
    /// Give the device back to the stock framework.
    FallbackStock,
}

/// Pure, host-testable. `strikes_after_append` is the ledger count the
/// audit will record for this boot — `strike_count` below computes it.
pub fn boot_decision(_kind: ShutdownKind, strikes_after_append: usize) -> BootDecision {
    if strikes_after_append >= STRIKE_FALLBACK {
        BootDecision::FallbackStock
    } else {
        BootDecision::Normal
    }
}

/// Pure: does this kind of shutdown add a strike to the ledger?
pub fn strike_count(kind: ShutdownKind, existing: usize) -> usize {
    match kind {
        ShutdownKind::DiedAwake => existing + 1,
        _ => existing,
    }
}

/// Pure: keep only strikes inside the decay window. Tolerant of junk
/// lines (a torn write ages out like everything else).
pub fn prune_strikes(content: &str, now: u64) -> Vec<u64> {
    content
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .filter(|&ts| now.saturating_sub(ts) <= STRIKE_WINDOW_SECS)
        .collect()
}

/// Append a strike (or just prune): rewrite the ledger with the recent
/// window plus `now`. Keeping only the window bounds the file for life.
pub fn append_strike(path: &Path, now: u64) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut kept = prune_strikes(&content, now);
    kept.push(now);
    write_lines(path, &kept)
}

/// The strike count as this boot will record it.
pub fn strike_count_at(path: &Path, now: u64, kind: ShutdownKind) -> usize {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    strike_count(kind, prune_strikes(&content, now).len())
}

fn write_lines(path: &Path, vals: &[u64]) -> std::io::Result<()> {
    let mut out = String::new();
    for v in vals {
        out.push_str(&v.to_string());
        out.push('\n');
    }
    std::fs::write(path, out)
}

/// The `reader bootaudit` entry point. Returns the exit code for
/// boot.sh: 0 normal, 5 fall back to stock.
pub fn run() -> i32 {
    let state = PathBuf::from(STATE_DIR);
    let _ = std::fs::create_dir_all(&state);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let running = state.join(RUNNING);
    let sleeping = state.join(SLEEPING);
    let kind = shutdown_kind(running.exists(), sleeping.exists());
    // The sleeping marker describes the previous session; consume it so
    // a stale one can never mask a later awake death (the app rewrites
    // it on its own next sleep).
    let _ = std::fs::remove_file(&sleeping);

    let strikes = state.join(STRIKES);
    let count = strike_count_at(&strikes, now, kind);
    if matches!(kind, ShutdownKind::DiedAwake) {
        if let Err(e) = append_strike(&strikes, now) {
            plog(&format!("bootaudit: strike append FAILED: {e}"));
        }
    }
    let decision = boot_decision(kind, count);
    match decision {
        BootDecision::FallbackStock => {
            plog(&format!(
                "bootaudit: died {:?} — strike {count} in window, falling back to stock",
                kind
            ));
        }
        BootDecision::Normal => {
            if matches!(kind, ShutdownKind::DiedAwake) {
                plog(&format!(
                    "bootaudit: died awake (strike {count} in window)"
                ));
            } else if matches!(kind, ShutdownKind::DiedSuspended) {
                plog("bootaudit: previous session died suspended — no action");
            }
        }
    }
    // Fresh marker for the session boot.sh starts next; the content is
    // decorative, existence is the signal.
    let _ = std::fs::write(&running, b"");
    match decision {
        BootDecision::FallbackStock => EXIT_FALLBACK,
        BootDecision::Normal => EXIT_NORMAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_classification() {
        // Clean: boot.sh observed the exit and removed `running`.
        assert_eq!(shutdown_kind(false, false), ShutdownKind::Clean);
        assert_eq!(shutdown_kind(false, true), ShutdownKind::Clean);
        // SoC death mid-suspend: both markers left behind.
        assert_eq!(shutdown_kind(true, true), ShutdownKind::DiedSuspended);
        // SoC death while awake: the emergency class.
        assert_eq!(shutdown_kind(true, false), ShutdownKind::DiedAwake);
    }

    #[test]
    fn decision_ladder() {
        use BootDecision as D;
        use ShutdownKind as K;
        // Clean and died-suspended boots never disturb takeover.
        assert_eq!(boot_decision(K::Clean, 0), D::Normal);
        assert_eq!(boot_decision(K::DiedSuspended, 3), D::Normal);
        // An awake death is just a recorded strike while under the
        // threshold — takeover proceeds (USB needs no arming).
        assert_eq!(boot_decision(K::DiedAwake, 1), D::Normal);
        assert_eq!(boot_decision(K::DiedAwake, 3), D::Normal);
        // The threshold is the only thing that hands over the device.
        assert_eq!(boot_decision(K::DiedAwake, STRIKE_FALLBACK), D::FallbackStock);
        assert_eq!(boot_decision(K::Clean, STRIKE_FALLBACK), D::FallbackStock);
    }

    #[test]
    fn strikes_only_from_awake_deaths() {
        assert_eq!(strike_count(ShutdownKind::DiedAwake, 2), 3);
        assert_eq!(strike_count(ShutdownKind::DiedSuspended, 2), 2);
        assert_eq!(strike_count(ShutdownKind::Clean, 2), 2);
    }

    #[test]
    fn strike_window_pruning() {
        let now = 1_000_000;
        let content = format!(
            "{}\n{}\n{}\n",
            now - STRIKE_WINDOW_SECS, // exactly at the window: kept
            now - STRIKE_WINDOW_SECS - 1, // one second too old: dropped
            now                        // fresh: kept
        );
        assert_eq!(prune_strikes(&content, now), vec![now - STRIKE_WINDOW_SECS, now]);
        // Junk lines (torn writes, stray bytes) are skipped, not fatal —
        // but a numeric line still has to be inside the window.
        assert_eq!(prune_strikes("garbage\n\n700000\n", now), vec![]);
        assert_eq!(prune_strikes(&format!("garbage\n\n{}\n", now), now), vec![now]);
        assert!(prune_strikes("", now).is_empty());
    }

    #[test]
    fn append_strike_bounds_the_ledger() {
        let dir = std::env::temp_dir().join(format!("yb-audit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("strikes");
        let _ = std::fs::remove_file(&path);
        let now = 1_000_000;
        // Seed with strikes that straddle the window.
        std::fs::write(&path, format!("{}\n{}\n", now - 500, now - 999_999))
            .unwrap();
        append_strike(&path, now).unwrap();
        // Only the in-window strike and the new one survive.
        assert_eq!(
            prune_strikes(std::fs::read_to_string(&path).unwrap().as_str(), now),
            vec![now - 500, now]
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
