//! The top curtain — the daily control sheet: big clock + date, the
//! 2x2 status grid (battery / wifi / ssh / memory), and two bare
//! frontlight sliders. Opened by the top-edge swipe / two-finger tap
//! from anywhere (the App edge overlay).
//!
//! Everything not daily (boot mode, reboot, refresh cadence, trivia)
//! moved to the System screen (home row) — this sheet stays one screen
//! tall. Visually it floats: the first paint snapshots the screen it
//! was opened over, and the sheet is drawn over a scanline-dimmed copy
//! of it (the same language the dialogs speak).

use std::process::Command;

use ybdev::frontlight::Frontlight;
use ybdev::input::{Gesture, SwipeDir};
use ybdev::sysinfo;

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

// --- layout (pt) — Total screen height is 395 pt (1648 px) ---
const HANDLE_TOP_PT: f32 = 8.0;
const CLOCK_BASE_PT: f32 = 36.0;
const CLOCK_SIZE_PT: f32 = 24.0;
const DATE_BASE_PT: f32 = 52.0;
const DATE_SIZE_PT: f32 = 8.5;

const CARD_TOP_PT: f32 = 62.0;
const CARD_H_PT: f32 = 30.0;
const CARD_GAP_PT: f32 = 5.0;
const PAD_PT: f32 = 18.0;

const BRIGHT_ROW_PT: f32 = 146.0;
const TONE_ROW_PT: f32 = 176.0;
const ROW_CY_OFF_PT: f32 = 11.0;
const SLIDER_X_OFF_PT: f32 = 52.0;
const KNOB_R_PX: i32 = 5;

const ACTIONS_TOP_PT: f32 = 210.0;
const ACTION_H_PT: f32 = 28.0;

const SHEET_H_PT: f32 = 252.0;

// --- grays on white ---
const DIM: u8 = 110;
const INK: u8 = 0;
const TRACK: u8 = 210;
const CARD_BG: u8 = 246;
const CARD_BORDER: u8 = 200;
const PILL_BG: u8 = 242;
const PILL_ACTIVE_BG: u8 = 30;

pub struct CurtainScreen {
    fl: Option<Frontlight>,
    time: String,
    date: String,
    // The screen underneath, captured on first paint (the buffer still
    // holds it then) — the sheet floats over a dimmed copy.
    bg: Option<Vec<u8>>,
    // Cached layout geometry in visual px
    w: i32,
    h: i32,
    track_x0: i32,
    track_x1: i32,
}

fn draw_box_text(p: &mut Painter, r: Rect, size_pt: f32, color: u8, text: &str) {
    let tw = p.text_width(size_pt, text) as i32;
    let tx = r.x + (r.w - tw).max(0) / 2;
    let ty = r.y + (r.h - pt(size_pt)).max(0) / 2 + pt(size_pt * 0.82);
    p.text(tx, ty, size_pt, color, text);
}


impl CurtainScreen {
    pub fn new() -> CurtainScreen {
        let (time, date) = Command::new("date")
            .arg("+%H:%M|%A, %d %B")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .map(|s| match s.split_once('|') {
                Some((t, d)) => (t.to_string(), d.to_string()),
                None => (s, String::new()),
            })
            .unwrap_or_else(|| ("--:--".to_string(), String::new()));

        CurtainScreen {
            fl: None,
            time,
            date,
            bg: None,
            w: 1236,
            h: 1648,
            track_x0: 0,
            track_x1: 0,
        }
    }

    fn draw_card(p: &mut Painter, r: Rect, title: &str, value: &str, sub: &str) {
        p.rect(r, CARD_BG);
        p.rect_outline_t(r, 1, CARD_BORDER);
        p.text(r.x + pt(8.0), r.y + pt(9.0), 6.5, DIM, title);
        p.text(r.x + pt(8.0), r.y + pt(20.0), 9.0, INK, value);
        if !sub.is_empty() {
            p.text(r.x + pt(8.0), r.y + pt(28.0), 6.0, DIM, sub);
        }
    }

