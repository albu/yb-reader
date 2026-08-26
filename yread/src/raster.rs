use std::collections::HashMap;
use std::sync::Arc;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};

use crate::font::FontSystem;
use crate::line::{LayoutLine, LineItem};
use crate::model::{Book, FontStyle};
use crate::paginate::{LayoutConfig, PageElement, PageLayout};

/// Non-linear E-ink alpha quantization table.
/// Maps subpixel coverage to stem-darkened alpha to prevent fuzzy edges
/// when the EPDC quantizes to 16 discrete grayscale levels.
const EINK_ALPHA_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // Below 14 coverage: discard faint fringe
        // Above 14 coverage: apply mild stem darkening for crisp e-ink edges
        let a = if i < 14 {
            0
        } else {
            let val = (i * 268) / 255;
            if val > 255 {
                255
            } else {
                val as u8
            }
        };
        lut[i] = a;
        i += 1;
    }
    lut
};

#[derive(Clone)]
struct CachedGlyph {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    data: Vec<u8>,
}

/// Cache ceilings. Overflow clears wholesale — crude but bounded; both
/// caches rewarm in a page or two of reading.
const GLYPH_CACHE_CAP: usize = 4096;
const IMAGE_CACHE_CAP: usize = 10;
/// Strict decode ceiling for embedded book images. The panel is ~2 MP and
/// images are downscaled to draw size anyway, so anything larger is wasted —
/// and a header declaring e.g. 60000×60000 would allocate gigabytes at
/// render time (a "decode bomb" that imports cleanly, then bricks the page).
const IMAGE_MAX_DIM: u32 = 4096;

pub struct Rasterizer {
    scale_ctx: ScaleContext,
    glyph_cache: HashMap<(u16, u16, FontStyle), Option<Arc<CachedGlyph>>>,
    image_cache: HashMap<(String, usize, usize), Arc<Vec<u8>>>,
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self {
            scale_ctx: ScaleContext::new(),
            glyph_cache: HashMap::with_capacity(2048),
            image_cache: HashMap::new(),
        }
    }
}

/// Pen position adjustment for a line's alignment: (leading offset,
/// extra advance per inter-word space). Solved ONCE at line-build time
/// and stored on the LayoutLine — this function only reads the stored
/// values, so the RASTER and the app's word-rect extraction can never
/// drift apart again (when they did, lookups hit the wrong word near
/// line ends on justified text — the dictionary "couldn't find" words
/// it had).
pub fn alignment_adjust(line: &crate::line::LayoutLine) -> (f32, f32) {
    line.alignment_adjust()
}

