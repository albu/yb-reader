//! All Settings — deep tuning modal for reader display & layout geometry.
//! Tabs: [1. Display & Contrast]  [2. Layout & Crop]

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::Orientation;
use yui::screen::{Action, Screen};

use crate::split::{detect_margins, ContrastMode, ReaderSettings, SplitConfig, SplitPreset};

pub type SettingsDialog = ReaderSettingsDialog;

pub struct ReaderSettingsDialog {
    pub settings: ReaderSettings,
    pub tab: usize, // 0 = Display & Contrast, 1 = Layout & Crop
    pub is_pdf: bool,
    on_change: Box<dyn FnMut(ReaderSettings) -> Action>,
    page_samples: Option<(Vec<u8>, usize, usize, usize)>,
    dims: (i32, i32),
}

impl ReaderSettingsDialog {
    pub fn new_legacy(
        book: String,
        page_no: usize,
        sub_idx: usize,
        total: usize,
        settings: ReaderSettings,
        _page_gray: Option<Vec<u8>>,
    ) -> Self {
        Self::new(settings, true, None, move |new_settings| {
            crate::dialogs::record_sub(&book, page_no, sub_idx, total, new_settings);
            Action::Pop
        })
    }

    pub fn new(
        settings: ReaderSettings,
        is_pdf: bool,
        page_samples: Option<(Vec<u8>, usize, usize, usize)>,
        on_change: impl FnMut(ReaderSettings) -> Action + 'static,
    ) -> Self {
        Self {
            settings,
            tab: 0,
            is_pdf,
            on_change: Box::new(on_change),
            page_samples,
            dims: (1236, 1648),
        }
    }
}

const PAD_PT: f32 = 16.0;
const DIALOG_W_PT: f32 = 265.0;
const DIALOG_H_PT: f32 = 280.0;
const ROW_H_PT: f32 = 28.0;
const BTN_H_PT: f32 = 22.0;

impl Screen for ReaderSettingsDialog {
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
        p.rect_outline_t(rect, 2, 0);

        // Header: 2 Tabs [ 1. Display & Contrast | 2. Layout & Crop ]
        let tab_h = pt(30.0);
        let tab_w = dw / 2;
        let tabs = ["1. Display & Contrast", "2. Layout & Crop"];

        for (i, label) in tabs.iter().enumerate() {
            let tx0 = dx + i as i32 * tab_w;
            let tx1 = if i == 1 { dx + dw } else { tx0 + tab_w };
            let tr = Rect::new(tx0, dy, tx1 - tx0, tab_h);

            if self.tab == i {
                p.rect(tr, 0);
                p.text_center_in(tx0, tx1, dy + pt(19.0), 8.0, 255, label);
            } else {
                p.rect_outline_t(tr, 1, 140);
                p.text_center_in(tx0, tx1, dy + pt(19.0), 8.0, 100, label);
            }
        }

        let mut y = dy + pt(42.0);

        match self.tab {
            0 => {
                // --- TAB 0: DISPLAY & CONTRAST ---

                // 1. Contrast Curve
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "CONTRAST CURVE");
                y += pt(18.0);
                let contrasts = [
                    (ContrastMode::Normal, "Normal"),
                    (ContrastMode::BoldText, "Bold"),
                    (ContrastMode::HighContrast, "Dark"),
                    (ContrastMode::ScanClean, "Max"),
                ];
                let c_btn_w = (dw - pt(28.0)) / 4;
                for (i, (c_mode, c_lbl)) in contrasts.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * c_btn_w;
                    let br = Rect::new(bx, y, c_btn_w - pt(3.0), pt(BTN_H_PT));
                    if self.settings.contrast == *c_mode {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, c_lbl);
                    } else {
                        p.rect_outline_t(br, 1, 120);
                        p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, c_lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(4.0);

                // 2. White Cutoff Snap (Paper Cleanup)
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "WHITE SNAP (PAPER CLEANUP)");
                let cut_str = match self.settings.white_cutoff {
                    0 => "Off",
                    245 => "245 (Subtle)",
                    235 => "235 (Aggressive)",
                    v => &format!("{}", v),
                };
                let cut_btn = Rect::new(dx + dw - pt(100.0), y, pt(86.0), pt(BTN_H_PT));
                p.rect_outline_t(cut_btn, 1, 0);
                p.text_center_in(cut_btn.x, cut_btn.x + cut_btn.w, y + pt(15.0), 7.5, 0, cut_str);
                y += pt(ROW_H_PT) + pt(4.0);