    /// One bare slider line: icon, percentage, thin track with a knob.
    /// Tap or drag anywhere on the row's band — no +/- buttons, no
    /// presets (compact by request; the knob is the only chrome).
    fn draw_line_slider(
        &self,
        p: &mut Painter,
        row_y: i32,
        amber: bool,
        frac: f32,
        pct: i32,
    ) {
        let pad = pt(PAD_PT);
        let cy = row_y + pt(ROW_CY_OFF_PT);

        // Icon
        let icon_r = pt(3.0);
        let icon_cx = pad + icon_r + pt(1.0);
        let icon_cy = cy - pt(2.0);
        if amber {
            // Warm Amber double ring
            p.circle_fill(icon_cx, icon_cy, icon_r, 60);
            p.circle_outline_t(icon_cx, icon_cy, icon_r + pt(1.5), 1, 140);
        } else {
            // Radiating Sun
            draw_sun_icon(p, icon_cx, icon_cy, icon_r, INK);
        }

        // Percentage
        p.text(pad + pt(16.0), cy + pt(3.0), 9.0, INK, &format!("{pct}%"));

        // Track + fill + knob
        let (x0, x1) = (self.track_x0, self.track_x1);
        p.rect(Rect::new(x0, cy - 1, x1 - x0, 2), TRACK);
        let fill = ((x1 - x0) as f32 * frac.clamp(0.0, 1.0)).round() as i32;
        if fill > 0 {
            p.rect(Rect::new(x0, cy - 1, fill, 2), INK);
        }
        let kx = (x0 + fill).clamp(x0, x1);
        p.circle_fill(kx, cy, KNOB_R_PX, INK);
        p.circle_fill(kx, cy, 2, 255);
    }
}

fn draw_sun_icon(p: &mut Painter, cx: i32, cy: i32, r: i32, color: u8) {
    p.circle_fill(cx, cy, r, color);
    let ray_len = pt(2.0);
    let ray_dist = r + pt(1.5);
    p.rect(Rect::new(cx - 1, cy - ray_dist - ray_len, 2, ray_len), color);
    p.rect(Rect::new(cx - 1, cy + ray_dist, 2, ray_len), color);
    p.rect(Rect::new(cx - ray_dist - ray_len, cy - 1, ray_len, 2), color);
    p.rect(Rect::new(cx + ray_dist, cy - 1, ray_len, 2), color);
}

fn draw_battery_icon(p: &mut Painter, x: i32, y: i32, cap: i32, plugged: bool) {
    let w = pt(13.0);
    let h = pt(7.5);
    p.rect_outline_t(Rect::new(x, y, w - pt(2.0), h), 1, INK);
    p.rect(Rect::new(x + w - pt(2.0), y + pt(2.0), pt(1.5), h - pt(4.0)), INK);
    let inner_w = ((w - pt(4.0)) as f32 * (cap as f32 / 100.0).clamp(0.0, 1.0)).round() as i32;
    if inner_w > 0 {
        p.rect(Rect::new(x + pt(1.0), y + pt(1.0), inner_w, h - pt(2.0)), INK);
    }
    if plugged {
        p.rect(Rect::new(x + pt(4.0), y + pt(2.0), pt(3.0), pt(3.5)), 255);
        p.line_w(x + pt(4.5), y + pt(1.0), x + pt(5.5), y + h - pt(1.0), 1, 0);
    }
}

fn draw_wifi_bars(p: &mut Painter, x: i32, y: i32, online: bool) {
    let bar_w = pt(1.8);
    let gap = pt(1.0);
    let color = if online { INK } else { DIM };
    for i in 0..4 {
        let bh = pt(2.0) + (i as i32 * pt(1.8));
        let bx = x + i as i32 * (bar_w + gap);
        let by = y + pt(8.0) - bh;
        p.rect(Rect::new(bx, by, bar_w, bh), color);
    }
}

fn draw_mem_icon(p: &mut Painter, x: i32, y: i32) {
    // Memory chip: outline + inner die + three pins top and bottom
    let w = pt(10.0);
    let h = pt(8.0);
    p.rect_outline_t(Rect::new(x, y, w, h), 1, INK);
    p.rect_outline_t(Rect::new(x + pt(2.5), y + pt(2.5), w - pt(5.0), h - pt(5.0)), 1, INK);
    for i in 0..3 {
        let px = x + pt(1.5) + i as i32 * pt(3.5);
        p.rect(Rect::new(px, y - pt(1.5), 1, pt(1.5)), INK);
        p.rect(Rect::new(px, y + h, 1, pt(1.5)), INK);
    }
}


