//! The top curtain — a full-screen control-center sheet: big clock +
//! date, status rows (battery / wifi / ssh / storage), brightness +
//! tone sliders. Opened by the top-edge swipe / two-finger tap from
//! anywhere (the App edge overlay).

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

const BRIGHT_TITLE_PT: f32 = 138.0;
const BRIGHT_BAR_PT: f32 = 150.0;
const BRIGHT_PRESET_PT: f32 = 178.0;

const TONE_TITLE_PT: f32 = 204.0;
const TONE_BAR_PT: f32 = 216.0;
const TONE_PRESET_PT: f32 = 244.0;

const REFRESH_TITLE_PT: f32 = 270.0;
const REFRESH_PRESET_PT: f32 = 284.0;

const ACTIONS_TOP_PT: f32 = 314.0;
const ACTION_H_PT: f32 = 28.0;

const BAR_H_PT: f32 = 20.0;
const BTN_SZ_PT: f32 = 20.0;


// --- grays on white ---
const DIM: u8 = 110;
const INK: u8 = 0;
const TRACK: u8 = 230;
const CARD_BG: u8 = 246;
const CARD_BORDER: u8 = 200;
const PILL_BG: u8 = 242;
const PILL_ACTIVE_BG: u8 = 30;

pub struct CurtainScreen {
    fl: Option<Frontlight>,
    time: String,
    date: String,
    // Cached layout geometry in visual px
    w: i32,
    h: i32,
    bright_track_x0: i32,
    bright_track_x1: i32,
    bright_bar_y: i32,
    tone_track_x0: i32,
    tone_track_x1: i32,
    tone_bar_y: i32,
    bar_h: i32,
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
            w: 1236,
            h: 1648,
            bright_track_x0: 0,
            bright_track_x1: 0,
            bright_bar_y: 0,
            tone_track_x0: 0,
            tone_track_x1: 0,
            tone_bar_y: 0,
            bar_h: 0,
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

    fn draw_stepper_slider(
        &self,
        p: &mut Painter,
        title_y: i32,
        title: &str,
        is_amber: bool,
        bar_y: i32,
        track_x0: i32,
        track_x1: i32,
        frac: f32,
        percent: i32,
    ) {
        let pad = pt(PAD_PT);
        let btn_w = pt(BTN_SZ_PT);
        let bar_h = self.bar_h;

        // Title row with icon
        let icon_r = pt(3.0);
        let icon_cx = pad + icon_r + pt(1.0);
        let icon_cy = title_y - pt(3.0);
        if is_amber {
            // Warm Amber double ring
            p.circle_fill(icon_cx, icon_cy, icon_r, 60);
            p.circle_outline_t(icon_cx, icon_cy, icon_r + pt(1.5), 1, 140);
        } else {
            // Radiating Sun
            draw_sun_icon(p, icon_cx, icon_cy, icon_r, INK);
        }

        p.text(pad + pt(12.0), title_y, 8.0, INK, title);
        let percent_str = format!("{}%", percent);
        p.text_right(self.w - pad, title_y, 8.0, DIM, &percent_str);

        // Minus Button [-]
        let minus_rect = Rect::new(pad, bar_y, btn_w, bar_h);
        p.rect(minus_rect, PILL_BG);
        p.rect_outline_t(minus_rect, 1, CARD_BORDER);
        draw_box_text(p, minus_rect, 11.0, INK, "−");

        // Slider Track
        let track_rect = Rect::new(track_x0, bar_y, track_x1 - track_x0, bar_h);
        p.rect(track_rect, TRACK);
        p.rect_outline_t(track_rect, 1, CARD_BORDER);

        let fill_w = ((track_x1 - track_x0) as f32 * frac.clamp(0.0, 1.0)).round() as i32;
        if fill_w > 0 {
            p.rect(Rect::new(track_x0, bar_y, fill_w, bar_h), INK);
        }

        // Plus Button [+]
        let plus_rect = Rect::new(self.w - pad - btn_w, bar_y, btn_w, bar_h);
        p.rect(plus_rect, PILL_BG);
        p.rect_outline_t(plus_rect, 1, CARD_BORDER);
        draw_box_text(p, plus_rect, 11.0, INK, "+");
    }

