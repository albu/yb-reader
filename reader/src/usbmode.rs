//! USB charge-only policy: while the reader lives on /mnt/us, the
//! stock USB drive mode must never engage. Exporting the partition
//! unmounts it from under the running app (binary, positions and logs
//! all live there), drops Wi-Fi, and — when the forced unmount evicts
//! the app's mappings — kills the process outright (the silent "dead
//! kindle" of 2026-08-23).
//!
//! The firmware binds the LEGACY g_mass_storage gadget driver on plug
//! (not a configfs tree — a 250 ms configfs unbind watchdog lost that
//! race on 2026-08-23). Both paths, legacy and configfs, need the
//! loadable modules g_mass_storage + usb_f_mass_storage, so removing
//! them at startup makes drive mode physically impossible: there is no
//! race, the firmware's bind simply fails and the port becomes a dumb
//! charger. The loop re-asserts the removal (in case anything modprobes
//! them back) and heals a lost-race unmount — remounting /mnt/us only
//! once no gadget remains, never while the laptop owns the disk.
//!
//! guard::restore modprobes the modules back on the way out, so stock
//! mode ("Exit to Kindle") keeps working drive mode. Deliberate
//! trade-off while the reader runs: plugging in never gives the laptop
//! a drive. File transfer goes over the web manager or SSH.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use ybdev::log::plog;

const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget";
const MOUNT_POINT: &str = "/mnt/us";
/// Persisted mode, alongside ssh/wifi state under /var/local/yb-reader.
const MODE_FILE: &str = "/var/local/yb-reader/usb";
/// The legacy gadget driver — stock volumd loads and unloads exactly
/// this (kdb TURN_ON/TURN_OFF_FILESTORAGE_COMMAND); the usb_f_mass_
/// storage function module stays resident, stock never removes it.
const MS_MODULE: &str = "g_mass_storage";

/// User's USB mode: false = charge-only (the reader keeps the mass-
/// storage modules out), true = file transfer (stock drive mode).
static TRANSFER: AtomicBool = AtomicBool::new(false);

pub fn parse_mode(s: &str) -> bool {
    s.trim() == "transfer"
}

pub fn format_mode(transfer: bool) -> &'static str {
    if transfer {
        "transfer"
    } else {
        "charging"
    }
}

pub fn transfer_mode() -> bool {
    TRANSFER.load(Ordering::Relaxed)
}

