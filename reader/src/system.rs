//! The System screen — device-level controls and administrative options:
//! guides, screensavers, trusted devices, E-Ink refresh interval,
//! boot mode, reboot, and exit.
//! Styled with the same zero-blink list row architecture as the Home screen.

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;
use ybdev::sysinfo;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;

// Persistent Header
const HDR_RULE_PT: f32 = 20.0;
const TRIVIA_BASE_PT: f32 = 34.0;
const TRIVIA_RULE_PT: f32 = 42.0;

// Section 1: Display & Guides
const SEC1_LABEL_PT: f32 = 54.0;
const SEC1_TOP_PT: f32 = 62.0;

// Section 2: Device & Lifecycle
const SEC2_LABEL_PT: f32 = 216.0;
const SEC2_TOP_PT: f32 = 224.0;

const ROW_H_PT: f32 = 36.0;
const ICON_BOX_PT: f32 = 14.0;
const ICON_GAP_PT: f32 = 10.0;

const FOOTER_BASE_PT: f32 = 370.0;

const INK: u8 = 0;
const DIM: u8 = 110;
const MUTED: u8 = 160;
const DIVIDER: u8 = 220;

pub struct SystemScreen {
    w: i32,
    h: i32,
    ss_count: usize,
    dev_count: usize,
}

impl SystemScreen {
    pub fn new() -> SystemScreen {
        SystemScreen {
            w: 1236,
            h: 1648,
            ss_count: 0,
            dev_count: 0,
        }
    }

    fn draw_persistent_header(p: &mut Painter, w: i32, pad: i32) {
        let t = crate::chrome::current_time_str();
        let (cap, plugged) = sysinfo::battery();
        let bat = if plugged {
            format!("+{}%", cap)
        } else {
            format!("{}%", cap)
        };
        let fg = 120;

        // Left: Time
        p.text(pad, pt(14.0), 7.0, fg, &t);

        // Center: Title
        p.text_center(pt(14.0), 7.5, INK, "System");

        // Right: Wi-Fi glyph + Battery + Build
        let xr = w - pad;
        let bat_w = p.text_width(7.0, &bat) as i32;
        p.text_right(xr, pt(14.0), 7.0, fg, &bat);
        crate::chrome::draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg);

        // Top divider rule
        p.hline_t(pt(HDR_RULE_PT), pad, w - pad, 1, 225);
    }

    fn draw_system_row(
        p: &mut Painter,
        pad: i32,
        w: i32,
        top: i32,
        icon_type: usize,
        title: &str,
        sub: &str,
    ) {
        let icon_y = top + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2;
        draw_system_icon(p, icon_type, pad, icon_y);

        let tx = pad + pt(ICON_BOX_PT) + pt(ICON_GAP_PT);
        let budget = (p.width_pt() - 2.0 * PAD_PT - ICON_BOX_PT - ICON_GAP_PT - 24.0).max(10.0);

        let title_trunc = p.truncate(9.5, title, budget);
        p.text(tx, top + pt(15.0), 9.5, INK, &title_trunc);

        let sub_trunc = p.truncate(7.0, sub, budget);
        p.text(tx, top + pt(27.0), 7.0, DIM, &sub_trunc);

        p.text_right(w - pad, top + pt(21.0), 12.0, MUTED, ">");
        p.hline_t(top + pt(ROW_H_PT), pad, w - pad, 1, DIVIDER);
    }
}

impl Default for SystemScreen {
    fn default() -> Self {
        SystemScreen::new()
    }
}

impl Screen for SystemScreen {
    fn on_enter(&mut self) -> Action {
        self.ss_count = crate::screensavers::scan().len();
        let devices_path = ybdev::devices::devices_path();
        self.dev_count = ybdev::devices::DeviceStore::load(&devices_path).devices.len();
        Action::Redraw
    }

    fn on_resume(&mut self) -> Action {
        self.ss_count = crate::screensavers::scan().len();
        let devices_path = ybdev::devices::devices_path();
        self.dev_count = ybdev::devices::DeviceStore::load(&devices_path).devices.len();
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);

