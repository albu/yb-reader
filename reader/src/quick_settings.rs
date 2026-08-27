//! Quick Settings bottom sheet for in-book reading adjustments.
//! Occupies the bottom ~165pt of the screen, leaving the top 60% of the
//! active book page visible. Changes apply and re-render live in real time.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::Orientation;

use crate::crop_dialog::CropDialog;
use crate::split::{ContrastMode, ReaderSettings, SplitConfig, SplitPreset};
use yread::model::TextAlign;

const SHEET_H_PT: f32 = 197.0;
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
    /// 0 = main page (size/margins/spacing/contrast/night), 1 = typography
    /// page (alignment/hyphenation/indent/paragraph spacing).
    page: u8,
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
            page: 0,
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
        // Live preview: the reader-side closure re-renders the page under
        // the sheet (mupdf pdf re-render, or a fresh yread layout of the
        // current chapter). None keeps the previous bitmap.
        if let Some(new_gray) = (self.on_change)(self.settings) {
            self.page_gray = Some(new_gray);
        }
        Action::Redraw
    }

    /// The typography page: body alignment, hyphenation, first-line indent
    /// and paragraph spacing. Every change rides the same live-render +
    /// persistence path as the main page (apply_change).
    fn draw_typography_page(&mut self, p: &mut Painter, sheet_y: i32, w: i32) {
        let pad = pt(PAD_PT);
        let mut y = sheet_y + pt(12.0);

        // Row 1: FONT (body family)
        p.text(pad, y + pt(15.0), 8.5, 0, "FONT");
        // Four buttons: keep them narrow enough to clear the "FONT"
        // label — pt(58) started at x≈133 and collided with the label's T.
        let fam_btn_w = pt(52.0);
        for (i, family) in yread::font::FontFamily::ALL.iter().enumerate() {
            let bx = w - pad - (4 - i as i32) * (fam_btn_w + pt(4.0));
            let br = Rect::new(bx, y, fam_btn_w, pt(BTN_H_PT));
            if self.settings.font_family == *family {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.0, 255, family.label());
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.0, 0, family.label());
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 1: ALIGN (body paragraphs: Justify vs ragged Left)
        p.text(pad, y + pt(15.0), 8.5, 0, "ALIGN");
        let aligns = [(TextAlign::Justify, "Justify"), (TextAlign::Left, "Left")];
        let a_btn_w = pt(62.0);
        for (i, (align, a_lbl)) in aligns.iter().enumerate() {
            let bx = w - pad - (2 - i as i32) * (a_btn_w + pt(4.0));
            let br = Rect::new(bx, y, a_btn_w, pt(BTN_H_PT));
            if self.settings.body_align == *align {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, a_lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, a_lbl);
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 2: HYPHEN
        p.text(pad, y + pt(15.0), 8.5, 0, "HYPHEN");
        let h_btn_w = pt(52.0);
        for (i, (on, h_lbl)) in [(true, "On"), (false, "Off")].iter().enumerate() {
            let bx = w - pad - (2 - i as i32) * (h_btn_w + pt(4.0));
            let br = Rect::new(bx, y, h_btn_w, pt(BTN_H_PT));
            if self.settings.hyphenate == *on {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, h_lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, h_lbl);
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 3: INDENT (first-line indent in em; 0.0 = flush block style)
        p.text(pad, y + pt(15.0), 8.5, 0, "INDENT");
        let indent_str = format!("{:.1}em", self.settings.indent_em);
        p.text_center_in(
            pad + pt(70.0),
            pad + pt(130.0),
            y + pt(15.0),
            9.0,
            0,
            &indent_str,
        );
        let i_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
        let i_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
        p.rect_outline_t(i_minus, 1, 0);
        p.text_center_in(i_minus.x, i_minus.x + i_minus.w, y + pt(15.0), 10.0, 0, "-");
        p.rect_outline_t(i_plus, 1, 0);
        p.text_center_in(i_plus.x, i_plus.x + i_plus.w, y + pt(15.0), 10.0, 0, "+");
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 4: PARA SPACE (paragraph spacing in em)
        p.text(pad, y + pt(15.0), 8.5, 0, "PARA SPACE");
        let ps_str = format!("{:.2}em", self.settings.paragraph_spacing);
        p.text_center_in(
            pad + pt(70.0),
            pad + pt(130.0),
            y + pt(15.0),
            9.0,
            0,
            &ps_str,
        );
        let ps_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
        let ps_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
        p.rect_outline_t(ps_minus, 1, 0);
        p.text_center_in(ps_minus.x, ps_minus.x + ps_minus.w, y + pt(15.0), 10.0, 0, "-");
        p.rect_outline_t(ps_plus, 1, 0);
        p.text_center_in(ps_plus.x, ps_plus.x + ps_plus.w, y + pt(15.0), 10.0, 0, "+");
        y += pt(ROW_H_PT) + pt(6.0);

        // Row 5: BACK
        let back_btn = Rect::new(pad, y, pt(90.0), pt(BTN_H_PT));
        p.rect_outline_t(back_btn, 1, 0);
        // ‹/› are the chevrons the UI font actually carries (▶/◀ are
        // missing from Noto Sans and rendered as nothing on-device).
        p.text_center_in(back_btn.x, back_btn.x + back_btn.w, y + pt(15.0), 8.0, 0, "‹ BACK");
        p.text_center_in(
            pad + pt(96.0),
            w - pad,
            y + pt(15.0),
            7.5,
            120,
            "live preview updates as you tap",
        );
    }

    /// The typography page's tap handling. Geometry mirrors
    /// `draw_typography_page` exactly.
    fn handle_typography_gesture(&mut self, vx: i32, vy: i32, sheet_y: i32, w: i32) -> Action {
        let pad = pt(PAD_PT);
        let mut y = sheet_y + pt(12.0);

        // Row 1: FONT
        let fam_btn_w = pt(52.0);
        for (i, family) in yread::font::FontFamily::ALL.iter().enumerate() {
            let bx = w - pad - (4 - i as i32) * (fam_btn_w + pt(4.0));
            let br = Rect::new(bx, y, fam_btn_w, pt(BTN_H_PT));
            if br.contains(vx, vy) {
                self.settings.font_family = *family;
                return self.apply_change();
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 1: ALIGN
        let aligns = [TextAlign::Justify, TextAlign::Left];
        let a_btn_w = pt(62.0);
        for (i, align) in aligns.iter().enumerate() {
            let bx = w - pad - (2 - i as i32) * (a_btn_w + pt(4.0));
            let br = Rect::new(bx, y, a_btn_w, pt(BTN_H_PT));
            if br.contains(vx, vy) {
                self.settings.body_align = *align;
                return self.apply_change();
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 2: HYPHEN
        let h_btn_w = pt(52.0);
        for (i, on) in [true, false].iter().enumerate() {
            let bx = w - pad - (2 - i as i32) * (h_btn_w + pt(4.0));
            let br = Rect::new(bx, y, h_btn_w, pt(BTN_H_PT));
            if br.contains(vx, vy) {
                self.settings.hyphenate = *on;
                return self.apply_change();
            }
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 3: INDENT (0.1 em steps)
        let i_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
        let i_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
        if i_minus.contains(vx, vy) {
            self.settings.indent_em =
                (((self.settings.indent_em - 0.1) * 10.0).round() / 10.0).max(0.0);
            return self.apply_change();
        }
        if i_plus.contains(vx, vy) {
            self.settings.indent_em =
                (((self.settings.indent_em + 0.1) * 10.0).round() / 10.0).min(2.5);
            return self.apply_change();
        }
        y += pt(ROW_H_PT) + pt(4.0);

        // Row 4: PARA SPACE (0.05 em steps)
        let ps_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
        let ps_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
        if ps_minus.contains(vx, vy) {
            self.settings.paragraph_spacing =
                (((self.settings.paragraph_spacing - 0.05) * 100.0).round() / 100.0).max(0.0);
            return self.apply_change();
        }
        if ps_plus.contains(vx, vy) {
            self.settings.paragraph_spacing =
                (((self.settings.paragraph_spacing + 0.05) * 100.0).round() / 100.0).min(1.0);
            return self.apply_change();
        }
        y += pt(ROW_H_PT) + pt(6.0);

        // Row 5: BACK
        let back_btn = Rect::new(pad, y, pt(90.0), pt(BTN_H_PT));
        if back_btn.contains(vx, vy) {
            self.page = 0;
            return Action::Redraw;
        }

        Action::Keep
    }
}

impl Screen for QuickSettingsSheet {
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::from_rotation(self.settings.split.rotation))
    }

    fn on_resume(&mut self) -> Action {
        // CropDialog mutates its own copy and records it before popping.
        // Adopt what was persisted, or the next sheet control would
        // re-record this stale pre-crop copy and silently wipe the crop.
        if let Some(s) = crate::positions::resume_pos(&self.book).settings {
            if s != self.settings {
                self.settings = s;
                return self.apply_change();
            }
        }
        Action::Redraw
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
        p.rect(
            Rect::new(handle_x, sheet_y + pt(4.0), handle_w, pt(2.5)),
            170,
        );

        if self.page == 1 {
            self.draw_typography_page(p, sheet_y, w);
            return;
        }

        let mut y = sheet_y + pt(12.0);
        let pad = pt(PAD_PT);
        let _usable_w = w - 2 * pad;

        if !self.is_pdf {
            // --- REFLOWABLE (EPUB / FB2 / TXT) ---

            // Row 1: Font Size
            p.text(pad, y + pt(15.0), 8.5, 0, "FONT SIZE");
            let size_str = format!("{:.1} pt", self.settings.font_size);
            p.text_center_in(
                pad + pt(70.0),
                pad + pt(130.0),
                y + pt(15.0),
                9.0,
                0,
                &size_str,
            );

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

            // Row 3: Line spacing (multiplier over the book's leading)
            p.text(pad, y + pt(15.0), 8.5, 0, "SPACING");
            let sp_str = format!("{:.1}\u{d7}", self.settings.line_spacing);
            p.text_center_in(
                pad + pt(70.0),
                pad + pt(130.0),
                y + pt(15.0),
                9.0,
                0,
                &sp_str,
            );
            let sp_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
            let sp_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
            p.rect_outline_t(sp_minus, 1, 0);
            p.text_center_in(
                sp_minus.x,
                sp_minus.x + sp_minus.w,
                y + pt(15.0),
                10.0,
                0,
                "-",
            );
            p.rect_outline_t(sp_plus, 1, 0);
            p.text_center_in(sp_plus.x, sp_plus.x + sp_plus.w, y + pt(15.0), 10.0, 0, "+");
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

            // Row 2: Visual Crop Studio Entry
            p.text(pad, y + pt(15.0), 8.5, 0, "CROP");
            let crop_status = if self.settings.split.margin_left == 0.0
                && self.settings.split.margin_top == 0.0
                && self.settings.split.margin_right == 0.0
                && self.settings.split.margin_bottom == 0.0
            {
                "[ Crop Margins: Off ➔ ]"
            } else if self.settings.split.mirror_even_odd {
                "[ Crop: Odd/Even Active ➔ ]"
            } else {
                "[ Crop: Active ➔ ]"
            };
            let crop_btn = Rect::new(w - pad - pt(175.0), y, pt(175.0), pt(BTN_H_PT));
            p.rect_outline_t(crop_btn, 1, 0);
            p.text_center_in(
                crop_btn.x,
                crop_btn.x + crop_btn.w,
                y + pt(15.0),
                7.5,
                0,
                crop_status,
            );
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

        // Row 4: Night / Invert (page inversion — the old night-reading
        // toggle the settings unification dropped).
        p.text(pad, y + pt(15.0), 8.5, 0, "NIGHT");
        let inv_btn = Rect::new(w - pad - pt(120.0), y, pt(120.0), pt(BTN_H_PT));
        if self.settings.invert {
            p.rect(inv_btn, 0);
            p.text_center_in(
                inv_btn.x,
                inv_btn.x + inv_btn.w,
                y + pt(15.0),
                7.5,
                255,
                "ON",
            );
        } else {
            p.rect_outline_t(inv_btn, 1, 120);
            p.text_center_in(
                inv_btn.x,
                inv_btn.x + inv_btn.w,
                y + pt(15.0),
                7.5,
                0,
                "OFF",
            );
        }
        y += pt(ROW_H_PT) + pt(6.0);

        // Footer: dismiss hint + Typography page entry
        p.text_center_in(
            pad,
            w - pad - pt(96.0),
            y + pt(15.0),
            7.5,
            120,
            "tap above · swipe down",
        );
        let typog_btn = Rect::new(w - pad - pt(88.0), y - pt(4.0), pt(88.0), pt(BTN_H_PT));
        p.rect_outline_t(typog_btn, 1, 120);
        p.text_center_in(
            typog_btn.x,
            typog_btn.x + typog_btn.w,
            typog_btn.y + pt(15.0),
            7.5,
            0,
            "TYPOG ›",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let (vx, vy) = match g {
            Gesture::Tap { x, y } => (x as i32, y as i32),
            Gesture::Swipe { dir: ybdev::input::SwipeDir::South, .. } => {
                return Action::Pop
            }
            // Long-press, drags and north/east/west swipes mean nothing
            // here; they must not fall through to (0,0) — which sits above
            // the sheet and read as "tap outside", dismissing it.
            _ => return Action::Keep,
        };

        let sheet_h = pt(SHEET_H_PT);
        let sheet_y = h - sheet_h;
        let pad = pt(PAD_PT);

        // Tap outside sheet -> dismiss
        if vy < sheet_y {
            return Action::Pop;
        }

        if self.page == 1 {
            return self.handle_typography_gesture(vx, vy, sheet_y, w);
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

            // Row 3: Spacing (0.1 steps, rounded — float steps drift)
            let sp_minus = Rect::new(w - pad - pt(70.0), y, pt(30.0), pt(BTN_H_PT));
            let sp_plus = Rect::new(w - pad - pt(34.0), y, pt(30.0), pt(BTN_H_PT));
            if sp_minus.contains(vx, vy) {
                self.settings.line_spacing =
                    (((self.settings.line_spacing - 0.1) * 10.0).round() / 10.0).max(0.9);
                return self.apply_change();
            }
            if sp_plus.contains(vx, vy) {
                self.settings.line_spacing =
                    (((self.settings.line_spacing + 0.1) * 10.0).round() / 10.0).min(1.8);
                return self.apply_change();
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
                    let old_split = self.settings.split;
                    let mut new_split = SplitConfig::for_preset(*preset);
                    new_split.margin_left = old_split.margin_left;
                    new_split.margin_top = old_split.margin_top;
                    new_split.margin_right = old_split.margin_right;
                    new_split.margin_bottom = old_split.margin_bottom;
                    self.settings.split = new_split;
                    return self.apply_change();
                }
            }
            y += pt(ROW_H_PT) + pt(4.0);

            // Row 2: Visual Crop Studio Entry
            let crop_btn = Rect::new(w - pad - pt(175.0), y, pt(175.0), pt(BTN_H_PT));
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

        // Row 4: Night / Invert
        let inv_btn = Rect::new(w - pad - pt(120.0), y, pt(120.0), pt(BTN_H_PT));
        if inv_btn.contains(vx, vy) {
            self.settings.invert = !self.settings.invert;
            return self.apply_change();
        }

        // Typography page entry (footer button)
        let footer_y = y + pt(ROW_H_PT) + pt(6.0);
        let typog_btn = Rect::new(w - pad - pt(88.0), footer_y - pt(4.0), pt(88.0), pt(BTN_H_PT));
        if typog_btn.contains(vx, vy) {
            self.page = 1;
            return Action::Redraw;
        }

       Action::Keep
   }
    fn default_edges(&self) -> bool {
        false
    }
}
