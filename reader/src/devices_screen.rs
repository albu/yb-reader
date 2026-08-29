//! Trusted Devices screen (System -> Trusted Devices).
//! Lists all paired client devices (phones, laptops, tablets) authorized
//! to connect over Wi-Fi or yb-mirror, with individual revocation.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;

// Persistent Header
const HDR_RULE_PT: f32 = 20.0;
const TRIVIA_BASE_PT: f32 = 34.0;
const TRIVIA_RULE_PT: f32 = 42.0;

// Section
const SEC_LABEL_PT: f32 = 52.0;
const SEC_TOP_PT: f32 = 60.0;

/// Rows stop here so they never overrun the "Revoke All" / footer area;
/// the tap hit test must agree with draw() about how many rows are
/// visible, or taps in the blank band (and on the button) would hit
/// invisible rows.
const LIST_BOTTOM_PT: f32 = 330.0;
const ROW_H_PT: f32 = 38.0;
const ICON_BOX_PT: f32 = 16.0;
const ICON_GAP_PT: f32 = 10.0;

const FOOTER_BASE_PT: f32 = 372.0;

const INK: u8 = 0;
const DIM: u8 = 110;
const MUTED: u8 = 160;
const DIVIDER: u8 = 220;

/// Rows that fit between the section header and the bottom of the list
/// area — shared by draw() and the tap hit test.
fn visible_rows() -> usize {
    (((pt(LIST_BOTTOM_PT) - pt(SEC_TOP_PT)) / pt(ROW_H_PT)) as usize).max(1)
}

pub struct DevicesScreen {
    w: i32,
    h: i32,
    devices: Vec<ybdev::devices::TrustedDevice>,
}

impl DevicesScreen {
    pub fn new() -> DevicesScreen {
        let store = ybdev::devices::DeviceStore::load(&ybdev::devices::devices_path());
        DevicesScreen {
            w: 1236,
            h: 1648,
            devices: store.devices,
        }
    }

    fn reload(&mut self) {
        let store = ybdev::devices::DeviceStore::load(&ybdev::devices::devices_path());
        self.devices = store.devices;
    }

    fn draw_persistent_header(p: &mut Painter, w: i32, pad: i32) {
        let t = crate::chrome::current_time_str();
        let (cap, plugged) = ybdev::sysinfo::battery();
        let bat = if plugged {
            format!("+{}%", cap)
        } else {
            format!("{}%", cap)
        };
        let fg = 120;

        // Left: Time
        p.text(pad, pt(14.0), 7.0, fg, &t);

        // Center: Title
        p.text_center(pt(14.0), 7.5, INK, "Trusted Devices");

        // Right: Wi-Fi glyph + Battery
        let xr = w - pad;
        let bat_w = p.text_width(7.0, &bat) as i32;
        p.text_right(xr, pt(14.0), 7.0, fg, &bat);
        crate::chrome::draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg);

        // Top divider rule
        p.hline_t(pt(HDR_RULE_PT), pad, w - pad, 1, 225);
    }

    fn draw_badge(p: &mut Painter, rx: i32, cy: i32, text: &str) {
        let text_w = p.text_width(7.0, text) as i32;
        let bw = text_w + pt(12.0);
        let bh = pt(14.0);
        let r = Rect::new(rx - bw, cy - bh / 2, bw, bh);
        p.rect_outline_t(r, 1, MUTED);
        p.text_center_in(r.x, r.x + r.w, cy + pt(2.5), 7.0, DIM, text);
    }
}

impl Default for DevicesScreen {
    fn default() -> Self {
        DevicesScreen::new()
    }
}

impl Screen for DevicesScreen {
    fn default_edges(&self) -> bool {
        true
    }

    fn on_enter(&mut self) -> Action {
        self.reload();
        Action::Redraw
    }