impl Rasterizer {
    pub fn new() -> Self {
        Self::default()
    }
    /// Render a page into an 8-bit grayscale framebuffer (0 = black, 255 = white).
    pub fn render_page(
        &mut self,
        book: &Book,
        page: &PageLayout,
        config: &LayoutConfig,
        fonts: &FontSystem,
        fb: &mut [u8],
        stride: usize,
    ) {
        // Fill background white
        fb.fill(255);

        let origin_x = config.margin_left as f32;
        let origin_y = config.margin_top as f32;
        let p_width = config.page_width as usize;
        let p_height = config.page_height as usize;

        for elem in &page.elements {
            match elem {
                PageElement::Line { line, x, y } => {
                    self.render_line(
                        line,
                        origin_x + x,
                        origin_y + y,
                        config.font_size,
                        fonts,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                }
                PageElement::Bullet {
                    shaped,
                    x,
                    y,
                    size_pt,
                } => {
                    let face = fonts.face_for_style(crate::model::FontStyle::Bold);
                    self.render_shaped_word(
                        shaped,
                        origin_x + x,
                        origin_y + y,
                        *size_pt,
                        crate::model::FontStyle::Bold,
                        face,
                        0,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                }
                PageElement::CodeLine {
                    shaped,
                    x,
                    y,
                    size_pt,
                } => {
                    let face = fonts.code_face();
                    self.render_shaped_word(
                        shaped,
                        origin_x + x,
                        origin_y + y,
                        *size_pt,
                        crate::model::FontStyle::Regular,
                        face,
                        0,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                }
                PageElement::CircleBullet { x, y, radius } => {
                    let cx = (origin_x + x).round() as i32;
                    let cy = (origin_y + y).round() as i32;
                    let r = radius.round() as i32;
                    let r2 = r * r;
                    for dy in -r..=r {
                        let row = cy + dy;
                        if row >= 0 && row < p_height as i32 {
                            let dst_row = row as usize * stride;
                            for dx in -r..=r {
                                if dx * dx + dy * dy <= r2 {
                                    let col = cx + dx;
                                    if col >= 0 && col < p_width as i32 {
                                        fb[dst_row + col as usize] = 0; // solid black dot
                                    }
                                }
                            }
                        }
                    }
                }
                PageElement::QuoteBar { x, y0, y1 } => {
                    let bar_x = (origin_x + x).round() as usize;
                    let row0 = (origin_y + y0).round() as usize;
                    let row1 = (origin_y + y1).round() as usize;
                    for row in row0..row1.min(p_height) {
                        for col in bar_x..(bar_x + 3).min(p_width) {
                            fb[row * stride + col] = 80;
                        }
                    }
                }
                PageElement::Rule { x, y, width } => {
                    let rx = (origin_x + x).round() as usize;
                    let ry = (origin_y + y).round() as usize;
                    let rw = (*width as usize).min(p_width.saturating_sub(rx));
                    if ry < p_height {
                        for col in rx..(rx + rw) {
                            fb[ry * stride + col] = 160;
                        }
                    }
                }
                PageElement::Image {
                    id,
                    x,
                    y,
                    width,
                    height,
                } => {
                    let img_w = (*width as usize).max(1);
                    let img_h = (*height as usize).max(1);
                    let img_x = (origin_x + x).round() as usize;
                    let img_y = (origin_y + y).round() as usize;

                    let cache_key = (id.clone(), img_w, img_h);
                    let tile = if let Some(t) = self.image_cache.get(&cache_key) {
                        Arc::clone(t)
                    } else {
                        // Eager store or lazy archive load, memoized in Book.
                        let Some(raw_data) = book.get_image(id) else {
                            continue;
                        };
                        // Strict limits: `load_from_memory` would run with
                        // unlimited width/height, letting a crafted header
                        // allocate gigabytes before the resize shrinks it.
                        let mut limits = image::Limits::default();
                        limits.max_image_width = Some(IMAGE_MAX_DIM);
                        limits.max_image_height = Some(IMAGE_MAX_DIM);
                        limits.max_alloc = Some(64 * 1024 * 1024);
                        let mut rdr =
                            image::ImageReader::new(std::io::Cursor::new(raw_data.as_slice()));
                        rdr.limits(limits);
                        let dyn_img = match rdr.with_guessed_format() {
                            Ok(r) => r.decode(),
                            Err(_) => continue,
                        };
                        let Ok(dyn_img) = dyn_img else { continue };
                        let gray = dyn_img
                            .resize_exact(
                                img_w as u32,
                                img_h as u32,
                                image::imageops::FilterType::Lanczos3,
                            )
                            .to_luma8();
                        let t = Arc::new(gray.into_raw());
                        if self.image_cache.len() >= IMAGE_CACHE_CAP {
                            self.image_cache.clear();
                        }
                        self.image_cache.insert(cache_key, Arc::clone(&t));
                        t
                    };

                    for row in 0..img_h {
                        let dst_row_idx = img_y + row;
                        if dst_row_idx >= p_height {
                            break;
                        }
                        let dst_offset = dst_row_idx * stride;
                        let src_offset = row * img_w;
                        for col in 0..img_w {
                            let dst_col_idx = img_x + col;
                            if dst_col_idx >= p_width {
                                break;
                            }
                            fb[dst_offset + dst_col_idx] = tile[src_offset + col];
                        }
                    }
                }
            }
        }
    }

    fn render_line(
        &mut self,
        line: &LayoutLine,
        line_start_x: f32,
        baseline_y: f32,
        base_font_size: f32,
        fonts: &FontSystem,
        fb: &mut [u8],
        stride: usize,
        p_width: usize,
        p_height: usize,
    ) {
        let mut cur_x = line_start_x;
        let (start_off, extra_space_per_gap) = alignment_adjust(line);
        cur_x += start_off;

        for item in &line.items {
            match item {
                LineItem::Word { shaped, style, .. } => {
                    let run_size = base_font_size * style.size_mult;
                    let face = fonts.face_for_style(style.font_style);
                    let baseline = if style.is_sup {
                        baseline_y - (run_size * 0.40 * (300.0 / 72.0))
                    } else if style.is_sub {
                        baseline_y + (run_size * 0.25 * (300.0 / 72.0))
                    } else {
                        baseline_y
                    };

                    let fg = style.color.unwrap_or(0);
                    self.render_shaped_word(
                        shaped,
                        cur_x,
                        baseline,
                        run_size,
                        style.font_style,
                        face,
                        fg,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                    cur_x += shaped.advance;
                }
                LineItem::HyphenatedPrefix {
                    prefix_shaped,
                    hyphen_adv,
                    style,
                    ..
                } => {
                    let run_size = base_font_size * style.size_mult;
                    let face = fonts.face_for_style(style.font_style);
                    let fg = style.color.unwrap_or(0);
                    self.render_shaped_word(
                        prefix_shaped,
                        cur_x,
                        baseline_y,
                        run_size,
                        style.font_style,
                        face,
                        fg,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                    cur_x += prefix_shaped.advance;
                    cur_x += self.render_hyphen_glyph(
                        cur_x,
                        baseline_y,
                        run_size,
                        style,
                        *hyphen_adv,
                        fonts,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                }
                LineItem::SoftHyphen { .. } => {
                    // Invisible when the line did not break here.
                }
                LineItem::Hyphen { adv, style } => {
                    let run_size = base_font_size * style.size_mult;
                    cur_x += self.render_hyphen_glyph(
                        cur_x,
                        baseline_y,
                        run_size,
                        style,
                        *adv,
                        fonts,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                }
                LineItem::Space { adv, .. } => {
                    cur_x += *adv + extra_space_per_gap;
                }
                LineItem::HardBreak => {}
            }
        }
    }

    /// Draw a hyphen glyph at the pen position and return its advance.
    fn render_hyphen_glyph(
        &mut self,
        cur_x: f32,
        baseline_y: f32,
        run_size: f32,
        style: &crate::model::Style,
        hyphen_adv: f32,
        fonts: &FontSystem,
        fb: &mut [u8],
        stride: usize,
        p_width: usize,
        p_height: usize,
    ) -> f32 {
        let face = fonts.face_for_style(style.font_style);
        let fg = style.color.unwrap_or(0);
        let swash_font = face.as_swash();
        if let Some(font_ref) = swash_font {
            let hyphen_gid = font_ref.charmap().map('-');
            if hyphen_gid != 0 {
                let hyp_shaped = crate::shape::ShapedWord {
                    advance: hyphen_adv,
                    glyphs: vec![crate::shape::ShapedGlyph {
                        glyph_id: hyphen_gid,
                        cluster: 0,
                        x_advance: hyphen_adv,
                        y_advance: 0.0,
                        x_offset: 0.0,
                        y_offset: 0.0,
                    }],
                };
                self.render_shaped_word(
                    &hyp_shaped,
                    cur_x,
                    baseline_y,
                    run_size,
                    style.font_style,
                    face,
                    fg,
                    fb,
                    stride,
                    p_width,
                    p_height,
                );
            }
        }
        // Always consume the hyphen advance, even if the face has no '-'
        // glyph: the word-rect extractor advances by the same amount
        // unconditionally, so the pen must too (raster ↔ hit-test parity —
        // the same drift class that broke dictionary taps once).
        hyphen_adv
    }

    fn render_shaped_word(
        &mut self,
        shaped: &crate::shape::ShapedWord,
        mut x: f32,
        baseline_y: f32,
        size_pt: f32,
        font_style: FontStyle,
        face: &crate::font::FontFace,
        fg_color: u8,
        fb: &mut [u8],
        stride: usize,
        p_width: usize,
        p_height: usize,
    ) {
        let swash_font = match face.as_swash() {
            Some(f) => f,
            None => return,
        };

        let px_size = size_pt * (300.0 / 72.0);
        let px_size_u16 = (px_size * 10.0).round() as u16;

        for glyph in &shaped.glyphs {
            let gx = x + glyph.x_offset;
            let gy = baseline_y - glyph.y_offset;
            let key = (glyph.glyph_id, px_size_u16, font_style);

            let cached_entry = if let Some(entry) = self.glyph_cache.get(&key) {
                entry.clone()
            } else {
                let mut scaler = self
                    .scale_ctx
                    .builder(swash_font)
                    .size(px_size)
                    .hint(true)
                    .build();

                let rendered = Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                ])
                .render(&mut scaler, glyph.glyph_id);

                let entry = rendered.map(|img| {
                    // Normalize to 1 byte/px coverage. Color sources (emoji)
                    // yield 4 B/px RGBA; the blit below would have read the
                    // red channel as alpha coverage and stamped garbage.
                    // Keep each pixel's alpha — e-ink shows shape, not hue.
                    // SubpixelMask is 3 B/px LCD coverage with no alpha:
                    // average the channels (the current source list never
                    // yields it, but the arm must not lie about the layout).
                    let data = match img.content {
                        Content::Color => img.data.chunks_exact(4).map(|px| px[3]).collect(),
                        Content::SubpixelMask => img
                            .data
                            .chunks_exact(3)
                            .map(|px| {
                                ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8
                            })
                            .collect(),
                        Content::Mask => img.data,
                    };
                    Arc::new(CachedGlyph {
                        left: img.placement.left,
                        top: img.placement.top,
                        width: img.placement.width as usize,
                        height: img.placement.height as usize,
                        data,
                    })
                });
                if self.glyph_cache.len() >= GLYPH_CACHE_CAP {
                    self.glyph_cache.clear();
                }
                self.glyph_cache.insert(key, entry.clone());
                entry
            };

            if let Some(cached) = cached_entry {
                let glyph_left = (gx + cached.left as f32).round() as i32;
                let glyph_top = (gy - cached.top as f32).round() as i32;
                let g_width = cached.width;
                let g_height = cached.height;

                for row in 0..g_height {
                    let dst_y = glyph_top + row as i32;
                    if dst_y < 0 || dst_y >= p_height as i32 {
                        continue;
                    }
                    let dst_row_idx = dst_y as usize * stride;

                    let dst_x0 = glyph_left.max(0) as usize;
                    let dst_x1 = ((glyph_left + g_width as i32).max(0) as usize).min(p_width);
                    if dst_x1 <= dst_x0 {
                        continue;
                    }

                    let src_col_offset = (dst_x0 as i32 - glyph_left) as usize;
                    let count = dst_x1 - dst_x0;
                    let src_row = &cached.data
                        [row * g_width + src_col_offset..row * g_width + src_col_offset + count];
                    let dst_row = &mut fb[dst_row_idx + dst_x0..dst_row_idx + dst_x1];

                    for (dst_pixel, &coverage) in dst_row.iter_mut().zip(src_row.iter()) {
                        if coverage > 0 {
                            let alpha = EINK_ALPHA_LUT[coverage as usize] as u32;
                            if alpha > 0 {
                                let curr = *dst_pixel as u32;
                                let fg = fg_color as u32;
                                let blended = ((255 - alpha) * curr + alpha * fg) / 255;
                                *dst_pixel = blended as u8;
                            }
                        }
                    }
                }
            }

            x += glyph.x_advance;
        }
    }
}
