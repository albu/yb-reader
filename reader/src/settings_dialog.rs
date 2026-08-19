//! Multi-tab Reader Control & Settings Screen.
//! Tabs: [1. Text & Layout]  [2. Split & Crop]  [3. Display & Contrast]

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::Orientation;
use yui::screen::{Action, Screen};

use crate::split::{detect_margins, ContrastMode, ReaderSettings, SplitConfig, SplitPreset};

pub struct ReaderSettingsDialog {
    pub settings: ReaderSettings,
    pub tab: usize, // 0 = Typography, 1 = Split & Crop, 2 = Contrast & Display, 3 = Vocab
    #[allow(dead_code)]
    pub is_pdf: bool,

    on_change: Box<dyn FnMut(ReaderSettings) -> Action>,
    page_samples: Option<(Vec<u8>, usize, usize, usize)>,
    dims: (i32, i32),
}

impl ReaderSettingsDialog {
    pub fn new(
        settings: ReaderSettings,
        is_pdf: bool,
        page_samples: Option<(Vec<u8>, usize, usize, usize)>,
        on_change: impl FnMut(ReaderSettings) -> Action + 'static,
    ) -> Self {
        // Default to Split & Crop tab for PDFs, Typography tab for reflowable books
        let initial_tab = if is_pdf { 1 } else { 0 };
        Self {
            settings,
            tab: initial_tab,
            is_pdf,
            on_change: Box::new(on_change),
            page_samples,
            dims: (1236, 1648),
        }
    }
}



const PAD_PT: f32 = 16.0;
const DIALOG_W_PT: f32 = 265.0;
const DIALOG_H_PT: f32 = 330.0;
const ROW_H_PT: f32 = 28.0;
const BTN_H_PT: f32 = 24.0;

impl Screen for ReaderSettingsDialog {
    /// Fixed-height tabbed content (330pt) authored for the tall canvas —
    /// always a portrait modal, whatever grip the book beneath is in.
    /// Rotation changes apply when the dialog closes and the reader
    /// resumes (App flips the panel then, with a full refresh).
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        let dw = pt(DIALOG_W_PT).min(w - pt(2.0 * PAD_PT));
        let dh = pt(DIALOG_H_PT);
        let dx = (w - dw) / 2;
        let dy = (h - dh) / 2;

        // Dialog background box
        let rect = Rect::new(dx, dy, dw, dh);
        p.rect(rect, 255);
        p.rect_outline_t(rect, 3, 0);

        // Header: Tab Bar
        let tab_h = pt(32.0);
        let tab_w = dw / 4;
        let tabs = ["1. Text", "2. Split", "3. Disp", "4. Vocab"];

        for (i, label) in tabs.iter().enumerate() {
            let tx0 = dx + i as i32 * tab_w;
            let tx1 = if i == 3 { dx + dw } else { tx0 + tab_w };
            let tr = Rect::new(tx0, dy, tx1 - tx0, tab_h);

            if self.tab == i {
                p.rect(tr, 0);
                p.text_center_in(tx0, tx1, dy + pt(21.0), 8.0, 255, label);
            } else {
                p.rect_outline_t(tr, 1, 150);
                p.text_center_in(tx0, tx1, dy + pt(21.0), 8.0, 100, label);
            }
        }


        let mut y = dy + pt(44.0);