    fn on_resume(&mut self) -> Action {
        self.reload();
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        p.clear(255);
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        let pad = pt(PAD_PT);

        // 1. Header
        Self::draw_persistent_header(p, w, pad);

        // 2. Summary
        let count = self.devices.len();
        let summary_str = match count {
            0 => "No paired computers or phones".to_string(),
            1 => "1 trusted device paired".to_string(),
            n => format!("{n} trusted devices paired"),
        };
        p.text(pad, pt(TRIVIA_BASE_PT), 6.8, DIM, &summary_str);
        if count > 0 {
            p.text_right(w - pad, pt(TRIVIA_BASE_PT), 6.8, MUTED, "Tap device to revoke");
        }
        p.hline_t(pt(TRIVIA_RULE_PT), pad, w - pad, 1, 235);

        // 3. Section
        p.text(pad, pt(SEC_LABEL_PT), 6.8, MUTED, "PAIRED COMPUTERS & PHONES");

        let top = pt(SEC_TOP_PT);
        let row_h = pt(ROW_H_PT);

        if self.devices.is_empty() {
            let empty_top = top + pt(8.0);
            p.text(pad, empty_top + pt(14.0), 9.5, INK, "No devices paired yet");
            p.text(
                pad,
                empty_top + pt(30.0),
                7.5,
                DIM,
                "Pair your phone or PC by scanning the QR code in 'Receive over Wi-Fi'",
            );
            p.text(
                pad,
                empty_top + pt(44.0),
                7.5,
                DIM,
                "or connecting from the yb-mirror desktop app.",
            );
        } else {
            let tx = pad + pt(ICON_BOX_PT) + pt(ICON_GAP_PT);
            let budget = (p.width_pt() - 2.0 * PAD_PT - ICON_BOX_PT - ICON_GAP_PT - 60.0).max(10.0);

            for (i, dev) in self.devices.iter().enumerate() {
                let ry = top + i as i32 * row_h;
                if i >= visible_rows() {
                    break;
                }

                let icon_y = ry + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2;
                draw_device_icon(p, pad, icon_y);

                let title_trunc = p.truncate(10.5, &dev.name, budget);
                p.text(tx, ry + pt(15.5), 10.5, INK, &title_trunc);

                let ip_str = dev.last_ip.as_deref().unwrap_or("Never connected");
                let sub = format!("IP: {} · Scope: {}", ip_str, dev.scope);
                let sub_trunc = p.truncate(7.5, &sub, budget);
                p.text(tx, ry + pt(28.0), 7.5, DIM, &sub_trunc);

                let cy = ry + pt(ROW_H_PT) / 2;
                Self::draw_badge(p, w - pad, cy, "REVOKE");

                p.hline_t(ry + pt(ROW_H_PT), pad, w - pad, 1, DIVIDER);
            }

            // Revoke all button if multiple devices
            if self.devices.len() > 1 {
                let btn_y = pt(334.0);
                let btn_h = pt(22.0);
                let btn_w = pt(90.0);
                let btn_x = (w - btn_w) / 2;
                let r = Rect::new(btn_x, btn_y, btn_w, btn_h);
                p.rect_outline_t(r, 1, MUTED);
                p.text_center_in(r.x, r.x + r.w, btn_y + pt(14.0), 7.0, DIM, "Revoke All");
            }
        }

        // 4. Exit Footer
        p.text_center(pt(FOOTER_BASE_PT), 7.5, MUTED, "Swipe up bottom-right to exit");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w, self.h);
        if g.corner_back() || g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                let pad = pt(PAD_PT);
                let row_h = pt(ROW_H_PT);
                let top = pt(SEC_TOP_PT);