impl Default for CurtainScreen {
    fn default() -> Self {
        CurtainScreen::new()
    }
}

impl Screen for CurtainScreen {
    fn on_enter(&mut self) -> Action {
        self.fl = Frontlight::open().ok();
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        self.track_x0 = pt(PAD_PT) + pt(SLIDER_X_OFF_PT);
        self.track_x1 = w - pt(PAD_PT);

        // The live screen underneath, dimmed by scanlines — the popup
        // pattern the dialogs use, so the reader stays visibly in place.
        if self.bg.is_none() {
            self.bg = Some(p.snapshot());
        }
        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
        }
        for y in (0..h).step_by(3) {
            p.hline_t(y, 0, w, 1, 235);
        }

        // The sheet itself
        let sheet_h = pt(SHEET_H_PT);
        p.rect(Rect::new(0, 0, w, sheet_h), 255);
        p.hline_t(sheet_h, 0, w, 2, INK);

        let pad = pt(PAD_PT);

        // 1. Top Sheet Handle Bar
        let handle_w = pt(36.0);
        let handle_h = pt(3.0);
        p.rect(Rect::new((w - handle_w) / 2, pt(HANDLE_TOP_PT), handle_w, handle_h), CARD_BORDER);

        // 2. Large Human Clock & Full Date
        p.text_center(pt(CLOCK_BASE_PT), CLOCK_SIZE_PT, INK, &self.time);
        if !self.date.is_empty() {
            p.text_center(pt(DATE_BASE_PT), DATE_SIZE_PT, DIM, &self.date);
        }

        // 3. Status Cards Grid (2x2): POWER | NETWORK, SSH | MEMORY
        let card_w = (w - 2 * pad - pt(CARD_GAP_PT)) / 2;
        let card_h = pt(CARD_H_PT);
        let row1_y = pt(CARD_TOP_PT);
        let row2_y = row1_y + card_h + pt(CARD_GAP_PT);

        let (cap, plugged) = sysinfo::battery();
        let bat_val = format!("{}%", cap);
        let bat_sub = if plugged { "Charging" } else { "Battery" };
        let r_bat = Rect::new(pad, row1_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_bat, "POWER", &bat_val, bat_sub);
        draw_battery_icon(p, r_bat.x + r_bat.w - pt(18.0), r_bat.y + pt(6.0), cap as i32, plugged);

        let ip = sysinfo::wifi_ip();
        let (wifi_val, wifi_sub, is_online) = if let Some(ip_str) = ip {
            ("Online", ip_str, true)
        } else {
            ("Offline", "Tap: turn on".to_string(), false)
        };
        let r_net = Rect::new(pad + card_w + pt(CARD_GAP_PT), row1_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_net, "NETWORK", wifi_val, &wifi_sub);
        draw_wifi_bars(p, r_net.x + r_net.w - pt(16.0), r_net.y + pt(6.0), is_online);

        let ssh_running = ybdev::ssh::running();
        let (ssh_val, ssh_sub) = if ssh_running {
            ("Active", "Port 2222 · Tap to Stop")
        } else {
            ("Inactive", "Tap to Start :2222")
        };
        let r_ssh = Rect::new(pad, row2_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_ssh, "SSH REMOTE", ssh_val, ssh_sub);
        if ssh_running {
            p.rect_outline_t(r_ssh, 2, INK);
        }
        p.text(r_ssh.x + r_ssh.w - pt(16.0), r_ssh.y + pt(12.0), 7.5, INK, ">_");

        // The fourth status: device-level RAM headroom (takeover freed
        // ~200 MB of it — nice to watch it go to page cache instead).
        let mem_val = sysinfo::mem_available_kib()
            .map(|k| format!("{:.0}M", k as f64 / 1024.0))
            .unwrap_or_else(|| "—".to_string());
        let mem_sub = sysinfo::mem_total_kib()
            .map(|k| format!("usable of {:.0}M", k as f64 / 1024.0))
            .unwrap_or_else(|| "usable".to_string());
        let r_mem = Rect::new(pad + card_w + pt(CARD_GAP_PT), row2_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_mem, "MEMORY", &mem_val, &mem_sub);
        draw_mem_icon(p, r_mem.x + r_mem.w - pt(20.0), r_mem.y + pt(7.0));

