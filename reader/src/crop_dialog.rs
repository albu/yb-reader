//! Interactive visual margin crop studio for PDF / fixed-layout books.
//! Displays full uncropped pages with draggable dashed guide lines,
//! facing-page (Even/Odd) preview flipping, and intelligent auto-crop detection.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::Orientation;

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
    pub view_page_no: usize,
    pub cached_page_gray: Option<(usize, Vec<u8>)>,
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
            view_page_no: page_no,
            cached_page_gray: None,
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
        let is_odd = self.settings.split.mirror_even_odd && (self.view_page_no % 2 == 1);
        let s = &mut self.settings.split;
        match self.active_edge {
            ActiveEdge::Top => s.margin_top = (s.margin_top + delta).clamp(0.0, 0.40),
            ActiveEdge::Bottom => s.margin_bottom = (s.margin_bottom + delta).clamp(0.0, 0.40),
            ActiveEdge::Left => {
                if is_odd {
                    s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40);
                } else {
                    s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40);
                }
            }
            ActiveEdge::Right => {
                if is_odd {
                    s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40);
                } else {
                    s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40);
                }
            }
            ActiveEdge::All => {
                s.margin_top = (s.margin_top + delta).clamp(0.0, 0.40);
                s.margin_bottom = (s.margin_bottom + delta).clamp(0.0, 0.40);
                s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40);
                s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40);
            }
        }
    }

    fn run_auto_crop(&mut self) {
        if let Some(doc) = &self.doc {
            if self.settings.split.mirror_even_odd {
                if let Some((ml, mt, mr, mb)) =
                    crate::render::detect_even_odd_margins(doc.as_ref(), self.view_page_no)
                {
                    self.settings.split.margin_left = ml;
                    self.settings.split.margin_top = mt;
                    self.settings.split.margin_right = mr;
                    self.settings.split.margin_bottom = mb;
                }
            } else if let Some((ml, mt, mr, mb)) =
                crate::render::detect_book_margins(doc.as_ref(), self.view_page_no)
            {
                self.settings.split.margin_left = ml;
                self.settings.split.margin_top = mt;
                self.settings.split.margin_right = mr;
                self.settings.split.margin_bottom = mb;
            }
        }
    }

    fn reset_crop(&mut self) {
        self.settings.split.margin_left = 0.0;
        self.settings.split.margin_top = 0.0;
        self.settings.split.margin_right = 0.0;
        self.settings.split.margin_bottom = 0.0;
        self.settings.split.mirror_even_odd = false;
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

        // Render full uncropped portrait page if not cached for current view_page_no
        let needs_render = match &self.cached_page_gray {
            Some((cached_p, _)) => *cached_p != self.view_page_no,
            None => true,
        };

        if needs_render {
            if let Some(doc) = &self.doc {
                let mut fit_s = self.settings;
                fit_s.show_header = false;
                fit_s.split.preset = crate::split::SplitPreset::FitPage;
                fit_s.split.rotation = 0;
                fit_s.split.margin_left = 0.0;
                fit_s.split.margin_top = 0.0;
                fit_s.split.margin_right = 0.0;
                fit_s.split.margin_bottom = 0.0;
                if let Some(gray) = crate::render::render_page(
                    doc.as_ref(),
                    self.view_page_no,
                    0,
                    &fit_s,
                    w as u32,
                    h as u32,
                ) {
                    self.cached_page_gray = Some((self.view_page_no, gray));
                }
            }
        }

        // 1. Draw the uncropped base page
        if let Some((_, gray)) = &self.cached_page_gray {
            p.blit_gray(0, 0, w, h, gray, w as usize);
        } else {
            p.clear(255);
        }

        let (pw, ph) = if let Some(doc) = &self.doc {
            if let Ok(page) = doc.load_page(self.view_page_no as i32) {
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
        let (ml, mr) = if s.mirror_even_odd && (self.view_page_no % 2 == 1) {
            (s.margin_right, s.margin_left)
        } else {
            (s.margin_left, s.margin_right)
        };
        let x0 = page_ox + (ml * rw as f32).round() as i32;
        let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
        let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
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

        // 3b. Render internal split guide line(s) for the current preset inside the cropped box (x0..x1, y0..y1)
        match self.settings.split.preset {
            crate::split::SplitPreset::Horizontal2 => {
                let mid_y = (y0 + y1) / 2;
                let mut sx = x0;
                while sx < x1 {
                    let seg_w = dash.min(x1 - sx);
                    p.rect(Rect::new(sx, mid_y, seg_w, 1), 100);
                    sx += dash + gap;
                }
                p.text_center_in(x0, x1, mid_y - pt(4.0), 6.5, 80, "2-Split Cut");
            }
            crate::split::SplitPreset::Horizontal3 => {
                let step = (y1 - y0) / 3;
                for i in 1..=2 {
                    let sy = y0 + i * step;
                    let mut sx = x0;
                    while sx < x1 {
                        let seg_w = dash.min(x1 - sx);
                        p.rect(Rect::new(sx, sy, seg_w, 1), 100);
                        sx += dash + gap;
                    }
                }
                p.text_center_in(x0, x1, y0 + step - pt(4.0), 6.5, 80, "3-Split Cuts");
            }
            crate::split::SplitPreset::Vertical2 => {
                let mid_x = (x0 + x1) / 2;
                let mut sy = y0;
                while sy < y1 {
                    let seg_h = dash.min(y1 - sy);
                    p.rect(Rect::new(mid_x, sy, 1, seg_h), 100);
                    sy += dash + gap;
                }
                p.text_center_in(
                    mid_x - pt(30.0),
                    mid_x + pt(30.0),
                    (y0 + y1) / 2,
                    6.5,
                    80,
                    "2-Col",
                );
            }
            crate::split::SplitPreset::Grid4 => {
                let mid_x = (x0 + x1) / 2;
                let mid_y = (y0 + y1) / 2;
                let mut sx = x0;
                while sx < x1 {
                    let seg_w = dash.min(x1 - sx);
                    p.rect(Rect::new(sx, mid_y, seg_w, 1), 100);
                    sx += dash + gap;
                }
                let mut sy = y0;
                while sy < y1 {
                    let seg_h = dash.min(y1 - sy);
                    p.rect(Rect::new(mid_x, sy, 1, seg_h), 100);
                    sy += dash + gap;
                }
                p.text_center_in(x0, x1, mid_y - pt(4.0), 6.5, 80, "4-Grid Cuts");
            }
            _ => {}
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

        // 5. Floating Bottom Control Palette
        let bar_h = pt(74.0);
        let bar_w = (w - pt(24.0)).min(pt(310.0));
        let bar_x = (w - bar_w) / 2;
        let bar_y = h - bar_h - pt(10.0);

        // Drop shadow + white rounded pill card
        p.rect(Rect::new(bar_x + 2, bar_y + 2, bar_w, bar_h), 120);
        p.rect(Rect::new(bar_x, bar_y, bar_w, bar_h), 255);
        p.rect_outline_t(Rect::new(bar_x, bar_y, bar_w, bar_h), 2, 0);

        // Row 1 (y = bar_y + pt(4.0)): Mode & Page Flip Switchers
        let r1_y = bar_y + pt(4.0);
        let r1_h = pt(15.0);

        // Mode: Unified
        let mode_uni_btn = Rect::new(bar_x + pt(6.0), r1_y, pt(56.0), r1_h);
        if !s.mirror_even_odd {
            p.rect(mode_uni_btn, 0);
            p.text_center_in(
                mode_uni_btn.x,
                mode_uni_btn.x + mode_uni_btn.w,
                r1_y + pt(10.5),
                6.5,
                255,
                "Unified",
            );
        } else {
            p.rect_outline_t(mode_uni_btn, 1, 120);
            p.text_center_in(
                mode_uni_btn.x,
                mode_uni_btn.x + mode_uni_btn.w,
                r1_y + pt(10.5),
                6.5,
                0,
                "Unified",
            );
        }

        // Mode: Odd/Even
        let mode_oe_btn = Rect::new(bar_x + pt(66.0), r1_y, pt(64.0), r1_h);
        if s.mirror_even_odd {
            p.rect(mode_oe_btn, 0);
            p.text_center_in(
                mode_oe_btn.x,
                mode_oe_btn.x + mode_oe_btn.w,
                r1_y + pt(10.5),
                6.5,
                255,
                "Odd/Even",
            );
        } else {
            p.rect_outline_t(mode_oe_btn, 1, 120);
            p.text_center_in(
                mode_oe_btn.x,
                mode_oe_btn.x + mode_oe_btn.w,
                r1_y + pt(10.5),
                6.5,
                0,
                "Odd/Even",
            );
        }

        // Page preview switchers
        if s.mirror_even_odd {
            let even_pno = if self.view_page_no % 2 == 0 {
                self.view_page_no
            } else {
                self.view_page_no.saturating_sub(1)
            };
            let odd_pno = if even_pno + 1 < self.total {
                even_pno + 1
            } else {
                even_pno
            };

            let even_btn = Rect::new(bar_x + pt(136.0), r1_y, pt(80.0), r1_h);
            let odd_btn = Rect::new(bar_x + pt(220.0), r1_y, pt(80.0), r1_h);

            if self.view_page_no % 2 == 0 {
                p.rect(even_btn, 0);
                p.text_center_in(
                    even_btn.x,
                    even_btn.x + even_btn.w,
                    r1_y + pt(10.5),
                    6.5,
                    255,
                    &format!("Even (p.{})", even_pno + 1),
                );
                p.rect_outline_t(odd_btn, 1, 120);
                p.text_center_in(
                    odd_btn.x,
                    odd_btn.x + odd_btn.w,
                    r1_y + pt(10.5),
                    6.5,
                    0,
                    &format!("Odd (p.{})", odd_pno + 1),
                );
            } else {
                p.rect_outline_t(even_btn, 1, 120);
                p.text_center_in(
                    even_btn.x,
                    even_btn.x + even_btn.w,
                    r1_y + pt(10.5),
                    6.5,
                    0,
                    &format!("Even (p.{})", even_pno + 1),
                );
                p.rect(odd_btn, 0);
                p.text_center_in(
                    odd_btn.x,
                    odd_btn.x + odd_btn.w,
                    r1_y + pt(10.5),
                    6.5,
                    255,
                    &format!("Odd (p.{})", odd_pno + 1),
                );
            }
        } else {
            let prev_btn = Rect::new(bar_x + pt(146.0), r1_y, pt(72.0), r1_h);
            let next_btn = Rect::new(bar_x + pt(224.0), r1_y, pt(72.0), r1_h);
            p.rect_outline_t(prev_btn, 1, 120);
            p.text_center_in(
                prev_btn.x,
                prev_btn.x + prev_btn.w,
                r1_y + pt(10.5),
                6.5,
                0,
                "◀ Prev Pg",
            );
            p.rect_outline_t(next_btn, 1, 120);
            p.text_center_in(
                next_btn.x,
                next_btn.x + next_btn.w,
                r1_y + pt(10.5),
                6.5,
                0,
                "Next Pg ▶",
            );
        }

        // Row 2: Edge selector pills [ Top | Bottom | Left | Right | All ]
        let edges = [
            (ActiveEdge::Top, "Top"),
            (ActiveEdge::Bottom, "Btm"),
            (ActiveEdge::Left, "Left"),
            (ActiveEdge::Right, "Right"),
            (ActiveEdge::All, "All"),
        ];
        let edge_btn_w = (bar_w - pt(16.0)) / 5;
        let edge_btn_h = pt(15.0);
        let edge_y = bar_y + pt(24.0);

        for (i, (edge, lbl)) in edges.iter().enumerate() {
            let bx = bar_x + pt(8.0) + i as i32 * edge_btn_w;
            let br = Rect::new(bx, edge_y, edge_btn_w - pt(2.0), edge_btn_h);
            if self.active_edge == *edge {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, edge_y + pt(10.5), 6.5, 255, lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, edge_y + pt(10.5), 6.5, 0, lbl);
            }
        }

        // Row 3: Precision Nudge [-1%] [%] [+1%] | [Auto] | [Reset] | [Apply]
        let r3_y = bar_y + pt(45.0);
        let btn_h = pt(18.0);

        // Minus button
        let m_btn = Rect::new(bar_x + pt(6.0), r3_y, pt(22.0), btn_h);
        p.rect_outline_t(m_btn, 1, 0);
        p.text_center_in(m_btn.x, m_btn.x + m_btn.w, r3_y + pt(12.5), 8.0, 0, "-");

        // Percentage display
        let is_odd = s.mirror_even_odd && (self.view_page_no % 2 == 1);
        let pct = match self.active_edge {
            ActiveEdge::Top => (s.margin_top * 100.0).round() as i32,
            ActiveEdge::Bottom => (s.margin_bottom * 100.0).round() as i32,
            ActiveEdge::Left => {
                if is_odd {
                    (s.margin_right * 100.0).round() as i32
                } else {
                    (s.margin_left * 100.0).round() as i32
                }
            }
            ActiveEdge::Right => {
                if is_odd {
                    (s.margin_left * 100.0).round() as i32
                } else {
                    (s.margin_right * 100.0).round() as i32
                }
            }
            ActiveEdge::All => (s.margin_top * 100.0).round() as i32,
        };
        let pct_str = format!("{}%", pct);
        p.text_center_in(
            bar_x + pt(28.0),
            bar_x + pt(56.0),
            r3_y + pt(12.5),
            7.0,
            0,
            &pct_str,
        );

        // Plus button
        let p_btn = Rect::new(bar_x + pt(56.0), r3_y, pt(22.0), btn_h);
        p.rect_outline_t(p_btn, 1, 0);
        p.text_center_in(p_btn.x, p_btn.x + p_btn.w, r3_y + pt(12.5), 8.0, 0, "+");

        // Auto button
        let auto_btn = Rect::new(bar_x + pt(84.0), r3_y, pt(48.0), btn_h);
        p.rect_outline_t(auto_btn, 1, 0);
        p.text_center_in(
            auto_btn.x,
            auto_btn.x + auto_btn.w,
            r3_y + pt(12.5),
            7.0,
            0,
            "Auto",
        );

        // Reset button
        let res_btn = Rect::new(bar_x + pt(136.0), r3_y, pt(48.0), btn_h);
        p.rect_outline_t(res_btn, 1, 100);
        p.text_center_in(
            res_btn.x,
            res_btn.x + res_btn.w,
            r3_y + pt(12.5),
            7.0,
            0,
            "Reset",
        );

        // Apply button
        let done_btn = Rect::new(bar_x + bar_w - pt(70.0), r3_y, pt(64.0), btn_h);
        p.rect(done_btn, 0);
        p.text_center_in(
            done_btn.x,
            done_btn.x + done_btn.w,
            r3_y + pt(12.5),
            7.5,
            255,
            "Apply",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        if g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }
        let (vx, vy) = match g {
            Gesture::Tap { x, y } | Gesture::LongPress { x, y } | Gesture::Drag { x, y } => {
                (x as i32, y as i32)
            }
            _ => return Action::Keep,
        };

        let bar_h = pt(74.0);
        let bar_w = (w - pt(24.0)).min(pt(310.0));
        let bar_x = (w - bar_w) / 2;
        let bar_y = h - bar_h - pt(10.0);

        match g {
            Gesture::Tap { .. } => {
                if vy >= bar_y
                    && vy <= bar_y + bar_h + pt(8.0)
                    && vx >= bar_x
                    && vx <= bar_x + bar_w
                {
                    // Row 1: Mode & Page Flip Switchers
                    let r1_y = bar_y + pt(4.0);
                    let r1_h = pt(18.0);
                    if vy >= r1_y && vy < r1_y + r1_h {
                        let mode_uni_btn = Rect::new(bar_x + pt(6.0), r1_y, pt(56.0), r1_h);
                        let mode_oe_btn = Rect::new(bar_x + pt(66.0), r1_y, pt(64.0), r1_h);
                        if mode_uni_btn.contains(vx, vy) {
                            self.settings.split.mirror_even_odd = false;
                            return Action::Redraw;
                        }
                        if mode_oe_btn.contains(vx, vy) {
                            self.settings.split.mirror_even_odd = true;
                            return Action::Redraw;
                        }

                        if self.settings.split.mirror_even_odd {
                            let even_pno = if self.view_page_no % 2 == 0 {
                                self.view_page_no
                            } else {
                                self.view_page_no.saturating_sub(1)
                            };
                            let odd_pno = if even_pno + 1 < self.total {
                                even_pno + 1
                            } else {
                                even_pno
                            };
                            let even_btn = Rect::new(bar_x + pt(136.0), r1_y, pt(80.0), r1_h);
                            let odd_btn = Rect::new(bar_x + pt(220.0), r1_y, pt(80.0), r1_h);
                            if even_btn.contains(vx, vy) {
                                self.view_page_no = even_pno;
                                return Action::Redraw;
                            }
                            if odd_btn.contains(vx, vy) {
                                self.view_page_no = odd_pno;
                                return Action::Redraw;
                            }
                        } else {
                            let prev_btn = Rect::new(bar_x + pt(146.0), r1_y, pt(72.0), r1_h);
                            let next_btn = Rect::new(bar_x + pt(224.0), r1_y, pt(72.0), r1_h);
                            if prev_btn.contains(vx, vy) && self.view_page_no > 0 {
                                self.view_page_no -= 1;
                                return Action::Redraw;
                            }
                            if next_btn.contains(vx, vy) && self.view_page_no + 1 < self.total {
                                self.view_page_no += 1;
                                return Action::Redraw;
                            }
                        }
                    }

                    // Row 2: Edge Selector Pills
                    let edge_y = bar_y + pt(24.0);
                    let edge_btn_h = pt(18.0);
                    if vy >= edge_y && vy < edge_y + edge_btn_h {
                        let edge_btn_w = (bar_w - pt(16.0)) / 5;
                        let idx = ((vx - (bar_x + pt(8.0))) / edge_btn_w).clamp(0, 4) as usize;
                        let edges = [
                            ActiveEdge::Top,
                            ActiveEdge::Bottom,
                            ActiveEdge::Left,
                            ActiveEdge::Right,
                            ActiveEdge::All,
                        ];
                        self.active_edge = edges[idx];
                        return Action::Redraw;
                    }

                    // Row 3: Precision Nudge [-1%] [%] [+1%] | [Auto] | [Reset] | [Apply]
                    let r3_y = bar_y + pt(45.0);
                    let btn_h = pt(22.0);
                    if Rect::new(bar_x + pt(6.0), r3_y, pt(22.0), btn_h).contains(vx, vy) {
                        self.nudge(-0.01);
                        return Action::Redraw;
                    }
                    if Rect::new(bar_x + pt(56.0), r3_y, pt(22.0), btn_h).contains(vx, vy) {
                        self.nudge(0.01);
                        return Action::Redraw;
                    }
                    if Rect::new(bar_x + pt(84.0), r3_y, pt(48.0), btn_h).contains(vx, vy) {
                        self.run_auto_crop();
                        return Action::Redraw;
                    }
                    if Rect::new(bar_x + pt(136.0), r3_y, pt(48.0), btn_h).contains(vx, vy) {
                        self.reset_crop();
                        return Action::Redraw;
                    }
                    if Rect::new(bar_x + bar_w - pt(70.0), r3_y, pt(64.0), btn_h).contains(vx, vy) {
                        return self.apply_crop();
                    }
                }

                // Tapping outside floating palette -> selects nearest margin guide line
                let (pw, ph) = if let Some(doc) = &self.doc {
                    if let Ok(page) = doc.load_page(self.view_page_no as i32) {
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
                let (ml, mr) = if s.mirror_even_odd && (self.view_page_no % 2 == 1) {
                    (s.margin_right, s.margin_left)
                } else {
                    (s.margin_left, s.margin_right)
                };
                let x0 = page_ox + (ml * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
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
                    if let Ok(page) = doc.load_page(self.view_page_no as i32) {
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
                let (ml, mr) = if s.mirror_even_odd && (self.view_page_no % 2 == 1) {
                    (s.margin_right, s.margin_left)
                } else {
                    (s.margin_left, s.margin_right)
                };
                let x0 = page_ox + (ml * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
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
                // Direct dragging of dashed margin lines
                if let Some(edge) = self.drag_edge {
                    let (pw, ph) = if let Some(doc) = &self.doc {
                        if let Ok(page) = doc.load_page(self.view_page_no as i32) {
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

                    let is_odd =
                        self.settings.split.mirror_even_odd && (self.view_page_no % 2 == 1);
                    let s = &mut self.settings.split;
                    match edge {
                        ActiveEdge::Top => {
                            s.margin_top = ((vy - page_oy) as f32 / rh as f32).clamp(0.0, 0.40);
                        }
                        ActiveEdge::Bottom => {
                            s.margin_bottom =
                                ((page_oy + rh - vy) as f32 / rh as f32).clamp(0.0, 0.40);
                        }
                        ActiveEdge::Left => {
                            let val = ((vx - page_ox) as f32 / rw as f32).clamp(0.0, 0.40);
                            if is_odd {
                                s.margin_right = val;
                            } else {
                                s.margin_left = val;
                            }
                        }
                        ActiveEdge::Right => {
                            let val = ((page_ox + rw - vx) as f32 / rw as f32).clamp(0.0, 0.40);
                            if is_odd {
                                s.margin_left = val;
                            } else {
                                s.margin_right = val;
                            }
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
