//! Interactive visual margin crop screen for PDF / fixed-layout books.
//! Displays the full uncropped page with 4 draggable dashed guide lines,
//! corner handles, and precision nudging controls.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::Orientation;
use yui::screen::{Action, Screen};

use crate::split::ReaderSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveEdge {
    Top,
    Bottom,
    Left,
    Right,
    All,
}

pub struct CropDialog {
    pub book: String,
    pub page_no: usize,
    pub sub_idx: usize,
    pub total: usize,
    pub settings: ReaderSettings,
    pub doc: Option<std::rc::Rc<mupdf::Document>>,
    pub page_gray: Option<Vec<u8>>,
    pub active_edge: ActiveEdge,
    drag_edge: Option<ActiveEdge>,
    dims: (i32, i32),
}

impl CropDialog {
    pub fn new(
        book: String,
        page_no: usize,
        sub_idx: usize,
        total: usize,
        settings: ReaderSettings,
        doc: Option<std::rc::Rc<mupdf::Document>>,
    ) -> Self {
        Self {
            book,
            page_no,
            sub_idx,
            total,
            settings,
            doc,
            page_gray: None,
            active_edge: ActiveEdge::Top,
            drag_edge: None,
            dims: (1236, 1648),
        }
    }

    fn apply_crop(&mut self) -> Action {
        crate::dialogs::record_sub(
            &self.book,
            self.page_no,
            self.sub_idx,
            self.total,
            self.settings,
        );
        Action::Pop
    }

    fn nudge(&mut self, delta: f32) {
        let s = &mut self.settings.split;
        match self.active_edge {
            ActiveEdge::Top => s.margin_top = (s.margin_top + delta).clamp(0.0, 0.40),
            ActiveEdge::Bottom => s.margin_bottom = (s.margin_bottom + delta).clamp(0.0, 0.40),
            ActiveEdge::Left => s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40),
            ActiveEdge::Right => s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40),
            ActiveEdge::All => {
                s.margin_top = (s.margin_top + delta).clamp(0.0, 0.40);
                s.margin_bottom = (s.margin_bottom + delta).clamp(0.0, 0.40);
                s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40);
                s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40);
            }
        }
    }
}

impl Screen for CropDialog {
    /// Crop adjustment always happens in Portrait mode for clean, unrotated 1:1 page editing.
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // Render full uncropped portrait page if not yet cached
        if self.page_gray.is_none() {
            if let Some(doc) = &self.doc {
                let mut fit_s = self.settings;
                fit_s.show_header = false;
                fit_s.split.preset = crate::split::SplitPreset::FitPage;
                fit_s.split.rotation = 0;
                fit_s.split.margin_left = 0.0;
                fit_s.split.margin_top = 0.0;
                fit_s.split.margin_right = 0.0;
                fit_s.split.margin_bottom = 0.0;
                self.page_gray = crate::render::render_page(doc.as_ref(), self.page_no, 0, &fit_s, w as u32, h as u32);
            }
        }

        // 1. Draw the uncropped base page
        if let Some(gray) = &self.page_gray {
            p.blit_gray(0, 0, w, h, gray, w as usize);
        } else {
            p.clear(255);
        }

        let (pw, ph) = if let Some(doc) = &self.doc {
            if let Ok(page) = doc.load_page(self.page_no as i32) {
                if let Ok(b) = page.bounds() {
                    (b.x1 - b.x0, b.y1 - b.y0)
                } else {
                    (w as f32, h as f32)
                }
            } else {
                (w as f32, h as f32)
            }
        } else {
            (w as f32, h as f32)
        };

        let zoom = (w as f32 / pw).min(h as f32 / ph);
        let rw = (pw * zoom).round() as i32;
        let rh = (ph * zoom).round() as i32;
        let page_ox = (w - rw) / 2;
        let page_oy = (h - rh) / 2;

        let s = &self.settings.split;
        let x0 = page_ox + (s.margin_left * rw as f32).round() as i32;
        let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
        let x1 = page_ox + ((1.0 - s.margin_right) * rw as f32).round() as i32;
        let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

