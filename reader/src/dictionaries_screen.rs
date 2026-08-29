//! Dictionaries screen — the settings view for user dictionaries.
//!
//! WordNet is the built-in definition row and is always on. User
//! dictionaries provide the translation row: tap a dictionary to activate
//! it (rebuilding happens automatically on activate if needed), and in a
//! book the word card's "next dictionary" cycles the translation through
//! the active list. Long-press any user dictionary to delete it.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;

// Persistent Header
const HDR_RULE_PT: f32 = 20.0;
const TRIVIA_BASE_PT: f32 = 34.0;
const TRIVIA_RULE_PT: f32 = 42.0;

// Section 1: Built-in Definition
const SEC1_LABEL_PT: f32 = 52.0;
const SEC1_TOP_PT: f32 = 60.0;

// Section 2: User Dictionaries
const SEC2_LABEL_PT: f32 = 108.0;
const SEC2_TOP_PT: f32 = 116.0;

/// Rows stop here so they never overrun the footer; the tap/long-press
/// hit tests must agree with draw() about how many rows are visible.
const LIST_BOTTOM_PT: f32 = 350.0;
const ROW_H_PT: f32 = 38.0;
const ICON_BOX_PT: f32 = 16.0;
const ICON_GAP_PT: f32 = 10.0;

const FOOTER_BASE_PT: f32 = 372.0;

const INK: u8 = 0;
const DIM: u8 = 110;
const MUTED: u8 = 160;
const DIVIDER: u8 = 220;

/// Rows that fit between the section header and the footer — draw() and
/// the gesture handlers share this so unpainted rows are never tappable.
fn visible_rows() -> usize {
    (((pt(LIST_BOTTOM_PT) - pt(SEC2_TOP_PT)) / pt(ROW_H_PT)) as usize).max(1)
}

pub struct DictionariesScreen {
    w: i32,
    h: i32,
    items: Vec<crate::dictionary::DictInfo>,
    selection: crate::dictionary::DictSelection,
}

impl DictionariesScreen {
    pub fn new() -> DictionariesScreen {
        DictionariesScreen {
            w: 1236,
            h: 1648,
            items: crate::dictionary::scan(),
            selection: crate::dictionary::load_selection(),
        }
    }

    fn is_active(&self, base: &str) -> bool {
        self.selection.active.iter().any(|b| b == base)
    }

    fn sub_line(&self, it: &crate::dictionary::DictInfo) -> String {
        let wc_str = if it.word_count > 0 {
            format!("{} words · ", it.word_count)
        } else {
            String::new()
        };
        if self.is_active(&it.base) {
            format!("{wc_str}Active translation")
        } else {
            format!("{wc_str}Tap to activate")
        }
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
        p.text_center(pt(14.0), 7.5, INK, "Dictionaries");

        // Right: Wi-Fi glyph + Battery
        let xr = w - pad;
        let bat_w = p.text_width(7.0, &bat) as i32;
        p.text_right(xr, pt(14.0), 7.0, fg, &bat);
        crate::chrome::draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg);

        // Top divider rule
        p.hline_t(pt(HDR_RULE_PT), pad, w - pad, 1, 225);
    }

    fn draw_badge(p: &mut Painter, rx: i32, cy: i32, text: &str, active: bool) {
        let text_w = p.text_width(7.0, text) as i32;
        let bw = text_w + pt(12.0);
        let bh = pt(14.0);
        let r = Rect::new(rx - bw, cy - bh / 2, bw, bh);
        if active {
            p.rect(r, INK);
            p.text_center_in(r.x, r.x + r.w, cy + pt(2.5), 7.0, 255, text);
        } else {
            p.rect_outline_t(r, 1, MUTED);
            p.text_center_in(r.x, r.x + r.w, cy + pt(2.5), 7.0, DIM, text);
        }
    }
}

impl Default for DictionariesScreen {
    fn default() -> Self {
        DictionariesScreen::new()
    }
}

impl Screen for DictionariesScreen {
    fn default_edges(&self) -> bool {
        true
    }

