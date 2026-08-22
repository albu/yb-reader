use std::collections::HashMap;
use std::sync::Arc;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};

use crate::font::FontSystem;
use crate::line::{LayoutLine, LineItem};
use crate::model::{Book, FontStyle, TextAlign};
use crate::paginate::{LayoutConfig, PageElement, PageLayout};

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
/// extra advance per inter-word space). Shared truth for the RASTER and
/// the app's word-rect extraction — when they drifted apart, lookups
/// hit the wrong word near line ends on justified text (the dictionary
/// "couldn't find" words it had).
pub fn alignment_adjust(line: &crate::line::LayoutLine) -> (f32, f32) {
    match line.align {
        TextAlign::Center => {
            let slack = (line.max_width - line.width).max(0.0);
            (slack / 2.0, 0.0)
        }
        TextAlign::Right => {
            let slack = (line.max_width - line.width).max(0.0);
            (slack, 0.0)
        }
        TextAlign::Justify => {
            if !line.is_last_in_paragraph && line.width < line.max_width {
                let space_count = line.items.iter().filter(|it| it.is_space()).count();
                if space_count > 0 {
                    let slack = line.max_width - line.width;
                    if slack < line.max_width * 0.40 {
                        return (0.0, slack / space_count as f32);
                    }
                }
            }
            (0.0, 0.0)
        }
        TextAlign::Left => (0.0, 0.0),
    }
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
                PageElement::Bullet { shaped, x, y, size_pt } => {
                    let face = fonts.face_for_style(crate::model::FontStyle::Bold);
                    self.render_shaped_word(
                        shaped,
                        origin_x + x,
                        origin_y + y,
                        *size_pt,
                        crate::model::FontStyle::Bold,
                        face,
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
                PageElement::Image { id, x, y, width, height } => {
                    let img_w = (*width as usize).max(1);
                    let img_h = (*height as usize).max(1);
                    let img_x = (origin_x + x).round() as usize;
                    let img_y = (origin_y + y).round() as usize;

                    let cache_key = (id.clone(), img_w, img_h);
                    let tile = if let Some(t) = self.image_cache.get(&cache_key) {
                        Arc::clone(t)
                    } else {
                        // Eager store or lazy archive load, memoized in Book.
                        let Some(raw_data) = book.get_image(id) else { continue };
                        let Ok(dyn_img) = image::load_from_memory(raw_data.as_slice()) else { continue };
                        let gray = dyn_img
                            .resize_exact(img_w as u32, img_h as u32, image::imageops::FilterType::Lanczos3)
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

                    self.render_shaped_word(
                        shaped,
                        cur_x,
                        baseline,
                        run_size,
                        style.font_style,
                        face,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                    cur_x += shaped.advance;
                }
                LineItem::HyphenatedPrefix { prefix_shaped, hyphen_adv, style, .. } => {
                    let run_size = base_font_size * style.size_mult;
                    let face = fonts.face_for_style(style.font_style);
                    self.render_shaped_word(
                        prefix_shaped,
                        cur_x,
                        baseline_y,
                        run_size,
                        style.font_style,
                        face,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                    cur_x += prefix_shaped.advance;

                    // Draw shaped hyphen
                    let swash_font = face.as_swash();
                    if let Some(font_ref) = swash_font {
                        let hyphen_gid = font_ref.charmap().map('-');
                        if hyphen_gid != 0 {
                            let hyp_shaped = crate::shape::ShapedWord {
                                advance: *hyphen_adv,
                                glyphs: vec![crate::shape::ShapedGlyph {
                                    glyph_id: hyphen_gid,
                                    cluster: 0,
                                    x_advance: *hyphen_adv,
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
                                fb,
                                stride,
                                p_width,
                                p_height,
                            );
                        }
                    }
                    cur_x += *hyphen_adv;
                }
                LineItem::Space { adv, .. } => {
                    cur_x += *adv + extra_space_per_gap;
                }
                LineItem::HardBreak => {}
            }
        }
    }

    fn render_shaped_word(
        &mut self,
        shaped: &crate::shape::ShapedWord,
        mut x: f32,
        baseline_y: f32,
        size_pt: f32,
        font_style: FontStyle,
        face: &crate::font::FontFace,
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
                    .hint(false)
                    .build();

                let rendered = Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                ])
                .render(&mut scaler, glyph.glyph_id);

                let entry = rendered.map(|img| Arc::new(CachedGlyph {
                    left: img.placement.left,
                    top: img.placement.top,
                    width: img.placement.width as usize,
                    height: img.placement.height as usize,
                    data: img.data,
                }));
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

                    for col in 0..g_width {
                        let dst_x = glyph_left + col as i32;
                        if dst_x < 0 || dst_x >= p_width as i32 {
                            continue;
                        }

                        let coverage = cached.data[row * g_width + col];
                        if coverage > 0 {
                            let dst_idx = dst_row_idx + dst_x as usize;
                            let curr = fb[dst_idx] as u32;
                            let alpha = coverage as u32;
                            let blended = ((255 - alpha) * curr) / 255;
                            fb[dst_idx] = blended as u8;
                        }
                    }
                }
            }

            x += glyph.x_advance;
        }
    }
}