        // 2. Dim outer excluded margins with a clean cross-hatch stipple pattern
        let dim_color = 160;
        // Top outer rect
        if y0 > 0 {
            for y in (0..y0).step_by(4) {
                p.hline_t(y, 0, w, 1, dim_color);
            }
        }
        // Bottom outer rect
        if y1 < h {
            for y in (y1..h).step_by(4) {
                p.hline_t(y, 0, w, 1, dim_color);
            }
        }
        // Left outer rect
        if x0 > 0 {
            for y in (y0..y1).step_by(4) {
                p.hline_t(y, 0, x0, 1, dim_color);
            }
        }
        // Right outer rect
        if x1 < w {
            for y in (y0..y1).step_by(4) {
                p.hline_t(y, x1, w, 1, dim_color);
            }
        }

        // 3. High-contrast dashed margin guide lines
        let dash = pt(5.0);
        let gap = pt(4.0);

        // Top horizontal dashed guide
        let mut x = 0;
        while x < w {
            let seg_w = dash.min(w - x);
            p.rect(Rect::new(x, y0 - 1, seg_w, 2), 0);
            p.rect(Rect::new(x, y0 + 1, seg_w, 1), 255);
            x += dash + gap;
        }

        // Bottom horizontal dashed guide
        let mut x = 0;
        while x < w {
            let seg_w = dash.min(w - x);
            p.rect(Rect::new(x, y1 - 1, seg_w, 2), 0);
            p.rect(Rect::new(x, y1 + 1, seg_w, 1), 255);
            x += dash + gap;
        }

        // Left vertical dashed guide
        let mut y = 0;
        while y < h {
            let seg_h = dash.min(h - y);
            p.rect(Rect::new(x0 - 1, y, 2, seg_h), 0);
            p.rect(Rect::new(x0 + 1, y, 1, seg_h), 255);
            y += dash + gap;
        }

        // Right vertical dashed guide
        let mut y = 0;
        while y < h {
            let seg_h = dash.min(h - y);
            p.rect(Rect::new(x1 - 1, y, 2, seg_h), 0);
            p.rect(Rect::new(x1 + 1, y, 1, seg_h), 255);
            y += dash + gap;
        }

        // 4. Bold corner brackets ⌜ ⌝ ⌞ ⌟ around the active crop box
        let clen = pt(14.0);
        let cth = 3;
        // Top-Left ⌜
        p.rect(Rect::new(x0, y0, clen, cth), 0);
        p.rect(Rect::new(x0, y0, cth, clen), 0);
        // Top-Right ⌝
        p.rect(Rect::new(x1 - clen, y0, clen, cth), 0);
        p.rect(Rect::new(x1 - cth, y0, cth, clen), 0);
        // Bottom-Left ⌞
        p.rect(Rect::new(x0, y1 - cth, clen, cth), 0);
        p.rect(Rect::new(x0, y1 - clen, cth, clen), 0);
        // Bottom-Right ⌟
        p.rect(Rect::new(x1 - clen, y1 - cth, clen, cth), 0);
        p.rect(Rect::new(x1 - cth, y1 - clen, cth, clen), 0);

        // 5. Floating Bottom Control Palette (pill)
        let bar_h = pt(42.0);
        let bar_w = (w - pt(32.0)).min(pt(280.0));
        let bar_x = (w - bar_w) / 2;
        let bar_y = h - bar_h - pt(16.0);

        // Drop shadow + white rounded pill card
        p.rect(Rect::new(bar_x + 2, bar_y + 2, bar_w, bar_h), 120);
        p.rect(Rect::new(bar_x, bar_y, bar_w, bar_h), 255);
        p.rect_outline_t(Rect::new(bar_x, bar_y, bar_w, bar_h), 2, 0);

        // Row 1: Edge selector pills [ Top | Bottom | Left | Right | All ]
        let edges = [
            (ActiveEdge::Top, "Top"),
            (ActiveEdge::Bottom, "Btm"),
            (ActiveEdge::Left, "Left"),
            (ActiveEdge::Right, "Right"),
            (ActiveEdge::All, "All"),
        ];
        let edge_btn_w = (bar_w - pt(16.0)) / 5;
        let edge_btn_h = pt(16.0);
        let edge_y = bar_y + pt(4.0);

