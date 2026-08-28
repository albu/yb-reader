//! Interactive visual margin crop studio for PDF / fixed-layout books.
//! Displays full uncropped pages fitted completely above controls, with draggable
//! dashed guide lines, facing-page (Even/Odd) preview flipping, and intelligent auto-crop detection.

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
    pub cached_page_gray: Option<(usize, Vec<u8>, u32, u32)>,
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
        let is_odd = (self.view_page_no + 1) % 2 == 1;
        let s = &mut self.settings.split;
        match self.active_edge {
            ActiveEdge::Top => s.margin_top = (s.margin_top + delta).clamp(0.0, 0.40),
            ActiveEdge::Bottom => s.margin_bottom = (s.margin_bottom + delta).clamp(0.0, 0.40),
            ActiveEdge::Left => {
                if s.mirror_even_odd && is_odd {
                    s.margin_right = (s.margin_right + delta).clamp(0.0, 0.40);
                } else {
                    s.margin_left = (s.margin_left + delta).clamp(0.0, 0.40);
                }
            }
            ActiveEdge::Right => {
                if s.mirror_even_odd && is_odd {
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
}

impl Screen for CropDialog {
    /// Crop adjustment always happens in Portrait mode for clean, unrotated 1:1 page editing.
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        let top_h = pt(26.0);
        let panel_h = pt(92.0);
        let panel_y = h - panel_h;

        // Middle Page Preview area strictly ABOVE bottom controls
        let prev_x = pt(8.0);
        let prev_y = top_h + pt(2.0);
        let prev_w = w - pt(16.0);
        let prev_h = panel_y - prev_y - pt(4.0);

        // 1. Clear background
        p.clear(255);

        // Top Header
        let close_btn = Rect::new(pt(8.0), pt(4.0), pt(60.0), pt(18.0));
        p.rect_outline_t(close_btn, 1, 120);
        p.text_center_in(
            close_btn.x,
            close_btn.x + close_btn.w,
            pt(16.0),
            7.0,
            0,
            "[ Close ]",
        );

        p.text_center_in(0, w, pt(14.0), 8.5, 0, "CROP STUDIO");
        let is_odd = (self.view_page_no + 1) % 2 == 1;
        let page_type = if is_odd { "Odd Page" } else { "Even Page" };
        p.text_center_in(
            0,
            w,
            pt(23.0),
            6.5,
            100,
            &format!("Page {} of {} - {}", self.view_page_no + 1, self.total, page_type),
        );

        // Render full uncropped portrait page fitted to the preview area
        let (pw, ph) = if let Some(doc) = &self.doc {
            if let Ok(page) = doc.load_page(self.view_page_no as i32) {
                if let Ok(b) = page.bounds() {
                    (b.x1 - b.x0, b.y1 - b.y0)
                } else {
                    (prev_w as f32, prev_h as f32)
                }
            } else {
                (prev_w as f32, prev_h as f32)
            }
        } else {
            (prev_w as f32, prev_h as f32)
        };

        let zoom = (prev_w as f32 / pw).min(prev_h as f32 / ph);
        let rw = (pw * zoom).round() as i32;
        let rh = (ph * zoom).round() as i32;
        let page_ox = prev_x + (prev_w - rw) / 2;
        let page_oy = prev_y + (prev_h - rh) / 2;

        let needs_render = match &self.cached_page_gray {
            Some((cached_p, _, cached_w, cached_h)) => {
                *cached_p != self.view_page_no || *cached_w != rw as u32 || *cached_h != rh as u32
            }
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
                    rw as u32,
                    rh as u32,
                ) {
                    self.cached_page_gray =
                        Some((self.view_page_no, gray, rw as u32, rh as u32));
                }
            }
        }

        // Draw outer backdrop for page
        p.rect(Rect::new(prev_x, prev_y, prev_w, prev_h), 245);
        p.rect_outline_t(Rect::new(page_ox - 1, page_oy - 1, rw + 2, rh + 2), 1, 180);

        // Blit uncropped page image
        if let Some((_, gray, gw, gh)) = &self.cached_page_gray {
            p.blit_gray(page_ox, page_oy, *gw as i32, *gh as i32, gray, *gw as usize);
        }

        // Compute crop guide coordinates in screen pixels
        let s = &self.settings.split;
        let (ml, mr) = if s.mirror_even_odd && is_odd {
            (s.margin_right, s.margin_left)
        } else {
            (s.margin_left, s.margin_right)
        };
        let x0 = page_ox + (ml * rw as f32).round() as i32;
        let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
        let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
        let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

        // Dim outer excluded margins with clean cross-hatch stippling
        let dim_color = 140;
        // Top outer rect
        if y0 > page_oy {
            for y in (page_oy..y0).step_by(4) {
                p.hline_t(y, page_ox, page_ox + rw, 1, dim_color);
            }
        }
        // Bottom outer rect
        if y1 < page_oy + rh {
            for y in (y1..page_oy + rh).step_by(4) {
                p.hline_t(y, page_ox, page_ox + rw, 1, dim_color);
            }
        }
        // Left outer rect
        if x0 > page_ox {
            for y in (y0..y1).step_by(4) {
                p.hline_t(y, page_ox, x0, 1, dim_color);
            }
        }
        // Right outer rect
        if x1 < page_ox + rw {
            for y in (y0..y1).step_by(4) {
                p.hline_t(y, x1, page_ox + rw, 1, dim_color);
            }
        }

        // Dashed margin guide lines
        let dash = pt(5.0);
        let gap = pt(4.0);

        // Top horizontal dashed guide
        let mut x = page_ox;
        while x < page_ox + rw {
            let seg_w = dash.min(page_ox + rw - x);
            p.rect(Rect::new(x, y0 - 1, seg_w, 2), 0);
            p.rect(Rect::new(x, y0 + 1, seg_w, 1), 255);
            x += dash + gap;
        }

        // Bottom horizontal dashed guide
        let mut x = page_ox;
        while x < page_ox + rw {
            let seg_w = dash.min(page_ox + rw - x);
            p.rect(Rect::new(x, y1 - 1, seg_w, 2), 0);
            p.rect(Rect::new(x, y1 + 1, seg_w, 1), 255);
            x += dash + gap;
        }

        // Left vertical dashed guide
        let mut y = page_oy;
        while y < page_oy + rh {
            let seg_h = dash.min(page_oy + rh - y);
            p.rect(Rect::new(x0 - 1, y, 2, seg_h), 0);
            p.rect(Rect::new(x0 + 1, y, 1, seg_h), 255);
            y += dash + gap;
        }

        // Right vertical dashed guide
        let mut y = page_oy;
        while y < page_oy + rh {
            let seg_h = dash.min(page_oy + rh - y);
            p.rect(Rect::new(x1 - 1, y, 2, seg_h), 0);
            p.rect(Rect::new(x1 + 1, y, 1, seg_h), 255);
            y += dash + gap;
        }

        // Corner brackets around the crop box
        let clen = pt(14.0);
        let cth = 3;
        // Top-Left
        p.rect(Rect::new(x0, y0, clen, cth), 0);
        p.rect(Rect::new(x0, y0, cth, clen), 0);
        // Top-Right
        p.rect(Rect::new(x1 - clen, y0, clen, cth), 0);
        p.rect(Rect::new(x1 - cth, y0, cth, clen), 0);
        // Bottom-Left
        p.rect(Rect::new(x0, y1 - cth, clen, cth), 0);
        p.rect(Rect::new(x0, y1 - clen, cth, clen), 0);
        // Bottom-Right
        p.rect(Rect::new(x1 - clen, y1 - cth, clen, cth), 0);
        p.rect(Rect::new(x1 - cth, y1 - clen, cth, clen), 0);

        // ==========================================
        // DOCKED BOTTOM CONTROL PANEL
        // ==========================================
        p.hline_t(panel_y, 0, w, 2, 0);
        p.rect(Rect::new(0, panel_y + 2, w, panel_h - 2), 255);

        let pad = pt(6.0);
        let avail_w = w - 2 * pad;

        // Row 1 (y = panel_y + pt(5.0), h = pt(22.0)): Mode & Page Flip Switchers
        let r1_y = panel_y + pt(5.0);
        let r1_h = pt(22.0);

        // Mode: Unified
        let mode_uni_btn = Rect::new(pad, r1_y, pt(50.0), r1_h);
        if !s.mirror_even_odd {
            p.rect(mode_uni_btn, 0);
            p.text_center_in(
                mode_uni_btn.x,
                mode_uni_btn.x + mode_uni_btn.w,
                r1_y + pt(15.0),
                7.0,
                255,
                "Unified",
            );
        } else {
            p.rect_outline_t(mode_uni_btn, 1, 120);
            p.text_center_in(
                mode_uni_btn.x,
                mode_uni_btn.x + mode_uni_btn.w,
                r1_y + pt(15.0),
                7.0,
                0,
                "Unified",
            );
        }

        // Mode: Odd/Even
        let mode_oe_btn = Rect::new(pad + pt(53.0), r1_y, pt(56.0), r1_h);
        if s.mirror_even_odd {
            p.rect(mode_oe_btn, 0);
            p.text_center_in(
                mode_oe_btn.x,
                mode_oe_btn.x + mode_oe_btn.w,
                r1_y + pt(15.0),
                7.0,
                255,
                "Odd / Even",
            );
        } else {
            p.rect_outline_t(mode_oe_btn, 1, 120);
            p.text_center_in(
                mode_oe_btn.x,
                mode_oe_btn.x + mode_oe_btn.w,
                r1_y + pt(15.0),
                7.0,
                0,
                "Odd / Even",
            );
        }

        // Page preview switchers (dynamically sized to fill remaining width)
        let nav_w = (avail_w - pt(113.0) - pt(4.0)) / 2;
        let btn1_x = pad + pt(113.0);
        let btn2_x = btn1_x + nav_w + pt(4.0);

        if s.mirror_even_odd {
            let (odd_pno, even_pno) = if (self.view_page_no + 1) % 2 == 1 {
                (
                    self.view_page_no,
                    (self.view_page_no + 1).min(self.total.saturating_sub(1)),
                )
            } else {
                (
                    self.view_page_no.saturating_sub(1),
                    self.view_page_no,
                )
            };

            let odd_btn = Rect::new(btn1_x, r1_y, nav_w, r1_h);
            let even_btn = Rect::new(btn2_x, r1_y, nav_w, r1_h);

            if is_odd {
                p.rect(odd_btn, 0);
                p.text_center_in(
                    odd_btn.x,
                    odd_btn.x + odd_btn.w,
                    r1_y + pt(15.0),
                    7.0,
                    255,
                    &format!("Odd (p.{}) *", odd_pno + 1),
                );
                p.rect_outline_t(even_btn, 1, 120);
                p.text_center_in(
                    even_btn.x,
                    even_btn.x + even_btn.w,
                    r1_y + pt(15.0),
                    7.0,
                    0,
                    &format!("Even (p.{})", even_pno + 1),
                );
            } else {
                p.rect_outline_t(odd_btn, 1, 120);
                p.text_center_in(
                    odd_btn.x,
                    odd_btn.x + odd_btn.w,
                    r1_y + pt(15.0),
                    7.0,
                    0,
                    &format!("Odd (p.{})", odd_pno + 1),
                );
                p.rect(even_btn, 0);
                p.text_center_in(
                    even_btn.x,
                    even_btn.x + even_btn.w,
                    r1_y + pt(15.0),
                    7.0,
                    255,
                    &format!("Even (p.{}) *", even_pno + 1),
                );
            }
        } else {
            let prev_btn = Rect::new(btn1_x, r1_y, nav_w, r1_h);
            let next_btn = Rect::new(btn2_x, r1_y, nav_w, r1_h);
            p.rect_outline_t(prev_btn, 1, 120);
            p.text_center_in(
                prev_btn.x,
                prev_btn.x + prev_btn.w,
                r1_y + pt(15.0),
                7.0,
                0,
                "< Prev Page",
            );
            p.rect_outline_t(next_btn, 1, 120);
            p.text_center_in(
                next_btn.x,
                next_btn.x + next_btn.w,
                r1_y + pt(15.0),
                7.0,
                0,
                "Next Page >",
            );
        }

        // Row 2 (y = panel_y + pt(32.0), h = pt(22.0)): Edge Selector Pills
        let r2_y = panel_y + pt(32.0);
        let r2_h = pt(22.0);
        let edges = [
            (ActiveEdge::Top, "Top"),
            (ActiveEdge::Bottom, "Bottom"),
            (ActiveEdge::Left, "Left"),
            (ActiveEdge::Right, "Right"),
            (ActiveEdge::All, "All 4"),
        ];
        let edge_btn_w = (avail_w - pt(12.0)) / 5;

        for (i, (edge, lbl)) in edges.iter().enumerate() {
            let bx = pad + i as i32 * (edge_btn_w + pt(3.0));
            let br = Rect::new(bx, r2_y, edge_btn_w, r2_h);
            if self.active_edge == *edge {
                p.rect(br, 0);
                p.text_center_in(br.x, br.x + br.w, r2_y + pt(15.0), 6.5, 255, lbl);
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, r2_y + pt(15.0), 6.5, 0, lbl);
            }
        }

        // Row 3 (y = panel_y + pt(59.0), h = pt(26.0)): Actions & Tuning Controls
        let r3_y = panel_y + pt(59.0);
        let r3_h = pt(26.0);

        // Minus button
        let m_btn = Rect::new(pad, r3_y, pt(26.0), r3_h);
        p.rect_outline_t(m_btn, 1, 0);
        p.text_center_in(m_btn.x, m_btn.x + m_btn.w, r3_y + pt(17.5), 9.0, 0, "-");

        // Percentage display box
        let pct_box = Rect::new(pad + pt(29.0), r3_y, pt(32.0), r3_h);
        p.rect_outline_t(pct_box, 1, 140);
        let pct = match self.active_edge {
            ActiveEdge::Top => (s.margin_top * 100.0).round() as i32,
            ActiveEdge::Bottom => (s.margin_bottom * 100.0).round() as i32,
            ActiveEdge::Left => {
                if s.mirror_even_odd && is_odd {
                    (s.margin_right * 100.0).round() as i32
                } else {
                    (s.margin_left * 100.0).round() as i32
                }
            }
            ActiveEdge::Right => {
                if s.mirror_even_odd && is_odd {
                    (s.margin_left * 100.0).round() as i32
                } else {
                    (s.margin_right * 100.0).round() as i32
                }
            }
            ActiveEdge::All => (s.margin_top * 100.0).round() as i32,
        };
        let pct_str = format!("{}%", pct);
        p.text_center_in(
            pct_box.x,
            pct_box.x + pct_box.w,
            r3_y + pt(17.5),
            7.5,
            0,
            &pct_str,
        );

        // Plus button
        let p_btn = Rect::new(pad + pt(64.0), r3_y, pt(26.0), r3_h);
        p.rect_outline_t(p_btn, 1, 0);
        p.text_center_in(p_btn.x, p_btn.x + p_btn.w, r3_y + pt(17.5), 9.0, 0, "+");

        // Auto-Crop button
        let auto_btn = Rect::new(pad + pt(94.0), r3_y, pt(72.0), r3_h);
        p.rect_outline_t(auto_btn, 1, 0);
        p.text_center_in(
            auto_btn.x,
            auto_btn.x + auto_btn.w,
            r3_y + pt(17.5),
            7.5,
            0,
            "[ Auto-Crop ]",
        );

        // Apply button
        let apply_btn = Rect::new(w - pad - pt(60.0), r3_y, pt(60.0), r3_h);
        p.rect(apply_btn, 0);
        p.text_center_in(
            apply_btn.x,
            apply_btn.x + apply_btn.w,
            r3_y + pt(17.5),
            8.0,
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

        let top_h = pt(26.0);
        let panel_h = pt(92.0);
        let panel_y = h - panel_h;
        let pad = pt(6.0);
        let avail_w = w - 2 * pad;

        match g {
            Gesture::Tap { .. } => {
                // Top Close button
                let close_btn = Rect::new(pt(8.0), pt(4.0), pt(60.0), pt(18.0));
                if close_btn.contains(vx, vy) {
                    return Action::Pop;
                }

                // Bottom Panel Interactions
                if vy >= panel_y {
                    // Row 1: Mode & Page Navigation
                    let r1_y = panel_y + pt(5.0);
                    let r1_h = pt(22.0);
                    if vy >= r1_y && vy < r1_y + r1_h + pt(4.0) {
                        let mode_uni_btn = Rect::new(pad, r1_y, pt(50.0), r1_h);
                        let mode_oe_btn = Rect::new(pad + pt(53.0), r1_y, pt(56.0), r1_h);
                        if mode_uni_btn.contains(vx, vy) {
                            self.settings.split.mirror_even_odd = false;
                            return Action::Redraw;
                        }
                        if mode_oe_btn.contains(vx, vy) {
                            self.settings.split.mirror_even_odd = true;
                            return Action::Redraw;
                        }

                        let nav_w = (avail_w - pt(113.0) - pt(4.0)) / 2;
                        let btn1_x = pad + pt(113.0);
                        let btn2_x = btn1_x + nav_w + pt(4.0);

                        if self.settings.split.mirror_even_odd {
                            let (odd_pno, even_pno) = if (self.view_page_no + 1) % 2 == 1 {
                                (
                                    self.view_page_no,
                                    (self.view_page_no + 1).min(self.total.saturating_sub(1)),
                                )
                            } else {
                                (
                                    self.view_page_no.saturating_sub(1),
                                    self.view_page_no,
                                )
                            };
                            let odd_btn = Rect::new(btn1_x, r1_y, nav_w, r1_h);
                            let even_btn = Rect::new(btn2_x, r1_y, nav_w, r1_h);
                            if odd_btn.contains(vx, vy) {
                                self.view_page_no = odd_pno;
                                return Action::Redraw;
                            }
                            if even_btn.contains(vx, vy) {
                                self.view_page_no = even_pno;
                                return Action::Redraw;
                            }
                        } else {
                            let prev_btn = Rect::new(btn1_x, r1_y, nav_w, r1_h);
                            let next_btn = Rect::new(btn2_x, r1_y, nav_w, r1_h);
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
                    let r2_y = panel_y + pt(32.0);
                    let r2_h = pt(22.0);
                    if vy >= r2_y && vy < r2_y + r2_h + pt(4.0) {
                        let edge_btn_w = (avail_w - pt(12.0)) / 5;
                        let idx = ((vx - pad) / (edge_btn_w + pt(3.0))).clamp(0, 4) as usize;
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

                    // Row 3: Action Buttons & Precision Nudge
                    let r3_y = panel_y + pt(59.0);
                    let r3_h = pt(26.0);
                    if vy >= r3_y {
                        let m_btn = Rect::new(pad, r3_y, pt(26.0), r3_h);
                        let p_btn = Rect::new(pad + pt(64.0), r3_y, pt(26.0), r3_h);
                        let auto_btn = Rect::new(pad + pt(94.0), r3_y, pt(72.0), r3_h);
                        let apply_btn = Rect::new(w - pad - pt(60.0), r3_y, pt(60.0), r3_h);

                        if m_btn.contains(vx, vy) {
                            self.nudge(-0.01);
                            return Action::Redraw;
                        }
                        if p_btn.contains(vx, vy) {
                            self.nudge(0.01);
                            return Action::Redraw;
                        }
                        if auto_btn.contains(vx, vy) {
                            self.run_auto_crop();
                            return Action::Redraw;
                        }
                        if apply_btn.contains(vx, vy) {
                            return self.apply_crop();
                        }
                    }
                }

                // Tapping inside the page preview area -> selects nearest guide line
                let prev_x = pt(8.0);
                let prev_y = top_h + pt(2.0);
                let prev_w = w - pt(16.0);
                let prev_h = panel_y - prev_y - pt(4.0);

                let (pw, ph) = if let Some(doc) = &self.doc {
                    if let Ok(page) = doc.load_page(self.view_page_no as i32) {
                        if let Ok(b) = page.bounds() {
                            (b.x1 - b.x0, b.y1 - b.y0)
                        } else {
                            (prev_w as f32, prev_h as f32)
                        }
                    } else {
                        (prev_w as f32, prev_h as f32)
                    }
                } else {
                    (prev_w as f32, prev_h as f32)
                };

                let zoom = (prev_w as f32 / pw).min(prev_h as f32 / ph);
                let rw = (pw * zoom).round() as i32;
                let rh = (ph * zoom).round() as i32;
                let page_ox = prev_x + (prev_w - rw) / 2;
                let page_oy = prev_y + (prev_h - rh) / 2;

                let s = &self.settings.split;
                let is_odd = (self.view_page_no + 1) % 2 == 1;
                let (ml, mr) = if s.mirror_even_odd && is_odd {
                    (s.margin_right, s.margin_left)
                } else {
                    (s.margin_left, s.margin_right)
                };
                let x0 = page_ox + (ml * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
                let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

                if (vy - y0).abs() < 40 {
                    self.active_edge = ActiveEdge::Top;
                } else if (vy - y1).abs() < 40 {
                    self.active_edge = ActiveEdge::Bottom;
                } else if (vx - x0).abs() < 40 {
                    self.active_edge = ActiveEdge::Left;
                } else if (vx - x1).abs() < 40 {
                    self.active_edge = ActiveEdge::Right;
                }
                Action::Redraw
            }

            Gesture::LongPress { .. } => {
                // Direct drag initiation: detect closest edge
                let prev_x = pt(8.0);
                let prev_y = top_h + pt(2.0);
                let prev_w = w - pt(16.0);
                let prev_h = panel_y - prev_y - pt(4.0);

                let (pw, ph) = if let Some(doc) = &self.doc {
                    if let Ok(page) = doc.load_page(self.view_page_no as i32) {
                        if let Ok(b) = page.bounds() {
                            (b.x1 - b.x0, b.y1 - b.y0)
                        } else {
                            (prev_w as f32, prev_h as f32)
                        }
                    } else {
                        (prev_w as f32, prev_h as f32)
                    }
                } else {
                    (prev_w as f32, prev_h as f32)
                };

                let zoom = (prev_w as f32 / pw).min(prev_h as f32 / ph);
                let rw = (pw * zoom).round() as i32;
                let rh = (ph * zoom).round() as i32;
                let page_ox = prev_x + (prev_w - rw) / 2;
                let page_oy = prev_y + (prev_h - rh) / 2;

                let s = &self.settings.split;
                let is_odd = (self.view_page_no + 1) % 2 == 1;
                let (ml, mr) = if s.mirror_even_odd && is_odd {
                    (s.margin_right, s.margin_left)
                } else {
                    (s.margin_left, s.margin_right)
                };
                let x0 = page_ox + (ml * rw as f32).round() as i32;
                let y0 = page_oy + (s.margin_top * rh as f32).round() as i32;
                let x1 = page_ox + ((1.0 - mr) * rw as f32).round() as i32;
                let y1 = page_oy + ((1.0 - s.margin_bottom) * rh as f32).round() as i32;

                if (vy - y0).abs() < 50 {
                    self.drag_edge = Some(ActiveEdge::Top);
                    self.active_edge = ActiveEdge::Top;
                } else if (vy - y1).abs() < 50 {
                    self.drag_edge = Some(ActiveEdge::Bottom);
                    self.active_edge = ActiveEdge::Bottom;
                } else if (vx - x0).abs() < 50 {
                    self.drag_edge = Some(ActiveEdge::Left);
                    self.active_edge = ActiveEdge::Left;
                } else if (vx - x1).abs() < 50 {
                    self.drag_edge = Some(ActiveEdge::Right);
                    self.active_edge = ActiveEdge::Right;
                }
                Action::Redraw
            }

            Gesture::Drag { .. } => {
                // Direct dragging of dashed margin lines
                if let Some(edge) = self.drag_edge {
                    let prev_x = pt(8.0);
                    let prev_y = top_h + pt(2.0);
                    let prev_w = w - pt(16.0);
                    let prev_h = panel_y - prev_y - pt(4.0);

                    let (pw, ph) = if let Some(doc) = &self.doc {
                        if let Ok(page) = doc.load_page(self.view_page_no as i32) {
                            if let Ok(b) = page.bounds() {
                                (b.x1 - b.x0, b.y1 - b.y0)
                            } else {
                                (prev_w as f32, prev_h as f32)
                            }
                        } else {
                            (prev_w as f32, prev_h as f32)
                        }
                    } else {
                        (prev_w as f32, prev_h as f32)
                    };

                    let zoom = (prev_w as f32 / pw).min(prev_h as f32 / ph);
                    let rw = (pw * zoom).round() as i32;
                    let rh = (ph * zoom).round() as i32;
                    let page_ox = prev_x + (prev_w - rw) / 2;
                    let page_oy = prev_y + (prev_h - rh) / 2;

                    let is_odd = (self.view_page_no + 1) % 2 == 1;
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
                            if s.mirror_even_odd && is_odd {
                                s.margin_right = val;
                            } else {
                                s.margin_left = val;
                            }
                        }
                        ActiveEdge::Right => {
                            let val = ((page_ox + rw - vx) as f32 / rw as f32).clamp(0.0, 0.40);
                            if s.mirror_even_odd && is_odd {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_sample_crop_preview() {
        let path = std::env::temp_dir().join("sample.pdf");
        if !path.exists() {
            return;
        }

        let doc = std::rc::Rc::new(mupdf::Document::open(path.as_os_str()).unwrap());
        let total = doc.page_count().unwrap() as usize;

        let font = yui::font::Font::load().unwrap();
        let mut settings = ReaderSettings::default();
        settings.split.preset = crate::split::SplitPreset::FitPage;

        // 1. Run detect_even_odd_margins on page 10
        let (ml, mt, mr, mb) = crate::render::detect_even_odd_margins(doc.as_ref(), 10).unwrap();
        println!("DETECTED EVEN/ODD MARGINS on sample.pdf: ml_even={ml:.4}, mt={mt:.4}, mr_even={mr:.4}, mb={mb:.4}");
        settings.split.margin_left = ml;
        settings.split.margin_top = mt;
        settings.split.margin_right = mr;
        settings.split.margin_bottom = mb;
        settings.split.mirror_even_odd = true;

        let out_dir = std::env::temp_dir().join("yb-crop-preview");
        std::fs::create_dir_all(&out_dir).unwrap();

        // 2. Render Crop Studio on Odd Page (Page 11, index 10)
        let mut dialog = CropDialog::new(
            "sample.pdf".to_string(),
            10,
            0,
            total,
            settings,
            Some(doc.clone()),
        );
        dialog.view_page_no = 10;

        let mut canvas = vec![0u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        {
            let mut p = yui::Painter::new(
                &mut panel,
                1236,
                1648,
                1248,
                Orientation::Portrait,
                &mut canvas,
                &font,
            );
            dialog.draw(&mut p);
        }

        let file = std::fs::File::create(format!("{}/crop_studio_odd_p11.png", out_dir.display())).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&canvas).unwrap();

        // 3. Render Crop Studio on Even Page (Page 12, index 11)
        dialog.view_page_no = 11;
        {
            let mut p = yui::Painter::new(
                &mut panel,
                1236,
                1648,
                1248,
                Orientation::Portrait,
                &mut canvas,
                &font,
            );
            dialog.draw(&mut p);
        }

        let file = std::fs::File::create(format!("{}/crop_studio_even_p12.png", out_dir.display())).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&canvas).unwrap();

        // 4. Render Auto-Cropped Reader view for Page 11 and Page 12
        for pno in [10, 11] {
            let gray = crate::render::render_page(doc.as_ref(), pno, 0, &settings, 1236, 1648).unwrap();
            let file = std::fs::File::create(format!("{}/reader_cropped_p{}.png", out_dir.display(), pno + 1)).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&gray).unwrap();
        }
        println!("Rendered crop studio and reader previews to {}", out_dir.display());
    }

    #[test]
    fn test_crop_studio_buttons_geometry_never_overlap() {
        let (w, _h) = (1236, 1648);
        let pad = pt(6.0);
        let avail_w = w - 2 * pad;

        // Row 1: Mode buttons
        let mode_uni = Rect::new(pad, 0, pt(50.0), pt(22.0));
        let mode_oe = Rect::new(pad + pt(53.0), 0, pt(56.0), pt(22.0));
        assert!(mode_uni.x + mode_uni.w < mode_oe.x);

        let nav_w = (avail_w - pt(113.0) - pt(4.0)) / 2;
        let btn1 = Rect::new(pad + pt(113.0), 0, nav_w, pt(22.0));
        let btn2 = Rect::new(pad + pt(113.0) + nav_w + pt(4.0), 0, nav_w, pt(22.0));
        assert!(mode_oe.x + mode_oe.w < btn1.x);
        assert!(btn1.x + btn1.w < btn2.x);
        assert!(btn2.x + btn2.w <= w - pad);

        // Row 2: 5 tabs
        let edge_btn_w = (avail_w - pt(12.0)) / 5;
        for i in 0..4 {
            let bx1 = pad + i as i32 * (edge_btn_w + pt(3.0));
            let bx2 = pad + (i + 1) as i32 * (edge_btn_w + pt(3.0));
            assert!(bx1 + edge_btn_w < bx2);
        }

        // Row 3: Nudge + Auto + Apply
        let m_btn = Rect::new(pad, 0, pt(26.0), pt(26.0));
        let pct_box = Rect::new(pad + pt(29.0), 0, pt(32.0), pt(26.0));
        let p_btn = Rect::new(pad + pt(64.0), 0, pt(26.0), pt(26.0));
        let auto_btn = Rect::new(pad + pt(94.0), 0, pt(72.0), pt(26.0));
        let apply_btn = Rect::new(w - pad - pt(60.0), 0, pt(60.0), pt(26.0));

        assert!(m_btn.x + m_btn.w < pct_box.x);
        assert!(pct_box.x + pct_box.w < p_btn.x);
        assert!(p_btn.x + p_btn.w < auto_btn.x);
        assert!(auto_btn.x + auto_btn.w < apply_btn.x);
        assert!(apply_btn.x + apply_btn.w <= w - pad);
    }
}