                // 3. Inverted / Night Mode
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "INVERT NIGHT READING");
                let inv_str = if self.settings.invert { "ON" } else { "OFF" };
                let inv_btn = Rect::new(dx + dw - pt(70.0), y, pt(56.0), pt(BTN_H_PT));
                if self.settings.invert {
                    p.rect(inv_btn, 0);
                    p.text_center_in(inv_btn.x, inv_btn.x + inv_btn.w, y + pt(15.0), 8.0, 255, inv_str);
                } else {
                    p.rect_outline_t(inv_btn, 1, 100);
                    p.text_center_in(inv_btn.x, inv_btn.x + inv_btn.w, y + pt(15.0), 8.0, 0, inv_str);
                }
                y += pt(ROW_H_PT) + pt(4.0);

                // 4. E-Ink Full Refresh Interval
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "E-INK FLASH INTERVAL");
                let ref_str = match self.settings.refresh_interval {
                    0 => "Manual",
                    n => &format!("Every {} pgs", n),
                };
                let ref_btn = Rect::new(dx + dw - pt(90.0), y, pt(76.0), pt(BTN_H_PT));
                p.rect_outline_t(ref_btn, 1, 0);
                p.text_center_in(ref_btn.x, ref_btn.x + ref_btn.w, y + pt(15.0), 7.5, 0, ref_str);
            }

            1 => {
                // --- TAB 1: LAYOUT & SPLIT GEOMETRY ---

                // 1. Split Presets
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "SPLIT PRESET");
                y += pt(18.0);
                let presets = [
                    (SplitPreset::FitPage, "Fit"),
                    (SplitPreset::Horizontal2, "2-Split"),
                    (SplitPreset::Horizontal3, "3-Split"),
                    (SplitPreset::Grid4, "Grid"),
                ];
                let s_btn_w = (dw - pt(28.0)) / 4;
                for (i, (preset, s_lbl)) in presets.iter().enumerate() {
                    let bx = dx + pt(14.0) + i as i32 * s_btn_w;
                    let br = Rect::new(bx, y, s_btn_w - pt(3.0), pt(BTN_H_PT));
                    if self.settings.split.preset == *preset {
                        p.rect(br, 0);
                        p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 255, s_lbl);
                    } else {
                        p.rect_outline_t(br, 1, 120);
                        p.text_center_in(br.x, br.x + br.w, y + pt(15.0), 7.5, 0, s_lbl);
                    }
                }
                y += pt(ROW_H_PT) + pt(4.0);

                // 2. Split Box Overlap %
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "SPLIT OVERLAP");
                let ov_pct = (self.settings.split.overlap * 100.0).round() as i32;
                let ov_str = format!("{}%", ov_pct);
                p.text_center_in(dx + pt(100.0), dx + pt(150.0), y + pt(15.0), 8.0, 0, &ov_str);

                let ov_minus = Rect::new(dx + dw - pt(66.0), y, pt(26.0), pt(BTN_H_PT));
                let ov_plus = Rect::new(dx + dw - pt(36.0), y, pt(26.0), pt(BTN_H_PT));
                p.rect_outline_t(ov_minus, 1, 0);
                p.text_center_in(ov_minus.x, ov_minus.x + ov_minus.w, y + pt(15.0), 8.5, 0, "-");
                p.rect_outline_t(ov_plus, 1, 0);
                p.text_center_in(ov_plus.x, ov_plus.x + ov_plus.w, y + pt(15.0), 8.5, 0, "+");
                y += pt(ROW_H_PT) + pt(4.0);

                // 3. Margin Crop Nudge & Auto-Detect
                p.text(dx + pt(14.0), y + pt(15.0), 8.5, 0, "MARGIN CROP NUDGE");
                let m_minus = Rect::new(dx + dw - pt(96.0), y, pt(22.0), pt(BTN_H_PT));
                let m_plus = Rect::new(dx + dw - pt(70.0), y, pt(22.0), pt(BTN_H_PT));
                let m_auto = Rect::new(dx + dw - pt(44.0), y, pt(34.0), pt(BTN_H_PT));
                p.rect_outline_t(m_minus, 1, 0);
                p.text_center_in(m_minus.x, m_minus.x + m_minus.w, y + pt(15.0), 8.5, 0, "-");
                p.rect_outline_t(m_plus, 1, 0);
                p.text_center_in(m_plus.x, m_plus.x + m_plus.w, y + pt(15.0), 8.5, 0, "+");
                p.rect_outline_t(m_auto, 1, 0);
                p.text_center_in(m_auto.x, m_auto.x + m_auto.w, y + pt(15.0), 7.0, 0, "Auto");
            }

            _ => {}
        }

        // Apply & Done Button
        let apply_y = dy + dh - pt(32.0);
        let apply_r = Rect::new(dx + pt(10.0), apply_y, dw - pt(20.0), pt(24.0));
        p.rect(apply_r, 0);
        p.text_center_in(apply_r.x, apply_r.x + apply_r.w, apply_y + pt(16.0), 8.5, 255, "APPLY & CLOSE");
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

                // Tab Bar
                let tab_h = pt(30.0);
                let tab_w = dw / 2;
                if ty >= dy && ty < dy + tab_h {
                    let clicked_tab = ((tx - dx) / tab_w).clamp(0, 1) as usize;
                    self.tab = clicked_tab;
                    return Action::Redraw;
                }

                // Apply button tap
                let apply_y = dy + dh - pt(32.0);
                let apply_r = Rect::new(dx + pt(10.0), apply_y, dw - pt(20.0), pt(24.0));
                if apply_r.contains(tx, ty) {
                    return (self.on_change)(self.settings);
                }

                let mut py = dy + pt(42.0);

                match self.tab {
                    0 => {
                        // TAB 0: Display & Contrast
                        py += pt(18.0);
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

                        let cut_btn = Rect::new(dx + dw - pt(100.0), py, pt(86.0), pt(BTN_H_PT));
                        if cut_btn.contains(tx, ty) {
                            self.settings.white_cutoff = match self.settings.white_cutoff {
                                0 => 245,
                                245 => 235,
                                _ => 0,
                            };
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

                        let inv_btn = Rect::new(dx + dw - pt(70.0), py, pt(56.0), pt(BTN_H_PT));
                        if inv_btn.contains(tx, ty) {
                            self.settings.invert = !self.settings.invert;
                            return Action::Redraw;
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

                        let ref_btn = Rect::new(dx + dw - pt(90.0), py, pt(76.0), pt(BTN_H_PT));
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

                    1 => {
                        // TAB 1: Layout & Split Geometry
                        py += pt(18.0);
                        let presets = [
                            SplitPreset::FitPage,
                            SplitPreset::Horizontal2,
                            SplitPreset::Horizontal3,
                            SplitPreset::Grid4,
                        ];
                        let s_btn_w = (dw - pt(28.0)) / 4;
                        for (i, preset) in presets.iter().enumerate() {
                            let bx = dx + pt(14.0) + i as i32 * s_btn_w;
                            let br = Rect::new(bx, py, s_btn_w - pt(3.0), pt(BTN_H_PT));
                            if br.contains(tx, ty) {
                                self.settings.split = SplitConfig::for_preset(*preset);
                                return Action::Redraw;
                            }
                        }
                        py += pt(ROW_H_PT) + pt(4.0);

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
                        py += pt(ROW_H_PT) + pt(4.0);

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
