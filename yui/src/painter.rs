//! Immediate-mode drawing onto any grayscale buffer. The Painter does not
//! own the framebuffer — App builds one over `panel.buf_mut()` per draw,
//! and tests build one over a `Vec<u8>` — so every screen renders
//! headlessly on the dev machine.
//!
//! All text/spacing units at this layer are POINTS (the unit reader UIs are
//! authored in); `PX` converts to this panel's 300 dpi pixels.

use crate::font::Font;

/// Points -> pixels on the 300 dpi panel.
pub const PX: f32 = 300.0 / 72.0;

pub fn pt(p: f32) -> i32 {
    (p * PX) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn clip_to(&self, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        let x0 = self.x.max(0);
        let y0 = self.y.max(0);
        let x1 = (self.x + self.w).min(w);
        let y1 = (self.y + self.h).min(h);
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some((x0, y0, x1, y1))
        }
    }
}

pub struct Painter<'a> {
    buf: &'a mut [u8],
    w: i32,
    h: i32,
    stride: usize,
    font: &'a Font,
}

impl<'a> Painter<'a> {
    /// `buf` must be at least `stride * h` bytes.
    pub fn new(buf: &'a mut [u8], w: u32, h: u32, stride: usize, font: &'a Font) -> Painter<'a> {
        Painter {
            buf,
            w: w as i32,
            h: h as i32,
            stride,
            font,
        }
    }

    /// Drawable size in pixels (gesture coordinates live in this space).
    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    /// Drawable size in points (for authoring layout).
    pub fn width_pt(&self) -> f32 {
        self.w as f32 / PX
    }

    pub fn height_pt(&self) -> f32 {
        self.h as f32 / PX
    }

    pub fn clear(&mut self, v: u8) {
        let len = self.buf.len().min(self.stride * self.h as usize);
        self.buf[..len].fill(v);
    }

    /// Text with the baseline at y. All geometry helpers below take pt.
    pub fn text(&mut self, x: i32, y: i32, size_pt: f32, color: u8, text: &str) {
        let font = &self.font;
        font.draw(self.buf, self.stride, x, y, size_pt * PX, color, text);
    }

    pub fn text_center(&mut self, y: i32, size_pt: f32, color: u8, text: &str) {
        let tw = self.text_width(size_pt, text) as i32;
        let x = (self.w - tw) / 2;
        self.text(x, y, size_pt, color, text);
    }

    /// Centered within [x0, x1) — for cells (nav tabs, list columns).
    pub fn text_center_in(&mut self, x0: i32, x1: i32, y: i32, size_pt: f32, color: u8, text: &str) {
        let tw = self.text_width(size_pt, text) as i32;
        let cx = (x0 + x1) / 2;
        self.text(cx - tw / 2, y, size_pt, color, text);
    }

    /// Right-aligned so the text ends at x.
    pub fn text_right(&mut self, x: i32, y: i32, size_pt: f32, color: u8, text: &str) {
        let tw = self.text_width(size_pt, text) as i32;
        self.text(x - tw, y, size_pt, color, text);
    }

    /// Text advance width in PIXELS (fontdue rasterizes in pixels).
    pub fn text_width(&self, size_pt: f32, text: &str) -> f32 {
        self.font.text_width(size_pt * PX, text)
    }

    /// Truncate with "…" to fit `max_w_pt` points.
    pub fn truncate(&self, size_pt: f32, text: &str, max_w_pt: f32) -> String {
        self.font
            .truncate(size_pt * PX, text, max_w_pt * PX)
    }

    pub fn rect(&mut self, r: Rect, color: u8) {
        if let Some((x0, y0, x1, y1)) = r.clip_to(self.w, self.h) {
            for y in y0..y1 {
                let row = y as usize * self.stride;
                self.buf[row + x0 as usize..row + x1 as usize].fill(color);
            }
        }
    }

    /// Horizontal rule `t` px thick. At 300 dpi, 1px lines are ~0.09mm
    /// and effectively invisible on e-ink — UI separators want 2-3px.
    pub fn hline_t(&mut self, y: i32, x0: i32, x1: i32, t: i32, color: u8) {
        self.rect(Rect::new(x0.min(x1), y, (x1 - x0).abs() + 1, t.max(1)), color);
    }

    /// Outline (frame) `t` px thick around a rect.
    pub fn rect_outline_t(&mut self, r: Rect, t: i32, color: u8) {
        let t = t.max(1);
        self.rect(Rect::new(r.x, r.y, r.w, t), color);
        self.rect(Rect::new(r.x, r.y + r.h.saturating_sub(t), r.w, t), color);
        self.rect(Rect::new(r.x, r.y, t, r.h), color);
        self.rect(Rect::new(r.x + r.w.saturating_sub(t), r.y, t, r.h), color);
    }

    /// Bresenham line with `t` px stroke (each point stamps a t×t block),
    /// clipped at the panel. Use t >= 2 for visible strokes at 300 dpi.
    pub fn line_w(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, color: u8) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let t = t.max(1);
        loop {
            self.rect(Rect::new(x - t / 2, y - t / 2, t, t), color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Slider bar: light-gray track with a black fill for `frac` of it.
    pub fn bar(&mut self, r: Rect, frac: f32) {
        self.rect(r, 200);
        let fw = (r.w as f32 * frac.clamp(0.0, 1.0)) as i32;
        if fw > 0 {
            self.rect(Rect::new(r.x, r.y, fw, r.h), 0);
        }
    }

    /// Copy a `w x h` grayscale block (row stride `src_stride`) into the
    /// buffer at (x, y). The reader's page cache is stride==width while the
    /// framebuffer is stride 1248 for 1236 visible px — the copy is per-row.
    pub fn blit_gray(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u8], src_stride: usize) {
        for row in 0..h {
            let dy = y + row;
            if dy < 0 || dy >= self.h {
                continue;
            }
            let dx0 = x.max(0);
            let dx1 = (x + w).min(self.w);
            if dx1 <= dx0 {
                continue;
            }
            let count = (dx1 - dx0) as usize;
            let skip = (dx0 - x) as usize;
            let dst = dy as usize * self.stride + dx0 as usize;
            let s = row as usize * src_stride + skip;
            if s + count <= src.len() && dst + count <= self.buf.len() {
                self.buf[dst..dst + count].copy_from_slice(&src[s..s + count]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> Font {
        Font::load().unwrap()
    }

    fn painter<'a>(buf: &'a mut [u8], f: &'a Font) -> Painter<'a> {
        // fb-like geometry: 1236 visible px, stride 1248.
        Painter::new(buf, 1236, 1648, 1248, f)
    }

    #[test]
    fn rect_fills_exactly_its_bounds() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 1648];
        {
            let mut p = painter(&mut buf, &f);
            p.rect(Rect::new(10, 20, 30, 40), 0);
        }
        for y in 0..1648 {
            for x in 0..1248 {
                let inside = x >= 10 && x < 40 && y >= 20 && y < 60;
                let want = if inside { 0 } else { 255 };
                assert_eq!(buf[y * 1248 + x], want, "at {x},{y}");
            }
        }
    }

    #[test]
    fn rect_clips_to_panel() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 1648];
        {
            let mut p = painter(&mut buf, &f);
            // Overhangs on every side; must not panic and must not wrap rows.
            p.rect(Rect::new(-10, -10, 1300, 1700), 0);
        }
        // Visible area fully black, right-edge stride padding untouched.
        for y in 0..1648 {
            assert_eq!(buf[y * 1248 + 1235], 0);
            assert_eq!(buf[y * 1248 + 1236], 255, "stride padding must stay clean");
        }
    }

    #[test]
    fn bar_fill_widths() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 100];
        {
            let mut p = Painter::new(&mut buf, 1236, 100, 1248, &f);
            p.bar(Rect::new(0, 0, 100, 10), 0.0);
            p.bar(Rect::new(0, 10, 100, 10), 0.5);
            p.bar(Rect::new(0, 20, 100, 10), 1.0);
        }
        assert_eq!(buf[5], 200, "frac 0: track only");
        assert_eq!(buf[10 * 1248 + 49], 0, "frac 0.5: half filled");
        assert_eq!(buf[10 * 1248 + 51], 200, "frac 0.5: second half track");
        assert_eq!(buf[20 * 1248 + 99], 0, "frac 1: fully filled");
    }

    #[test]
    fn blit_gray_handles_src_stride_ne_dst_stride() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 10];
        // Source: 8 rows of 10 px, stride 10 (tight), each row one flat value.
        let src: Vec<u8> = (0..8).flat_map(|r| vec![r + 1; 10]).collect();
        {
            let mut p = Painter::new(&mut buf, 1236, 10, 1248, &f);
            p.blit_gray(100, 2, 10, 8, &src, 10);
        }
        for r in 0..8 {
            assert_eq!(buf[(2 + r) * 1248 + 105], (r + 1) as u8, "row {r}");
        }
        // Neighbors untouched.
        assert_eq!(buf[2 * 1248 + 99], 255);
        assert_eq!(buf[2 * 1248 + 110], 255);
        assert_eq!(buf[1 * 1248 + 105], 255);
    }

    #[test]
    fn blit_gray_skips_negative_x_prefix() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 4];
        let src = vec![7u8; 10];
        {
            let mut p = Painter::new(&mut buf, 1236, 4, 1248, &f);
            // x=-4: the first 4 src columns fall off-panel.
            p.blit_gray(-4, 0, 10, 1, &src, 10);
        }
        // Visible part is src cols 4..10 landing at dst x 0..6.
        assert_eq!(buf[0], 7, "first on-panel px copied");
        assert_eq!(buf[3], 7, "visible part copied");
        assert_eq!(buf[5], 7, "last on-panel px copied");
        assert_eq!(buf[6], 255, "past the src width untouched");
        assert_eq!(buf[9], 255, "well past untouched");
    }

    #[test]
    fn text_center_places_glyphs_symmetrically() {
        let f = font();
        let mut buf = vec![255u8; 1248 * 60];
        // NB: Font::draw drops a glyph WHOLESALE if its top would be above
        // row 0 (gy < 0 guard) — a 14pt cap glyph is ~42px tall, so the
        // baseline must sit at least that low. Real screens never draw
        // this close to the edge.
        let (tw, ink) = {
            let mut p = Painter::new(&mut buf, 1236, 60, 1248, &f);
            p.text_center(52, 14.0, 0, "W");
            let tw = p.text_width(14.0, "W") as i32;
            let mut first = None;
            let mut last = None;
            for y in 0..60 {
                for x in 0..1236 {
                    if buf[y * 1248 + x] != 255 {
                        first.get_or_insert(x);
                        last = Some(x);
                    }
                }
            }
            (tw, (first.unwrap(), last.unwrap()))
        };
        let cx = 1236 / 2;
        let a = ink.0 as i32 - (cx - tw / 2);
        let b = (cx + tw / 2) - ink.1 as i32;
        // Centering positions the ADVANCE box around the panel center; the
        // glyph's own side bearings ('W' at 58px: ~0 left, ~12 right) keep
        // the ink inside it. Both must be non-negative and modest.
        assert!(
            (0..=14).contains(&a) && (0..=14).contains(&b),
            "tw={tw} ink={ink:?} a={a} b={b}"
        );
    }

    #[test]
    fn truncate_fits_and_terminates() {
        let f = font();
        let mut buf = vec![255u8; 16];
        let out = {
            let p = Painter::new(&mut buf, 16, 1, 16, &f);
            p.truncate(10.0, "abcdefgh", 30.0)
        };
        assert!(out.ends_with('…') || out == "abcdefgh");
        let w = {
            let p = Painter::new(&mut buf, 16, 1, 16, &f);
            p.text_width(10.0, &out) / super::PX
        };
        // Font::truncate cuts when a candidate exceeds max-12px, then
        // appends the ellipsis — inherited slack of up to ~12px (~3pt).
        assert!(w <= 30.0 + 3.0, "truncated width {w}pt must fit ~30pt");
    }

    #[test]
    fn contains_and_pt_roundtrip() {
        let r = Rect::new(10, 10, 5, 5);
        assert!(r.contains(10, 10));
        assert!(r.contains(14, 14));
        assert!(!r.contains(15, 10));
        assert!(!r.contains(9, 10));
        // 72 pt = 1 inch = 300 px.
        assert_eq!(pt(72.0), 300);
    }
}
