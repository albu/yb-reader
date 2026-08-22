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

use crate::split::{ReaderSettings, SplitConfig};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

// --- layout (pt) — Total screen height is 395.5 pt (1648 px) ---
const HANDLE_TOP_PT: f32 = 8.0;
const HANDLE_W_PT: f32 = 38.0;
const HANDLE_H_PT: f32 = 3.0;

const CLOCK_BASE_PT: f32 = 36.0;
const CLOCK_SIZE_PT: f32 = 25.0;
const DATE_BASE_PT: f32 = 49.0;
const DATE_SIZE_PT: f32 = 8.0;

const CARD_TOP_PT: f32 = 58.0;
const CARD_H_PT: f32 = 32.0;
const CARD_GAP_PT: f32 = 7.0;
const PAD_PT: f32 = 18.0;

const BRIGHT_ROW_PT: f32 = 138.0;
const TONE_ROW_PT: f32 = 164.0;
const ROW_CY_OFF_PT: f32 = 11.0;
const SLIDER_X_OFF_PT: f32 = 44.0;

/// Composite light presets as one segmented track: Day | Warm | Night,
/// the active segment filled. One (brightness, warmth) pair per tap.
const SEG_TOP_PT: f32 = 194.0;
const SEG_H_PT: f32 = 22.0;

const KNOB_R_PX: i32 = 8;

const SHEET_H_PT: f32 = 230.0;

/// The user's own frontlight point, remembered across sessions — only
/// written while the light sits OFF every preset (custom mode); preset
/// taps never touch it. Format: "b w" (slider space, 4 decimals).
const CUSTOM_FL_PATH: &str = "/var/local/yb-reader/frontlight";

fn load_custom() -> Option<(f32, f32)> {
    let s = std::fs::read_to_string(CUSTOM_FL_PATH).ok()?;
    let (b, w) = s.split_once(' ')?;
    Some((b.trim().parse().ok()?, w.trim().parse().ok()?))
}

fn save_custom(b: f32, w: f32) {
    let _ = std::fs::create_dir_all("/var/local/yb-reader");
    let _ = std::fs::write(CUSTOM_FL_PATH, format!("{b:.4} {w:.4}\n"));
}

/// Apply slider-space levels and remember them when the result is OFF
/// every preset (custom mode). Preset tiles apply exact preset values
/// and never save. The hardware read-back (not the requested fraction)
/// is what gets saved, so re-applying reproduces identical registers.
fn apply_and_remember(fl: &mut Frontlight, b: f32, w: f32) {
    fl.apply_levels(b, w);
    let (lb, lw) = fl.levels();
    if ybdev::frontlight::nearest_preset(lb, lw).is_none() {
        save_custom(lb, lw);
    }
}

// --- grays on white ---
const DIM: u8 = 110;
const INK: u8 = 0;
const TRACK: u8 = 215;
const CARD_BG: u8 = 248;
const CARD_BORDER: u8 = 205;
const PILL_ACTIVE_BG: u8 = 30;

/// Everything the ROTATE pill needs to change the orientation of the
/// book beneath — and nothing else. Rotation is the only setting the
/// curtain owns, and it owns it through the same `record_sub` writer as
/// the settings dialog, so the two entry points cannot disagree.
pub struct RotateCtx {
    pub book: String,
    pub page: usize,
    pub sub: usize,
    pub total: usize,
    pub settings: ReaderSettings,
}

pub struct CurtainScreen {
    fl: Option<Frontlight>,
    time: String,
    date: String,
    // The screen underneath, captured on first paint (the buffer still
    // holds it then) — the sheet floats over a dimmed copy.
    bg: Option<Vec<u8>>,
    // When opened from the reader: the ROTATE pill's book context.
    rotate: Option<RotateCtx>,
    // Cached layout geometry in visual px
    w: i32,
    h: i32,
    track_x0: i32,
    track_x1: i32,
}

impl CurtainScreen {
    pub fn new() -> CurtainScreen {
        Self::build(None)
    }

    /// Reader-opened curtain: adds the ROTATE pill (cycles the book's
    /// orientation, preset/crop untouched).
    pub fn new_with_rotation(ctx: RotateCtx) -> CurtainScreen {
        Self::build(Some(ctx))
    }