        // 1. Persistent Ambient Header
        Self::draw_persistent_header(p, w, pad);

        // 2. System Status Trivia Sub-header
        let mut bits: Vec<String> = Vec::new();
        if let Some(g) = sysinfo::storage_free_gb() {
            bits.push(format!("{g:.1} GB free"));
        }
        if let Some(k) = sysinfo::mem_available_kib() {
            bits.push(format!("{:.0}M RAM free", k as f64 / 1024.0));
        }
        let info_str = if bits.is_empty() {
            "Kindle Paperwhite (PW5)".to_string()
        } else {
            bits.join("   ·   ")
        };
        p.text(pad, pt(TRIVIA_BASE_PT), 6.8, DIM, &info_str);
        p.text_right(
            w - pad,
            pt(TRIVIA_BASE_PT),
            6.8,
            MUTED,
            concat!("v", env!("YB_BUILD")),
        );
        p.hline_t(pt(TRIVIA_RULE_PT), pad, w - pad, 1, 235);

        // --- SECTION 1: DISPLAY & GUIDES ---
        p.text(pad, pt(SEC1_LABEL_PT), 6.8, MUTED, "DISPLAY & GUIDES");

        let top1 = pt(SEC1_TOP_PT);
        let row_h = pt(ROW_H_PT);

        // Row 0: How to Use
        Self::draw_system_row(
            p,
            pad,
            w,
            top1,
            0,
            "How to Use",
            "Gestures & Navigation Guide (Page turns, curtain, dictionary)",
        );

        // Row 1: Screensavers
        let ss_label = match self.ss_count {
            0 => "No custom screensavers yet".to_string(),
            1 => "1 image in lock screen rotation".to_string(),
            n => format!("{n} images in lock screen rotation"),
        };
        Self::draw_system_row(
            p,
            pad,
            w,
            top1 + row_h,
            1,
            "Screensavers",
            &ss_label,
        );

        // Row 2: Trusted Devices
        let dev_label = match self.dev_count {
            0 => "No paired computers or phones".to_string(),
            1 => "1 trusted device paired · Tap to manage or revoke".to_string(),
            n => format!("{n} trusted devices paired · Tap to manage or revoke"),
        };
        Self::draw_system_row(
            p,
            pad,
            w,
            top1 + 2 * row_h,
            2,
            "Trusted Devices",
            &dev_label,
        );

        // Row 3: E-Ink Full Refresh
        let refresh_cur = crate::positions::global_refresh_interval();
        let refresh_val = match refresh_cur {
            0 => "Off (never flash)".to_string(),
            1 => "Every 1 page".to_string(),
            n => format!("Every {n} pages"),
        };
        Self::draw_system_row(
            p,
            pad,
            w,
            top1 + 3 * row_h,
            3,
            "E-Ink Full Refresh",
            &format!("Current: {refresh_val} · Tap to cycle presets"),
        );

        // --- SECTION 2: DEVICE & LIFECYCLE ---
        p.text(pad, pt(SEC2_LABEL_PT), 6.8, MUTED, "DEVICE & LIFECYCLE");

        let top2 = pt(SEC2_TOP_PT);
        let upstart_installed = std::path::Path::new("/etc/upstart/yb-reader.conf").exists();
        let os_boot = std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists() && upstart_installed;

        // Row 4: Boot Mode
        let (bv, bs) = if os_boot {
            ("Boot Mode: yb OS (Direct Boot)", "Tap to switch to Stock Kindle mode")
        } else if !upstart_installed {
            ("Boot Mode: Stock Kindle", "Requires root upstart job")
        } else {
            ("Boot Mode: Stock Kindle", "Tap to switch to yb OS mode")
        };
        Self::draw_system_row(p, pad, w, top2, 4, bv, bs);

        // Row 5: Reboot
        let next = if os_boot { "yb OS" } else { "Stock Kindle" };
        Self::draw_system_row(
            p,
            pad,
            w,
            top2 + row_h,
            5,
            "Reboot Device",
            &format!("Restart Kindle hardware · Next boot: {next}"),
        );

