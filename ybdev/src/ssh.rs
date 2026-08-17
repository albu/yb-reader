//! ssh, owned by the app: the KOReader-bundled dropbear on port 2222 —
//! the very one the KOReader scriptlet starts (and the only thing that
//! ever started it, so a reboot left ssh dead until someone tapped that
//! menu). Same recipe as koreader-ext.sh's start_ssh/stop_ssh: an
//! iptables accept rule plus `/mnt/us/koreader/dropbear -p 2222`.

use std::fs;
use std::process::{Command, Stdio};

const PIDFILE: &str = "/tmp/dropbear_koreader.pid";
const DIR: &str = "/mnt/us/koreader";
const IPTABLES: &str = "/usr/sbin/iptables";

/// Any live dropbear? A /proc comm scan — no forks, cheap enough to call
/// per redraw. (busybox `ps` is unreliable on this FW; /proc is truth.)
pub fn running() -> bool {
    pids().next().is_some()
}

fn pids() -> impl Iterator<Item = i32> {
    fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(|e| {
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
            .map(|st| !st.lines().any(|l| l.starts_with("State:\tZ")))
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
    rules("A");
    Command::new("./dropbear")
        .current_dir(DIR)
        .args(["-E", "-R", "-p", "2222", "-P", PIDFILE])
        .spawn()
        .and_then(|mut c| c.wait())
        .is_ok()
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
}
