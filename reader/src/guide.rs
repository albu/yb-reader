//! Gesture & Onboarding Guide Screen.
//! Full-screen visual diagram showing how to use yb-reader's gestures
//! for reading (page turns, dictionary, refresh) and navigation
//! (curtain, quick settings, back/exit, TOC).

use ybdev::input::{Gesture, SwipeDir};
use ybdev::sysinfo;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;

// Persistent ambient header
const HDR_RULE_PT: f32 = 20.0;

// Tabs
const TAB_TOP_PT: f32 = 28.0;
const TAB_H_PT: f32 = 22.0;

// Main Diagram
const DIAGRAM_TOP_PT: f32 = 56.0;
const DIAGRAM_H_PT: f32 = 296.0;

// Footer
const FOOTER_BASE_PT: f32 = 370.0;

const INK: u8 = 0;
const DIM: u8 = 100;
const MUTED: u8 = 160;
const CARD_BG: u8 = 248;
const CARD_BORDER: u8 = 200;
const DIAGRAM_BG: u8 = 253;
const HIGHLIGHT_BG: u8 = 238;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuidePage {
    Reading = 0,
    System = 1,
}

pub struct GuideScreen {
    w: i32,
    h: i32,
    pub page: GuidePage,
}

impl GuideScreen {
    pub fn new() -> GuideScreen {
        GuideScreen {
            w: 1236,
            h: 1648,
            page: GuidePage::Reading,
        }
    }

    pub fn with_page(page: GuidePage) -> GuideScreen {
        GuideScreen {
            w: 1236,
            h: 1648,
            page,
        }
    }

    fn draw_persistent_header(p: &mut Painter, w: i32, pad: i32, page: GuidePage) {
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

        // Center: Standard screen title + page tracker
        let title = match page {
            GuidePage::Reading => "How to Use  ·  1/2 Reading",
            GuidePage::System => "How to Use  ·  2/2 Navigation",
        };
        p.text_center(pt(14.0), 7.5, INK, title);

        // Right: Wi-Fi glyph + Battery
        let xr = w - pad;
        let bat_w = p.text_width(7.0, &bat) as i32;
        p.text_right(xr, pt(14.0), 7.0, fg, &bat);
        crate::chrome::draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg);