    fn build(rotate: Option<RotateCtx>) -> CurtainScreen {
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
            rotate,
            w: 1236,
            h: 1648,
            track_x0: 0,
            track_x1: 0,
        }
    }

    fn draw_card(p: &mut Painter, r: Rect, title: &str, value: &str, sub: &str) {
        p.rect(r, CARD_BG);
        p.rect_outline_t(r, 1, CARD_BORDER);
        p.text(r.x + pt(8.0), r.y + pt(8.5), 6.5, DIM, title);
        p.text(r.x + pt(8.0), r.y + pt(19.5), 9.5, INK, value);
        if !sub.is_empty() {
            p.text(r.x + pt(8.0), r.y + pt(27.5), 6.0, DIM, sub);
        }
    }

    /// One bare slider line: icon, percentage, thick smooth track with a knob.
    /// Tap or drag anywhere on the row's band.
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
        let icon_r = pt(3.2);
        let icon_cx = pad + icon_r + pt(1.0);
        let icon_cy = cy;
        if amber {
            // Warm Amber double ring / glowing circle
            p.circle_fill(icon_cx, icon_cy, icon_r, 60);
            p.circle_outline_t(icon_cx, icon_cy, icon_r + pt(1.5), 1, 140);
        } else {
            // Radiating Sun
            draw_sun_icon(p, icon_cx, icon_cy, icon_r, INK);
        }

        // Percentage label
        p.text(pad + pt(14.0), cy + pt(3.2), 8.5, INK, &format!("{pct}%"));

        // Track + fill + knob
        let (x0, x1) = (self.track_x0, self.track_x1);
        let track_h = 4;
        p.rect(Rect::new(x0, cy - track_h / 2, x1 - x0, track_h), TRACK);
        let fill = ((x1 - x0) as f32 * frac.clamp(0.0, 1.0)).round() as i32;
        if fill > 0 {
            p.rect(Rect::new(x0, cy - track_h / 2, fill, track_h), INK);
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

        // The live screen underneath, dimmed by scanlines
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
        // Soft drop shadow
        p.hline_t(sheet_h + 2, 0, w, 1, 180);
        p.hline_t(sheet_h + 3, 0, w, 1, 215);
        p.hline_t(sheet_h + 4, 0, w, 1, 240);

        let pad = pt(PAD_PT);

        // 1. Top Sheet Handle Bar (pill style)
        let handle_w = pt(HANDLE_W_PT);
        let handle_h = pt(HANDLE_H_PT);
        let handle_x = (w - handle_w) / 2;
        let handle_y = pt(HANDLE_TOP_PT);
        p.rect(Rect::new(handle_x, handle_y, handle_w, handle_h), CARD_BORDER);

        // 2. Human Clock & Full Date
        p.text_center(pt(CLOCK_BASE_PT), CLOCK_SIZE_PT, INK, &self.time);
        if !self.date.is_empty() {
            p.text_center(pt(DATE_BASE_PT), DATE_SIZE_PT, DIM, &self.date);
        }

        // 3. Status Cards Grid (2x2): POWER | NETWORK, SSH | ORIENTATION
        let card_w = (w - 2 * pad - pt(CARD_GAP_PT)) / 2;
        let card_h = pt(CARD_H_PT);
        let row1_y = pt(CARD_TOP_PT);
        let row2_y = row1_y + card_h + pt(CARD_GAP_PT);

        // Card 1 (Top-Left): Power
        let (cap, plugged) = sysinfo::battery();
        let bat_val = format!("{}%", cap);
        let bat_sub = if plugged { "Charging" } else { "Battery" };
        let r_bat = Rect::new(pad, row1_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_bat, "POWER", &bat_val, bat_sub);
        draw_battery_icon(p, r_bat.x + r_bat.w - pt(18.0), r_bat.y + pt(11.0), cap as i32, plugged);

        // Card 2 (Top-Right): Network (Wi-Fi toggle)
        let ip = sysinfo::wifi_ip();
        let (wifi_val, wifi_sub, is_online) = if let Some(ip_str) = ip {
            ("Online", ip_str, true)
        } else {
            ("Offline", "Tap: turn on".to_string(), false)
        };
        let r_net = Rect::new(pad + card_w + pt(CARD_GAP_PT), row1_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_net, "NETWORK", wifi_val, &wifi_sub);
        draw_wifi_bars(p, r_net.x + r_net.w - pt(16.0), r_net.y + pt(11.0), is_online);

        // Card 3 (Bottom-Left): SSH Remote toggle
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
        p.text(r_ssh.x + r_ssh.w - pt(16.0), r_ssh.y + pt(19.5), 8.0, INK, ">_");

        // Card 4 (Bottom-Right): Orientation / Rotation
        let (rot_val, rot_sub, rot_icon_color) = if let Some(ctx) = &self.rotate {
            let val = match ctx.settings.split.rotation {
                0 => "Portrait",
                270 => "Landscape",
                90 => "Landscape R",
                _ => "Inverted",
            };
            (val, "Tap to rotate", INK)
        } else {
            ("Portrait", "Active in book", 150)
        };
        let r_rot = Rect::new(pad + card_w + pt(CARD_GAP_PT), row2_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_rot, "ORIENTATION", rot_val, rot_sub);
        draw_rotate_icon(p, r_rot.x + r_rot.w - pt(15.0), r_rot.y + pt(16.0), rot_icon_color);

        // 4. Frontlight: two bare sliders
        let Some(fl) = &self.fl else {
            p.text_center(
                pt((BRIGHT_ROW_PT + TONE_ROW_PT) / 2.0 + ROW_CY_OFF_PT),
                10.0,
                DIM,
                "Frontlight hardware not available",
            );
            return;
        };

        let (bright_frac, tone_frac) = fl.levels();
        self.draw_line_slider(
            p,
            pt(BRIGHT_ROW_PT),
            false,
            bright_frac,
            (bright_frac * 100.0).round() as i32,
        );

        let tmax = fl.tone_max();
        if tmax > 0 {
            self.draw_line_slider(
                p,
                pt(TONE_ROW_PT),
                true,
                tone_frac,
                (tone_frac * 100.0).round() as i32,
            );
        }

        // 4b. Composite light points: the four presets plus the user's
        // Custom slot (remembers the last off-preset light). The active
        // segment is whichever the light currently matches; an
        // off-preset light highlights Custom.
        let (lb, lw) = fl.levels();
        let active = ybdev::frontlight::nearest_preset(lb, lw);
        let n_seg = ybdev::frontlight::PRESETS.len() + 1;
        let seg_w = w - 2 * pad;
        let sw = seg_w / n_seg as i32;
        let track = Rect::new(pad, pt(SEG_TOP_PT), seg_w, pt(SEG_H_PT));
        p.rect(track, CARD_BG);
        p.rect_outline_t(track, 1, CARD_BORDER);

        let mut prev_on = false;
        for i in 0..n_seg {
            let name = if i < ybdev::frontlight::PRESETS.len() {
                ybdev::frontlight::PRESETS[i].0
            } else {
                "Custom"
            };
            let on = if i < ybdev::frontlight::PRESETS.len() {
                active == Some(i)
            } else {
                active.is_none()
            };
            let r = Rect::new(pad + i as i32 * sw, track.y, sw, track.h);
            if on {
                p.rect(r, PILL_ACTIVE_BG);
                p.text_center_in(r.x, r.x + r.w, r.y + pt(14.5), 7.5, 255, name);
            } else {
                p.text_center_in(r.x, r.x + r.w, r.y + pt(14.5), 7.5, INK, name);
            }
            if i > 0 {
                let beside_fill = on || prev_on;
                p.line_w(r.x, r.y + 2, r.x, r.y + r.h - 2, 1, if beside_fill { 255 } else { CARD_BORDER });
            }
            prev_on = on;
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

        let tmax = fl.tone_max();

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);

                // 0. Status card taps: Wi-Fi toggle (NETWORK), ssh
                //    toggle (SSH), Rotation cycle (ORIENTATION in book).
                let card_w = (w - 2 * pad - pt(CARD_GAP_PT)) / 2;
                let card_h = pt(CARD_H_PT);
                let row2_y = pt(CARD_TOP_PT) + card_h + pt(CARD_GAP_PT);

                let r_net = Rect::new(pad + card_w + pt(CARD_GAP_PT), pt(CARD_TOP_PT), card_w, card_h);
                if r_net.contains(x, y) {
                    if crate::wifi::wifi_state() == Some(true) {
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

                let r_rot = Rect::new(pad + card_w + pt(CARD_GAP_PT), row2_y, card_w, card_h);
                if r_rot.contains(x, y) && self.rotate.is_some() {
                    let mut ctx = self.rotate.take().expect("rotate ctx");
                    ctx.settings.split.rotation =
                        SplitConfig::next_rotation(ctx.settings.split.rotation);
                    crate::dialogs::record_sub(
                        &ctx.book, ctx.page, ctx.sub, ctx.total, ctx.settings,
                    );
                    return Action::Pop;
                }

                // 0b. Light points: presets apply their fixed pair; the
                // Custom slot applies the remembered off-preset light,
                // claiming the current light on its first ever tap.
                if y >= pt(SEG_TOP_PT) && y < pt(SEG_TOP_PT) + pt(SEG_H_PT) {
                    if let Some(i) = seg_at(x, w) {
                        if i < ybdev::frontlight::PRESETS.len() {
                            let (_, pb, pw) = ybdev::frontlight::PRESETS[i];
                            fl.apply_levels(pb, pw);
                        } else if let Some((cb, cw)) = load_custom() {
                            fl.apply_levels(cb, cw);
                        } else {
                            let (cb, cw) = fl.levels();
                            save_custom(cb, cw);
                        }
                        return Action::Redraw;
                    }
                }

                // 1. Slider bands: tap sets by position.
                if (y - bright_cy).abs() <= 16 && x >= x0 && x <= x1 {
                    let frac = ((x - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    let (_, w) = fl.levels();
                    apply_and_remember(fl, frac, w);
                    return Action::Redraw;
                }
                if tmax > 0 && (y - tone_cy).abs() <= 16 && x >= x0 && x <= x1 {
                    let frac = ((x - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    let (b, _) = fl.levels();
                    apply_and_remember(fl, b, frac);
                    return Action::Redraw;
                }

                // Dismiss if tapped below the sheet (on the dimmed screen)
                if y > sheet_h + pt(8.0) {
                    return Action::Pop;
                }

                Action::Keep
            }

            // Drag along a slider row adjusts smoothly
            Gesture::Swipe { y, ex, dir, .. } => {
                let (y, ex) = (y as i32, ex as i32);
                if (y - bright_cy).abs() <= 16 && ex >= x0 && ex <= x1 {
                    let frac = ((ex - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    let (_, w) = fl.levels();
                    apply_and_remember(fl, frac, w);
                    return Action::Redraw;
                }
                if tmax > 0 && (y - tone_cy).abs() <= 16 && ex >= x0 && ex <= x1 {
                    let frac = ((ex - x0) as f32 / (x1 - x0) as f32).clamp(0.0, 1.0);
                    let (b, _) = fl.levels();
                    apply_and_remember(fl, b, frac);
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

/// Segment under an x inside the light-point track (y is checked by the
/// caller). Index PRESETS.len() is the Custom slot.
fn seg_at(x: i32, w: i32) -> Option<usize> {
    let pad = pt(PAD_PT);
    let n = ybdev::frontlight::PRESETS.len() + 1;
    if x < pad || x >= w - pad || n == 0 {
        return None;
    }
    let sw = (w - 2 * pad) / n as i32;
    Some((((x - pad) / sw) as usize).min(n - 1))
}

/// Circular rotation arrow ↻.
fn draw_rotate_icon(p: &mut Painter, cx: i32, cy: i32, color: u8) {
    let r = pt(4.5) as f32;
    let mut prev: Option<(i32, i32)> = None;
    for k in 0..=20 {
        let a = (k as f32 / 20.0) * (std::f32::consts::PI * 1.5) - std::f32::consts::FRAC_PI_2;
        let (s, c) = a.sin_cos();
        let (x, y) = (cx + (r * c).round() as i32, cy + (r * s).round() as i32);
        if let Some((px, py)) = prev {
            p.line_w(px, py, x, y, 2, color);
        }
        prev = Some((x, y));
    }
    // Arrowhead at top opening
    let (ax, ay) = (cx + (r * 0.0) as i32, cy - r as i32);
    p.line_w(ax, ay, ax + pt(3.0), ay - pt(3.0), 2, color);
    p.line_w(ax, ay, ax + pt(3.0), ay + pt(3.0), 2, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    #[test]
    fn preset_segments_hit_cleanly() {
        let w = 1236;
        let n = ybdev::frontlight::PRESETS.len() + 1;
        assert_eq!(n, 5);
        let pad = pt(PAD_PT);
        let sw = (w - 2 * pad) / n as i32;
        for i in 0..n {
            assert_eq!(seg_at(pad + i as i32 * sw + 5, w), Some(i));
        }
        assert_eq!(seg_at(pad - 1, w), None);
        assert_eq!(seg_at(w - pad, w), None);

        let tone_band_bottom = pt(TONE_ROW_PT + ROW_CY_OFF_PT) + 16;
        assert!(pt(SEG_TOP_PT) > tone_band_bottom);
        assert!(pt(SEG_TOP_PT) + pt(SEG_H_PT) < pt(SHEET_H_PT));
    }

    #[test]
    fn curtain_draws_sheet_with_dimmed_backdrop() {
        let font = Font::load().unwrap();
        let mut buf = vec![255u8; 1248 * 1648];
        let mut c = CurtainScreen::new();
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
            c.draw(&mut p);
            p.flush();
        }
        let ink = |name: &str, y0: usize, y1: usize, min: usize| {
            let n = buf[y0 * 1248..y1 * 1248].iter().filter(|&&b| b < 140).count();
            assert!(n >= min, "{name}: only {n} ink pixels in rows {y0}-{y1}");
        };
        ink("clock", 60, 200, 200);
        ink("date", 180, 240, 50);
        ink("cards", 240, 550, 100);
        ink("no-fl message", 550, 750, 50);

        let dim = buf[1000 * 1248..1500 * 1248]
            .iter()
            .filter(|&&b| b < 250)
            .count();
        assert!(dim > 1000, "backdrop not dimmed below the sheet: {dim}");
        let lit = buf[0..800 * 1248].iter().filter(|&&b| b > 200).count();
        assert!(
            lit > 800 * 1248 * 90 / 100,
            "sheet is not white: {lit}"
        );
    }
}