        // 4. Bottom Action Pills: [ Sleep ] [ Full Refresh ] [ Close ]
        //    (before the frontlight section: none of these need the fl,
        //    and a frontlight-less device must not lose its Close button)
        let act_y = pt(ACTIONS_TOP_PT);
        let act_w = (w - 2 * pad - 2 * pt(CARD_GAP_PT)) / 3;
        let act_h = pt(ACTION_H_PT);

        let labels = ["Sleep Screen", "Full Refresh", "Close"];
        for (i, label) in labels.iter().enumerate() {
            let r = Rect::new(pad + i as i32 * (act_w + pt(CARD_GAP_PT)), act_y, act_w, act_h);
            if *label == "Close" {
                p.rect(r, PILL_ACTIVE_BG);
                draw_box_text(p, r, 8.0, 255, label);
            } else {
                p.rect(r, PILL_BG);
                p.rect_outline_t(r, 1, CARD_BORDER);
                draw_box_text(p, r, 8.0, INK, label);
            }
        }

        // 5. Frontlight: two bare sliders
        let Some(fl) = &self.fl else {
            p.text_center(
                pt((BRIGHT_ROW_PT + TONE_ROW_PT) / 2.0 + ROW_CY_OFF_PT),
                10.0,
                DIM,
                "Frontlight hardware not available",
            );
            return;
        };

        let max = fl.max().max(1);
        let bright_frac = (fl.get() as f32 / max as f32).clamp(0.0, 1.0);
        self.draw_line_slider(
            p,
            pt(BRIGHT_ROW_PT),
            false,
            bright_frac,
            (bright_frac * 100.0).round() as i32,
        );

        let tmax = fl.tone_max();
        if tmax > 0 {
            let tone_frac = (fl.tone_get() as f32 / tmax as f32).clamp(0.0, 1.0);
            self.draw_line_slider(
                p,
                pt(TONE_ROW_PT),
                true,
                tone_frac,
                (tone_frac * 100.0).round() as i32,
            );
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let w = self.w;
        let pad = pt(PAD_PT);
        let (x0, x1) = (self.track_x0, self.track_x1);
        let bright_cy = pt(BRIGHT_ROW_PT + ROW_CY_OFF_PT);
        let tone_cy = pt(TONE_ROW_PT + ROW_CY_OFF_PT);
        let sheet_h = pt(SHEET_H_PT);

        let Some(fl) = &mut self.fl else {
            return Action::Pop;
        };

        let max = fl.max().max(1);
        let tmax = fl.tone_max();

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);

                // 0. Status card taps: Wi-Fi toggle (NETWORK), ssh
                //    toggle (SSH). POWER and MEMORY are status-only.
                let card_w = (w - 2 * pad - pt(CARD_GAP_PT)) / 2;
                let card_h = pt(CARD_H_PT);
                let row2_y = pt(CARD_TOP_PT) + card_h + pt(CARD_GAP_PT);

                let r_net = Rect::new(pad + card_w + pt(CARD_GAP_PT), pt(CARD_TOP_PT), card_w, card_h);
                if r_net.contains(x, y) {
                    // Unknown state counts as off — an optimistic default
                    // here turns the tile into a no-op exactly when the
                    // network is already unreachable.
                    if crate::wifi::wifi_state() == Some(true) {
                        // Pure lipc — a raw `ifconfig wlan0 down` leaves
                        // the interface administratively down and
                        // `wifid enable 1` can never bring it back
                        // (found on device 2026-08-19).
                        let _ = std::process::Command::new("lipc-set-prop").args(&["-i", "com.lab126.wifid", "enable", "0"]).status();
                        let _ = std::process::Command::new("lipc-set-prop").args(&["-i", "com.lab126.cmd", "wirelessEnable", "0"]).status();
                    } else {
                        crate::wifi::turn_on_wifi();
                    }
                    return Action::Redraw;
                }

