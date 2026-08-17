//! Interactive Page Split & Crop configuration dialog overlay.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

use crate::split::{detect_margins, SplitConfig, SplitPreset};

pub struct CropDialogScreen {
    pub config: SplitConfig,
    /// Callback when user modifies config or closes dialog
    on_change: Box<dyn FnMut(SplitConfig) -> Action>,
    /// Optional preview pixmap samples for auto-detection
    page_samples: Option<(Vec<u8>, usize, usize, usize)>,
    dims: (i32, i32),
}

impl CropDialogScreen {
    pub fn new(
        config: SplitConfig,
        page_samples: Option<(Vec<u8>, usize, usize, usize)>,
        on_change: impl FnMut(SplitConfig) -> Action + 'static,
    ) -> Self {
        Self {
            config,
            on_change: Box::new(on_change),
            page_samples,
            dims: (1236, 1648),
        }
    }
}

const PAD_PT: f32 = 20.0;
const DIALOG_W_PT: f32 = 250.0;
const ROW_H_PT: f32 = 28.0;
const BTN_H_PT: f32 = 24.0;

impl Screen for CropDialogScreen {
    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // Semi-opaque / clear background dialog
        let dw = pt(DIALOG_W_PT).min(w - pt(2.0 * PAD_PT));
        let dh = pt(320.0);
        let dx = (w - dw) / 2;
        let dy = (h - dh) / 2;

        // Background box with thick border
        let rect = Rect::new(dx, dy, dw, dh);
        p.rect(rect, 255);
        p.rect_outline_t(rect, 3, 0);

        // Header
        p.rect(Rect::new(dx, dy, dw, pt(32.0)), 0);
        p.text_center_in(dx, dx + dw, dy + pt(21.0), 10.0, 255, "PAGE SPLIT & CROP");

        let mut y = dy + pt(42.0);

        // Presets list
        let presets = [
            (SplitPreset::FitPage, "1. Fit Page (Single)"),
            (SplitPreset::Horizontal2, "2. 2-Split Landscape (Top/Bottom)"),
            (SplitPreset::Vertical2, "3. 2-Column Portrait (Left/Right)"),
            (SplitPreset::Grid4, "4. 4-Grid Landscape (2x2)"),
        ];

        for (preset, label) in presets {
            let active = self.config.preset == preset;
            let btn_r = Rect::new(dx + pt(10.0), y, dw - pt(20.0), pt(BTN_H_PT));
            if active {
                p.rect(btn_r, 0);
                p.text(dx + pt(18.0), y + pt(16.0), 8.5, 255, label);
                p.text_right(dx + dw - pt(18.0), y + pt(16.0), 8.5, 255, "[ACTIVE]");
            } else {
                p.rect_outline_t(btn_r, 2, 100);
                p.text(dx + pt(18.0), y + pt(16.0), 8.5, 0, label);
            }
            y += pt(ROW_H_PT);
        }

        y += pt(6.0);
        p.hline_t(y, dx + pt(10.0), dx + dw - pt(10.0), 2, 200);
        y += pt(10.0);

        // Orientation row
        let rot_label = match self.config.rotation {
            0 => "Rotation: Portrait (0°)",
            90 => "Rotation: Landscape CCW (90°)",
            270 => "Rotation: Landscape CW (270°)",
            _ => "Rotation: Portrait (0°)",
        };
        let rot_btn = Rect::new(dx + pt(10.0), y, dw - pt(20.0), pt(BTN_H_PT));
        p.rect_outline_t(rot_btn, 2, 0);
        p.text_center_in(rot_btn.x, rot_btn.x + rot_btn.w, y + pt(16.0), 8.5, 0, rot_label);
        y += pt(ROW_H_PT);

        // Margins & Overlap controls
        let margin_pct = (self.config.margin_left * 100.0).round() as i32;
        let ov_pct = (self.config.overlap * 100.0).round() as i32;