    fn draw_pills(p: &mut Painter, y: i32, w: i32, presets: &[(&str, f32)], cur_frac: f32) {
        let pad = pt(PAD_PT);
        let gap = pt(5.0);
        let total_w = w - 2 * pad;
        let n = presets.len() as i32;
        let pill_w = (total_w - (n - 1) * gap) / n;
        let pill_h = pt(16.0);

        for (i, (label, target_frac)) in presets.iter().enumerate() {
            let px = pad + i as i32 * (pill_w + gap);
            let r = Rect::new(px, y, pill_w, pill_h);
            let is_active = (cur_frac - target_frac).abs() < 0.08;

            if is_active {
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
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);
        let btn_sz = pt(BTN_SZ_PT);
        let gap = pt(10.0);

        self.bar_h = pt(BAR_H_PT);
        self.bright_track_x0 = pad + btn_sz + gap;
        self.bright_track_x1 = w - pad - btn_sz - gap;
        self.bright_bar_y = pt(BRIGHT_BAR_PT);

        self.tone_track_x0 = pad + btn_sz + gap;
        self.tone_track_x1 = w - pad - btn_sz - gap;
        self.tone_bar_y = pt(TONE_BAR_PT);

        // 1. Top Sheet Handle Bar
        let handle_w = pt(36.0);
        let handle_h = pt(3.0);
        p.rect(Rect::new((w - handle_w) / 2, pt(HANDLE_TOP_PT), handle_w, handle_h), CARD_BORDER);

        // 2. Large Human Clock & Full Date
        p.text_center(pt(CLOCK_BASE_PT), CLOCK_SIZE_PT, INK, &self.time);
        if !self.date.is_empty() {
            p.text_center(pt(DATE_BASE_PT), DATE_SIZE_PT, DIM, &self.date);
        }

        // 3. Status Cards Grid (2x2)
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
            ("Offline", "Wi-Fi Disconnected".to_string(), false)
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

        let storage_free = sysinfo::storage_free_gb()
            .map(|gb| format!("{:.1} GB", gb))
            .unwrap_or_else(|| "—".to_string());
        let r_store = Rect::new(pad + card_w + pt(CARD_GAP_PT), row2_y, card_w, card_h);
        CurtainScreen::draw_card(p, r_store, "STORAGE", &storage_free, "Free Space");


        // 4. Frontlight Controls
        let Some(fl) = &self.fl else {
            p.text_center(pt(BRIGHT_TITLE_PT), 10.0, DIM, "Frontlight hardware not available");
            return;
        };

        let max = fl.max().max(1);
        let cur_bright = fl.get();
        let bright_frac = (cur_bright as f32 / max as f32).clamp(0.0, 1.0);
        let bright_pct = (bright_frac * 100.0).round() as i32;

        self.draw_stepper_slider(
            p,
            pt(BRIGHT_TITLE_PT),
            "Brightness",
            false,
            self.bright_bar_y,
            self.bright_track_x0,
            self.bright_track_x1,
            bright_frac,
            bright_pct,
        );

        let bright_presets = [("Off", 0.0), ("Night", 0.15), ("Read", 0.40), ("Bright", 0.70), ("Max", 1.0)];
        CurtainScreen::draw_pills(p, pt(BRIGHT_PRESET_PT), w, &bright_presets, bright_frac);

        // 5. Warm Tone Controls
        let tmax = fl.tone_max();
        if tmax > 0 {
            let cur_tone = fl.tone_get();
            let tone_frac = (cur_tone as f32 / tmax as f32).clamp(0.0, 1.0);
            let tone_pct = (tone_frac * 100.0).round() as i32;

            self.draw_stepper_slider(
                p,
                pt(TONE_TITLE_PT),
                "Warm Tone (Amber)",
                true,
                self.tone_bar_y,
                self.tone_track_x0,
                self.tone_track_x1,
                tone_frac,
                tone_pct,
            );

            let tone_presets = [("Cool", 0.0), ("Candle", 0.30), ("Cozy", 0.50), ("Amber", 0.70), ("Warm", 1.0)];
            CurtainScreen::draw_pills(p, pt(TONE_PRESET_PT), w, &tone_presets, tone_frac);
        }

        // 6. E-Ink Full Refresh Setting
        let cur_interval = crate::positions::global_refresh_interval();
        p.text(pad, pt(REFRESH_TITLE_PT), 8.0, INK, "E-Ink Full Refresh Interval");
        let refresh_presets = [
            ("Fast (Off)", 0.0),
            ("Every 5 pgs", 5.0),
            ("Every 10 pgs", 10.0),
            ("Every 20 pgs", 20.0),
        ];
        CurtainScreen::draw_pills(p, pt(REFRESH_PRESET_PT), w, &refresh_presets, cur_interval as f32);

        // 7. Bottom Action Pills: [ Sleep Screen ] [ Full Refresh ] [ Close ]
        let act_y = pt(ACTIONS_TOP_PT);
        let act_w = (w - 2 * pad - 2 * pt(CARD_GAP_PT)) / 3;
        let act_h = pt(ACTION_H_PT);

        // Sleep Button
        let sleep_rect = Rect::new(pad, act_y, act_w, act_h);
        p.rect(sleep_rect, PILL_BG);
        p.rect_outline_t(sleep_rect, 1, CARD_BORDER);
        draw_box_text(p, sleep_rect, 8.0, INK, "Sleep Screen");

        // Refresh Button
        let ref_rect = Rect::new(pad + act_w + pt(CARD_GAP_PT), act_y, act_w, act_h);
        p.rect(ref_rect, PILL_BG);
        p.rect_outline_t(ref_rect, 1, CARD_BORDER);
        draw_box_text(p, ref_rect, 8.0, INK, "Full Refresh");

        // Close Button
        let close_rect = Rect::new(pad + 2 * (act_w + pt(CARD_GAP_PT)), act_y, act_w, act_h);
        p.rect(close_rect, PILL_ACTIVE_BG);
        draw_box_text(p, close_rect, 8.0, 255, "Close");
    }



    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, _h) = (self.w, self.h);
        let pad = pt(PAD_PT);
        let bar_h = self.bar_h;
        let (bx0, bx1) = (self.bright_track_x0, self.bright_track_x1);
        let (tx0, tx1) = (self.tone_track_x0, self.tone_track_x1);
        let bright_y = self.bright_bar_y;
        let tone_y = self.tone_bar_y;

