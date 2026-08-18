//! Die with dignity. On a headless device the default panic reporter
//! writes to a stderr that goes nowhere, and a terminating signal gives
//! the process no chance to undo the sleep/receive hardware state — a
//! crash mid-sleep would leave the frontlight dark and Wi-Fi off, a
//! crash mid-receive an open firewall rule. This module installs a hook
//! that logs the reason and restores what it can before the process goes
//! down. Best effort by design: it runs on the way out, once.

use std::sync::atomic::{AtomicBool, Ordering};

/// Reentrancy guard: a panic raised inside the restore path must not
/// recurse into the hook again.
static RESTORING: AtomicBool = AtomicBool::new(false);

pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        ybdev::log::plog(&format!("panic: {}", info));
        restore();
        // Fall through to the default abort/unwind behavior; the hook
        // doesn't return a process exit, and the log already has the
        // reason.
    }));
    // SIGKILL cannot be caught (deploy's -9 fallback uses it); TERM/INT
    // run the same restore before exit. The handler calls into std
    // (fs, Command), which is not async-signal-safe in the strict POSIX
    // sense — an accepted trade for a last-ditch courtesy path on a
    // single-user device, the same pragmatism KOReader's signal handling
    // carries.
    unsafe {
        libc::signal(libc::SIGTERM, handle_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_term as *const () as libc::sighandler_t);
    }
}

extern "C" fn handle_term(_sig: libc::c_int) {
    ybdev::log::plog("signal: TERM/INT — restoring hardware state");
    restore();
    std::process::exit(0);
}

fn restore() {
    if RESTORING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Pre-sleep frontlight (if we died sleeping with it zeroed), Wi-Fi
    // back to the framework default, and the receive listener's firewall
    // rule plus any half-written upload gone.
    yui::widgets::emergency_wake_restore();
    crate::receive::emergency_cleanup();
}