/// The curtain toggle: persist, then apply immediately.
pub fn set_transfer(on: bool) {
    TRANSFER.store(on, Ordering::Relaxed);
    let _ = std::fs::write(MODE_FILE, format_mode(on));
    if on {
        if restore_modules() {
            plog("usb: transfer mode — stock drive mode engaged");
        } else {
            plog("usb: transfer mode — modprobe FAILED (see dmesg)");
        }
    } else {
        remove_modules();
        plog("usb: charging mode — mass-storage driver removed");
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UsbAction {
    /// Nothing to fight: no gadget bound, disk where it belongs.
    None,
    /// A configfs gadget is bound to the UDC — unbind before the host
    /// mounts (belt: the module removal should have prevented it).
    UnbindGadget,
    /// Gadget gone but /mnt/us unmounted (a lost race) — remount.
    Remount,
}

/// Pure decision, host-testable. `gadget_bound`: any configfs gadget
/// currently bound to the UDC (drive mode engaging). `mounted`: the
/// reader partition is present in the mount table.
pub fn decide(gadget_bound: bool, mounted: bool) -> UsbAction {
    if gadget_bound {
        UsbAction::UnbindGadget
    } else if !mounted {
        UsbAction::Remount
    } else {
        UsbAction::None
    }
}

/// Pure: does the mass-storage driver appear in a /proc/modules dump?
pub fn ms_modules_loaded(proc_modules: &str) -> bool {
    proc_modules.lines().any(|l| l.starts_with(MS_MODULE))
}

fn ms_modules_loaded_live() -> bool {
    std::fs::read_to_string("/proc/modules")
        .map(|t| ms_modules_loaded(&t))
        .unwrap_or(false)
}

fn modprobe(args: &[&str]) -> bool {
    Command::new("modprobe")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Stock's TURN_OFF: rmmod the legacy driver only (volumd's kdb recipe,
/// verbatim — the function module stays resident like stock leaves it).
pub fn remove_modules() -> bool {
    modprobe(&["-r", MS_MODULE])
}

/// Stock's TURN_ON, verbatim (kdb TURN_ON_FILESTORAGE_COMMAND): the
/// module REFUSES to start without an explicit LUN file param — a bare
/// `modprobe g_mass_storage` fails with -22 "no file given for LUN0"
/// (found on device 2026-08-23). The empty medium + removable flag let
/// the daemons attach the real store after binding.
pub fn restore_modules() -> bool {
    modprobe(&[
        MS_MODULE,
        "file=",
        "removable=1",
        "idVendor=0x1949",
        "idProduct=0x0324",
        "stall=0",
    ])
}

/// UDC control files of all currently bound configfs gadgets.
fn bound_udcs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(GADGET_DIR) else {
        return out;
    };
    for e in rd.flatten() {
        let udc = e.path().join("UDC");
        if let Ok(s) = std::fs::read_to_string(&udc) {
            if !s.trim().is_empty() {
                out.push(udc);
            }
        }
    }
    out
}

/// The /proc/mounts entry for the reader partition, as (dev, fstype,
/// opts) — captured while healthy so a later remount can reproduce it.
fn mount_spec() -> Option<(String, String, String)> {
    let txt = std::fs::read_to_string("/proc/mounts").ok()?;
    for l in txt.lines() {
        let mut it = l.split_whitespace();
        match (it.next(), it.next(), it.next(), it.next(), it.next()) {
            (Some(dev), Some(mnt), Some(fs), Some(_), Some(opts)) if mnt == MOUNT_POINT => {
                return Some((dev.to_string(), fs.to_string(), opts.to_string()));
            }
            _ => {}
        }
    }
    None
}

/// Remount a lost block filesystem. FUSE-backed mounts (the fsp daemon
/// owns /mnt/us on this firmware) are volumd's job — it re-serves the
/// export after unplug itself, and `mount -t fuse.fsp` from us would
/// just fail — so those report false without trying.
fn remount(spec: &(String, String, String)) -> bool {
    let (dev, fs, opts) = spec;
    if fs.starts_with("fuse") {
        return false;
    }
    Command::new("mount")
        .args(["-t", fs, "-o", opts, dev, MOUNT_POINT])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One-shot startup: adopt the persisted mode and take the modules down
/// if charge-only. Synchronous by design — a modprobe at boot is not on
/// any latency path (this used to be the prologue of a dedicated 1s
/// watchdog thread).
pub fn init() {
    // Persisted mode wins: transfer mode leaves stock USB alone.
    let transfer = std::fs::read_to_string(MODE_FILE)
        .map(|t| parse_mode(&t))
        .unwrap_or(false);
    TRANSFER.store(transfer, Ordering::Relaxed);
    if transfer {
        plog("usb: transfer mode (persisted) — drive mode untouched");
    } else if remove_modules() {
        plog("usb: mass-storage modules removed — charge-only while reading");
    } else {
        // Expected when plugged at launch (module in use): the tick
        // below wins once the firmware unbinds on unplug.
        plog("usb: mass-storage modules busy at start — healer will retry");
    }
}

/// Loop-carried state of the old watchdog.
struct WatchdogState {
    /// Captured at startup — the reader only launches with its
    /// partition mounted, so this always succeeds in practice.
    spec: Option<(String, String, String)>,
    was_bound: bool,
    healed: bool,
}
static WATCHDOG: std::sync::Mutex<Option<WatchdogState>> = std::sync::Mutex::new(None);

/// One pass of what used to be the 1 Hz watchdog thread. The healer's
/// real work is rare (plug/unplug edges, module reappearances), so it
/// now rides awake's 5 s tick — called from there and on the vbus edge
/// it already detects. One thread fewer, and the SoC sleeps through
/// what used to be 86k wakeups/day of /proc polling.
pub fn tick() {
    let mut guard = WATCHDOG.lock().unwrap_or_else(|e| e.into_inner());
    let st = guard.get_or_insert_with(|| WatchdogState {
        spec: mount_spec(),
        was_bound: false,
        healed: false,
    });
    if transfer_mode() {
        // Stock behavior, except the healer: with a frozen
        // framework nothing remounts /mnt/us after the laptop
        // ejects, and the library would stay empty forever.
        if mount_spec().is_none() && bound_udcs().is_empty() {
            if let Some(s) = st.spec.as_ref() {
                if remount(s) && !st.healed {
                    plog("usb: remounted /mnt/us after transfer mode");
                    st.healed = true;
                }
            }
        } else {
            st.healed = false;
        }
    } else {
        if ms_modules_loaded_live() {
            if remove_modules() {
                plog("usb: mass-storage modules came back — removed again");
            }
        }
        let udcs = bound_udcs();
        let bound = !udcs.is_empty();
        let mounted = mount_spec().is_some();
        match decide(bound, mounted) {
            UsbAction::UnbindGadget => {
                for udc in &udcs {
                    let _ = std::fs::write(udc, "");
                }
                if !st.was_bound {
                    plog("usb: configfs gadget bound — unbinding");
                    st.was_bound = true;
                }
            }
            UsbAction::Remount => {
                // Only when no gadget remains: remounting under a
                // host-owned export would double-mount the disk.
                if let Some(s) = st.spec.as_ref() {
                    if remount(s) && !st.healed {
                        plog("usb: remounted /mnt/us after drive-mode race");
                        st.healed = true;
                    }
                }
            }
            UsbAction::None => {
                st.was_bound = false;
                st.healed = false;
                if let Some(s) = mount_spec() {
                    st.spec = Some(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_only_truth_table() {
        // Healthy idle: nothing bound, disk mounted.
        assert_eq!(decide(false, true), UsbAction::None);
        // Plug: gadget appears — unbind wins over everything.
        assert_eq!(decide(true, true), UsbAction::UnbindGadget);
        assert_eq!(decide(true, false), UsbAction::UnbindGadget);
        // Lost race aftermath: gadget gone (unplug), disk still away.
        assert_eq!(decide(false, false), UsbAction::Remount);
    }

    #[test]
    fn module_detection_matches_proc_modules_lines() {
        let loaded = "libcomposite 32776 2 g_mass_storage,usb_f_mass_storage\n\
                      g_mass_storage 2340 0\n";
        assert!(ms_modules_loaded(loaded));
        // Function module present but driver absent = drive mode off
        // (stock leaves usb_f_mass_storage resident — only the legacy
        // driver matters). Use-count columns must not false-positive.
        let resident = "usb_f_mass_storage 32216 2 g_mass_storage\n\
                        libcomposite 32776 0 Live 0x0000\n";
        assert!(!ms_modules_loaded(resident));
        assert!(!ms_modules_loaded(""));
    }

    #[test]
    fn mode_file_round_trip() {
        for on in [false, true] {
            assert_eq!(parse_mode(format_mode(on)), on);
        }
        // Tolerant of trailing newline / case of unknown junk.
        assert!(parse_mode("transfer\n"));
        assert!(!parse_mode("charging\n"));
        assert!(!parse_mode(""));
        assert!(!parse_mode("garbage"));
    }
}