        // Persistent divider rule
        p.hline_t(pt(HDR_RULE_PT), pad, w - pad, 1, 225);
    }

    fn draw_reading_diagram(p: &mut Painter, r: Rect) {
        p.rect(r, DIAGRAM_BG);
        p.rect_outline_t(r, 2, INK);

        // Screen bezel padding inside mockup
        let pad = pt(6.0);
        let sr = Rect::new(r.x + pad, r.y + pad, r.w - 2 * pad, r.h - 2 * pad);
        p.rect(sr, 255);
        p.rect_outline_t(sr, 1, CARD_BORDER);

        // 1. Top header strip in mockup (TOC / Bookmark)
        let hdr_h = pt(22.0);
        p.rect(Rect::new(sr.x, sr.y, sr.w, hdr_h), CARD_BG);
        p.hline_t(sr.y + hdr_h, sr.x, sr.x + sr.w, 1, CARD_BORDER);
        p.text(sr.x + pt(8.0), sr.y + pt(14.5), 6.5, INK, "TOC (Tap top-left)");
        p.text_right(sr.x + sr.w - pt(8.0), sr.y + pt(14.5), 6.5, INK, "Bookmark (Tap top-right)");

        // 2. Reading area split: Left 1/3 (Previous) vs Right 2/3 (Next)
        let split_x = sr.x + sr.w / 3;
        let content_y = sr.y + hdr_h;
        let ftr_h = pt(26.0);
        let content_h = sr.h - hdr_h - ftr_h;

        // Left zone: Previous Page
        p.rect(Rect::new(sr.x, content_y, split_x - sr.x, content_h), HIGHLIGHT_BG);
        p.rect(Rect::new(split_x, content_y, 2, content_h), CARD_BORDER);

        p.text(sr.x + pt(10.0), content_y + pt(30.0), 9.0, INK, "< PREV PAGE");
        p.text(sr.x + pt(10.0), content_y + pt(48.0), 7.0, DIM, "• Tap left 1/3 zone");
        p.text(sr.x + pt(10.0), content_y + pt(64.0), 7.0, DIM, "• Or Swipe Right ->");

        // Right zone: Next Page
        p.text(split_x + pt(16.0), content_y + pt(30.0), 9.0, INK, "NEXT PAGE >");
        p.text(split_x + pt(16.0), content_y + pt(48.0), 7.0, DIM, "• Tap anywhere in right 2/3");
        p.text(split_x + pt(16.0), content_y + pt(64.0), 7.0, DIM, "• Or Swipe Left <-");

        // 3. Center Callout Box: Long-Press for Dictionary
        let callout_w = pt(185.0);
        let callout_h = pt(50.0);
        let callout_x = sr.x + (sr.w - callout_w) / 2 + pt(15.0);
        let callout_y = content_y + pt(98.0);
        let callout_r = Rect::new(callout_x, callout_y, callout_w, callout_h);

        // Drop shadow for callout
        p.rect(Rect::new(callout_x + 2, callout_y + 2, callout_w, callout_h), 220);
        p.rect(callout_r, 255);
        p.rect_outline_t(callout_r, 2, INK);

        p.circle_fill(callout_x + pt(14.0), callout_y + pt(18.0), pt(4.5), INK);
        p.text(callout_x + pt(24.0), callout_y + pt(17.5), 8.5, INK, "LONG-PRESS ANY WORD");
        p.text(callout_x + pt(10.0), callout_y + pt(32.5), 7.0, DIM, "Instant Dictionary definition & translation");
        p.text(callout_x + pt(10.0), callout_y + pt(43.5), 6.5, DIM, "Long-press link references for footnotes");

        // 4. Footer in mockup: Screen Refresh
        let ftr_y = sr.y + sr.h - ftr_h;
        p.rect(Rect::new(sr.x, ftr_y, sr.w, ftr_h), CARD_BG);
        p.hline_t(ftr_y, sr.x, sr.x + sr.w, 1, CARD_BORDER);
        p.text_center_in(
            sr.x,
            sr.x + sr.w,
            ftr_y + pt(16.5),
            7.2,
            INK,
            "TWO-FINGER TAP ANYWHERE  ->  Full Refresh (Clears Ghosting)",
        );
    }

    fn draw_system_diagram(p: &mut Painter, r: Rect) {
        p.rect(r, DIAGRAM_BG);
        p.rect_outline_t(r, 2, INK);

        // Screen bezel padding inside mockup
        let pad = pt(6.0);
        let sr = Rect::new(r.x + pad, r.y + pad, r.w - 2 * pad, r.h - 2 * pad);
        p.rect(sr, 255);
        p.rect_outline_t(sr, 1, CARD_BORDER);

        // 1. Top swipe down zone (Curtain)
        let top_zone_h = pt(48.0);
        p.rect(Rect::new(sr.x, sr.y, sr.w, top_zone_h), HIGHLIGHT_BG);
        p.hline_t(sr.y + top_zone_h, sr.x, sr.x + sr.w, 2, CARD_BORDER);

        p.text_center_in(sr.x, sr.x + sr.w, sr.y + pt(19.0), 9.0, INK, "[v]  SWIPE DOWN FROM TOP EDGE  [v]");
        p.text_center_in(
            sr.x,
            sr.x + sr.w,
            sr.y + pt(35.0),
            6.8,
            DIM,
            "System Curtain: Brightness · Wi-Fi · SSH · Battery · Rotation",
        );

        // 2. Middle area: Library / Reading content placeholder
        p.text_center_in(
            sr.x,
            sr.x + sr.w,
            sr.y + pt(110.0),
            8.5,
            MUTED,
            "Reading Screen  /  Library",
        );

        // 3. TOC Bottom Bar
        let toc_bar_h = pt(26.0);
        let toc_bar_y = sr.y + sr.h - pt(58.0) - toc_bar_h;
        p.rect(Rect::new(sr.x, toc_bar_y, sr.w, toc_bar_h), CARD_BG);
        p.hline_t(toc_bar_y, sr.x, sr.x + sr.w, 1, CARD_BORDER);
        p.text_center_in(
            sr.x,
            sr.x + sr.w,
            toc_bar_y + pt(16.5),
            7.5,
            INK,
            "TAP BOTTOM BAR (OR TOP-LEFT)  ->  Table of Contents (TOC)",
        );

        // 4. Bottom corners: Quick Settings (Left) vs Exit / Back (Right)
        let corner_w = (sr.w - pt(4.0)) / 2;
        let corner_h = pt(54.0);
        let corner_y = sr.y + sr.h - corner_h;

        // Bottom-Left: Quick Settings
        let bl_r = Rect::new(sr.x, corner_y, corner_w, corner_h);
        p.rect(bl_r, HIGHLIGHT_BG);
        p.rect_outline_t(bl_r, 1, CARD_BORDER);
        p.text(bl_r.x + pt(10.0), bl_r.y + pt(17.0), 7.5, DIM, "[^] SWIPE UP (LEFT)");
        p.text(bl_r.x + pt(10.0), bl_r.y + pt(30.0), 8.5, INK, "Quick Settings");
        p.text(bl_r.x + pt(10.0), bl_r.y + pt(43.0), 6.5, DIM, "Font, margins & contrast");

        // Bottom-Right: Exit / Back
        let br_r = Rect::new(sr.x + sr.w - corner_w, corner_y, corner_w, corner_h);
        p.rect(br_r, HIGHLIGHT_BG);
        p.rect_outline_t(br_r, 1, CARD_BORDER);
        p.text(br_r.x + pt(10.0), br_r.y + pt(17.0), 7.5, DIM, "[^] SWIPE UP (RIGHT)");
        p.text(br_r.x + pt(10.0), br_r.y + pt(30.0), 8.5, INK, "Exit / Back");
        p.text(br_r.x + pt(10.0), br_r.y + pt(43.0), 6.5, DIM, "Close book or dialog");
    }
}

