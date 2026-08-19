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

use crate::split::{RectF, ReaderSettings};

pub const HEADER_H: u32 = 48; // px
pub const FOOTER_H: u32 = 72; // px

/// The available text area in points for a reflow layout — shared by the
/// async open/reflow paths so a warm document and a cold one lay out
/// identically. Callers pass VISUAL dims (Orientation decides the swap).
pub fn avail_pt(w: u32, h: u32, margin_pad: u32) -> (f32, f32) {
    (
        (w - 2 * margin_pad) as f32 * 72.0 / 300.0,
        (h - 2 * margin_pad - FOOTER_H - HEADER_H) as f32 * 72.0 / 300.0,
    )
}

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
    /// Document→screen scale, fit inside the reading area.
    pub zoom: f32,
    /// Screen offset (px) of the rendered box inside the reading area.
    pub vis_ox: usize,
    pub vis_oy: usize,
    w: u32,
    h: u32,
}

impl LayoutGeom {
    /// `w`/`h` are the VISUAL dims of the target buffer.
    pub fn new(
        settings: &ReaderSettings,
        bounds: mupdf::Rect,
        sub_idx: usize,
        w: u32,
        h: u32,
    ) -> Option<LayoutGeom> {
        let pw = bounds.x1 - bounds.x0;
        let ph = bounds.y1 - bounds.y0;
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        let config = &settings.split;
        let sub_box = config
            .sub_boxes()
            .get(sub_idx)
            .copied()
            .unwrap_or(RectF::new(0.0, 0.0, 1.0, 1.0));

        let margin_pad = settings.margin_pad;
        let (vis_w, vis_h) = (
            (w - 2 * margin_pad) as f32,
            (h - 2 * margin_pad - FOOTER_H - HEADER_H) as f32,
        );

        let bw = sub_box.width() * pw;
        let bh = sub_box.height() * ph;
        let zoom = (vis_w / bw).min(vis_h / bh);

        // Rounded box size on screen — the caller may clamp these to the
        // rendered pixmap, then ask for offsets again.
        let rw = (bw * zoom).round() as usize;
        let rh = (bh * zoom).round() as usize;

        let (vis_ox, vis_oy) = Self::offsets(vis_w as usize, vis_h as usize, margin_pad, rw, rh);

        Some(LayoutGeom {
            sub_box,
            pw,
            ph,
            zoom,
            vis_ox,
            vis_oy,
            w,
            h,
        })
    }

    /// Screen offsets for a box of (rw, rh) px, centered in the reading
    /// area of a (vis_w × vis_h) buffer.
    pub fn offsets(
        vis_w: usize,
        vis_h: usize,
        margin_pad: u32,
        rw: usize,
        rh: usize,
    ) -> (usize, usize) {
        (
            vis_w.saturating_sub(rw) / 2 + margin_pad as usize,
            vis_h.saturating_sub(rh) / 2 + (HEADER_H + margin_pad) as usize,
        )
    }

