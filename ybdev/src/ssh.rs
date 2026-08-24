//! ssh, owned by the app: the bundled dropbear on port 2222 (koreader's
//! binary as fallback — no KOReader install required). boot.sh starts
//! the same server at boot in takeover mode. The binary is PATCHED to
//! resolve settings/SSH/ (authorized_keys and host keys) relative to
//! its CWD — so both launchers run it from the tree root that holds
//! our settings/SSH/, with no -r at all (koreader's own plugin does
//! exactly this). Recipe as koreader-ext.sh's start_ssh/stop_ssh: an
//! iptables accept rule pair plus the dropbear invocation.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const PIDFILE: &str = "/tmp/dropbear_koreader.pid";
const OURS: &str = "/mnt/us/extensions/reader/bin/dropbear";
const OURS_TREE: &str = "/mnt/us/extensions/reader";
const KOREADER: &str = "/mnt/us/koreader/dropbear";
const KOREADER_TREE: &str = "/mnt/us/koreader";
const IPTABLES: &str = "/usr/sbin/iptables";

static CACHED_RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_CHECK_MS: AtomicU32 = AtomicU32::new(0);

const CACHE_TTL_MS: u32 = 1000;

/// Any live dropbear? A /proc comm scan with a 1-second TTL — cheap enough
/// to call per redraw without thrashing /proc on busy ticks.
/// (busybox `ps` is unreliable on this FW; /proc is truth.)
pub fn running() -> bool {
    let now = crate::log::now_ms() as u32;
    let last = LAST_CHECK_MS.load(Ordering::Relaxed);
    if last != 0 && now.wrapping_sub(last) < CACHE_TTL_MS {
        return CACHED_RUNNING.load(Ordering::Relaxed);
    }
    let res = pids().next().is_some();
    CACHED_RUNNING.store(res, Ordering::Relaxed);
    LAST_CHECK_MS.store(now, Ordering::Relaxed);
    res
}

pub fn invalidate_cache() {
    LAST_CHECK_MS.store(0, Ordering::Relaxed);
}

fn pids() -> impl Iterator<Item = i32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            // /proc/<digits> only
            if !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let comm = fs::read_to_string(e.path().join("comm")).ok()?;
            if comm.trim() != "dropbear" {
                return None;
            }
            // ALIVE only: a daemonizing dropbear leaves unreaped zombie
            // intermediates — TERM-immune, comm intact — and counting one
            // makes the toggle lie "on" forever (bug found on device,
            // 2026-08-17).
            let alive = fs::read_to_string(e.path().join("status"))
                .map(|st| !st.lines().any(|l| l.starts_with("State:") && l.contains('Z')))
                .unwrap_or(false);
            alive.then(|| s.parse::<i32>().ok())?
        })
}

/// Add (action "A") or remove ("D") the firewall rule pair — the same
/// specs koreader-ext.sh uses. -A only when the rule is absent (-C):
/// re-enables after a missed -D must not stack duplicates.
fn rules(action: &str) {
    let specs: [&[&str]; 2] = [
        &[
            "INPUT",
            "-p",
            "tcp",
            "--dport",
            "2222",
            "-m",
            "conntrack",
            "--ctstate",
            "NEW,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
        &[
            "OUTPUT",
            "-p",
            "tcp",
            "--sport",
            "2222",
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    ];
    for s in specs {
        if action == "A" {
            let present = Command::new(IPTABLES)
                .arg("-C")
                .args(s.iter())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|st| st.success())
                .unwrap_or(false);
            if present {
                continue;
            }
        }
        let _ = Command::new(IPTABLES)
            .arg(format!("-{action}"))
            .args(s.iter())
            .status();
    }
}

/// Bring ssh up (no-op when already running). dropbear daemonizes: our
/// direct child exits right after forking the real daemon — wait() for
/// it, or it becomes the zombie that poisons running() above.
pub fn enable() -> bool {
    if running() {
        return true;
    }
    invalidate_cache();
    rules("A");
    // cwd = the tree whose settings/SSH/ the patched binary resolves.
    // No -r: a single host-key path breaks ed25519 negotiation (banner,
    // then connection death at first KEX — found on device 2026-08-19;
    // koreader ships an ed25519 key only). -s: pubkey auth only.
    let (bin, tree) = if Path::new(OURS).exists() {
        (OURS, OURS_TREE)
    } else {
        (KOREADER, KOREADER_TREE)
    };
    let ok = Command::new(bin)
        .args(["-E", "-R", "-s", "-p", "2222", "-P", PIDFILE])
        .current_dir(tree)
        .spawn()
        .and_then(|mut c| c.wait())
        .is_ok();
    invalidate_cache();
    ok
}

/// Bring ssh down: TERM every dropbear (the pidfile may be stale after a
/// crash), then drop the firewall rules.
pub fn disable() {
    for pid in pids() {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    rules("D");
    let _ = fs::remove_file(PIDFILE);
    invalidate_cache();
}