        match self.tab {
            0 => {
                // --- TAB 0: TEXT & TYPOGRAPHY ---
                p.text(dx + pt(14.0), y + pt(16.0), 9.0, 0, "FONT SIZE");
                let size_str = format!("{:.1} pt", self.settings.font_size);
                p.text(dx + pt(90.0), y + pt(16.0), 9.0, 0, &size_str);

                let f_minus = Rect::new(dx + dw - pt(70.0), y, pt(26.0), pt(BTN_H_PT));
                let f_plus = Rect::new(dx + dw - pt(38.0), y, pt(26.0), pt(BTN_H_PT));
                p.rect_outline_t(f_minus, 2, 0);
                p.text_center_in(f_minus.x, f_minus.x + f_minus.w, y + pt(16.0), 10.0, 0, "-");
                p.rect_outline_t(f_plus, 2, 0);
                p.text_center_in(f_plus.x, f_plus.x + f_plus.w, y + pt(16.0), 10.0, 0, "+");
                y += pt(ROW_H_PT) + pt(4.0);

                // Margin options
                p.text(dx + pt(14.0), y + pt(16.0), 9.0, 0, "PAGE MARGINS");
                y += pt(20.0);
                let margins = [(36, "Compact (36px)"), (72, "Normal (72px)"), (108, "Wide (108px)")];
                let m_btn_w = (dw - pt(28.0)) / 3;
                for (i, (pad, m_lbl)) in margins.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * m_btn_w;
                    let br = Rect::new(bx, y, m_btn_w - pt(4.0), pt(BTN_H_PT));
                    if self.settings.margin_pad == *pad {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 255, m_lbl);
                    } else {
                        p.rect_outline_t(br, 1, 100);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 0, m_lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(8.0);

                // Header status line toggle
                p.text(dx + pt(14.0), y + pt(16.0), 9.0, 0, "CLOCK & BATTERY HEADER");
                let h_btn = Rect::new(dx + dw - pt(70.0), y, pt(58.0), pt(BTN_H_PT));
                if self.settings.show_header {
                    p.rect(h_btn, 0);
                    p.text_center_in(h_btn.x, h_btn.x + h_btn.w, y + pt(16.0), 8.0, 255, "SHOW");
                } else {
                    p.rect_outline_t(h_btn, 1, 100);
                    p.text_center_in(h_btn.x, h_btn.x + h_btn.w, y + pt(16.0), 8.0, 100, "HIDE");
                }
                y += pt(ROW_H_PT) + pt(10.0);

                p.text(
                    dx + pt(14.0),
                    y + pt(14.0),
                    7.5,
                    120,
                    "Note: Reflow settings apply to EPUB/FB2/TXT books.",
                );
            }

            1 => {
                // --- TAB 1: PDF SPLIT & CROP ---
                let presets = [
                    (SplitPreset::FitPage, "Fit Page (Single)"),
                    (SplitPreset::Horizontal2, "2-Split Landscape (Top/Bottom)"),
                    (SplitPreset::Horizontal3, "3-Split Landscape (Top/Mid/Bot)"),
                    (SplitPreset::Vertical2, "2-Column Portrait (Left/Right)"),
                    (SplitPreset::Grid4, "4-Grid Landscape (2x2)"),
                ];

                for (preset, label) in presets {
                    let active = self.settings.split.preset == preset;
                    let btn_r = Rect::new(dx + pt(10.0), y, dw - pt(20.0), pt(20.0));
                    if active {
                        p.rect(btn_r, 0);
                        p.text(dx + pt(16.0), y + pt(14.0), 7.5, 255, label);
                        p.text_right(dx + dw - pt(16.0), y + pt(14.0), 7.5, 255, "[ON]");
                    } else {
                        p.rect_outline_t(btn_r, 1, 120);
                        p.text(dx + pt(16.0), y + pt(14.0), 7.5, 0, label);
                    }
                    y += pt(22.0);
                }

                y += pt(4.0);

                // Orientation row
                let rot_label = match self.settings.split.rotation {
                    0 => "Orientation: Portrait (0°)",
                    90 => "Orientation: Landscape CCW (90°)",
                    270 => "Orientation: Landscape CW (270°)",
                    _ => "Orientation: Portrait (0°)",
                };
                let rot_btn = Rect::new(dx + pt(10.0), y, dw - pt(20.0), pt(22.0));
                p.rect_outline_t(rot_btn, 1, 0);
                p.text_center_in(rot_btn.x, rot_btn.x + rot_btn.w, y + pt(15.0), 8.0, 0, rot_label);
                y += pt(26.0);

                // Margin & Overlap
                let margin_pct = (self.settings.split.margin_left * 100.0).round() as i32;
                p.text(dx + pt(14.0), y + pt(15.0), 7.5, 0, &format!("Margin: {}%", margin_pct));

                let m_minus = Rect::new(dx + dw - pt(96.0), y, pt(22.0), pt(22.0));
                let m_plus = Rect::new(dx + dw - pt(70.0), y, pt(22.0), pt(22.0));
                let m_auto = Rect::new(dx + dw - pt(44.0), y, pt(34.0), pt(22.0));

                p.rect_outline_t(m_minus, 1, 0);
                p.text_center_in(m_minus.x, m_minus.x + m_minus.w, y + pt(15.0), 8.5, 0, "-");
                p.rect_outline_t(m_plus, 1, 0);
                p.text_center_in(m_plus.x, m_plus.x + m_plus.w, y + pt(15.0), 8.5, 0, "+");
                p.rect_outline_t(m_auto, 1, 0);
                p.text_center_in(m_auto.x, m_auto.x + m_auto.w, y + pt(15.0), 7.0, 0, "AUTO");
                y += pt(26.0);

                let ov_pct = (self.settings.split.overlap * 100.0).round() as i32;
                p.text(dx + pt(14.0), y + pt(15.0), 7.5, 0, &format!("Overlap: {}%", ov_pct));

                let ov_minus = Rect::new(dx + dw - pt(66.0), y, pt(26.0), pt(22.0));
                let ov_plus = Rect::new(dx + dw - pt(36.0), y, pt(26.0), pt(22.0));
                p.rect_outline_t(ov_minus, 1, 0);
                p.text_center_in(ov_minus.x, ov_minus.x + ov_minus.w, y + pt(15.0), 8.5, 0, "-");
                p.rect_outline_t(ov_plus, 1, 0);
                p.text_center_in(ov_plus.x, ov_plus.x + ov_plus.w, y + pt(15.0), 8.5, 0, "+");
            }


            2 => {
                // --- TAB 2: DISPLAY & CONTRAST ---
                p.text(dx + pt(14.0), y + pt(16.0), 8.5, 0, "CONTRAST & TEXT BOLDNESS");
                y += pt(20.0);

                let contrasts = [
                    (ContrastMode::Normal, "Normal"),
                    (ContrastMode::BoldText, "Bold"),
                    (ContrastMode::HighContrast, "High"),
                    (ContrastMode::ScanClean, "Scans"),
                ];
                let c_btn_w = (dw - pt(28.0)) / 4;
                for (i, (c_mode, c_lbl)) in contrasts.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * c_btn_w;
                    let br = Rect::new(bx, y, c_btn_w - pt(3.0), pt(BTN_H_PT));
                    if self.settings.contrast == *c_mode {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 255, c_lbl);
                    } else {
                        p.rect_outline_t(br, 1, 100);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 0, c_lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(4.0);

                // Background Whitening Cutoff
                let cut_str = if self.settings.white_cutoff == 0 {
                    "Off".to_string()
                } else {
                    format!("Level {}", self.settings.white_cutoff)
                };
                p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, &format!("Background Clean: {}", cut_str));

                let cut_minus = Rect::new(dx + dw - pt(66.0), y, pt(26.0), pt(BTN_H_PT));
                let cut_plus = Rect::new(dx + dw - pt(36.0), y, pt(26.0), pt(BTN_H_PT));
                p.rect_outline_t(cut_minus, 1, 0);
                p.text_center_in(cut_minus.x, cut_minus.x + cut_minus.w, y + pt(16.0), 8.5, 0, "-");
                p.rect_outline_t(cut_plus, 1, 0);
                p.text_center_in(cut_plus.x, cut_plus.x + cut_plus.w, y + pt(16.0), 8.5, 0, "+");
                y += pt(ROW_H_PT);

                // Night Mode (Invert Colors)
                p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, "Night Mode (Invert Colors)");
                let inv_btn = Rect::new(dx + dw - pt(66.0), y, pt(56.0), pt(BTN_H_PT));
                if self.settings.invert {
                    p.rect(inv_btn, 0);
                    p.text_center_in(inv_btn.x, inv_btn.x + inv_btn.w, y + pt(16.0), 8.0, 255, "ON");
                } else {
                    p.rect_outline_t(inv_btn, 1, 100);
                    p.text_center_in(inv_btn.x, inv_btn.x + inv_btn.w, y + pt(16.0), 8.0, 100, "OFF");
                }
                y += pt(ROW_H_PT);

