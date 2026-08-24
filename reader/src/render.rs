//! Page rendering and text-geometry extraction for the reader screen.
//!
//! Two jobs that must agree on one coordinate system:
//! - [`render_page`] rasterizes a (page, sub-box) to VISUAL-space
//!   grayscale pixels — the caller's w/h are the visual dims (landscape
//!   means `Orientation` already swapped them; rotation is not this
//!   module's business, Painter::flush owns it);
//! - [`words_from_text_page`] / [`links_from_page`] map MuPDF text
//!   coordinates onto the SAME visual space, so selection spans, vocab
//!   annotations and link hit-boxes land on what is actually painted.
//!
//! The shared math lives in [`LayoutGeom`]: one source for zoom, offsets
//! and the sub-box crop. Before this module existed it was pasted three
//! times (pixel render, word walk, link walk) and the rotation transforms
//! three more — the exact duplication that made landscape behavior
//! impossible to test on the host.

use mupdf::{Colorspace, Document, Matrix};

use crate::split::{ReaderSettings, RectF};

pub const HEADER_H: u32 = 98; // px (covers clock/battery status header with clean top margin)
pub const FOOTER_H: u32 = 72; // px (covers progress track and page number without text overlap)
pub const TEXT_AA_LEVEL: i32 = 8;

/// Reading-area geometry for one (settings, page bounds, sub_idx, visual
/// dims) combination. Everything downstream — pixel placement and text
/// coordinate mapping — derives from these fields, never from its own
/// copy of the formulas.
pub struct LayoutGeom {
    /// Normalized sub-box of the page this view renders (split mode).
    pub sub_box: RectF,
    /// Full page size in document units.
    pub pw: f32,
    pub ph: f32,
    /// Page-box origin in the coordinate space `bounds` reports. mupdf
    /// normalizes the page CTM, so this is normally 0 — bounds AND text
    /// quads both come back origin-relative. Kept explicit so to_screen
    /// stays correct in whatever space the page reports, rather than
    /// assuming the normalization.
    bx0: f32,
    by0: f32,
    /// Document→screen scale, fit inside the reading area.
    pub zoom: f32,
    /// Screen offset (px) of the rendered box inside the reading area.
    pub vis_ox: usize,
    pub vis_oy: usize,
}

impl LayoutGeom {
    /// `w`/`h` are the VISUAL dims of the target buffer.
    #[allow(dead_code)]
    pub fn new(
        settings: &ReaderSettings,
        bounds: mupdf::Rect,
        sub_idx: usize,
        w: u32,
        h: u32,
    ) -> Option<LayoutGeom> {
        Self::new_for_page(settings, bounds, 0, sub_idx, w, h)
    }

    /// `w`/`h` are the VISUAL dims of the target buffer, with page number awareness for even/odd margin mirroring.
    pub fn new_for_page(
        settings: &ReaderSettings,
        bounds: mupdf::Rect,
        page_no: usize,
        sub_idx: usize,
        w: u32,
        h: u32,
    ) -> Option<LayoutGeom> {
        let pw = bounds.x1 - bounds.x0;
        let ph = bounds.y1 - bounds.y0;
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }

        let header_h = if settings.show_header { HEADER_H } else { 0 };
        let footer_h = FOOTER_H;
        let (vis_w, vis_h) = (w as f32, (h.saturating_sub(footer_h + header_h)) as f32);

        let config = &settings.split;
        let sub_boxes = config.sub_boxes_for_page(page_no);
        let sub_box = sub_boxes
            .get(sub_idx)
            .copied()
            .unwrap_or(RectF::new(0.0, 0.0, 1.0, 1.0));

        let bw = sub_box.width() * pw;
        let bh = sub_box.height() * ph;
        let zoom = (vis_w / bw).min(vis_h / bh);

        let rw = (bw * zoom).round() as usize;
        let rh = (bh * zoom).round() as usize;

        let (vis_ox, vis_oy) = Self::offsets(w as usize, vis_h as usize, header_h, rw, rh);