    /// Map a document-space rect inside the current sub-box to visual
    /// (screen) coordinates.
    fn to_screen(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (f32, f32, f32, f32) {
        let sx0 = self.vis_ox as f32 + (x0 - self.sub_box.x0 * self.pw) * self.zoom;
        let sy0 = self.vis_oy as f32 + (y0 - self.sub_box.y0 * self.ph) * self.zoom;
        let sx1 = self.vis_ox as f32 + (x1 - self.sub_box.x0 * self.pw) * self.zoom;
        let sy1 = self.vis_oy as f32 + (y1 - self.sub_box.y0 * self.ph) * self.zoom;
        (sx0, sy0, sx1, sy1)
    }

    /// Visual-space rect for a document-space rect. What is painted and
    /// what is hit-tested share this one mapping.
    pub fn to_visual(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> RectF {
        let (sx0, sy0, sx1, sy1) = self.to_screen(x0, y0, x1, y1);
        RectF::new(sx0.min(sx1), sy0.min(sy1), sx0.max(sx1), sy0.max(sy1))
    }

    /// Visual rect of the sub-box itself, in document units (test helper:
    /// maps exactly the region this sub-page renders).
    #[cfg(test)]
    pub fn sub_rect_doc(&self) -> (f32, f32, f32, f32) {
        (
            self.sub_box.x0 * self.pw,
            self.sub_box.y0 * self.ph,
            self.sub_box.x1 * self.pw,
            self.sub_box.y1 * self.ph,
        )
    }
}

/// Extract words with visual-space bounding boxes from a text page, in
/// reading order. Words accumulate per line until whitespace; the
/// end-of-line remainder is flushed too. This loop was previously pasted
/// twice in compute_annotations and once in tests.
pub fn words_from_text_page(tp: &mupdf::TextPage, g: &LayoutGeom) -> Vec<(String, RectF)> {
    let mut words = Vec::new();
    let flush = |cur: &mut String, min_x: &mut f32, min_y: &mut f32, max_x: &mut f32,
                     max_y: &mut f32, out: &mut Vec<(String, RectF)>| {
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
                        flush(&mut cur, &mut min_x, &mut min_y, &mut max_x, &mut max_y, &mut words);
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
            flush(&mut cur, &mut min_x, &mut min_y, &mut max_x, &mut max_y, &mut words);
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

/// Rasterize (page, sub_idx) to visual-sized grayscale over the shared
/// [`LayoutGeom`] math. Pure function over the document.
pub fn render_page(
    doc: &Document,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let page = doc.load_page(page_no as i32).ok()?;
    let bounds = page.bounds().ok()?;
    let geom = LayoutGeom::new(settings, bounds, sub_idx, w, h)?;

    let config = &settings.split;
    let sub_box = geom.sub_box;
    let zoom = geom.zoom;

    let margin_pad = settings.margin_pad;
    let mut out = vec![255u8; (w as usize) * (h as usize)];

    let mut m = Matrix::IDENTITY;
    m.scale(zoom, zoom);
    let pm = page
        .to_pixmap(&m, &Colorspace::device_gray(), false, true)
        .ok()?;

    let pm_w = pm.width() as usize;
    let pm_h = pm.height() as usize;
    let stride = pm.stride() as usize;
    let samples = pm.samples();

    let src_x = (sub_box.x0 * geom.pw * zoom).round() as usize;
    let src_y = (sub_box.y0 * geom.ph * zoom).round() as usize;
    let rw = ((sub_box.width() * geom.pw * zoom).round() as usize).min(pm_w.saturating_sub(src_x));
    let rh =
        ((sub_box.height() * geom.ph * zoom).round() as usize).min(pm_h.saturating_sub(src_y));

    if rw == 0 || rh == 0 {
        settings.apply_lut(&mut out);
        return Some(out);
    }

    let (vis_ox, vis_oy) = LayoutGeom::offsets(
        (w - 2 * margin_pad) as usize,
        (h - 2 * margin_pad - FOOTER_H as u32 - HEADER_H as u32) as usize,
        margin_pad,
        rw,
        rh,
    );

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

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::{SplitConfig, SplitPreset};
    use mupdf::TextPageFlags;

    const PDF: &str =
        "/tmp/sample_book.pdf";

    #[test]
    fn test_render_subbox_portrait() {
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
        let settings = ReaderSettings::default();
        let rendered = render_page(&doc, 20, 0, &settings, 1236, 1648);
        assert!(rendered.is_some());
        assert_eq!(rendered.unwrap().len(), 1236 * 1648);
    }

    #[test]
    fn test_render_subbox_horizontal2_landscape() {
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
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
    fn test_render_subbox_horizontal3_landscape() {
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal3);
        for sub in 0..3 {
            let r = render_page(&doc, 20, sub, &settings, 1648, 1236);
            assert!(r.is_some());
        }
    }

    #[test]
    fn test_diagnostic_subboxes() {
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");

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
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
        let page = doc.load_page(20).expect("load page 20");
        let settings = ReaderSettings::default();
        let g = LayoutGeom::new(&settings, page.bounds().unwrap(), 0, 1236, 1648)
            .expect("geometry");
        let tp = page
            .to_text_page(TextPageFlags::empty())
            .expect("to_text_page");

        let words = words_from_text_page(&tp, &g);
        println!("Extracted {} words from page 20. First 10:", words.len());
        for (w, r) in words.iter().take(10) {
            println!("  '{}' at [{:.1}, {:.1}, {:.1}, {:.1}]", w, r.x0, r.y0, r.x1, r.y1);
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
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
        let page = doc.load_page(20).expect("load page");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        let (vw, vh) = (1648usize, 1236usize);
        let gray = render_page(&doc, 20, 0, &settings, 1648, 1236).expect("render");
        let g = LayoutGeom::new(&settings, page.bounds().unwrap(), 0, 1648, 1236).expect("geom");
        let tp = page.to_text_page(TextPageFlags::empty()).expect("text page");
        let words = words_from_text_page(&tp, &g);
        assert!(!words.is_empty());

        let mut word_mean = 0f64;
        let mut n = 0usize;
        for (_, r) in words.iter() {
            let (x0, y0) = (r.x0.round().max(0.0) as usize, r.y0.round().max(0.0) as usize);
            let (x1, y1) = ((r.x1.round() as usize).min(vw), (r.y1.round() as usize).min(vh));
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
        if !std::path::Path::new(PDF).exists() {
            return;
        }
        let doc = Document::open(PDF).expect("open doc");
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
            assert!(r.x0 >= -1.0 && r.x1 <= vw as f32 + 1.0, "{vw}x{vh}: {:?}", r);
            assert!(r.y0 >= -1.0 && r.y1 <= vh as f32 + 1.0, "{vw}x{vh}: {:?}", r);
            assert!(r.x0 < r.x1 && r.y0 < r.y1);
        }
    }
}
