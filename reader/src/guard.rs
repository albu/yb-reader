//! Die with dignity. On a headless device the default panic reporter
//! writes to a stderr that goes nowhere, and a terminating signal gives
//! the process no chance to undo the sleep/receive hardware state — a
//! crash mid-sleep would leave the frontlight dark and Wi-Fi off, a
//! crash mid-receive an open firewall rule. This module installs a hook
//! that logs the reason and restores what it can before the process goes
//! down. Best effort by design: it runs on the way out, once.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Reentrancy guard: a panic raised inside the restore path must not
/// recurse into the hook again.
static RESTORING: AtomicBool = AtomicBool::new(false);

/// TERM/INT received since the main loop last polled. The signal handler
/// is async-signal-safe by construction (a plain atomic store); the actual
/// restore — plog, fs, Command, lipc — is NOT safe inside a signal frame
/// (malloc and std locks may be held by another thread and deadlock), so
/// it runs on the main loop's next tick via [`pending`] and
/// `App::with_quit_check`.
static TERM_PENDING: AtomicI32 = AtomicI32::new(0);

pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        ybdev::log::plog(&format!("panic: {}", info));
        restore();
        // Fall through to the default abort/unwind behavior; the hook
        // doesn't return a process exit, and the log already has the
        // reason.
    }));
    // SIGKILL cannot be caught (deploy's -9 fallback uses it); TERM/INT
    // flag the main loop, which runs the same restore before exit.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_term as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, handle_term as *const () as libc::sighandler_t);
    }
}

extern "C" fn handle_term(sig: libc::c_int) {
    TERM_PENDING.store(sig, Ordering::SeqCst);
}

/// Non-consuming poll: true once a TERM/INT has arrived. Read from the
/// main loop each tick (via `App::with_quit_check`) and again in main
/// after the loop returns; both see the same sticky flag, and the restore
/// + exit path that follows makes consuming unnecessary.
pub fn pending() -> bool {
    TERM_PENDING.load(Ordering::SeqCst) != 0
}

/// Clean exit through the same restore path the TERM guard uses, with a
/// caller-chosen code. 42 is the takeover "return to stock" handshake:
/// boot.sh removes the flag and starts the framework when it sees it.
pub fn graceful_exit(code: i32) -> ! {
    ybdev::log::plog(&format!("graceful exit ({code})"));
    restore();
    std::process::exit(code);
}

fn restore() {
    if RESTORING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Pre-sleep frontlight (if we died sleeping with it zeroed), Wi-Fi
    // back to the framework default, and the receive listener's firewall
    // rule plus any half-written upload gone. USB mass-storage comes
    // back too — whoever runs next (stock framework on the exit-42
    // handoff, the launcher in stock mode) must find working drive mode.
    yui::widgets::emergency_wake_restore();
    crate::receive::emergency_cleanup();
    crate::usbmode::restore_modules();
}
