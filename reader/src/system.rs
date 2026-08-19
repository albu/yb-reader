//! The System screen — device-level controls that are *not* daily use:
//! boot mode, reboot, Wi-Fi, the E-Ink refresh cadence, and status
//! trivia. Reached from the home rows. The curtain keeps the daily
//! controls (clock, statuses, light) and nothing else — this screen is
//! where the rare stuff went when the curtain grew too heavy.

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;
use ybdev::sysinfo;

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;
const TITLE_BASE_PT: f32 = 30.0;
const TITLE_SIZE_PT: f32 = 13.0;
const INFO_BASE_PT: f32 = 47.0;

const CARD_TOP_PT: f32 = 66.0;
const CARD_H_PT: f32 = 34.0;
const CARD_GAP_PT: f32 = 8.0;

const REFRESH_TITLE_PT: f32 = 208.0;
const REFRESH_PILL_PT: f32 = 224.0;

const DIM: u8 = 110;
const INK: u8 = 0;
const CARD_BG: u8 = 246;
const CARD_BORDER: u8 = 200;
const PILL_BG: u8 = 242;
const PILL_ACTIVE_BG: u8 = 30;

const REFRESH_PRESETS: [(&str, usize); 4] = [("Off", 0), ("5 pgs", 5), ("10 pgs", 10), ("20 pgs", 20)];

pub struct SystemScreen {
    w: i32,
    h: i32,
}

fn draw_box_text(p: &mut Painter, r: Rect, size_pt: f32, color: u8, text: &str) {
    let tw = p.text_width(size_pt, text) as i32;
    let tx = r.x + (r.w - tw).max(0) / 2;
    let ty = r.y + (r.h - pt(size_pt)).max(0) / 2 + pt(size_pt * 0.82);
    p.text(tx, ty, size_pt, color, text);
}

impl SystemScreen {
    pub fn new() -> SystemScreen {
        SystemScreen { w: 1236, h: 1648 }
    }

    fn draw_card(p: &mut Painter, r: Rect, title: &str, value: &str, sub: &str, armed: bool) {
        p.rect(r, CARD_BG);
        p.rect_outline_t(r, 1, CARD_BORDER);
        if armed {
            p.rect_outline_t(r, 2, INK);
        }
        p.text(r.x + pt(8.0), r.y + pt(9.0), 6.5, DIM, title);
        p.text(r.x + pt(8.0), r.y + pt(21.0), 9.0, INK, value);
        if !sub.is_empty() {
            p.text(r.x + pt(8.0), r.y + pt(29.0), 6.0, DIM, sub);
        }
    }

    /// The one card that is an action, not a status: dark fill, light
    /// text — same language as the curtain's Close pill.
    fn draw_action_card(p: &mut Painter, r: Rect, title: &str, value: &str, sub: &str) {
        p.rect(r, PILL_ACTIVE_BG);
        p.text(r.x + pt(8.0), r.y + pt(9.0), 6.5, 180, title);
        p.text(r.x + pt(8.0), r.y + pt(21.0), 9.0, 255, value);
        if !sub.is_empty() {
            p.text(r.x + pt(8.0), r.y + pt(29.0), 6.0, 180, sub);
        }
    }

    fn draw_pills(p: &mut Painter, y: i32, w: i32, cur: usize) {
        let pad = pt(PAD_PT);
        let gap = pt(5.0);
        let total_w = w - 2 * pad;
        let n = REFRESH_PRESETS.len() as i32;
        let pill_w = (total_w - (n - 1) * gap) / n;
        let pill_h = pt(16.0);

        for (i, (label, val)) in REFRESH_PRESETS.iter().enumerate() {
            let r = Rect::new(pad + i as i32 * (pill_w + gap), y, pill_w, pill_h);
            if *val == cur {
                p.rect(r, PILL_ACTIVE_BG);
                draw_box_text(p, r, 7.0, 255, label);
            } else {
                p.rect(r, PILL_BG);
                p.rect_outline_t(r, 1, CARD_BORDER);
                draw_box_text(p, r, 7.0, INK, label);
            }
        }
    }
}

