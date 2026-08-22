//! Glyph rasterization using swash and 8-bit grayscale framebuffer blitting.

use swash::scale::{Render, ScaleContext, Source, StrikeWith};

use crate::font::FontSystem;
use crate::line::{LayoutLine, LineItem};
use crate::model::{Book, TextAlign};
use crate::paginate::{LayoutConfig, PageElement, PageLayout};

pub struct Rasterizer {
    scale_ctx: ScaleContext,
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self {
            scale_ctx: ScaleContext::new(),
        }
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
                        face,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
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
                    let rw = (*width).round() as usize;
                    if ry < p_height {
                        for col in rx..(rx + rw).min(p_width) {
                            fb[ry * stride + col] = 0;
                        }
                    }
                }
                PageElement::Image { id, x, y, width, height } => {
                    if let Some(data) = book.images.get(id) {
                        if let Ok(img) = image::load_from_memory(data) {
                            let gray = img.to_luma8();
                            let target_w = width.round() as u32;
                            let target_h = height.round() as u32;
                            if target_w > 0 && target_h > 0 {
                                let resized = image::imageops::resize(
                                    &gray,
                                    target_w,
                                    target_h,
                                    image::imageops::FilterType::Lanczos3,
                                );

                                let dst_x = (origin_x + x).round() as usize;
                                let dst_y = (origin_y + y).round() as usize;

                                for (ix, iy, px) in resized.enumerate_pixels() {
                                    let target_col = dst_x + ix as usize;
                                    let target_row = dst_y + iy as usize;
                                    if target_col < p_width && target_row < p_height {
                                        fb[target_row * stride + target_col] = px.0[0];
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn render_line(
        &mut self,
        line: &LayoutLine,
        x: f32,
        baseline_y: f32,
        base_font_size: f32,
        fonts: &FontSystem,
        fb: &mut [u8],
        stride: usize,
        p_width: usize,
        p_height: usize,
    ) {
        // Calculate justification extra space
        let mut extra_space_per_gap = 0.0f32;
        let mut cur_x = x;

        match line.align {
            TextAlign::Center => {
                let slack = (line.max_width - line.width).max(0.0);
                cur_x += slack / 2.0;
            }
            TextAlign::Right => {
                let slack = (line.max_width - line.width).max(0.0);
                cur_x += slack;
            }
            TextAlign::Justify => {
                if !line.is_last_in_paragraph && line.width < line.max_width {
                    let space_count = line.items.iter().filter(|it| it.is_space()).count();
                    if space_count > 0 {
                        let slack = line.max_width - line.width;
                        // Avoid extreme justification distortion on short lines
                        if slack < line.max_width * 0.35 {
                            extra_space_per_gap = slack / space_count as f32;
                        }
                    }
                }
            }
            TextAlign::Left => {}
        }

        for item in &line.items {
            match item {
                LineItem::Word { shaped, style, .. } => {
                    let run_size = base_font_size * style.size_mult;
                    let face = fonts.face_for_style(style.font_style);
                    let mut baseline = baseline_y;
                    if style.is_sup {
                        baseline -= run_size * (300.0 / 72.0) * 0.35;
                    } else if style.is_sub {
                        baseline += run_size * (300.0 / 72.0) * 0.20;
                    }
                    self.render_shaped_word(
                        shaped,
                        cur_x,
                        baseline,
                        run_size,
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
                        face,
                        fb,
                        stride,
                        p_width,
                        p_height,
                    );
                    cur_x += prefix_shaped.advance;

                    // Draw hyphen
                    // Simplified: rustybuzz hyphen glyph or simple bar
                    let hyp_y = baseline_y - (run_size * 0.3 * (300.0 / 72.0));
                    let hyp_x = cur_x;
                    let hyp_w = (hyphen_adv * 0.7).max(4.0) as usize;
                    let h_row = hyp_y.round() as usize;
                    if h_row < p_height {
                        for col in (hyp_x.round() as usize)..(hyp_x.round() as usize + hyp_w).min(p_width) {
                            fb[h_row * stride + col] = 0;
                        }
                    }
                    cur_x += *hyphen_adv;
                }
                LineItem::Space { adv, .. } => {
                    cur_x += *adv + extra_space_per_gap;
                }
            }
        }
    }

    fn render_shaped_word(
        &mut self,
        shaped: &crate::shape::ShapedWord,
        mut x: f32,
        baseline_y: f32,
        size_pt: f32,
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
        let mut scaler = self
            .scale_ctx
            .builder(swash_font)
            .size(px_size)
            .hint(false)
            .build();

        for glyph in &shaped.glyphs {
            let gx = x + glyph.x_offset;
            let gy = baseline_y - glyph.y_offset;

            if let Some(image) = Render::new(&[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ])
            .render(&mut scaler, glyph.glyph_id)
            {
                let glyph_left = (gx + image.placement.left as f32).round() as i32;
                let glyph_top = (gy - image.placement.top as f32).round() as i32;
                let g_width = image.placement.width as usize;
                let g_height = image.placement.height as usize;

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

                        let coverage = image.data[row * g_width + col];
                        if coverage > 0 {
                            let dst_idx = dst_row_idx + dst_x as usize;
                            // Blend black glyph on existing pixel:
                            // dst = (cov * 0 + (255 - cov) * dst) / 255
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
