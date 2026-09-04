//! yb-reader on the yui stack: setup, then one MenuScreen whose selection
//! closures push the feature screens. All loops, refresh discipline and
//! edge gestures live in yui::App.

mod ai_stream;
mod awake;
mod backend;
mod backend_mupdf;
mod backend_yread;
mod books;
mod bootaudit;
mod cache;
mod chrome;
mod confirm_dialog;
mod crop_dialog;
mod curtain;
mod devices_screen;
mod dictionaries_screen;
mod dictionary;
mod dialogs;
mod document;
mod easter_egg;
mod flashcards;
mod footnote_dialog;
mod guard;
pub mod guide;
mod highlights_dialog;
mod home;
mod library;
mod mirror;
mod notes;
mod positions;
mod protocol;
mod quick_settings;
mod receive;
mod render;
mod screensavers;
mod scrubber_dialog;
mod selection;
mod split;
mod system;
#[cfg(test)]
pub mod testutil;
mod toc_dialog;
mod usb_screen;
mod vocab;
mod watchdog;
mod wifi;
mod word_dialog;

use ybdev::input::{self, Input};
use ybdev::log;
use ybdev::panel::Panel;
use yui::App;

use crate::curtain::CurtainScreen;

use crate::home::HomeScreen;

fn main() {
    let mut log_path = "/mnt/us/extensions/mirror/plugin.log".to_string();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i].as_str() == "--log"
            && i + 1 < args.len() {
                log_path = args[i + 1].clone();
                i += 1;
            }
        i += 1;
    }
    log::set_path(&log_path);
    // Service subcommands dispatch before any hardware is touched: the
    // watchdog and boot audit must survive (and stay cheap) even when
    // the panel/input setup paths of a broken build would not. They are
    // the recovery rungs for exactly that binary.
    if args.len() >= 2 {
        match args[1].as_str() {
            "bootaudit" => std::process::exit(bootaudit::run()),
            "--watchdog" => {
                let pid = args
                    .get(2)
                    .and_then(|p| p.parse::<i32>().ok())
                    .unwrap_or(0);
                if pid <= 0 {
                    eprintln!("yb-reader: --watchdog <pid>");
                    std::process::exit(2);
                }
                std::process::exit(watchdog::run(pid));
            }
            _ => {}
        }
    }
    // From here on, any panic or TERM/INT leaves a reason in the log and
    // the hardware (frontlight, Wi-Fi, firewall) in a sane state.
    guard::install();
    ybdev::sysinfo::set_cpu_governor("ondemand");

    let panel = match Panel::open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("yb-reader: {}", e);
            std::process::exit(1);
        }
    };
    // Ground truth for every field report: what the panel actually resolved
    // to (fbink -I says 1236x1648, stride 1248, hwtcon_v2 on this PW5).
    log::plog(&format!(
        "panel: {}x{} stride={} bpp={} map={}",
        panel.width,
        panel.height,
        panel.stride,
        panel.bpp,
        panel.map_len()
    ));
    let touch = input::discover().unwrap_or_else(|| "/dev/input/touch".to_string());
    log::plog(&format!("touch: {}", touch));
    let input = match Input::open(&touch) {
        Ok(i) => i,
        Err(e) => {
            log::plog(&format!("fatal: cannot open touch input {touch}: {e}"));
            eprintln!("yb-reader: no input device: {e}");
            guard::graceful_exit(1);
        }
    };

    let mut app = match App::new(panel, input) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("yb-reader: {}", e);
            std::process::exit(1);
        }
    }
    .with_edge_overlay(Box::new(|| Box::new(CurtainScreen::new())))
    .with_resume(Box::new(awake::on_resume))
    .with_quit_check(Box::new(guard::pending))
    .with_heartbeat(Box::new(watchdog::heartbeat_touch))
    .with_usb_exit(
        Box::new(|| Box::new(usb_screen::UsbScreen::new())),
        Box::new(awake::usb_drive_mode),
    )
    .with_sleep_state(Box::new(|asleep| {
        // Persistent sleep-state marker: lets the boot audit tell an
        // overnight battery death in suspend from an awake hang. One
        // tiny write per sleep/wake edge — never per tick.
        let marker = std::path::Path::new(bootaudit::STATE_DIR).join("sleeping");
        if asleep {
            let _ = std::fs::create_dir_all(bootaudit::STATE_DIR);
            let _ = std::fs::write(&marker, b"");
        } else {
            let _ = std::fs::remove_file(&marker);
        }
    }));
    let (w, h) = app.dims();

    awake::spawn();
    awake::boot_restore();
    screensavers::prewarm();
    let root = HomeScreen::new(w, h);
    app.run(Box::new(root));
    // Release the awake hold as the last hardware call: whoever comes
    // next (the framework on exit-42, the launcher in stock mode) must
    // not inherit a preventScreenSaver we set.
    wifi::keep_awake(false);
    // TERM/INT: restore and exit 0 — boot.sh clears its crash counter on
    // rc 0, and a shutdown cascade must not look like the crash fallback.
    if guard::pending() {
        guard::graceful_exit(0);
    }
    // USB bow-out: a host has drive mode configured (/mnt/us exported).
    // Exit 43 — boot.sh's plug branch parks at its unplug wait (never
    // respawning against an exported disk) and upstart restarts exactly
    // one fresh instance after the cable is out.
    if awake::usb_drive_mode() {
        log::plog("usb: host drive mode configured — bowing out (43)");
        // Docked drive mode: e-ink holds the farewell frame for free,
        // so the backlight is pure waste — off it goes for the plug.
        if let Ok(fl) = ybdev::frontlight::Frontlight::open() {
            fl.set(0);
            fl.tone_set(0);
        }
        guard::graceful_exit(43);
    }
    // Takeover mode: leaving the app means "back to the stock Kindle" —
    // exit 42 is boot.sh's cue to remove the flag and start the
    // framework. If the upstart job is not installed, auto-remove the orphan flag.
    if std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists() {
        if std::path::Path::new("/etc/upstart/yb-reader.conf").exists() {
            guard::graceful_exit(42);
        } else {
            let _ = std::fs::remove_file("/mnt/us/DONT_START_FRAMEWORK");
        }
    }
    log::plog("yb-reader exit");
}
