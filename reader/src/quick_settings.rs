//! Quick Settings bottom sheet for in-book reading adjustments.
//! Occupies the bottom ~165pt of the screen, leaving the top 60% of the
//! active book page visible. Changes apply and re-render live in real time.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::Orientation;
use yui::screen::{Action, Screen};

use crate::crop_dialog::CropDialog;
use crate::settings_dialog::SettingsDialog;
use crate::split::{ContrastMode, ReaderSettings, SplitConfig, SplitPreset};

const SHEET_H_PT: f32 = 168.0;
const PAD_PT: f32 = 16.0;
const ROW_H_PT: f32 = 25.0;
const BTN_H_PT: f32 = 22.0;

pub struct QuickSettingsSheet {
    pub book: String,
    pub page_no: usize,
    pub sub_idx: usize,
    pub total: usize,
    pub settings: ReaderSettings,
    pub is_pdf: bool,
    pub doc: Option<std::rc::Rc<mupdf::Document>>,
    pub page_gray: Option<Vec<u8>>,
    dims: (i32, i32),
    on_change: Box<dyn FnMut(ReaderSettings) -> Option<Vec<u8>>>,
}

impl QuickSettingsSheet {
    pub fn new(
        book: String,
        page_no: usize,
        sub_idx: usize,
        total: usize,
        settings: ReaderSettings,
        is_pdf: bool,
        doc: Option<std::rc::Rc<mupdf::Document>>,
        page_gray: Option<Vec<u8>>,
        on_change: impl FnMut(ReaderSettings) -> Option<Vec<u8>> + 'static,
    ) -> Self {
        Self {
            book,
            page_no,
            sub_idx,
            total,
            settings,
            is_pdf,
            doc,
            page_gray,
            dims: (1236, 1648),
            on_change: Box::new(on_change),
        }
    }

    fn apply_change(&mut self) -> Action {
        crate::dialogs::record_sub(
            &self.book,
            self.page_no,
            self.sub_idx,
            self.total,
            self.settings,
        );
        if let Some(new_gray) = (self.on_change)(self.settings) {
            self.page_gray = Some(new_gray);
        }
        Action::Redraw
    }
}

