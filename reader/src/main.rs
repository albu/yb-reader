//! yb-reader on the yui stack: setup, then one MenuScreen whose selection
//! closures push the feature screens. All loops, refresh discipline and
//! edge gestures live in yui::App.

mod books;
mod cache;
mod confirm_dialog;
mod curtain;
mod flashcards;
mod footnote_dialog;
mod guard;
mod highlights_dialog;
mod home;
mod mirror;
mod notes;
mod positions;
mod protocol;
mod receive;
mod scrubber_dialog;
mod settings_dialog;
mod split;
mod toc_dialog;
mod vocab;
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
        match args[i].as_str() {
            "--log" => {
                if i + 1 < args.len() {
                    log_path = args[i + 1].clone();
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    log::set_path(&log_path);
    // From here on, any panic or TERM/INT leaves a reason in the log and
    // the hardware (frontlight, Wi-Fi, firewall) in a sane state.
    guard::install();

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
            log::plog(&format!("no input: {} — running without touch", e));
            // Keep the app alive; menus just won't respond.
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    };

    let mut app = match App::new(panel, input) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("yb-reader: {}", e);
            std::process::exit(1);
        }
    }
    .with_edge_overlay(Box::new(|| Box::new(CurtainScreen::new())));
    let (w, h) = app.dims();

    let root = HomeScreen::new(w, h);
    app.run(Box::new(root));
    log::plog("yb-reader exit");
}