                // Full Refresh Interval
                let ref_str = if self.settings.refresh_interval == 0 {
                    "Manual only".to_string()
                } else {
                    format!("Every {} p.", self.settings.refresh_interval)
                };
                p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, &format!("Full Refresh: {}", ref_str));
                let ref_btn = Rect::new(dx + dw - pt(76.0), y, pt(66.0), pt(BTN_H_PT));
                p.rect_outline_t(ref_btn, 1, 0);
                p.text_center_in(ref_btn.x, ref_btn.x + ref_btn.w, y + pt(16.0), 7.5, 0, "CYCLE");
            }

            3 => {
                // --- TAB 3: VOCAB & WORD WISE ---
                let prof = crate::vocab::VocabProfile::load();

                // Style selector
                p.text(dx + pt(14.0), y + pt(16.0), 9.0, 0, "WORD WISE GLOSS STYLE");
                y += pt(20.0);
                let styles = [
                    (crate::vocab::AnnotationStyle::Interlinear, "Interlinear"),
                    (crate::vocab::AnnotationStyle::Margin, "Margin"),
                    (crate::vocab::AnnotationStyle::DottedUnderline, "Dotted"),
                    (crate::vocab::AnnotationStyle::Off, "Off"),
                ];
                let s_btn_w = (dw - pt(28.0)) / 4;
                for (i, (s_mode, s_lbl)) in styles.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * s_btn_w;
                    let br = Rect::new(bx, y, s_btn_w - pt(3.0), pt(BTN_H_PT));
                    if prof.style == *s_mode {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 255, s_lbl);
                    } else {
                        p.rect_outline_t(br, 1, 100);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 0, s_lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(4.0);

                // User Level
                let cefr_name = if prof.user_level < 25 {
                    "A1 (Basic)"
                } else if prof.user_level < 45 {
                    "A2 (Elem)"
                } else if prof.user_level < 65 {
                    "B1 (Inter)"
                } else if prof.user_level < 80 {
                    "B2 (Upper)"
                } else if prof.user_level < 92 {
                    "C1 (Adv)"
                } else {
                    "C2 (Master)"
                };
                p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, &format!("Target Level: {} ({})", prof.user_level, cefr_name));

                let l_minus = Rect::new(dx + dw - pt(66.0), y, pt(26.0), pt(BTN_H_PT));
                let l_plus = Rect::new(dx + dw - pt(36.0), y, pt(26.0), pt(BTN_H_PT));
                p.rect_outline_t(l_minus, 1, 0);
                p.text_center_in(l_minus.x, l_minus.x + l_minus.w, y + pt(16.0), 8.5, 0, "-");
                p.rect_outline_t(l_plus, 1, 0);
                p.text_center_in(l_plus.x, l_plus.x + l_plus.w, y + pt(16.0), 8.5, 0, "+");
                y += pt(ROW_H_PT) + pt(4.0);

                // Words per page budget
                p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, "Max Glosses / Page");
                let counts = [1, 2, 3, 5];
                let c_btn_w = (dw - pt(28.0)) / 4;
                for (i, cnt) in counts.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * c_btn_w;
                    let br = Rect::new(bx, y, c_btn_w - pt(3.0), pt(BTN_H_PT));
                    let lbl = format!("{} / pg", cnt);
                    if prof.max_per_page == *cnt {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 255, &lbl);
                    } else {
                        p.rect_outline_t(br, 1, 100);
                        p.text_center_in(br.x, br.x + br.w, y + pt(16.0), 7.5, 0, &lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(8.0);

                // Stats
                let stats = format!("Learned: {} · Starred: {}", prof.known_words.len(), prof.learning_words.len());
                p.text(dx + pt(14.0), y + pt(14.0), 7.5, 120, &stats);
            }

            _ => {}
        }

        // Apply & Read Button
        let apply_y = dy + dh - pt(36.0);
        let apply_r = Rect::new(dx + pt(10.0), apply_y, dw - pt(20.0), pt(26.0));
        p.rect(apply_r, 0);
        p.text_center_in(apply_r.x, apply_r.x + apply_r.w, apply_y + pt(17.0), 9.5, 255, "APPLY & READ");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let dw = pt(DIALOG_W_PT).min(w - pt(2.0 * PAD_PT));
        let dh = pt(DIALOG_H_PT);
        let dx = (w - dw) / 2;
        let dy = (h - dh) / 2;

        match g {
            Gesture::Tap { x, y } => {
                let (tx, ty) = (x as i32, y as i32);
                let dialog_rect = Rect::new(dx, dy, dw, dh);
                if !dialog_rect.contains(tx, ty) {
                    return (self.on_change)(self.settings);
                }

                // Check Tab Bar taps
                let tab_h = pt(32.0);
                let tab_w = dw / 4;
                if ty >= dy && ty < dy + tab_h {
                    let clicked_tab = ((tx - dx) / tab_w).clamp(0, 3) as usize;
                    self.tab = clicked_tab;
                    return Action::Redraw;
                }

                // Apply button tap
                let apply_y = dy + dh - pt(36.0);
                let apply_r = Rect::new(dx + pt(10.0), apply_y, dw - pt(20.0), pt(26.0));
                if apply_r.contains(tx, ty) {
                    return (self.on_change)(self.settings);
                }



                let mut py = dy + pt(44.0);

                match self.tab {
                    0 => {
                        // TAB 0: Typography
                        let f_minus = Rect::new(dx + dw - pt(70.0), py, pt(26.0), pt(BTN_H_PT));
                        let f_plus = Rect::new(dx + dw - pt(38.0), py, pt(26.0), pt(BTN_H_PT));
                        if f_minus.contains(tx, ty) {
                            self.settings.font_size = (self.settings.font_size - 1.5).max(7.0);
                            return Action::Redraw;
                        }
                        if f_plus.contains(tx, ty) {
                            self.settings.font_size = (self.settings.font_size + 1.5).min(26.0);
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT) + pt(4.0) + pt(20.0);

                        let margins = [36, 72, 108];
                        let m_btn_w = (dw - pt(28.0)) / 3;
                        for (i, pad) in margins.iter().enumerate() {
                            let bx = dx + pt(14.0) + i as i32 * m_btn_w;
                            let br = Rect::new(bx, py, m_btn_w - pt(4.0), pt(BTN_H_PT));
                            if br.contains(tx, ty) {
                                self.settings.margin_pad = *pad;
                                return Action::Redraw;
                            }
                        }
                        py += pt(ROW_H_PT) + pt(8.0);

                        let h_btn = Rect::new(dx + dw - pt(70.0), py, pt(58.0), pt(BTN_H_PT));
                        if h_btn.contains(tx, ty) {
                            self.settings.show_header = !self.settings.show_header;
                            return Action::Redraw;
                        }
                    }

                    1 => {
                        // TAB 1: Split & Crop
                        let presets = [
                            SplitPreset::FitPage,
                            SplitPreset::Horizontal2,
                            SplitPreset::Horizontal3,
                            SplitPreset::Vertical2,
                            SplitPreset::Grid4,
                        ];
                        for preset in presets {
                            let btn_r = Rect::new(dx + pt(10.0), py, dw - pt(20.0), pt(20.0));
                            if btn_r.contains(tx, ty) {
                                self.settings.split = SplitConfig::for_preset(preset);
                                return Action::Redraw;
                            }
                            py += pt(22.0);
                        }

                        py += pt(4.0);

                        let rot_btn = Rect::new(dx + pt(10.0), py, dw - pt(20.0), pt(22.0));
                        if rot_btn.contains(tx, ty) {
                            self.settings.split.rotation =
                                SplitConfig::next_rotation(self.settings.split.rotation);
                            return Action::Redraw;
                        }
                        py += pt(26.0);


                        let m_minus = Rect::new(dx + dw - pt(96.0), py, pt(22.0), pt(BTN_H_PT));
                        let m_plus = Rect::new(dx + dw - pt(70.0), py, pt(22.0), pt(BTN_H_PT));
                        let m_auto = Rect::new(dx + dw - pt(44.0), py, pt(34.0), pt(BTN_H_PT));

                        if m_minus.contains(tx, ty) {
                            self.settings.split.margin_left = (self.settings.split.margin_left - 0.02).max(0.0);
                            self.settings.split.margin_right = (self.settings.split.margin_right - 0.02).max(0.0);
                            self.settings.split.margin_top = (self.settings.split.margin_top - 0.02).max(0.0);
                            self.settings.split.margin_bottom = (self.settings.split.margin_bottom - 0.02).max(0.0);
                            return Action::Redraw;
                        }
                        if m_plus.contains(tx, ty) {
                            self.settings.split.margin_left = (self.settings.split.margin_left + 0.02).min(0.35);
                            self.settings.split.margin_right = (self.settings.split.margin_right + 0.02).min(0.35);
                            self.settings.split.margin_top = (self.settings.split.margin_top + 0.02).min(0.35);
                            self.settings.split.margin_bottom = (self.settings.split.margin_bottom + 0.02).min(0.35);
                            return Action::Redraw;
                        }
                        if m_auto.contains(tx, ty) {
                            if let Some((samples, width, height, stride)) = &self.page_samples {
                                let (ml, mt, mr, mb) = detect_margins(samples, *width, *height, *stride, 240);
                                self.settings.split.margin_left = ml;
                                self.settings.split.margin_top = mt;
                                self.settings.split.margin_right = mr;
                                self.settings.split.margin_bottom = mb;
                                return Action::Redraw;
                            }
                        }
                        py += pt(ROW_H_PT);

                        let ov_minus = Rect::new(dx + dw - pt(66.0), py, pt(26.0), pt(BTN_H_PT));
                        let ov_plus = Rect::new(dx + dw - pt(36.0), py, pt(26.0), pt(BTN_H_PT));
                        if ov_minus.contains(tx, ty) {
                            self.settings.split.overlap = (self.settings.split.overlap - 0.02).max(0.0);
                            return Action::Redraw;
                        }
                        if ov_plus.contains(tx, ty) {
                            self.settings.split.overlap = (self.settings.split.overlap + 0.02).min(0.30);
                            return Action::Redraw;
                        }

                    }

                    2 => {
                        // TAB 2: Display & Contrast
                        py += pt(20.0);
                        let contrasts = [
                            ContrastMode::Normal,
                            ContrastMode::BoldText,
                            ContrastMode::HighContrast,
                            ContrastMode::ScanClean,
                        ];
                        let c_btn_w = (dw - pt(28.0)) / 4;
                        for (i, c_mode) in contrasts.iter().enumerate() {
                            let bx = dx + pt(14.0) + i as i32 * c_btn_w;
                            let br = Rect::new(bx, py, c_btn_w - pt(3.0), pt(BTN_H_PT));
                            if br.contains(tx, ty) {
                                self.settings.contrast = *c_mode;
                                return Action::Redraw;
                            }
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

                        let cut_minus = Rect::new(dx + dw - pt(66.0), py, pt(26.0), pt(BTN_H_PT));
                        let cut_plus = Rect::new(dx + dw - pt(36.0), py, pt(26.0), pt(BTN_H_PT));
                        if cut_minus.contains(tx, ty) {
                            self.settings.white_cutoff = match self.settings.white_cutoff {
                                0 => 0,
                                245 => 0,
                                235 => 245,
                                _ => 0,
                            };
                            return Action::Redraw;
                        }
                        if cut_plus.contains(tx, ty) {
                            self.settings.white_cutoff = match self.settings.white_cutoff {
                                0 => 245,
                                245 => 235,
                                _ => 235,
                            };
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT);

                        let inv_btn = Rect::new(dx + dw - pt(66.0), py, pt(56.0), pt(BTN_H_PT));
                        if inv_btn.contains(tx, ty) {
                            self.settings.invert = !self.settings.invert;
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT);

                        let ref_btn = Rect::new(dx + dw - pt(76.0), py, pt(66.0), pt(BTN_H_PT));
                        if ref_btn.contains(tx, ty) {
                            self.settings.refresh_interval = match self.settings.refresh_interval {
                                10 => 20,
                                20 => 5,
                                5 => 0,
                                _ => 10,
                            };
                            return Action::Redraw;
                        }
                    }

                    3 => {
                        // TAB 3: Vocab & Word Wise
                        let mut prof = crate::vocab::VocabProfile::load();

                        py += pt(20.0);
                        let styles = [
                            crate::vocab::AnnotationStyle::Interlinear,
                            crate::vocab::AnnotationStyle::Margin,
                            crate::vocab::AnnotationStyle::DottedUnderline,
                            crate::vocab::AnnotationStyle::Off,
                        ];
                        let s_btn_w = (dw - pt(28.0)) / 4;
                        for (i, s_mode) in styles.iter().enumerate() {
                            let bx = dx + pt(14.0) + i as i32 * s_btn_w;
                            let br = Rect::new(bx, py, s_btn_w - pt(3.0), pt(BTN_H_PT));
                            if br.contains(tx, ty) {
                                prof.style = *s_mode;
                                prof.save();
                                return Action::Redraw;
                            }
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

                        let l_minus = Rect::new(dx + dw - pt(66.0), py, pt(26.0), pt(BTN_H_PT));
                        let l_plus = Rect::new(dx + dw - pt(36.0), py, pt(26.0), pt(BTN_H_PT));
                        if l_minus.contains(tx, ty) {
                            prof.user_level = prof.user_level.saturating_sub(5).max(10);
                            prof.save();
                            return Action::Redraw;
                        }
                        if l_plus.contains(tx, ty) {
                            prof.user_level = (prof.user_level + 5).min(95);
                            prof.save();
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

                        let counts = [1, 2, 3, 5];
                        let c_btn_w = (dw - pt(28.0)) / 4;
                        for (i, cnt) in counts.iter().enumerate() {
                            let bx = dx + pt(14.0) + i as i32 * c_btn_w;
                            let br = Rect::new(bx, py, c_btn_w - pt(3.0), pt(BTN_H_PT));
                            if br.contains(tx, ty) {
                                prof.max_per_page = *cnt;
                                prof.save();
                                return Action::Redraw;
                            }
                        }
                    }

                    _ => {}
                }

                Action::Keep
            }
            _ => Action::Keep,
        }
    }


    fn default_edges(&self) -> bool {
        false
    }
}