        for (i, (edge, lbl)) in edges.iter().enumerate() {
            let bx = bar_x + pt(8.0) + i as i32 * edge_btn_w;
            let br = Rect::new(bx, edge_y, edge_btn_w - pt(2.0), edge_btn_h);
            if self.active_edge == *edge {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, edge_y + pt(11.0), 6.5, 255, lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, edge_y + pt(11.0), 6.5, 0, lbl);
            }
        }

        // Row 2: Precision Nudge [-1%] [+1%] | [Reset] | [Done]
        let r2_y = bar_y + pt(23.0);
        let btn_h = pt(15.0);

        // Minus button
        let m_btn = Rect::new(bar_x + pt(8.0), r2_y, pt(24.0), btn_h);
        p.rect_outline_t(m_btn, 1, 0);
        p.text_center_in(m_btn.x, m_btn.x + m_btn.w, r2_y + pt(10.5), 8.0, 0, "-");

        // Percentage display
        let pct = match self.active_edge {
            ActiveEdge::Top => (s.margin_top * 100.0).round() as i32,
            ActiveEdge::Bottom => (s.margin_bottom * 100.0).round() as i32,
            ActiveEdge::Left => (s.margin_left * 100.0).round() as i32,
            ActiveEdge::Right => (s.margin_right * 100.0).round() as i32,
            ActiveEdge::All => (s.margin_top * 100.0).round() as i32,
        };
        let pct_str = format!("{}%", pct);
        p.text_center_in(bar_x + pt(34.0), bar_x + pt(66.0), r2_y + pt(10.5), 7.0, 0, &pct_str);

        // Plus button
        let p_btn = Rect::new(bar_x + pt(68.0), r2_y, pt(24.0), btn_h);
        p.rect_outline_t(p_btn, 1, 0);
        p.text_center_in(p_btn.x, p_btn.x + p_btn.w, r2_y + pt(10.5), 8.0, 0, "+");

        // Reset button
        let res_btn = Rect::new(bar_x + pt(100.0), r2_y, pt(52.0), btn_h);
        p.rect_outline_t(res_btn, 1, 100);
        p.text_center_in(res_btn.x, res_btn.x + res_btn.w, r2_y + pt(10.5), 7.0, 0, "Reset");

        // Done button
        let done_btn = Rect::new(bar_x + bar_w - pt(68.0), r2_y, pt(60.0), btn_h);
        p.rect(done_btn, 0);
        p.text_center_in(done_btn.x, done_btn.x + done_btn.w, r2_y + pt(10.5), 7.5, 255, "Apply Crop");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let (vx, vy) = match g {
            Gesture::Tap { x, y } | Gesture::LongPress { x, y } | Gesture::Drag { x, y } => {
                (x as i32, y as i32)
            }
            _ => (0, 0),
        };

        let bar_h = pt(42.0);
        let bar_w = (w - pt(32.0)).min(pt(280.0));
        let bar_x = (w - bar_w) / 2;
        let bar_y = h - bar_h - pt(16.0);

        match g {
            Gesture::Tap { .. } => {
                // Check floating palette buttons
                if vy >= bar_y && vy <= bar_y + bar_h + pt(8.0) && vx >= bar_x && vx <= bar_x + bar_w {
                    if vy < bar_y + bar_h / 2 {
                        // Row 1: Edge buttons
                        let edge_btn_w = bar_w / 5;
                        let idx = ((vx - bar_x) / edge_btn_w).clamp(0, 4) as usize;
                        let edges = [
                            ActiveEdge::Top,
                            ActiveEdge::Bottom,
                            ActiveEdge::Left,
                            ActiveEdge::Right,
                            ActiveEdge::All,
                        ];
                        self.active_edge = edges[idx];
                        return Action::Redraw;
                    } else {
                        // Row 2: Actions
                        let rel_x = vx - bar_x;
                        if rel_x < bar_w * 3 / 10 {
                            // Minus [-1%]
                            self.nudge(-0.01);
                            return Action::Redraw;
                        } else if rel_x < bar_w * 5 / 10 {
                            // Plus [+1%]
                            self.nudge(0.01);
                            return Action::Redraw;
                        } else if rel_x < bar_w * 7 / 10 {
                            // Reset
                            self.settings.split.margin_left = 0.0;
                            self.settings.split.margin_top = 0.0;
                            self.settings.split.margin_right = 0.0;
                            self.settings.split.margin_bottom = 0.0;
                            return Action::Redraw;
                        } else {
                            // Apply Crop
                            return self.apply_crop();
                        }
                    }
                }

                // Tapping outside floating palette -> sets active edge
                let (pw, ph) = if let Some(doc) = &self.doc {
                    if let Ok(page) = doc.load_page(self.page_no as i32) {
                        if let Ok(b) = page.bounds() {
                            (b.x1 - b.x0, b.y1 - b.y0)
                        } else {
                            (w as f32, h as f32)
                        }
                    } else {
                        (w as f32, h as f32)
                    }
                } else {
                    (w as f32, h as f32)
                };

                let zoom = (w as f32 / pw).min(h as f32 / ph);
                let rw = (pw * zoom).round() as i32;
                let rh = (ph * zoom).round() as i32;
                let page_ox = (w - rw) / 2;
                let page_oy = (h - rh) / 2;

                let s = &self.settings.split;
                let x0 = page_ox + (s.margin_left * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - s.margin_right) * rw as f32).round() as i32;
                let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

                if (vy - y0).abs() < 50 {
                    self.active_edge = ActiveEdge::Top;
                } else if (vy - y1).abs() < 50 {
                    self.active_edge = ActiveEdge::Bottom;
                } else if (vx - x0).abs() < 50 {
                    self.active_edge = ActiveEdge::Left;
                } else if (vx - x1).abs() < 50 {
                    self.active_edge = ActiveEdge::Right;
                }
                Action::Redraw
            }

            Gesture::LongPress { .. } => {
                // Direct drag initiation: detect closest edge
                let (pw, ph) = if let Some(doc) = &self.doc {
                    if let Ok(page) = doc.load_page(self.page_no as i32) {
                        if let Ok(b) = page.bounds() {
                            (b.x1 - b.x0, b.y1 - b.y0)
                        } else {
                            (w as f32, h as f32)
                        }
                    } else {
                        (w as f32, h as f32)
                    }
                } else {
                    (w as f32, h as f32)
                };

                let zoom = (w as f32 / pw).min(h as f32 / ph);
                let rw = (pw * zoom).round() as i32;
                let rh = (ph * zoom).round() as i32;
                let page_ox = (w - rw) / 2;
                let page_oy = (h - rh) / 2;

                let s = &self.settings.split;
                let x0 = page_ox + (s.margin_left * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - s.margin_right) * rw as f32).round() as i32;
                let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

                if (vy - y0).abs() < 60 {
                    self.drag_edge = Some(ActiveEdge::Top);
                    self.active_edge = ActiveEdge::Top;
                } else if (vy - y1).abs() < 60 {
                    self.drag_edge = Some(ActiveEdge::Bottom);
                    self.active_edge = ActiveEdge::Bottom;
                } else if (vx - x0).abs() < 60 {
                    self.drag_edge = Some(ActiveEdge::Left);
                    self.active_edge = ActiveEdge::Left;
                } else if (vx - x1).abs() < 60 {
                    self.drag_edge = Some(ActiveEdge::Right);
                    self.active_edge = ActiveEdge::Right;
                }
                Action::Redraw
            }

            Gesture::Drag { .. } => {
                // Direct dragging of dashed margin lines!
                if let Some(edge) = self.drag_edge {
                    let (pw, ph) = if let Some(doc) = &self.doc {
                        if let Ok(page) = doc.load_page(self.page_no as i32) {
                            if let Ok(b) = page.bounds() {
                                (b.x1 - b.x0, b.y1 - b.y0)
                            } else {
                                (w as f32, h as f32)
                            }
                        } else {
                            (w as f32, h as f32)
                        }
                    } else {
                        (w as f32, h as f32)
                    };

                    let zoom = (w as f32 / pw).min(h as f32 / ph);
                    let rw = (pw * zoom).round() as i32;
                    let rh = (ph * zoom).round() as i32;
                    let page_ox = (w - rw) / 2;
                    let page_oy = (h - rh) / 2;

                    let s = &mut self.settings.split;
                    match edge {
                        ActiveEdge::Top => {
                            s.margin_top = ((vy - page_oy) as f32 / rh as f32).clamp(0.0, 0.40);
                        }
                        ActiveEdge::Bottom => {
                            s.margin_bottom = ((page_oy + rh - vy) as f32 / rh as f32).clamp(0.0, 0.40);
                        }
                        ActiveEdge::Left => {
                            s.margin_left = ((vx - page_ox) as f32 / rw as f32).clamp(0.0, 0.40);
                        }
                        ActiveEdge::Right => {
                            s.margin_right = ((page_ox + rw - vx) as f32 / rw as f32).clamp(0.0, 0.40);
                        }
                        _ => {}
                    }
                    return Action::Redraw;
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