        let Some(fl) = &mut self.fl else {
            return Action::Pop;
        };

        let max = fl.max().max(1);
        let tmax = fl.tone_max();
        let bright_step = (max / 24).max(1);
        let tone_step = (tmax / 24).max(1);

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);

                // 0. Hardware Status Card Taps: SSH Remote Toggle & Network Wi-Fi Toggle
                let card_w = (w - 2 * pad - pt(CARD_GAP_PT)) / 2;
                let card_h = pt(CARD_H_PT);
                let row2_y = pt(CARD_TOP_PT) + card_h + pt(CARD_GAP_PT);

                let r_net = Rect::new(pad + card_w + pt(CARD_GAP_PT), pt(CARD_TOP_PT), card_w, card_h);
                if r_net.contains(x, y) {
                    if crate::wifi::is_wifi_on() {
                        let _ = std::process::Command::new("/sbin/ifconfig").args(&["wlan0", "down"]).output();
                        let _ = std::process::Command::new("lipc-set-prop").args(&["-i", "com.lab126.cmd", "wirelessEnable", "0"]).status();
                        let _ = std::process::Command::new("lipc-set-prop").args(&["-i", "com.lab126.wifid", "enable", "0"]).status();
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


                // 1. Brightness Bar Controls
                if y >= bright_y - 12 && y < bright_y + bar_h + 12 {

                    if x < bx0 {
                        // Minus Button [-]
                        let cur = fl.get();
                        fl.set((cur - bright_step).max(0));
                        return Action::Redraw;
                    } else if x > bx1 {
                        // Plus Button [+]
                        let cur = fl.get();
                        fl.set((cur + bright_step).min(max));
                        return Action::Redraw;
                    } else {
                        // Slider Track Tap
                        let frac = ((x - bx0) as f32 / (bx1 - bx0) as f32).clamp(0.0, 1.0);
                        fl.set((frac * max as f32).round() as i32);
                        return Action::Redraw;
                    }
                }

                // 2. Brightness Preset Pills
                let b_pill_y = pt(BRIGHT_PRESET_PT);
                let pill_h = pt(16.0);
                if y >= b_pill_y - 6 && y < b_pill_y + pill_h + 6 {
                    let presets = [("Off", 0.0), ("Night", 0.15), ("Read", 0.40), ("Bright", 0.70), ("Max", 1.0)];
                    let gap = pt(5.0);
                    let total_w = w - 2 * pad;
                    let n = presets.len() as i32;
                    let pill_w = (total_w - (n - 1) * gap) / n;
                    for (i, (_label, frac)) in presets.iter().enumerate() {
                        let px = pad + i as i32 * (pill_w + gap);
                        if x >= px && x < px + pill_w {
                            fl.set((frac * max as f32).round() as i32);
                            return Action::Redraw;
                        }
                    }
                }

                // 3. Tone Controls (if available)
                if tmax > 0 {
                    // Tone Bar Controls
                    if y >= tone_y - 12 && y < tone_y + bar_h + 12 {
                        if x < tx0 {
                            // Minus [-]
                            let cur = fl.tone_get();
                            fl.tone_set((cur - tone_step).max(0));
                            return Action::Redraw;
                        } else if x > tx1 {
                            // Plus [+]
                            let cur = fl.tone_get();
                            fl.tone_set((cur + tone_step).min(tmax));
                            return Action::Redraw;
                        } else {
                            // Slider Track Tap
                            let frac = ((x - tx0) as f32 / (tx1 - tx0) as f32).clamp(0.0, 1.0);
                            fl.tone_set((frac * tmax as f32).round() as i32);
                            return Action::Redraw;
                        }
                    }

                    // Tone Preset Pills
                    let t_pill_y = pt(TONE_PRESET_PT);
                    if y >= t_pill_y - 6 && y < t_pill_y + pill_h + 6 {
                        let presets = [("Cool", 0.0), ("Candle", 0.30), ("Cozy", 0.50), ("Amber", 0.70), ("Warm", 1.0)];
                        let gap = pt(5.0);
                        let total_w = w - 2 * pad;
                        let n = presets.len() as i32;
                        let pill_w = (total_w - (n - 1) * gap) / n;
                        for (i, (_label, frac)) in presets.iter().enumerate() {
                            let px = pad + i as i32 * (pill_w + gap);
                            if x >= px && x < px + pill_w {
                                fl.tone_set((frac * tmax as f32).round() as i32);
                                return Action::Redraw;
                            }
                        }
                    }
                }

                // 4. E-Ink Refresh Interval Preset Pills
                let ref_pill_y = pt(REFRESH_PRESET_PT);
                if y >= ref_pill_y - 6 && y < ref_pill_y + pill_h + 6 {
                    let intervals = [0usize, 5, 10, 20];
                    let gap = pt(5.0);
                    let total_w = w - 2 * pad;
                    let n = intervals.len() as i32;
                    let pill_w = (total_w - (n - 1) * gap) / n;
                    for (i, &int_val) in intervals.iter().enumerate() {
                        let px = pad + i as i32 * (pill_w + gap);
                        if x >= px && x < px + pill_w {
                            crate::positions::set_global_refresh_interval(int_val);
                            return Action::Redraw;
                        }
                    }
                }



                // 6. Bottom Action Buttons: [ Sleep ] [ Refresh ] [ Close ]
                let act_y = pt(ACTIONS_TOP_PT);
                let act_h = pt(ACTION_H_PT);
                let act_w = (w - 2 * pad - 2 * pt(CARD_GAP_PT)) / 3;
                if y >= act_y - 6 && y < act_y + act_h + 6 {
                    if x >= pad && x < pad + act_w {
                        // Sleep Screen
                        return Action::Push(Box::new(yui::widgets::SleepScreen::new()));
                    } else if x >= pad + act_w && x < pad + 2 * act_w + pt(CARD_GAP_PT) {
                        // Full Screen Refresh
                        return Action::RedrawFull;
                    } else if x >= pad + 2 * act_w {
                        // Close
                        return Action::Pop;
                    }
                }

                // Dismiss if tapped in blank bottom region
                if y > pt(ACTIONS_TOP_PT) + pt(ACTION_H_PT) + pt(15.0) {
                    return Action::Pop;
                }

                Action::Keep
            }

            // Swipe / drag on brightness or tone slider adjusts smoothly
            Gesture::Swipe { y, ex, dir, .. } => {
                let (y, ex) = (y as i32, ex as i32);
                if y >= bright_y - 15 && y < bright_y + bar_h + 15 && ex >= bx0 && ex <= bx1 {
                    let frac = ((ex - bx0) as f32 / (bx1 - bx0) as f32).clamp(0.0, 1.0);
                    fl.set((frac * max as f32).round() as i32);
                    return Action::Redraw;
                }
                if tmax > 0 && y >= tone_y - 15 && y < tone_y + bar_h + 15 && ex >= tx0 && ex <= tx1 {
                    let frac = ((ex - tx0) as f32 / (tx1 - tx0) as f32).clamp(0.0, 1.0);
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

    /// Headless render: the curtain must put dark ink in every band on
    /// the white sheet — written while chasing a (false-alarm) "full
    /// black screen" report, kept as a regression guard.
    #[test]
    fn curtain_draws_visible_content_on_white() {
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
        // White sheet: everything else must stay light.
        let lit = buf.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1248 * 1648 * 90 / 100, "sheet is not white: {lit}");
        ink("clock", 100, 220, 200);
        ink("date", 220, 280, 50);
        ink("cards", 280, 550, 100);
        ink("no-fl message", 550, 700, 50);
    }
}