        // Margin adjust
        let m_label = format!("Crop Margin: {}%", margin_pct);
        p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, &m_label);

        let m_minus = Rect::new(dx + dw - pt(100.0), y, pt(24.0), pt(BTN_H_PT));
        let m_plus = Rect::new(dx + dw - pt(70.0), y, pt(24.0), pt(BTN_H_PT));
        let m_auto = Rect::new(dx + dw - pt(40.0), y, pt(30.0), pt(BTN_H_PT));

        p.rect_outline_t(m_minus, 2, 0);
        p.text_center_in(m_minus.x, m_minus.x + m_minus.w, y + pt(16.0), 9.0, 0, "-");

        p.rect_outline_t(m_plus, 2, 0);
        p.text_center_in(m_plus.x, m_plus.x + m_plus.w, y + pt(16.0), 9.0, 0, "+");

        p.rect_outline_t(m_auto, 2, 0);
        p.text_center_in(m_auto.x, m_auto.x + m_auto.w, y + pt(16.0), 7.0, 0, "AUTO");

        y += pt(ROW_H_PT);

        // Overlap adjust
        let ov_label = format!("Split Overlap: {}%", ov_pct);
        p.text(dx + pt(14.0), y + pt(16.0), 8.0, 0, &ov_label);

        let ov_minus = Rect::new(dx + dw - pt(70.0), y, pt(24.0), pt(BTN_H_PT));
        let ov_plus = Rect::new(dx + dw - pt(40.0), y, pt(24.0), pt(BTN_H_PT));

        p.rect_outline_t(ov_minus, 2, 0);
        p.text_center_in(ov_minus.x, ov_minus.x + ov_minus.w, y + pt(16.0), 9.0, 0, "-");

        p.rect_outline_t(ov_plus, 2, 0);
        p.text_center_in(ov_plus.x, ov_plus.x + ov_plus.w, y + pt(16.0), 9.0, 0, "+");

        y += pt(ROW_H_PT) + pt(4.0);

        // Apply Button
        let apply_r = Rect::new(dx + pt(10.0), y, dw - pt(20.0), pt(28.0));
        p.rect(apply_r, 0);
        p.text_center_in(apply_r.x, apply_r.x + apply_r.w, y + pt(18.0), 9.5, 255, "APPLY & READ");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let dw = pt(DIALOG_W_PT).min(w - pt(2.0 * PAD_PT));
        let dh = pt(320.0);
        let dx = (w - dw) / 2;
        let dy = (h - dh) / 2;

        match g {
            Gesture::Tap { x, y } => {
                let (tx, ty) = (x as i32, y as i32);
                let dialog_rect = Rect::new(dx, dy, dw, dh);
                if !dialog_rect.contains(tx, ty) {
                    // Tap outside -> Apply & Close
                    return (self.on_change)(self.config);
                }

                // Check presets
                let mut py = dy + pt(42.0);
                let presets = [
                    SplitPreset::FitPage,
                    SplitPreset::Horizontal2,
                    SplitPreset::Vertical2,
                    SplitPreset::Grid4,
                ];

                for preset in presets {
                    let btn_r = Rect::new(dx + pt(10.0), py, dw - pt(20.0), pt(BTN_H_PT));
                    if btn_r.contains(tx, ty) {
                        self.config = SplitConfig::for_preset(preset);
                        return Action::Redraw;
                    }
                    py += pt(ROW_H_PT);
                }

                py += pt(6.0) + pt(10.0);

                // Rotation button
                let rot_btn = Rect::new(dx + pt(10.0), py, dw - pt(20.0), pt(BTN_H_PT));
                if rot_btn.contains(tx, ty) {
                    self.config.rotation = match self.config.rotation {
                        0 => 270,
                        270 => 90,
                        _ => 0,
                    };
                    return Action::Redraw;
                }
                py += pt(ROW_H_PT);

                // Margins: minus, plus, auto
                let m_minus = Rect::new(dx + dw - pt(100.0), py, pt(24.0), pt(BTN_H_PT));
                let m_plus = Rect::new(dx + dw - pt(70.0), py, pt(24.0), pt(BTN_H_PT));
                let m_auto = Rect::new(dx + dw - pt(40.0), py, pt(30.0), pt(BTN_H_PT));

                if m_minus.contains(tx, ty) {
                    self.config.margin_left = (self.config.margin_left - 0.02).max(0.0);
                    self.config.margin_right = (self.config.margin_right - 0.02).max(0.0);
                    self.config.margin_top = (self.config.margin_top - 0.02).max(0.0);
                    self.config.margin_bottom = (self.config.margin_bottom - 0.02).max(0.0);
                    return Action::Redraw;
                }
                if m_plus.contains(tx, ty) {
                    self.config.margin_left = (self.config.margin_left + 0.02).min(0.35);
                    self.config.margin_right = (self.config.margin_right + 0.02).min(0.35);
                    self.config.margin_top = (self.config.margin_top + 0.02).min(0.35);
                    self.config.margin_bottom = (self.config.margin_bottom + 0.02).min(0.35);
                    return Action::Redraw;
                }
                if m_auto.contains(tx, ty) {
                    if let Some((samples, width, height, stride)) = &self.page_samples {
                        let (ml, mt, mr, mb) = detect_margins(samples, *width, *height, *stride, 240);
                        self.config.margin_left = ml;
                        self.config.margin_top = mt;
                        self.config.margin_right = mr;
                        self.config.margin_bottom = mb;
                        return Action::Redraw;
                    }
                }
                py += pt(ROW_H_PT);

                // Overlap: minus, plus
                let ov_minus = Rect::new(dx + dw - pt(70.0), py, pt(24.0), pt(BTN_H_PT));
                let ov_plus = Rect::new(dx + dw - pt(40.0), py, pt(24.0), pt(BTN_H_PT));

                if ov_minus.contains(tx, ty) {
                    self.config.overlap = (self.config.overlap - 0.02).max(0.0);
                    return Action::Redraw;
                }
                if ov_plus.contains(tx, ty) {
                    self.config.overlap = (self.config.overlap + 0.02).min(0.20);
                    return Action::Redraw;
                }
                py += pt(ROW_H_PT) + pt(4.0);

                // Apply button
                let apply_r = Rect::new(dx + pt(10.0), py, dw - pt(20.0), pt(28.0));
                if apply_r.contains(tx, ty) {
                    return (self.on_change)(self.config);
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