impl Default for GuideScreen {
    fn default() -> Self {
        GuideScreen::new()
    }
}

impl Screen for GuideScreen {
    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);

        // 1. Persistent Ambient Header (Time · Title · Wi-Fi · Battery)
        Self::draw_persistent_header(p, w, pad, self.page);

        // 2. Tab Bar: [ 1. Reading Gestures ] [ 2. Navigation & System ]
        let tab_y = pt(TAB_TOP_PT);
        let tab_h = pt(TAB_H_PT);
        let tab_w = (w - 2 * pad - pt(8.0)) / 2;

        let r_tab1 = Rect::new(pad, tab_y, tab_w, tab_h);
        let r_tab2 = Rect::new(pad + tab_w + pt(8.0), tab_y, tab_w, tab_h);

        // Tab 1
        if self.page == GuidePage::Reading {
            p.rect(r_tab1, INK);
            p.text_center_in(r_tab1.x, r_tab1.x + r_tab1.w, r_tab1.y + pt(15.0), 8.5, 255, "1. Reading Gestures");
        } else {
            p.rect(r_tab1, CARD_BG);
            p.rect_outline_t(r_tab1, 1, CARD_BORDER);
            p.text_center_in(r_tab1.x, r_tab1.x + r_tab1.w, r_tab1.y + pt(15.0), 8.5, DIM, "1. Reading Gestures");
        }

        // Tab 2
        if self.page == GuidePage::System {
            p.rect(r_tab2, INK);
            p.text_center_in(r_tab2.x, r_tab2.x + r_tab2.w, r_tab2.y + pt(15.0), 8.5, 255, "2. Navigation & System");
        } else {
            p.rect(r_tab2, CARD_BG);
            p.rect_outline_t(r_tab2, 1, CARD_BORDER);
            p.text_center_in(r_tab2.x, r_tab2.x + r_tab2.w, r_tab2.y + pt(15.0), 8.5, DIM, "2. Navigation & System");
        }

        // 3. Large Full-Diagram View
        let diag_r = Rect::new(pad, pt(DIAGRAM_TOP_PT), w - 2 * pad, pt(DIAGRAM_H_PT));
        match self.page {
            GuidePage::Reading => Self::draw_reading_diagram(p, diag_r),
            GuidePage::System => Self::draw_system_diagram(p, diag_r),
        }

        // 4. Clean Minimalist Footer
        p.text_center(
            pt(FOOTER_BASE_PT),
            7.5,
            MUTED,
            "Swipe Left / Right to switch tabs   ·   Swipe up bottom-right to exit",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w, self.h);
        let pad = pt(PAD_PT);

        // Universal Kindle back / exit gesture: swipe up in bottom-right corner
        if g.corner_back() || g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }

        match g {
            Gesture::Swipe { dir, .. } => match dir {
                SwipeDir::West | SwipeDir::East => {
                    self.page = match self.page {
                        GuidePage::Reading => GuidePage::System,
                        GuidePage::System => GuidePage::Reading,
                    };
                    Action::RedrawFull
                }
                _ => Action::Keep,
            },
            Gesture::Tap { x, y } => {
                let (vx, vy) = (x as i32, y as i32);

                // Tab bar taps
                let tab_y = pt(TAB_TOP_PT);
                let tab_h = pt(TAB_H_PT);
                let tab_w = (w - 2 * pad - pt(8.0)) / 2;
                let r_tab1 = Rect::new(pad, tab_y, tab_w, tab_h);
                let r_tab2 = Rect::new(pad + tab_w + pt(8.0), tab_y, tab_w, tab_h);

                if r_tab1.contains(vx, vy) && self.page != GuidePage::Reading {
                    self.page = GuidePage::Reading;
                    return Action::RedrawFull;
                }
                if r_tab2.contains(vx, vy) && self.page != GuidePage::System {
                    self.page = GuidePage::System;
                    return Action::RedrawFull;
                }

                Action::Keep
            }
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn renders_reading_and_system_guide_pages() {
        let font = yui::font::Font::load().unwrap();
        let mut s = GuideScreen::new();

        // Page 1: Reading
        s.page = GuidePage::Reading;
        let mut canvas = vec![255u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);
        save_preview_artifact("guide_reading_preview.png", &canvas);

        // Page 2: System
        s.page = GuidePage::System;
        let mut canvas2 = vec![255u8; 1236 * 1648];
        let mut panel2 = vec![255u8; 1248 * 1648];
        let mut p2 = yui::Painter::new(
            &mut panel2,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas2,
            &font,
        );
        s.draw(&mut p2);
        save_preview_artifact("guide_system_preview.png", &canvas2);
    }

    #[test]
    fn page_toggle_on_swipe_and_tab_tap() {
        let mut s = GuideScreen::new();
        assert_eq!(s.page, GuidePage::Reading);

        // Swipe West flips to System
        let act = s.on_gesture(Gesture::Swipe {
            dir: SwipeDir::West,
            x: 500,
            y: 500,
            ex: 200,
            ey: 500,
        });
        assert!(matches!(act, Action::RedrawFull));
        assert_eq!(s.page, GuidePage::System);

        // Swipe East flips back to Reading
        let act = s.on_gesture(Gesture::Swipe {
            dir: SwipeDir::East,
            x: 200,
            y: 500,
            ex: 500,
            ey: 500,
        });
        assert!(matches!(act, Action::RedrawFull));
        assert_eq!(s.page, GuidePage::Reading);
    }
}