        // Row 6: Exit
        let (ev, es) = if os_boot {
            ("Exit to Kindle", "Return to stock UI · Next reboot starts yb OS")
        } else {
            ("Quit yb-reader", "Return to the stock launcher")
        };
        Self::draw_system_row(p, pad, w, top2 + 2 * row_h, 6, ev, es);

        // 4. Subtle Minimalist Footer
        p.text_center(
            pt(FOOTER_BASE_PT),
            7.5,
            MUTED,
            "Swipe up bottom-right to exit",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w, self.h);

        // Universal Kindle back / exit gesture: swipe up in bottom-right corner
        if g.corner_back() || g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                let pad = pt(PAD_PT);
                let row_h = pt(ROW_H_PT);

                let top1 = pt(SEC1_TOP_PT);
                let top2 = pt(SEC2_TOP_PT);

                // Section 1 Hit Testing
                if x >= pad && x <= w - pad && y >= top1 && y < top1 + 4 * row_h {
                    let idx = ((y - top1) / row_h) as usize;
                    match idx {
                        // 0. Gesture Guide
                        0 => return Action::Push(Box::new(crate::guide::GuideScreen::new())),
                        // 1. Screensavers
                        1 => return Action::Push(Box::new(crate::screensavers::ScreensaversScreen::new())),
                        // 2. Trusted Devices
                        2 => {
                            let devices_path = ybdev::devices::devices_path();
                            let store = ybdev::devices::DeviceStore::load(&devices_path);
                            if store.devices.is_empty() {
                                return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                                    "No Paired Devices",
                                    "Pair your phone or PC by scanning the QR code in 'Receive over Wi-Fi' or connecting with yb-mirror.",
                                    "OK",
                                    None,
                                    |_| Action::Pop,
                                )));
                            } else {
                                let mut msg = String::new();
                                for (i, d) in store.devices.iter().enumerate() {
                                    if i > 0 {
                                        msg.push('\n');
                                    }
                                    let ip_str = d.last_ip.as_deref().unwrap_or("Never connected");
                                    msg.push_str(&format!("{}. {} ({})", i + 1, d.name, ip_str));
                                }
                                return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                                    &format!("Trusted Devices ({})", store.devices.len()),
                                    &format!("{}\n\nTap 'Revoke All' to forget all paired devices.", msg),
                                    "Revoke All",
                                    None,
                                    move |act| {
                                        if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                            match ybdev::devices::with_store_mut(|s| {
                                                s.devices.clear();
                                            }) {
                                                Ok(_) => plog("system: all trusted devices revoked"),
                                                Err(e) => plog(&format!("system: revoke all FAILED: {}", e)),
                                            }
                                        }
                                        Action::Pop
                                    },
                                )));
                            }
                        }
                        // 3. E-Ink Refresh interval cycle
                        3 => {
                            let cur = crate::positions::global_refresh_interval();
                            let next = match cur {
                                0 => 5,
                                5 => 10,
                                10 => 20,
                                _ => 0,
                            };
                            crate::positions::set_global_refresh_interval(next);
                            return Action::Redraw;
                        }
                        _ => {}
                    }
                }

                // Section 2 Hit Testing
                if x >= pad && x <= w - pad && y >= top2 && y < top2 + 3 * row_h {
                    let idx = ((y - top2) / row_h) as usize;
                    match idx {
                        // 4. Boot mode toggle
                        0 => {
                            let upstart_installed = std::path::Path::new("/etc/upstart/yb-reader.conf").exists();
                            if !upstart_installed {
                                return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                                    "Root Upstart Required",
                                    "Direct OS Boot requires /etc/upstart/yb-reader.conf to be installed on rootfs. Without it, the Kindle cannot start yb-reader automatically.",
                                    "OK",
                                    None,
                                    |_| Action::Pop,
                                )));
                            }
                            let flag = std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK");
                            if flag.exists() {
                                let _ = std::fs::remove_file(flag);
                            } else {
                                let _ = std::fs::File::create(flag);
                            }
                            return Action::Redraw;
                        }
                        // 5. Reboot
                        1 => {
                            let next = if std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists()
                                && std::path::Path::new("/etc/upstart/yb-reader.conf").exists()
                            {
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
                                        let spawned =
                                            std::process::Command::new("reboot").spawn().or_else(|_| {
                                                std::process::Command::new("/sbin/reboot").spawn()
                                             });
                                        if spawned.is_err() {
                                            plog("system: reboot command failed");
                                        }
                                    }
                                    Action::Pop
                                },
                            )));
                        }
                        // 6. Exit
                        2 => {
                            let (title, body, yes) = if crate::home::takeover() {
                                (
                                    "Exit to Kindle?",
                                    "The stock Kindle UI returns.\nReboot brings yb-reader back.",
                                    "Exit",
                                )
                            } else {
                                ("Quit yb-reader?", "Back to the stock launcher.", "Quit")
                            };
                            return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                                title,
                                body,
                                yes,
                                None,
                                move |act| {
                                    if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                        Action::Quit
                                    } else {
                                        Action::Pop
                                    }
                                },
                            )));
                        }
                        _ => {}
                    }
                }

                Action::Keep
            }
            Gesture::Swipe { dir: SwipeDir::North | SwipeDir::South | SwipeDir::East, .. } => {
                Action::Pop
            }
            Gesture::TwoFingerTap => Action::Pop,
            _ => Action::Keep,
        }
    }
}