impl Screen for QuickSettingsSheet {
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::from_rotation(self.settings.split.rotation))
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // 1. Draw live underlying book page in background
        if let Some(gray) = &self.page_gray {
            p.blit_gray(0, 0, w, h, gray, w as usize);
        } else {
            p.clear(255);
        }

        let sheet_h = pt(SHEET_H_PT);
        let sheet_y = h - sheet_h;

        // 2. Translucent dark drop-shadow + crisp white sheet body
        p.hline_t(sheet_y - 2, 0, w, 1, 140);
        p.hline_t(sheet_y - 1, 0, w, 1, 100);
        p.rect(Rect::new(0, sheet_y, w, sheet_h), 255);
        p.hline_t(sheet_y, 0, w, 2, 0);

        // Grab handle pill at top center
        let handle_w = pt(28.0);
        let handle_x = (w - handle_w) / 2;
        p.rect(Rect::new(handle_x, sheet_y + pt(4.0), handle_w, pt(2.5)), 170);

        let mut y = sheet_y + pt(12.0);
        let pad = pt(PAD_PT);
        let _usable_w = w - 2 * pad;

        if !self.is_pdf {
            // --- REFLOWABLE (EPUB / FB2 / TXT) ---

            // Row 1: Font Size
            p.text(pad, y + pt(15.0), 8.5, 0, "FONT SIZE");
            let size_str = format!("{:.1} pt", self.settings.font_size);
            p.text_center_in(pad + pt(70.0), pad + pt(130.0), y + pt(15.0), 9.0, 0, &size_str);

            let f_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
            let f_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
            p.rect_outline_t(f_minus, 1, 0);
            p.text_center_in(f_minus.x, f_minus.x + f_minus.w, y + pt(15.0), 10.0, 0, "-");
            p.rect_outline_t(f_plus, 1, 0);
            p.text_center_in(f_plus.x, f_plus.x + f_plus.w, y + pt(15.0), 10.0, 0, "+");
            y += pt(ROW_H_PT) + pt(4.0);

            // Row 2: Margins
            p.text(pad, y + pt(15.0), 8.5, 0, "MARGINS");
            let margins = [(36, "Compact"), (72, "Normal"), (108, "Wide")];
            let m_btn_w = pt(52.0);
            for (i, (pad_val, m_lbl)) in margins.iter().enumerate() {
                let bx = w - pad - (3 - i as i32) * (m_btn_w + pt(4.0));
                let br = Rect::new(bx, y, m_btn_w, pt(BTN_H_PT));
                if self.settings.margin_pad == *pad_val {
                    p.rect(br, 0);
                    p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, m_lbl);
                } else {
                    p.rect_outline_t(br, 1, 120);
                    p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, m_lbl);
                }
            }
            y += pt(ROW_H_PT) + pt(4.0);
        } else {
            // --- FIXED-LAYOUT (PDF / MANGA) ---

            // Row 1: Split Presets
            p.text(pad, y + pt(15.0), 8.5, 0, "SPLIT");
            let presets = [
                (SplitPreset::FitPage, "Fit"),
                (SplitPreset::Horizontal2, "2-Split"),
                (SplitPreset::Horizontal3, "3-Split"),
                (SplitPreset::Grid4, "Grid"),
            ];
            let s_btn_w = pt(44.0);
            for (i, (preset, s_lbl)) in presets.iter().enumerate() {
                let bx = w - pad - (4 - i as i32) * (s_btn_w + pt(4.0));
                let br = Rect::new(bx, y, s_btn_w, pt(BTN_H_PT));
                if self.settings.split.preset == *preset {
                    p.rect(br, 0);
                    p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, s_lbl);
                } else {
                    p.rect_outline_t(br, 1, 120);
                    p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, s_lbl);
                }
            }
            y += pt(ROW_H_PT) + pt(4.0);

            // Row 2: Interactive Margin Crop Guides
            p.text(pad, y + pt(15.0), 8.5, 0, "CROP");
            let crop_btn = Rect::new(w - pad - pt(188.0), y, pt(188.0), pt(BTN_H_PT));
            p.rect_outline_t(crop_btn, 1, 0);
            p.text_center_in(crop_btn.x, crop_btn.x + crop_btn.w, y + pt(15.0), 7.5, 0, "[ ⛶ Adjust Crop Guides ]");
            y += pt(ROW_H_PT) + pt(4.0);
        }

        // Row 3: Contrast (Applies to both)
        p.text(pad, y + pt(15.0), 8.5, 0, "CONTRAST");
        let contrasts = [
            (ContrastMode::Normal, "Normal"),
            (ContrastMode::BoldText, "Dark"),
            (ContrastMode::HighContrast, "Max"),
        ];
        let c_btn_w = pt(52.0);
        for (i, (c_mode, c_lbl)) in contrasts.iter().enumerate() {
            let bx = w - pad - (3 - i as i32) * (c_btn_w + pt(4.0));
            let br = Rect::new(bx, y, c_btn_w, pt(BTN_H_PT));
            if self.settings.contrast == *c_mode {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, c_lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, c_lbl);
            }
        }
        y += pt(ROW_H_PT) + pt(6.0);

        // Row 4: Action Footer [ All Settings… ]  [ Done ]
        let all_btn = Rect::new(pad, y, pt(110.0), pt(BTN_H_PT));
        p.rect_outline_t(all_btn, 1, 100);
        p.text_center_in(all_btn.x, all_btn.x + all_btn.w, y + pt(15.0), 7.5, 0, "All Settings…");

        let done_btn = Rect::new(w - pad - pt(70.0), y, pt(70.0), pt(BTN_H_PT));
        p.rect(done_btn, 0);
        p.text_center_in(done_btn.x, done_btn.x + done_btn.w, y + pt(15.0), 8.0, 255, "Done");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let (vx, vy) = match g {
            Gesture::Tap { x, y } => (x as i32, y as i32),
            Gesture::Swipe { dir, .. } if dir == ybdev::input::SwipeDir::South => return Action::Pop,
            _ => (0, 0),
        };

        let sheet_h = pt(SHEET_H_PT);
        let sheet_y = h - sheet_h;
        let pad = pt(PAD_PT);

        // Tap outside sheet -> dismiss
        if vy < sheet_y {
            return Action::Pop;
        }

        let mut y = sheet_y + pt(12.0);

        if !self.is_pdf {
            // Row 1: Font size
            let f_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
            let f_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
            if f_minus.contains(vx, vy) {
                self.settings.font_size = (self.settings.font_size - 1.0).max(7.0);
                return self.apply_change();
            }
            if f_plus.contains(vx, vy) {
                self.settings.font_size = (self.settings.font_size + 1.0).min(24.0);
                return self.apply_change();
            }
            y += pt(ROW_H_PT) + pt(4.0);

            // Row 2: Margins
            let margins = [(36, "Compact"), (72, "Normal"), (108, "Wide")];
            let m_btn_w = pt(52.0);
            for (i, (pad_val, _)) in margins.iter().enumerate() {
                let bx = w - pad - (3 - i as i32) * (m_btn_w + pt(4.0));
                let br = Rect::new(bx, y, m_btn_w, pt(BTN_H_PT));
                if br.contains(vx, vy) {
                    self.settings.margin_pad = *pad_val;
                    return self.apply_change();
                }
            }
            y += pt(ROW_H_PT) + pt(4.0);
        } else {
            // Row 1: Split Presets
            let presets = [
                SplitPreset::FitPage,
                SplitPreset::Horizontal2,
                SplitPreset::Horizontal3,
                SplitPreset::Grid4,
            ];
            let s_btn_w = pt(44.0);
            for (i, preset) in presets.iter().enumerate() {
                let bx = w - pad - (4 - i as i32) * (s_btn_w + pt(4.0));
                let br = Rect::new(bx, y, s_btn_w, pt(BTN_H_PT));
                if br.contains(vx, vy) {
                    self.settings.split = SplitConfig::for_preset(*preset);
                    return self.apply_change();
                }
            }
            y += pt(ROW_H_PT) + pt(4.0);

            // Row 2: Adjust Crop Guides button
            let crop_btn = Rect::new(w - pad - pt(188.0), y, pt(188.0), pt(BTN_H_PT));
            if crop_btn.contains(vx, vy) {
                let book = self.book.clone();
                let page = self.page_no;
                let sub = self.sub_idx;
                let tot = self.total;
                let s = self.settings;
                let doc = self.doc.clone();
                return Action::Push(Box::new(CropDialog::new(book, page, sub, tot, s, doc)));
            }
            y += pt(ROW_H_PT) + pt(4.0);
        }

        // Row 3: Contrast
        let contrasts = [
            ContrastMode::Normal,
            ContrastMode::BoldText,
            ContrastMode::HighContrast,
        ];
        let c_btn_w = pt(52.0);
        for (i, c_mode) in contrasts.iter().enumerate() {
            let bx = w - pad - (3 - i as i32) * (c_btn_w + pt(4.0));
            let br = Rect::new(bx, y, c_btn_w, pt(BTN_H_PT));
            if br.contains(vx, vy) {
                self.settings.contrast = *c_mode;
                return self.apply_change();
            }
        }
        y += pt(ROW_H_PT) + pt(6.0);

        // Row 4: All Settings / Done
        let all_btn = Rect::new(pad, y, pt(110.0), pt(BTN_H_PT));
        if all_btn.contains(vx, vy) {
            let name = self.book.clone();
            let page = self.page_no;
            let sub = self.sub_idx;
            let tot = self.total;
            let s = self.settings;
            let gray = self.page_gray.clone();
            return Action::Push(Box::new(SettingsDialog::new_legacy(name, page, sub, tot, s, gray)));
        }

        let done_btn = Rect::new(w - pad - pt(70.0), y, pt(70.0), pt(BTN_H_PT));
        if done_btn.contains(vx, vy) {
            return Action::Pop;
        }

        Action::Keep
    }

    fn default_edges(&self) -> bool {
        false
    }
}