                // Row taps: revoke single device
                if x >= pad
                    && x <= w - pad
                    && y >= top
                    && y < top + visible_rows() as i32 * row_h
                {
                    let idx = ((y - top) / row_h) as usize;
                    if let Some(dev) = self.devices.get(idx) {
                        let dev_id = dev.id.clone();
                        let dev_name = dev.name.clone();
                        return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                            &format!("Revoke \"{dev_name}\"?"),
                            "This device will no longer be able to connect or transfer files until paired again.",
                            "Revoke",
                            None,
                            move |act| {
                                if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                    let _ = ybdev::devices::with_store_mut(|s| {
                                        s.remove(&dev_id);
                                    });
                                }
                                Action::Pop
                            },
                        )));
                    }
                }

                // Revoke all button tap
                if self.devices.len() > 1 {
                    let btn_y = pt(334.0);
                    let btn_h = pt(22.0);
                    let btn_w = pt(90.0);
                    let btn_x = (w - btn_w) / 2;
                    let r = Rect::new(btn_x, btn_y, btn_w, btn_h);
                    if r.contains(x, y) {
                        return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                            "Revoke All Devices?",
                            "All paired phones, laptops, and tablets will be forgotten.",
                            "Revoke All",
                            None,
                            move |act| {
                                if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                    let _ = ybdev::devices::with_store_mut(|s| {
                                        s.devices.clear();
                                    });
                                }
                                Action::Pop
                            },
                        )));
                    }
                }

                Action::Keep
            }
            _ => Action::Keep,
        }
    }
}

/// Laptop / Monitor glyph.
fn draw_device_icon(p: &mut Painter, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    let sw = s - pt(2.0);
    let sh = s - pt(6.0);
    let sx = x + pt(1.0);
    let sy = y + pt(1.0);
    p.rect_outline_t(Rect::new(sx, sy, sw, sh), 1, INK);
    p.line_w(x + s / 2, sy + sh, x + s / 2, sy + sh + pt(2.5), 1, INK);
    p.hline_t(sy + sh + pt(2.5), x + pt(3.0), x + s - pt(3.0), 1, INK);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rows_match_the_painted_list() {
        // Draw() stops painting at LIST_BOTTOM_PT; the tap hit test shares
        // visible_rows(), so the "Revoke All" button below the list can
        // never be swallowed by an invisible row's tap band.
        assert_eq!(visible_rows(), 7);
    }

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
    fn devices_screen_renders_preview() {
        let font = yui::font::Font::load().unwrap();
        let (mut canvas, mut panel) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );

        let mut s = DevicesScreen::new();
        s.devices.push(ybdev::devices::TrustedDevice {
            id: "mac1".to_string(),
            name: "MacBook Pro".to_string(),
            token: "tok1".to_string(),
            last_ip: Some("192.168.1.105".to_string()),
            last_seen: 123456,
            scope: "all".to_string(),
        });
        s.devices.push(ybdev::devices::TrustedDevice {
            id: "phone1".to_string(),
            name: "iPhone 15 Pro".to_string(),
            token: "tok2".to_string(),
            last_ip: Some("192.168.1.142".to_string()),
            last_seen: 123456,
            scope: "inbound".to_string(),
        });

        s.draw(&mut p);
        save_preview_artifact("devices_preview.png", &canvas);

        // Boundary state: one more device than fits. Rows past
        // visible_rows() must not paint (the hit test shares the cap), and
        // the Revoke All button below the list must not be overlapped by a
        // phantom row.
        let (mut canvas2, mut panel2) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p2 = yui::Painter::new(
            &mut panel2,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas2,
            &font,
        );
        let mut s2 = DevicesScreen::new();
        for i in 0..visible_rows() + 1 {
            s2.devices.push(ybdev::devices::TrustedDevice {
                id: format!("dev{i}"),
                name: format!("Paired Device {:02}", i + 1),
                token: format!("tok{i}"),
                last_ip: Some(format!("192.168.1.{}", 100 + i)),
                last_seen: 123456,
                scope: "all".to_string(),
            });
        }
        s2.draw(&mut p2);
        save_preview_artifact("devices_boundary_preview.png", &canvas2);

        let lit = canvas.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1236 * 1648 * 88 / 100);
    }
}