        Some(LayoutGeom {
            sub_box,
            pw,
            ph,
            bx0: bounds.x0,
            by0: bounds.y0,
            zoom,
            vis_ox,
            vis_oy,
        })
    }

    /// Screen offsets for a box of (rw, rh) px centered in the reading area of a (vis_w × vis_h) buffer.
    pub fn offsets(
        vis_w: usize,
        vis_h: usize,
        header_h: u32,
        rw: usize,
        rh: usize,
    ) -> (usize, usize) {
        let ox = vis_w.saturating_sub(rw) / 2;
        let oy = vis_h.saturating_sub(rh) / 2 + header_h as usize;
        (ox, oy)
    }

    /// Map a document-space rect inside the current sub-box to visual
    /// (screen) coordinates. Input is ABSOLUTE document space (what
    /// text-page quads and link bounds arrive in); the rendered box
    /// starts at the page-box origin, so it is subtracted here — a
    /// non-origin CropBox used to shift words off their ink by
    /// `bx0 * zoom`.
    fn to_screen(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (f32, f32, f32, f32) {
        let ox = self.bx0 + self.sub_box.x0 * self.pw;
        let oy = self.by0 + self.sub_box.y0 * self.ph;
        let sx0 = self.vis_ox as f32 + (x0 - ox) * self.zoom;
        let sy0 = self.vis_oy as f32 + (y0 - oy) * self.zoom;
        let sx1 = self.vis_ox as f32 + (x1 - ox) * self.zoom;
        let sy1 = self.vis_oy as f32 + (y1 - oy) * self.zoom;
        (sx0, sy0, sx1, sy1)
    }

    /// Visual-space rect for a document-space rect. What is painted and
    /// what is hit-tested share this one mapping.
    pub fn to_visual(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> RectF {
        let (sx0, sy0, sx1, sy1) = self.to_screen(x0, y0, x1, y1);
        RectF::new(sx0.min(sx1), sy0.min(sy1), sx0.max(sx1), sy0.max(sy1))
    }

    /// Visual rect of the sub-box itself, in absolute document units
    /// (test helper: maps exactly the region this sub-page renders).
    #[cfg(test)]
    pub fn sub_rect_doc(&self) -> (f32, f32, f32, f32) {
        (
            self.bx0 + self.sub_box.x0 * self.pw,
            self.by0 + self.sub_box.y0 * self.ph,
            self.bx0 + self.sub_box.x1 * self.pw,
            self.by0 + self.sub_box.y1 * self.ph,
        )
    }
}

/// Extract words with visual-space bounding boxes from a text page, in
/// reading order. Words accumulate per line until whitespace; the
/// end-of-line remainder is flushed too. This loop was previously pasted
/// twice in compute_annotations and once in tests.
pub fn words_from_text_page(tp: &mupdf::TextPage, g: &LayoutGeom) -> Vec<(String, RectF)> {
    let mut words = Vec::new();
    let flush = |cur: &mut String,
                 min_x: &mut f32,
                 min_y: &mut f32,
                 max_x: &mut f32,
                 max_y: &mut f32,
                 out: &mut Vec<(String, RectF)>| {
        if cur.is_empty() {
            return;
        }
        let r = g.to_visual(*min_x, *min_y, *max_x, *max_y);
        out.push((std::mem::take(cur), r));
        *min_x = f32::MAX;
        *min_y = f32::MAX;
        *max_x = f32::MIN;
        *max_y = f32::MIN;
    };

    for block in tp.blocks() {
        for line in block.lines() {
            let mut cur = String::new();
            let (mut min_x, mut min_y, mut max_x, mut max_y) =
                (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for ch in line.chars() {
                if let Some(c) = ch.char() {
                    if c.is_whitespace() {
                        flush(
                            &mut cur, &mut min_x, &mut min_y, &mut max_x, &mut max_y, &mut words,
                        );
                    } else {
                        cur.push(c);
                        let q = ch.quad();
                        min_x = min_x.min(q.ul.x).min(q.ll.x);
                        min_y = min_y.min(q.ul.y).min(q.ur.y);
                        max_x = max_x.max(q.ur.x).max(q.lr.x);
                        max_y = max_y.max(q.ll.y).max(q.lr.y);
                    }
                }
            }
            flush(
                &mut cur, &mut min_x, &mut min_y, &mut max_x, &mut max_y, &mut words,
            );
        }
    }
    words
}

/// Interactive links on this page with visual-space hit boxes.
pub fn links_from_page(page: &mupdf::Page, g: &LayoutGeom) -> Vec<(RectF, String)> {
    let mut out = Vec::new();
    if let Ok(links) = page.links() {
        for l in links {
            let r = g.to_visual(l.bounds.x0, l.bounds.y0, l.bounds.x1, l.bounds.y1);
            out.push((r, l.uri));
        }
    }
    out
}

/// Auto-detect content margins for a single PDF page based on text & ink bounding box.
/// Returns `(margin_left, margin_top, margin_right, margin_bottom)` as fractions in 0.0..0.40.
pub fn detect_page_margins(page: &mupdf::Page, pad_pt: f32) -> Option<(f32, f32, f32, f32)> {
    let bounds = page.bounds().ok()?;
    let pw = bounds.x1 - bounds.x0;
    let ph = bounds.y1 - bounds.y0;
    if pw <= 1.0 || ph <= 1.0 {
        return None;
    }

    let tp = page.to_text_page(mupdf::TextPageFlags::empty()).ok()?;
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut found = false;

    for block in tp.blocks() {
        for line in block.lines() {
            for ch in line.chars() {
                if let Some(c) = ch.char() {
                    if !c.is_whitespace() {
                        found = true;
                        let q = ch.quad();
                        min_x = min_x.min(q.ul.x).min(q.ll.x);
                        min_y = min_y.min(q.ul.y).min(q.ur.y);
                        max_x = max_x.max(q.ur.x).max(q.lr.x);
                        max_y = max_y.max(q.ll.y).max(q.lr.y);
                    }
                }
            }
        }
    }

    // If text page was empty (e.g. scanned page or vector diagram), scan downscaled pixmap
    if !found {
        let mut m = Matrix::IDENTITY;
        let scale = 300.0 / pw.max(ph);
        m.scale(scale, scale);
        if let Ok(pm) = page.to_pixmap(&m, &Colorspace::device_gray(), false, true) {
            let pm_w = pm.width() as usize;
            let pm_h = pm.height() as usize;
            let samples = pm.samples();
            let mut p_min_x = pm_w;
            let mut p_max_x = 0;
            let mut p_min_y = pm_h;
            let mut p_max_y = 0;
            let mut ink_found = false;

            for y in 0..pm_h {
                for x in 0..pm_w {
                    let val = samples[y * pm.stride() as usize + x];
                    if val < 235 {
                        ink_found = true;
                        p_min_x = p_min_x.min(x);
                        p_max_x = p_max_x.max(x);
                        p_min_y = p_min_y.min(y);
                        p_max_y = p_max_y.max(y);
                    }
                }
            }
            if ink_found && p_max_x > p_min_x && p_max_y > p_min_y {
                let ml = ((p_min_x as f32 / pm_w as f32) - 0.02).clamp(0.0, 0.40);
                let mr = (1.0 - (p_max_x as f32 / pm_w as f32) - 0.02).clamp(0.0, 0.40);
                let mt = ((p_min_y as f32 / pm_h as f32) - 0.02).clamp(0.0, 0.40);
                let mb = (1.0 - (p_max_y as f32 / pm_h as f32) - 0.02).clamp(0.0, 0.40);
                return Some((ml, mt, mr, mb));
            }
        }
        return None;
    }

    if max_x > min_x && max_y > min_y {
        let ml = ((min_x - pad_pt - bounds.x0) / pw).clamp(0.0, 0.40);
        let mr = ((bounds.x1 - (max_x + pad_pt)) / pw).clamp(0.0, 0.40);
        let mt = ((min_y - pad_pt - bounds.y0) / ph).clamp(0.0, 0.40);
        let mb = ((bounds.y1 - (max_y + pad_pt)) / ph).clamp(0.0, 0.40);
        Some((ml, mt, mr, mb))
    } else {
        None
    }
}

/// Compute safe, book-wide auto-crop margins by sampling pages around `cur_page`.
/// Protects facing-page (odd/even) asymmetrical margins from text clipping.
pub fn detect_book_margins(doc: &Document, cur_page: usize) -> Option<(f32, f32, f32, f32)> {
    let total = doc.page_count().ok()?.max(0) as usize;
    if total == 0 {
        return None;
    }

    let mut samples = Vec::new();
    let mut indices = vec![cur_page];
    if cur_page + 1 < total {
        indices.push(cur_page + 1);
    }
    if cur_page > 0 {
        indices.push(cur_page - 1);
    }
    if total > 5 {
        indices.push(total / 4);
        indices.push(total / 2);
    }

    for pno in indices {
        if let Ok(page) = doc.load_page(pno as i32) {
            if let Some(m) = detect_page_margins(&page, 8.0) {
                samples.push(m);
            }
        }
    }

    if samples.is_empty() {
        return None;
    }

    let mut min_l = f32::MAX;
    let mut min_r = f32::MAX;
    let mut min_t = f32::MAX;
    let mut min_b = f32::MAX;

    for (ml, mt, mr, mb) in samples {
        min_l = min_l.min(ml);
        min_r = min_r.min(mr);
        min_t = min_t.min(mt);
        min_b = min_b.min(mb);
    }

    let lr_safe = min_l.min(min_r);
    Some((lr_safe, min_t, lr_safe, min_b))
}

/// Auto-detect facing-page (even/odd) margins by sampling even and odd body pages.
/// Returns `(even_margin_left, margin_top, even_margin_right, margin_bottom)` suited for `mirror_even_odd = true`.
pub fn detect_even_odd_margins(doc: &Document, cur_page: usize) -> Option<(f32, f32, f32, f32)> {
    let total = doc.page_count().ok()?.max(0) as usize;
    if total == 0 {
        return None;
    }

    let mut even_samples = Vec::new();
    let mut odd_samples = Vec::new();

    let mut sample_pages = Vec::new();
    for p in [
        cur_page,
        cur_page + 1,
        cur_page.saturating_sub(1),
        cur_page + 2,
        total / 4,
        total / 4 + 1,
        total / 2,
        total / 2 + 1,
    ] {
        if p < total && !sample_pages.contains(&p) {
            sample_pages.push(p);
        }
    }

    for pno in sample_pages {
        if let Ok(page) = doc.load_page(pno as i32) {
            if let Some(m) = detect_page_margins(&page, 8.0) {
                if (pno + 1) % 2 == 0 {
                    even_samples.push(m);
                } else {
                    odd_samples.push(m);
                }
            }
        }
    }

    if even_samples.is_empty() && odd_samples.is_empty() {
        return None;
    }

    let mut min_outer = f32::MAX;
    let mut min_inner = f32::MAX;
    let mut min_t = f32::MAX;
    let mut min_b = f32::MAX;

    for (ml, mt, mr, mb) in &even_samples {
        min_outer = min_outer.min(*ml);
        min_inner = min_inner.min(*mr);
        min_t = min_t.min(*mt);
        min_b = min_b.min(*mb);
    }

    for (ml, mt, mr, mb) in &odd_samples {
        // On odd pages: Left is inner margin (spine), Right is outer margin
        min_inner = min_inner.min(*ml);
        min_outer = min_outer.min(*mr);
        min_t = min_t.min(*mt);
        min_b = min_b.min(*mb);
    }

    if min_outer == f32::MAX {
        min_outer = min_inner;
    }
    if min_inner == f32::MAX {
        min_inner = min_outer;
    }
    if min_t == f32::MAX {
        min_t = 0.0;
    }
    if min_b == f32::MAX {
        min_b = 0.0;
    }

    Some((min_outer, min_t, min_inner, min_b))
}

/// Rasterize (page, sub_idx) to visual-sized grayscale over the shared
/// [`LayoutGeom`] math. Loads the page itself — for callers that only
/// need ink (crop preview, tests). The backend uses [`render_page_on`]
/// so one load feeds ink, words and links.
pub fn render_page(
    doc: &Document,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let page = match doc.load_page(page_no as i32) {
        Ok(p) => p,
        Err(e) => {
            ybdev::log::plog(&format!("render: load_page {}: {}", page_no, e));
            return None;
        }
    };
    let bounds = match page.bounds() {
        Ok(b) => b,
        Err(e) => {
            ybdev::log::plog(&format!("render: page {} bounds: {}", page_no, e));
            return None;
        }
    };
    let geom = LayoutGeom::new_for_page(settings, bounds, page_no, sub_idx, w, h)?;
    render_page_on(&page, &geom, sub_idx, settings, w, h)
}

/// Core rasterizer over an already-loaded page — the caller owns the
/// single `load_page` and the geometry it shares with the word/link
/// walkers.
pub fn render_page_on(
    page: &mupdf::Page,
    geom: &LayoutGeom,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    // Thread-local mupdf context: set on every call so any thread that
    // rasterizes picks the knob up (contexts are per-thread clones).
    mupdf::Context::get().set_text_aa_level(TEXT_AA_LEVEL);

    let mut m = Matrix::IDENTITY;
    m.scale(geom.zoom, geom.zoom);
    let pm = match page.to_pixmap(&m, &Colorspace::device_gray(), false, true) {
        Ok(pm) => pm,
        Err(e) => {
            ybdev::log::plog(&format!("render: to_pixmap failed: {}", e));
            return None;
        }
    };

    Some(slice_pixmap(&pm, geom, sub_idx, settings, w, h))
}

/// Slices a sub-box from an existing full-page pixmap and applies LUT/contrast,
/// enabling instant (<3ms) sub-page turns without re-rasterizing the PDF page.
pub fn slice_pixmap(
    pm: &mupdf::Pixmap,
    geom: &LayoutGeom,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let config = &settings.split;
    let sub_box = geom.sub_box;
    let zoom = geom.zoom;

    let mut out = vec![255u8; (w as usize) * (h as usize)];

    let pm_w = pm.width() as usize;
    let pm_h = pm.height() as usize;
    let stride = pm.stride() as usize;
    let samples = pm.samples();

    let src_x = (sub_box.x0 * geom.pw * zoom).round() as usize;
    let src_y = (sub_box.y0 * geom.ph * zoom).round() as usize;
    let rw = ((sub_box.width() * geom.pw * zoom).round() as usize).min(pm_w.saturating_sub(src_x));
    let rh = ((sub_box.height() * geom.ph * zoom).round() as usize).min(pm_h.saturating_sub(src_y));

    if rw == 0 || rh == 0 {
        settings.apply_lut(&mut out);
        return out;
    }

    let (vis_ox, vis_oy) = (geom.vis_ox, geom.vis_oy);

    // Calculate dashed reading boundary line position (where previous sub-page ended)
    let dash_y = if sub_idx > 0 && config.sub_box_count() > 1 {
        let n = config.sub_box_count() as f32;
        let ov = config.overlap.clamp(0.0, 0.35);
        let overlap_frac = (n * ov) / (1.0 + (n - 1.0) * ov);
        let dy = (overlap_frac * rh as f32).round() as usize;
        if dy > 2 && dy < rh.saturating_sub(2) {
            Some(dy)
        } else {
            None
        }
    } else {
        None
    };

    let w_u = w as usize;
    let h_u = h as usize;

    for vy in 0..rh {
        let dst_y = vis_oy + vy;
        if dst_y >= h_u {
            break;
        }
        let src_start = (src_y + vy) * stride + src_x;
        let dst_start = dst_y * w_u + vis_ox;
        let len = rw.min(w_u.saturating_sub(vis_ox));
        if src_start + len <= samples.len() && dst_start + len <= out.len() {
            out[dst_start..dst_start + len].copy_from_slice(&samples[src_start..src_start + len]);

            if dash_y == Some(vy) {
                for vx in 0..len {
                    if (vx / 8) % 2 == 0 {
                        let p = &mut out[dst_start + vx];
                        if *p > 140 {
                            *p = 140;
                        }
                    }
                }
            }
        }
    }

    // Apply Contrast / Whitening / Invert LUT
    settings.apply_lut(&mut out);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::{SplitConfig, SplitPreset};
    use mupdf::TextPageFlags;

    fn test_pdf() -> Option<String> {
        std::env::var("YB_TEST_PDF").ok().filter(|p| std::path::Path::new(p).exists())
    }

    #[test]
    fn test_render_subbox_portrait() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let settings = ReaderSettings::default();
        let rendered = render_page(&doc, 20, 0, &settings, 1236, 1648);
        assert!(rendered.is_some());
        assert_eq!(rendered.unwrap().len(), 1236 * 1648);
    }

    #[test]
    fn test_render_subbox_horizontal2_landscape() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        // Visual dims for landscape: swapped.
        let r0 = render_page(&doc, 20, 0, &settings, 1648, 1236);
        assert!(r0.is_some());
        let r1 = render_page(&doc, 20, 1, &settings, 1648, 1236);
        assert!(r1.is_some());
        assert_eq!(r1.unwrap().len(), 1648 * 1236);
    }

    #[test]
    fn test_slice_pixmap_matches_render_page() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let page = doc.load_page(20).expect("load page");
        let bounds = page.bounds().expect("bounds");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);

        let geom0 = LayoutGeom::new(&settings, bounds, 0, 1648, 1236).unwrap();
        let mut m = Matrix::IDENTITY;
        m.scale(geom0.zoom, geom0.zoom);
        let pm = page.to_pixmap(&m, &Colorspace::device_gray(), false, true).unwrap();

        let slice0 = slice_pixmap(&pm, &geom0, 0, &settings, 1648, 1236);
        let full0 = render_page(&doc, 20, 0, &settings, 1648, 1236).unwrap();
        assert_eq!(slice0, full0);

        let geom1 = LayoutGeom::new(&settings, bounds, 1, 1648, 1236).unwrap();
        let slice1 = slice_pixmap(&pm, &geom1, 1, &settings, 1648, 1236);
        let full1 = render_page(&doc, 20, 1, &settings, 1648, 1236).unwrap();
        assert_eq!(slice1, full1);
    }

    #[test]
    fn test_render_subbox_horizontal3_landscape() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal3);
        for sub in 0..3 {
            let r = render_page(&doc, 20, sub, &settings, 1648, 1236);
            assert!(r.is_some());
        }
    }

    #[test]
    fn test_diagnostic_subboxes() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");

        for page_no in [0, 1, 5, 20, 21, 50] {
            let page = doc.load_page(page_no).unwrap();
            let bounds = page.bounds().unwrap();
            println!(
                "\n=== Page {} bounds: x0={} y0={} x1={} y1={} ===",
                page_no, bounds.x0, bounds.y0, bounds.x1, bounds.y1
            );
            let mut settings = ReaderSettings::default();
            settings.split = SplitConfig::for_preset(SplitPreset::Horizontal3);

            let boxes = settings.split.sub_boxes();
            for (sub_idx, b) in boxes.iter().enumerate() {
                let rendered = render_page(&doc, page_no as usize, sub_idx, &settings, 1648, 1236);
                assert!(rendered.is_some());
                println!(
                    "  Sub {}: box=[{:.4}, {:.4}, {:.4}, {:.4}] len={}",
                    sub_idx,
                    b.x0,
                    b.y0,
                    b.x1,
                    b.y1,
                    rendered.unwrap().len()
                );
            }
        }
    }

    /// Word extraction through the SHARED path (this used to be a
    /// hand-copied variant of the loop under test).
    #[test]
    fn words_from_text_page_extracts_in_reading_order() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let page = doc.load_page(20).expect("load page 20");
        let settings = ReaderSettings::default();
        let tp = page
            .to_text_page(TextPageFlags::empty())
            .expect("to_text_page");
        let g =
            LayoutGeom::new(&settings, page.bounds().unwrap(), 0, 1236, 1648).expect("geometry");

        let words = words_from_text_page(&tp, &g);
        println!("Extracted {} words from page 20. First 10:", words.len());
        for (w, r) in words.iter().take(10) {
            println!(
                "  '{}' at [{:.1}, {:.1}, {:.1}, {:.1}]",
                w, r.x0, r.y0, r.x1, r.y1
            );
        }
        assert!(!words.is_empty());
        // Visual boxes live inside the visual buffer.
        for (_, r) in words.iter().take(50) {
            assert!(r.x0 >= -1.0 && r.x1 <= 1237.0 && r.y0 >= -1.0 && r.y1 <= 1649.0);
        }
    }

    /// End-to-end: in an H2 landscape visual buffer (1648x1236), extracted
    /// word boxes must sit on painted ink — darker than the page
    /// background. A mis-offset mapping lands on whitespace and fails
    /// this.
    #[test]
    fn extracted_words_land_on_painted_ink() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let page = doc.load_page(20).expect("load page");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        let (vw, vh) = (1648usize, 1236usize);
        let gray = render_page(&doc, 20, 0, &settings, 1648, 1236).expect("render");
        let tp = page
            .to_text_page(TextPageFlags::empty())
            .expect("text page");
        let g = LayoutGeom::new(&settings, page.bounds().unwrap(), 0, 1648, 1236).expect("geom");
        let words = words_from_text_page(&tp, &g);
        assert!(!words.is_empty());

        let mut word_mean = 0f64;
        let mut n = 0usize;
        for (_, r) in words.iter() {
            let (x0, y0) = (
                r.x0.round().max(0.0) as usize,
                r.y0.round().max(0.0) as usize,
            );
            let (x1, y1) = (
                (r.x1.round() as usize).min(vw),
                (r.y1.round() as usize).min(vh),
            );
            for y in y0..y1 {
                for x in x0..x1 {
                    word_mean += gray[y * vw + x] as f64;
                    n += 1;
                }
            }
        }
        let page_mean = gray.iter().map(|&v| v as f64).sum::<f64>() / gray.len() as f64;
        let word_mean = word_mean / n as f64;
        assert!(
            word_mean < page_mean - 8.0,
            "word boxes not on ink: word={:.1} page={:.1}",
            word_mean,
            page_mean
        );
    }

    #[test]
    fn test_probe_mupdf() {
        let Some(pdf) = test_pdf() else { return; };
        let doc = Document::open(&pdf).expect("open doc");
        let page = doc.load_page(8).unwrap();
        if let Ok(links) = page.links() {
            for l in links.take(5) {
                let uri = &l.uri;
                println!("URI: {}", uri);
                if let Ok(dest) = doc.resolve_link(uri) {
                    println!("  Resolved link to page: {:?}", dest);
                }
            }
        }
    }

    #[test]
    fn test_autocrop_detect_margins() {
        let path = std::env::temp_dir().join("yb_autocrop_test.pdf");
        std::fs::write(&path, cropbox_pdf()).expect("write test pdf");
        let doc = Document::open(path.to_str().unwrap()).expect("open test pdf");
        let page = doc.load_page(0).expect("load page");
        let margins = detect_page_margins(&page, 8.0);
        assert!(margins.is_some());
        let (ml, mt, mr, mb) = margins.unwrap();
        assert!(ml > 0.0 && mt > 0.0 && mr > 0.0 && mb > 0.0);
        let book_margins = detect_book_margins(&doc, 0);
        assert!(book_margins.is_some());
        let _ = std::fs::remove_file(&path);
    }

    /// Autocrop must not swap top/bottom. Quads and bounds both arrive
    /// in mupdf's normalized (origin-relative, y-DOWN) space — the same
    /// invariant `words_follow_nonzero_cropbox_origin` pins for word
    /// boxes. Text pinned near the TOP edge (PDF y=700 of 792) must
    /// yield a small margin_top and a huge margin_bottom (clamped to
    /// 0.40). Reading the quad corners as PDF y-up would flip the two
    /// and fail here.
    #[test]
    fn autocrop_pins_top_vs_bottom() {
        let path = std::env::temp_dir().join("yb_autocrop_top.pdf");
        std::fs::write(
            &path,
            build_pdf(&[(
                "BT /F1 24 Tf 100 700 Td (TopText) Tj ET".to_string(),
                String::new(),
            )]),
        )
        .expect("write test pdf");
        let doc = Document::open(path.as_os_str()).expect("open test pdf");
        let page = doc.load_page(0).expect("load page");
        let (ml, mt, mr, mb) = detect_page_margins(&page, 8.0).expect("margins");
        assert!(mt < 0.12, "margin_top {mt:.3} too big — top/bottom swapped?");
        assert!(mb > 0.35, "margin_bottom {mb:.3} too small");
        assert!(ml > 0.10 && ml < 0.19, "margin_left {ml:.3}");
        assert!(mr > 0.35, "margin_right {mr:.3}");
        let _ = std::fs::remove_file(&path);
    }

    /// A text-free page (vector ink only) takes the downscaled-pixmap
    /// fallback. A 100x100 black square at PDF (50,50) sits at the
    /// page's bottom-left: mb and ml stay small, mt/mr clamp high.
    #[test]
    fn autocrop_pixmap_fallback_ink_only() {
        let path = std::env::temp_dir().join("yb_autocrop_ink.pdf");
        std::fs::write(
            &path,
            build_pdf(&[("0 0 0 rg 50 50 100 100 re f".to_string(), String::new())]),
        )
        .expect("write test pdf");
        let doc = Document::open(path.as_os_str()).expect("open test pdf");
        let page = doc.load_page(0).expect("load page");
        let (ml, mt, mr, mb) = detect_page_margins(&page, 8.0).expect("margins");
        assert!(mb < 0.10, "margin_bottom {mb:.3} — ink placed wrong or axis swapped");
        assert!(ml < 0.10, "margin_left {ml:.3}");
        assert!(mt > 0.35, "margin_top {mt:.3}");
        assert!(mr > 0.35, "margin_right {mr:.3}");
        let _ = std::fs::remove_file(&path);
    }

    /// Book-wide detection folds L/R to the smaller side so facing
    /// pages (ink near the left on one, near the right on the other)
    /// never clip.
    #[test]
    fn autocrop_book_margins_symmetric_lr() {
        let path = std::env::temp_dir().join("yb_autocrop_book.pdf");
        std::fs::write(
            &path,
            build_pdf(&[
                (
                    "BT /F1 24 Tf 60 400 Td (L) Tj ET".to_string(),
                    String::new(),
                ),
                (
                    "BT /F1 24 Tf 540 400 Td (R) Tj ET".to_string(),
                    String::new(),
                ),
            ]),
        )
        .expect("write test pdf");
        let doc = Document::open(path.as_os_str()).expect("open test pdf");
        let (ml, _mt, mr, _mb) = detect_book_margins(&doc, 0).expect("book margins");
        assert!(
            (ml - mr).abs() < 1e-4,
            "L/R not symmetrized: ml={ml:.3} mr={mr:.3}"
        );
        assert!(
            ml < 0.12 && mr < 0.12,
            "facing-page clip guard failed: ml={ml:.3} mr={mr:.3}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Facing-page detection finds asymmetric odd/even margins without flattening to symmetric.
    #[test]
    fn autocrop_even_odd_margins() {
        let path = std::env::temp_dir().join("yb_autocrop_even_odd.pdf");
        std::fs::write(
            &path,
            build_pdf(&[
                // Page 0 (Book Page 1, Odd): Text on the left (ml=0.10 inner, mr=0.20 outer)
                (
                    "BT /F1 24 Tf 60 400 Td (OddPageText) Tj ET".to_string(),
                    String::new(),
                ),
                // Page 1 (Book Page 2, Even): Text on the right (ml=0.20 outer, mr=0.10 inner)
                (
                    "BT /F1 24 Tf 150 400 Td (EvenPageText) Tj ET".to_string(),
                    String::new(),
                ),
            ]),
        )
        .expect("write test pdf");
        let doc = Document::open(path.as_os_str()).expect("open test pdf");
        let (ml, _mt, mr, _mb) = detect_even_odd_margins(&doc, 0).expect("even odd margins");
        // ml (even page left) should be larger than mr (even page right)
        assert!(ml > mr, "Even page left should reflect outer margin: ml={ml:.3}, mr={mr:.3}");
        let _ = std::fs::remove_file(&path);
    }

    /// A minimal N-page letter-size PDF with one Helvetica content
    /// stream per page. Same byte-exact construction as `cropbox_pdf`,
    /// generalized so autocrop tests can place ink asymmetrically and
    /// build multi-page books.
    fn build_pdf(pages: &[(String, String)]) -> Vec<u8> {
        let n = pages.len();
        let font_no = 3 + 2 * n;
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        let mut objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), n),
        ];
        for (i, (content, extra)) in pages.iter().enumerate() {
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {} /Contents {} 0 R \
                 /Resources << /Font << /F1 {} 0 R >> >> >>",
                extra,
                4 + 2 * i,
                font_no
            ));
            objs.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len() + 1,
                content
            ));
        }
        objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());

        let count = objs.len();
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offs = vec![0usize; count];
        for (i, o) in objs.iter().enumerate() {
            offs[i] = pdf.len();
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", count + 1).as_bytes());
        for o in offs {
            pdf.extend_from_slice(format!("{:010} 00000 n \n", o).as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                count + 1,
                xref
            )
            .as_bytes(),
        );
        pdf
    }

    /// A minimal one-page PDF whose CropBox sits at a real offset from
    /// the MediaBox origin (x=300). Built byte-exact here so the test
    /// needs no fixture file.
    fn cropbox_pdf() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 320 400 Td (Hello CropBox) Tj ET";
        let objs: [String; 5] = [
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [300 100 512 692] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .into(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len() + 1,
                String::from_utf8_lossy(content)
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offs = [0usize; 5];
        for (i, o) in objs.iter().enumerate() {
            offs[i] = pdf.len();
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for o in offs {
            pdf.extend_from_slice(format!("{:010} 00000 n \n", o).as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{}\n%%EOF", xref).as_bytes(),
        );
        pdf
    }

    /// Words must land on ink for pages whose CropBox origin is not
    /// (0,0). mupdf normalizes the page CTM (bounds and text quads both
    /// come back origin-relative), so this pins the SPACE CONSISTENCY
    /// invariant rather than an absolute-coordinates assumption: if a
    /// future mupdf or backend returns non-normalized bounds, to_screen
    /// must keep subtracting the page-box origin or every word box
    /// shifts by `x0 * zoom` (here that would be ~760 px — clean off
    /// the word's own ink).
    #[test]
    fn words_follow_nonzero_cropbox_origin() {
        let path = std::env::temp_dir().join("yb_cropbox_test.pdf");
        std::fs::write(&path, cropbox_pdf()).expect("write test pdf");
        let doc = Document::open(path.as_os_str()).expect("open");

        let page = doc.load_page(0).expect("page");
        let bounds = page.bounds().expect("bounds");
        // Premise: mupdf honored the CropBox dimensions (212x592, not the
        // 612x792 MediaBox). x0 is normalized to 0 — that IS the contract
        // this test documents.
        assert!(
            (bounds.x1 - bounds.x0 - 212.0).abs() < 0.5 && (bounds.y1 - bounds.y0 - 592.0).abs() < 0.5,
            "bounds: {bounds:?}"
        );

        let settings = ReaderSettings::default();
        let (vw, vh) = (1236u32, 1648u32);
        let gray = render_page(&doc, 0, 0, &settings, vw, vh).expect("render");
        let geom = LayoutGeom::new(&settings, bounds, 0, vw, vh).expect("geom");
        let tp = page
            .to_text_page(mupdf::TextPageFlags::empty())
            .expect("text page");
        let words = words_from_text_page(&tp, &geom);
        let Some((first, r)) = words.first() else {
            panic!("no words extracted from cropbox page");
        };
        assert!(first.starts_with("Hello"), "first word: {first}");

        // Where the ink is: zoom is height-limited (212x592 box into
        // 1236x1506) ≈ 2.544; the box is centered → vis_ox ≈ 348; word
        // starts 20pt inside the box → x0 ≈ 348 + 20*2.544 ≈ 399. A
        // mapping that forgot the space normalization lands at ≈ 1162.
        assert!(
            r.x0 > 350.0 && r.x0 < 450.0,
            "word box x0 {:.1} not on its ink (expected ~399)",
            r.x0
        );
        assert!(r.x1 < 800.0, "word box spills off-buffer: {r:?}");

        // Belt and braces: the box's mean gray must be ink, not paper.
        let (x0, y0) = (r.x0.round().max(0.0) as usize, r.y0.round().max(0.0) as usize);
        let (x1, y1) = ((r.x1.round() as usize).min(vw as usize), (r.y1.round() as usize).min(vh as usize));
        let (mut sum, mut n) = (0u64, 0usize);
        for y in y0..y1 {
            for x in x0..x1 {
                sum += gray[y * vw as usize + x] as u64;
                n += 1;
            }
        }
        let page_mean = gray.iter().map(|&v| v as f64).sum::<f64>() / gray.len() as f64;
        let word_mean = sum as f64 / n as f64;
        assert!(
            word_mean < page_mean - 8.0,
            "word box not on ink: word={word_mean:.1} page={page_mean:.1}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Mapping invariants: the sub-box maps inside the visual buffer and
    /// stays a box, in both the portrait and the landscape visual buffer
    /// (landscape = swapped dims — LayoutGeom no longer knows or cares
    /// which grip produced them).
    #[test]
    fn to_visual_keeps_subbox_inside_buffer() {
        let mut settings = ReaderSettings::default();
        let bounds = mupdf::Rect::new(0.0, 0.0, 600.0, 800.0);
        settings.split = SplitConfig::for_preset(SplitPreset::FitPage);

        // Portrait: identity mapping for a rect at the origin.
        let g = LayoutGeom::new(&settings, bounds, 0, 1236, 1648).unwrap();
        let r = g.to_visual(0.0, 0.0, 10.0, 10.0);
        assert!(r.x0 < r.x1 && r.y0 < r.y1);
        assert!((r.x1 - r.x0 - 10.0 * g.zoom).abs() < 1.0);

        // Landscape splits over the swapped visual dims: sub-box on-panel,
        // still a box. (Rects OUTSIDE the sub-box legitimately map
        // off-buffer — they aren't on this screen.)
        for (vw, vh) in [(1648u32, 1236u32), (1236, 1648)] {
            settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
            let g = LayoutGeom::new(&settings, bounds, 0, vw, vh).unwrap();
            let (a, b, c, d) = g.sub_rect_doc();
            let r = g.to_visual(a, b, c, d);
            assert!(
                r.x0 >= -1.0 && r.x1 <= vw as f32 + 1.0,
                "{vw}x{vh}: {:?}",
                r
            );
            assert!(
                r.y0 >= -1.0 && r.y1 <= vh as f32 + 1.0,
                "{vw}x{vh}: {:?}",
                r
            );
            assert!(r.x0 < r.x1 && r.y0 < r.y1);
        }
    }
}