/// Dedicated line-art icons in a 14pt box for System screen rows:
/// 0: Guide (Book / Navigation map)
/// 1: Screensavers (Picture frame)
/// 2: Trusted Devices (Paired computers / screens)
/// 3: E-Ink Refresh (Circular cycle arrows)
/// 4: Boot Mode (OS Toggle chip)
/// 5: Reboot (Circular restart arrow)
/// 6: Exit (Door / Exit arrow)
fn draw_system_icon(p: &mut Painter, icon: usize, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    match icon {
        // 0: Guide book
        0 => {
            let bw = s - pt(2.0);
            let bh = s - pt(3.0);
            let bx = x + pt(1.0);
            let by = y + pt(1.5);
            let mid = bx + bw / 2;
            p.rect_outline_t(Rect::new(bx, by, bw, bh), 1, INK);
            p.line_w(mid, by, mid, by + bh, 1, INK);
            p.hline_t(by + pt(3.0), bx + pt(2.0), mid - pt(2.0), 1, INK);
            p.hline_t(by + pt(6.0), bx + pt(2.0), mid - pt(2.0), 1, INK);
            p.hline_t(by + pt(3.0), mid + pt(2.0), bx + bw - pt(2.0), 1, INK);
            p.hline_t(by + pt(6.0), mid + pt(2.0), bx + bw - pt(2.0), 1, INK);
        }
        // 1: Screensavers (Picture frame + mountains)
        1 => {
            let r = Rect::new(x + pt(1.0), y + pt(1.5), s - pt(2.0), s - pt(3.0));
            p.rect_outline_t(r, 1, INK);
            // Sun circle
            p.rect(Rect::new(r.x + pt(3.0), r.y + pt(3.0), pt(2.5), pt(2.5)), INK);
            // Mountain peaks
            p.line_w(r.x + pt(2.0), r.y + r.h - pt(2.0), r.x + pt(5.0), r.y + pt(5.0), 1, INK);
            p.line_w(r.x + pt(5.0), r.y + pt(5.0), r.x + pt(8.0), r.y + r.h - pt(2.0), 1, INK);
            p.line_w(r.x + pt(7.0), r.y + r.h - pt(2.0), r.x + pt(9.5), r.y + pt(6.5), 1, INK);
            p.line_w(r.x + pt(9.5), r.y + pt(6.5), r.x + r.w - pt(2.0), r.y + r.h - pt(2.0), 1, INK);
        }
        // 2: Trusted Devices (Desktop / connected screen)
        2 => {
            let sw = s - pt(3.0);
            let sh = s - pt(6.0);
            let sx = x + pt(1.5);
            let sy = y + pt(1.0);
            p.rect_outline_t(Rect::new(sx, sy, sw, sh), 1, INK);
            // Stand
            p.line_w(sx + sw / 2, sy + sh, sx + sw / 2, sy + sh + pt(3.0), 1, INK);
            p.hline_t(sy + sh + pt(3.0), sx + pt(2.0), sx + sw - pt(2.0), 1, INK);
        }
        // 3: E-Ink Refresh (Circular cycle arrows)
        3 => {
            let r = Rect::new(x + pt(2.0), y + pt(2.0), s - pt(4.0), s - pt(4.0));
            p.rect_outline_t(r, 1, INK);
            // Arrowhead top-right
            p.line_w(r.x + r.w - pt(2.5), r.y - pt(1.5), r.x + r.w + pt(1.0), r.y + pt(1.0), 1, INK);
            // Arrowhead bottom-left
            p.line_w(r.x - pt(1.0), r.y + r.h - pt(1.0), r.x + pt(2.5), r.y + r.h + pt(1.5), 1, INK);
        }
        // 4: Boot Mode (OS Toggle / Chip)
        4 => {
            let r = Rect::new(x + pt(2.0), y + pt(2.5), s - pt(4.0), s - pt(5.0));
            p.rect_outline_t(r, 1, INK);
            // Inset switch indicator
            p.rect(Rect::new(r.x + pt(2.0), r.y + pt(2.0), pt(3.0), r.h - pt(4.0)), INK);
        }
        // 5: Reboot (Power circular icon)
        5 => {
            let cx = x + s / 2;
            let cy = y + s / 2;
            let rad = s / 2 - pt(2.0);
            p.rect_outline_t(Rect::new(cx - rad, cy - rad, rad * 2, rad * 2), 1, INK);
            p.line_w(cx, cy - rad - pt(1.0), cx, cy, 1, INK);
        }
        // 6: Exit (Door / Exit arrow)
        _ => {
            let dx = x + pt(2.0);
            let dy = y + pt(1.5);
            let dw = s - pt(5.0);
            let dh = s - pt(3.0);
            p.rect_outline_t(Rect::new(dx, dy, dw, dh), 1, INK);
            // Arrow pointing right out of frame
            p.line_w(dx + pt(2.0), dy + dh / 2, dx + dw + pt(3.0), dy + dh / 2, 1, INK);
            p.line_w(dx + dw + pt(1.0), dy + dh / 2 - pt(2.5), dx + dw + pt(3.0), dy + dh / 2, 1, INK);
            p.line_w(dx + dw + pt(1.0), dy + dh / 2 + pt(2.5), dx + dw + pt(3.0), dy + dh / 2, 1, INK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    fn save_preview_artifact(name: &str, canvas: &[u8]) {
        let artifact_dir = match std::env::var("YB_AI_PREVIEW_DIR")
            .or_else(|_| std::env::var("ARTIFACT_DIR"))
        {
            Ok(d) if !d.is_empty() => d,
            _ => return,
        };
        let path = std::path::Path::new(&artifact_dir).join(name);
        if let Ok(file) = std::fs::File::create(&path) {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut w) = enc.write_header() {
                let _ = w.write_image_data(canvas);
            }
        }
    }

    #[test]
    fn system_screen_renders_content() {
        let font = Font::load().unwrap();
        let mut buf = vec![255u8; 1248 * 1648];
        let mut s = SystemScreen::new();
        let mut canvas = vec![0u8; 1236 * 1648];
        {
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
        save_preview_artifact("system_preview.png", &canvas);

        let ink = |name: &str, y0: usize, y1: usize, min: usize| {
            let n = buf[y0 * 1248..y1 * 1248]
                .iter()
                .filter(|&&b| b < 140)
                .count();
            assert!(n >= min, "{name}: only {n} ink pixels in rows {y0}-{y1}");
        };
        let lit = buf.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1248 * 1648 * 88 / 100, "page is not white: {lit}");
        ink("header", 20, 100, 30);
        ink("sec1_rows", 200, 800, 150);
        ink("sec2_rows", 800, 1400, 150);
    }
}