impl Default for SystemScreen {
    fn default() -> Self {
        SystemScreen::new()
    }
}

impl Screen for SystemScreen {
    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);

        p.text(pad, pt(TITLE_BASE_PT), TITLE_SIZE_PT, INK, "System");
        p.text_right(w - pad, pt(TITLE_BASE_PT), 8.0, 160, concat!("v", env!("YB_BUILD")));
        p.hline_t(pt(TITLE_BASE_PT) + pt(9.0), pad, w - pad, 2, 180);

        // Status trivia — the stuff that used to be a curtain card.
        let mut bits: Vec<String> = Vec::new();
        if let Some(g) = sysinfo::storage_free_gb() {
            bits.push(format!("{g:.1} GB free"));
        }
        if let Some(k) = sysinfo::mem_available_kib() {
            bits.push(format!("{:.0}M RAM free", k as f64 / 1024.0));
        }
        let (cap, plugged) = sysinfo::battery();
        bits.push(if plugged {
            format!("charging {}%", cap)
        } else {
            format!("battery {}%", cap)
        });
        p.text(pad, pt(INFO_BASE_PT), 7.0, DIM, &bits.join("   ·   "));

        // Card stack: boot mode / Wi-Fi / reboot.
        let cw = w - 2 * pad;
        let ch = pt(CARD_H_PT);
        let gap = pt(CARD_GAP_PT);
        let top = pt(CARD_TOP_PT);

        let os_boot = std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists();
        let (bv, bs) = if os_boot {
            ("yb OS", "Tap: switch to Stock")
        } else {
            ("Stock", "Tap: switch to yb OS")
        };
        SystemScreen::draw_card(
            p,
            Rect::new(pad, top, cw, ch),
            "BOOT MODE",
            bv,
            bs,
            os_boot,
        );

        let wifi_on = crate::wifi::wifi_state() == Some(true);
        let (wv, ws) = if wifi_on {
            ("On", sysinfo::wifi_ip().unwrap_or_else(|| "connecting…".to_string()))
        } else {
            ("Off", "Tap to turn on".to_string())
        };
        SystemScreen::draw_card(
            p,
            Rect::new(pad, top + ch + gap, cw, ch),
            "WI-FI",
            wv,
            &ws,
            wifi_on,
        );

        let next = if os_boot { "yb OS" } else { "Stock Kindle" };
        SystemScreen::draw_action_card(
            p,
            Rect::new(pad, top + 2 * (ch + gap), cw, ch),
            "REBOOT",
            "Reboot now",
            &format!("Next boot: {next}"),
        );

        // E-Ink full-refresh cadence.
        let cur = crate::positions::global_refresh_interval();
        p.text(pad, pt(REFRESH_TITLE_PT), 8.0, INK, "E-Ink full refresh");
        p.text_right(
            w - pad,
            pt(REFRESH_TITLE_PT),
            7.0,
            DIM,
            "pages between full flashes",
        );
        SystemScreen::draw_pills(p, pt(REFRESH_PILL_PT), w, cur);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                let pad = pt(PAD_PT);
                let cw = self.w - 2 * pad;
                let ch = pt(CARD_H_PT);
                let gap = pt(CARD_GAP_PT);
                let top = pt(CARD_TOP_PT);

                // Boot mode: flip the takeover flag for the next boot.
                // Applying it is the Reboot card (or any power-cycle) — a
                // hot-switch from a live session would race a second
                // reader instance (start.sh's lock exists for that).
                let r_boot = Rect::new(pad, top, cw, ch);
                if r_boot.contains(x, y) {
                    let flag = std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK");
                    if flag.exists() {
                        let _ = std::fs::remove_file(flag);
                    } else {
                        let _ = std::fs::File::create(flag);
                    }
                    return Action::Redraw;
                }

                let r_wifi = Rect::new(pad, top + ch + gap, cw, ch);
                if r_wifi.contains(x, y) {
                    if crate::wifi::wifi_state() == Some(true) {
                        // Pure lipc — a raw `ifconfig wlan0 down` leaves
                        // the interface administratively down and
                        // `wifid enable 1` can never bring it back
                        // (found on device 2026-08-19).
                        let _ = std::process::Command::new("lipc-set-prop")
                            .args(["-i", "com.lab126.wifid", "enable", "0"])
                            .status();
                        let _ = std::process::Command::new("lipc-set-prop")
                            .args(["-i", "com.lab126.cmd", "wirelessEnable", "0"])
                            .status();
                    } else {
                        crate::wifi::turn_on_wifi();
                    }
                    return Action::Redraw;
                }

                let r_reboot = Rect::new(pad, top + 2 * (ch + gap), cw, ch);
                if r_reboot.contains(x, y) {
                    // Plain `reboot` rides the same init cascade as a
                    // long-press power (TERM -> reader guard restores
                    // frontlight/wifi/firewall) — no teardown of our own
                    // needed.
                    let next = if std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists() {
                        "yb OS"
                    } else {
                        "Stock Kindle"
                    };
                    return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                        "Reboot now?",
                        &format!("Next boot: {next}."),
                        "Reboot",
                        None,
                        move |act| {
                            if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                plog("system: reboot requested");
                                let spawned = std::process::Command::new("reboot")
                                    .spawn()
                                    .or_else(|_| std::process::Command::new("/sbin/reboot").spawn());
                                if spawned.is_err() {
                                    plog("system: reboot command failed");
                                }
                            }
                            Action::Pop
                        },
                    )));
                }

                // Refresh-interval pills.
                let py = pt(REFRESH_PILL_PT);
                let ph = pt(16.0);
                if y >= py - 6 && y < py + ph + 6 {
                    let gap2 = pt(5.0);
                    let total = self.w - 2 * pad;
                    let n = REFRESH_PRESETS.len() as i32;
                    let pw = (total - (n - 1) * gap2) / n;
                    for (i, (_, val)) in REFRESH_PRESETS.iter().enumerate() {
                        let px = pad + i as i32 * (pw + gap2);
                        if x >= px && x < px + pw {
                            crate::positions::set_global_refresh_interval(*val);
                            return Action::Redraw;
                        }
                    }
                }

                Action::Keep
            }
            Gesture::Swipe { dir, .. } if matches!(dir, SwipeDir::North | SwipeDir::South) => {
                Action::Pop
            }
            Gesture::TwoFingerTap => Action::Pop,
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    /// Headless render: title, card stack, and pills must all put ink on
    /// the page (mirrors the curtain's regression guard).
    #[test]
    fn system_screen_renders_content() {
        let font = Font::load().unwrap();
        let mut buf = vec![255u8; 1248 * 1648];
        let mut s = SystemScreen::new();
        {
            let mut canvas = vec![0u8; 1236 * 1648];
            let mut p = yui::Painter::new(
                &mut buf,
                1236,
                1648,
                1248,
                yui::Orientation::Portrait,
                &mut canvas,
                &font,
            );
            s.draw(&mut p);
            p.flush();
        }
        let ink = |name: &str, y0: usize, y1: usize, min: usize| {
            let n = buf[y0 * 1248..y1 * 1248].iter().filter(|&&b| b < 140).count();
            assert!(n >= min, "{name}: only {n} ink pixels in rows {y0}-{y1}");
        };
        let lit = buf.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1248 * 1648 * 90 / 100, "page is not white: {lit}");
        ink("title", 90, 170, 60);
        ink("cards", 270, 780, 150);
        ink("refresh", 860, 1010, 40);
    }
}