                let r_ssh = Rect::new(pad, row2_y, card_w, card_h);
                if r_ssh.contains(x, y) {
                    let on = ybdev::ssh::running();
                    if on {
                        ybdev::ssh::disable();
                    } else {
                        ybdev::ssh::enable();
                    }
                    return Action::Redraw;
                }

                // 1. Slider bands: tap sets by position
                if (y - bright_cy).abs() <= 14 && x >= x0 && x <= x1 {
                    let frac = ((x - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    fl.set((frac * max as f32).round() as i32);
                    return Action::Redraw;
                }
                if tmax > 0 && (y - tone_cy).abs() <= 14 && x >= x0 && x <= x1 {
                    let frac = ((x - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    fl.tone_set((frac * tmax as f32).round() as i32);
                    return Action::Redraw;
                }

                // 2. Bottom Action Buttons: [ Sleep ] [ Refresh ] [ Close ]
                let act_y = pt(ACTIONS_TOP_PT);
                let act_h = pt(ACTION_H_PT);
                let act_w = (w - 2 * pad - 2 * pt(CARD_GAP_PT)) / 3;
                if y >= act_y - 6 && y < act_y + act_h + 6 && x >= pad {
                    let idx = ((x - pad) / (act_w + pt(CARD_GAP_PT))) as usize;
                    match idx {
                        0 => {
                            return Action::Push(Box::new(yui::widgets::SleepScreen::new()));
                        }
                        1 => {
                            // Full Screen Refresh
                            return Action::RedrawFull;
                        }
                        _ => {
                            // Close
                            return Action::Pop;
                        }
                    }
                }

                // Dismiss if tapped below the sheet (on the dimmed screen)
                if y > sheet_h + pt(10.0) {
                    return Action::Pop;
                }

                Action::Keep
            }

            // Drag along a slider row adjusts smoothly
            Gesture::Swipe { y, ex, dir, .. } => {
                let (y, ex) = (y as i32, ex as i32);
                if (y - bright_cy).abs() <= 15 && ex >= x0 && ex <= x1 {
                    let frac = ((ex - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    fl.set((frac * max as f32).round() as i32);
                    return Action::Redraw;
                }
                if tmax > 0 && (y - tone_cy).abs() <= 15 && ex >= x0 && ex <= x1 {
                    let frac = ((ex - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    fl.tone_set((frac * tmax as f32).round() as i32);
                    return Action::Redraw;
                }
                if matches!(dir, SwipeDir::North | SwipeDir::South) {
                    return Action::Pop;
                }
                Action::Keep
            }

            Gesture::TwoFingerTap => Action::Pop,
            _ => Action::Keep,
        }
    }

    fn default_edges(&self) -> bool {
        false
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    /// Headless render: the sheet must put dark ink in every band on
    /// white, and the region below the sheet must be the dimmed
    /// snapshot, not blank white (scanlines prove the backdrop blit).
    #[test]
    fn curtain_draws_sheet_with_dimmed_backdrop() {
        let font = Font::load().unwrap();
        let mut buf = vec![255u8; 1248 * 1648];
        let mut c = CurtainScreen::new();
        {
            let mut p = yui::Painter::new(&mut buf, 1236, 1648, 1248, &font);
            c.draw(&mut p);
        }
        let ink = |name: &str, y0: usize, y1: usize, min: usize| {
            let n = buf[y0 * 1248..y1 * 1248].iter().filter(|&&b| b < 140).count();
            assert!(n >= min, "{name}: only {n} ink pixels in rows {y0}-{y1}");
        };
        ink("clock", 60, 230, 200);
        ink("date", 210, 280, 50);
        ink("cards", 260, 530, 100);
        ink("no-fl message", 600, 830, 50);
        ink("actions", 870, 1000, 60);
        // Below the sheet: scanline dim over the white snapshot — every
        // third row is 235, so plenty of pixels sit below 250 but none
        // need to be ink.
        let dim = buf[1100 * 1248..1600 * 1248]
            .iter()
            .filter(|&&b| b < 250)
            .count();
        assert!(dim > 1000, "backdrop not dimmed below the sheet: {dim}");
        let lit = buf[0..1000 * 1248].iter().filter(|&&b| b > 200).count();
        assert!(
            lit > 1000 * 1248 * 90 / 100,
            "sheet is not white: {lit}"
        );
    }
}