    fn on_enter(&mut self) -> Action {
        self.items = crate::dictionary::scan();
        self.selection = crate::dictionary::load_selection();
        Action::Redraw
    }

    fn on_resume(&mut self) -> Action {
        self.items = crate::dictionary::scan();
        self.selection = crate::dictionary::load_selection();
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        p.clear(255);
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        let pad = pt(PAD_PT);

        // 1. Persistent Ambient Header
        Self::draw_persistent_header(p, w, pad);

        // 2. Summary Sub-header
        let installed = self.items.len();
        let active = self.selection.active.len();
        let summary_str = if installed == 0 {
            "No user dictionaries installed".to_string()
        } else if active == 0 {
            format!("{installed} installed   ·   none active")
        } else {
            format!("{installed} installed   ·   {active} active")
        };
        p.text(pad, pt(TRIVIA_BASE_PT), 6.8, DIM, &summary_str);
        p.text_right(w - pad, pt(TRIVIA_BASE_PT), 6.8, MUTED, "Built-in: WordNet");
        p.hline_t(pt(TRIVIA_RULE_PT), pad, w - pad, 1, 235);

        // --- SECTION 1: BUILT-IN DEFINITION ---
        p.text(pad, pt(SEC1_LABEL_PT), 6.8, MUTED, "BUILT-IN DEFINITIONS");

        let top1 = pt(SEC1_TOP_PT);

        // WordNet Row
        let icon_y1 = top1 + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2;
        draw_book_icon(p, pad, icon_y1);

        let tx = pad + pt(ICON_BOX_PT) + pt(ICON_GAP_PT);
        let budget = (p.width_pt() - 2.0 * PAD_PT - ICON_BOX_PT - ICON_GAP_PT - 60.0).max(10.0);

        let wn_title = p.truncate(10.5, "WordNet (English)", budget);
        p.text(tx, top1 + pt(15.5), 10.5, INK, &wn_title);

        let wn_sub = if crate::dictionary::builtin_available() {
            "Built-in English definitions · Always active"
        } else {
            "Not installed · Deploy wordnet.ybdict"
        };
        let wn_sub_trunc = p.truncate(7.5, wn_sub, budget);
        p.text(tx, top1 + pt(28.0), 7.5, DIM, &wn_sub_trunc);

        Self::draw_badge(p, w - pad, top1 + pt(ROW_H_PT) / 2, "ALWAYS ON", true);
        p.hline_t(top1 + pt(ROW_H_PT), pad, w - pad, 1, DIVIDER);

        // --- SECTION 2: USER DICTIONARIES ---
        p.text(pad, pt(SEC2_LABEL_PT), 6.8, MUTED, "USER TRANSLATION DICTIONARIES");

        let top2 = pt(SEC2_TOP_PT);
        let row_h = pt(ROW_H_PT);

        if self.items.is_empty() {
            let empty_top = top2 + pt(8.0);
            p.text(pad, empty_top + pt(14.0), 9.5, INK, "No user dictionaries installed");
            p.text(
                pad,
                empty_top + pt(30.0),
                7.5,
                DIM,
                "Upload FreeDict (*.tei or *.ybdict) files via Wi-Fi (Receive page)",
            );
            p.text(
                pad,
                empty_top + pt(44.0),
                7.5,
                DIM,
                "or place them in /mnt/us/dictionaries/ over USB.",
            );
        } else {
            for (i, it) in self.items.iter().enumerate() {
                let ry = top2 + i as i32 * row_h;
                if i >= visible_rows() {
                    break; // prevent overrunning the footer area
                }

                let icon_y = ry + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2;
                draw_dict_icon(p, pad, icon_y);

                // Long dictionary name boundary protection (truncated)
                let name_trunc = p.truncate(10.5, &it.name, budget);
                p.text(tx, ry + pt(15.5), 10.5, INK, &name_trunc);

                let sub = self.sub_line(it);
                let sub_trunc = p.truncate(7.5, &sub, budget);
                p.text(tx, ry + pt(28.0), 7.5, DIM, &sub_trunc);

                let cy = ry + pt(ROW_H_PT) / 2;
                if self.is_active(&it.base) {
                    Self::draw_badge(p, w - pad, cy, "ACTIVE", true);
                } else {
                    Self::draw_badge(p, w - pad, cy, "OFF", false);
                }

                p.hline_t(ry + pt(ROW_H_PT), pad, w - pad, 1, DIVIDER);
            }
        }

        // 4. Subtle Minimalist Footer
        p.text_center(
            pt(FOOTER_BASE_PT),
            7.5,
            MUTED,
            "Tap to toggle · Hold to delete · Swipe up to exit",
        );
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
                let top2 = pt(SEC2_TOP_PT);

                if x >= pad
                    && x <= w - pad
                    && y >= top2
                    && y < top2 + visible_rows() as i32 * row_h
                {
                    let idx = ((y - top2) / row_h) as usize;
                    if let Some(it) = self.items.get(idx) {
                        crate::dictionary::toggle_active(&mut self.selection, &it.base);
                        self.items = crate::dictionary::scan();
                        return Action::Redraw;
                    }
                }
                Action::Keep
            }
            Gesture::LongPress { x, y } => {
                let (x, y) = (x as i32, y as i32);
                let pad = pt(PAD_PT);
                let row_h = pt(ROW_H_PT);
                let top2 = pt(SEC2_TOP_PT);

                if x >= pad
                    && x <= w - pad
                    && y >= top2
                    && y < top2 + visible_rows() as i32 * row_h
                {
                    let idx = ((y - top2) / row_h) as usize;
                    if let Some(it) = self.items.get(idx).cloned() {
                        let base = it.base.clone();
                        let name = it.name.clone();
                        return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                            "Delete dictionary?",
                            &format!("{}\n\nThe dictionary and its index will be deleted.", name),
                            "Delete",
                            None,
                            move |act| {
                                if let crate::confirm_dialog::ConfirmAction::Yes = act {
                                    crate::dictionary::delete_dictionary(&base);
                                    ybdev::log::plog(&format!("dictionaries: deleted {}", base));
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

/// Open book glyph for built-in definition dictionary.
fn draw_book_icon(p: &mut Painter, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    let mid_x = x + s / 2;
    let b_top = y + pt(2.0);
    let b_h = s - pt(4.0);
    p.rect_outline_t(Rect::new(x + pt(1.0), b_top, s / 2 - pt(1.0), b_h), 1, INK);
    p.rect_outline_t(Rect::new(mid_x, b_top, s / 2 - pt(1.0), b_h), 1, INK);
    p.hline_t(b_top + pt(3.0), x + pt(3.0), mid_x - pt(2.0), 1, INK);
    p.hline_t(b_top + pt(5.5), x + pt(3.0), mid_x - pt(2.0), 1, INK);
    p.hline_t(b_top + pt(3.0), mid_x + pt(2.0), x + s - pt(3.0), 1, INK);
    p.hline_t(b_top + pt(5.5), mid_x + pt(2.0), x + s - pt(3.0), 1, INK);
}

/// Dictionary glyph with text lines.
fn draw_dict_icon(p: &mut Painter, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    let r = Rect::new(x + pt(1.0), y + pt(1.5), s - pt(2.0), s - pt(3.0));
    p.rect_outline_t(r, 1, INK);
    p.hline_t(r.y + pt(3.0), r.x + pt(2.0), r.x + r.w - pt(2.0), 1, INK);
    p.hline_t(r.y + pt(6.0), r.x + pt(2.0), r.x + r.w - pt(2.0), 1, INK);
    p.hline_t(r.y + pt(9.0), r.x + pt(2.0), r.x + pt(5.0), 1, INK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn visible_rows_match_the_painted_list() {
        // Draw() stops painting at LIST_BOTTOM_PT; the tap/long-press hit
        // tests share visible_rows(), so a row that is never painted can
        // never be toggled or deleted. Pin the exact count so a layout
        // change that would make them disagree is caught here.
        assert_eq!(visible_rows(), 6);
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
    fn renders_empty_and_populated() {
        let _guard = crate::dictionary::TEST_ENV_LOCK.lock().unwrap();
        let font = yui::font::Font::load().unwrap();
        let dir = std::env::temp_dir().join(format!("yb_dict_screen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

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
        let mut s = DictionariesScreen::new();
        s.draw(&mut p); // empty state must not panic

        // A valid zero-entry .ybdict exercises the populated list path
        // (scan() lists valid bare .ybdicts even without a source).
        let mut yb = b"YBDICT02".to_vec();
        yb.extend_from_slice(&0u32.to_be_bytes());
        yb.extend_from_slice(&24u32.to_be_bytes());
        yb.extend_from_slice(&24u32.to_be_bytes());
        yb.extend_from_slice(&24u32.to_be_bytes());
        std::fs::write(dir.join("empty.ybdict"), &yb).unwrap();

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
        let mut s2 = DictionariesScreen::new();
        s2.items.push(crate::dictionary::DictInfo {
            base: "eng-rus-mueller".to_string(),
            name: "English-Russian Comprehensive Dictionary (Mueller Edition 7th Revision with Complete Transcriptions)".to_string(),
            word_count: 65420,
            imported: true,
        });
        s2.items.push(crate::dictionary::DictInfo {
            base: "freedict-eng-deu".to_string(),
            name: "FreeDict English-German (Comprehensive)".to_string(),
            word_count: 82150,
            imported: true,
        });
        s2.selection.active.push("empty".to_string());
        s2.selection.active.push("eng-rus-mueller".to_string());

        s2.draw(&mut p2);
        save_preview_artifact("dictionaries_preview.png", &canvas2);

        // Boundary state: one more dictionary than fits. Rows past
        // visible_rows() must not paint — the blank band below the list
        // (and the footer) must be dead space, matching the hit test that
        // shares visible_rows().
        let (mut canvas3, mut panel3) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p3 = yui::Painter::new(
            &mut panel3,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas3,
            &font,
        );
        let mut s3 = DictionariesScreen::new();
        for i in 0..visible_rows() + 1 {
            s3.items.push(crate::dictionary::DictInfo {
                base: format!("dict-{i:02}"),
                name: format!("Boundary Dictionary Number {:02} (FreeDict)", i + 1),
                word_count: 10_000 + i as u32,
                imported: true,
            });
        }
        s3.selection.active.push("dict-00".to_string());
        s3.draw(&mut p3);
        save_preview_artifact("dictionaries_boundary_preview.png", &canvas3);

        // Mostly white page with real ink (header + WordNet + rows).
        let lit = canvas2.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1236 * 1648 * 88 / 100, "page is not white: {lit}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggling_a_row_persists_active_state_and_deletes() {
        let _guard = crate::dictionary::TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("yb_dict_toggle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        // An importable dictionary appears and starts off; tapping it
        // activates and persists.
        fs::write(
            dir.join("eng-rus.tei"),
            "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\
             <teiHeader><fileDesc><titleStmt><title>Test</title></titleStmt>\
             <extent>1 headword</extent></fileDesc></teiHeader></TEI>",
        )
        .unwrap();
        let mut screen = DictionariesScreen::new();
        assert_eq!(screen.items.len(), 1);
        assert!(!screen.is_active("eng-rus"));

        let base = screen.items[0].base.clone();
        crate::dictionary::toggle_active(&mut screen.selection, &base);
        assert!(screen.is_active("eng-rus"));
        let loaded = crate::dictionary::load_selection();
        assert!(loaded.active.iter().any(|b| b == "eng-rus"));

        // Delete removes the dictionary and cleans selection
        assert!(crate::dictionary::delete_dictionary("eng-rus"));
        assert!(!dir.join("eng-rus.tei").exists());
        assert!(!dir.join("eng-rus.ybdict").exists());
        let reloaded_sel = crate::dictionary::load_selection();
        assert!(!reloaded_sel.active.iter().any(|b| b == "eng-rus"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
