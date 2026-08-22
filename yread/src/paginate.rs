//! Multi-page layout calculation and PageTable construction.

use hypher::Lang;
use crate::font::FontSystem;
use crate::line::{break_paragraph_lines_streaming, LayoutLine};
use crate::model::{Block, Chapter, ChapterPageTable, PageBreak, TextAlign};
use crate::shape::ShapeCache;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutConfig {
    pub page_width: u32,
    pub page_height: u32,
    pub margin_left: u32,
    pub margin_right: u32,
    pub margin_top: u32,
    pub margin_bottom: u32,
    pub font_size: f32,
    pub line_spacing: f32,
    pub paragraph_spacing: f32,
    pub indent_em: f32,
    pub hyphenate: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            page_width: 1236,
            page_height: 1648,
            margin_left: 54,
            margin_right: 54,
            margin_top: 54,
            margin_bottom: 54,
            font_size: 8.5,
            line_spacing: 1.10,
            paragraph_spacing: 0.15,
            indent_em: 1.2,
            hyphenate: true,
        }
    }
}

impl LayoutConfig {
    pub fn content_width(&self) -> f32 {
        self.page_width.saturating_sub(self.margin_left + self.margin_right) as f32
    }

    pub fn content_height(&self) -> f32 {
        self.page_height.saturating_sub(self.margin_top + self.margin_bottom) as f32
    }
}

#[derive(Debug, Clone)]
pub enum PageElement {
    Line {
        line: LayoutLine,
        x: f32,
        y: f32, // baseline Y
    },
    Bullet {
        shaped: std::sync::Arc<crate::shape::ShapedWord>,
        x: f32,
        y: f32,
        size_pt: f32,
    },
    CodeLine {
        shaped: std::sync::Arc<crate::shape::ShapedWord>,
        x: f32,
        y: f32,
        size_pt: f32,
    },
    CircleBullet {
        x: f32,
        y: f32,
        radius: f32,
    },
    QuoteBar {
        x: f32,
        y0: f32,
        y1: f32,
    },
    Image {
        id: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Rule {
        x: f32,
        y: f32,
        width: f32,
    },
}

#[derive(Debug, Clone, Default)]
pub struct PageLayout {
    pub page_idx: usize,
    pub start_char: usize,
    pub end_char: usize,
    pub elements: Vec<PageElement>,
}

use std::collections::HashMap;
use crate::model::FontStyle;

/// Push a finished page + its break record. `start_char`/`start_byte` may be
/// the pending sentinel (`usize::MAX`) when the page began at a
/// Rule/Image/CodeBlock and no text claimed it — then the last known char
/// position stands in, keeping page-break offsets monotonic.
fn finish_page(
    pages: &mut Vec<PageLayout>,
    breaks: &mut Vec<PageBreak>,
    mut page: PageLayout,
    block_idx: usize,
    start_char: usize,
    start_byte: usize,
    last_char: usize,
    last_byte: usize,
) {
    let sc = if start_char == usize::MAX { last_char } else { start_char };
    let sb = if start_byte == usize::MAX { last_byte } else { start_byte };
    page.start_char = sc;
    if page.end_char < sc {
        page.end_char = sc;
    }
    pages.push(page);
    breaks.push(PageBreak {
        block_idx,
        byte_offset: sb,
        char_offset: sc,
    });
}

/// Paginate a single chapter and return both its PageTable and precalculated PageLayouts.
pub fn paginate_chapter(
    chapter: &Chapter,
    config: &LayoutConfig,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
) -> (ChapterPageTable, Vec<PageLayout>) {
    paginate_chapter_with_images(chapter, None, config, fonts, cache, lang)
}

/// Paginate a single chapter with exact image dimension lookup to preserve aspect ratio.
pub fn paginate_chapter_with_images(
    chapter: &Chapter,
    image_sizes: Option<&HashMap<String, (u32, u32)>>,
    config: &LayoutConfig,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
) -> (ChapterPageTable, Vec<PageLayout>) {
    let content_w = config.content_width();
    let content_h = config.content_height();
    let indent_px = config.indent_em * config.font_size * (300.0 / 72.0);

    let mut pages = Vec::new();
    let mut page_breaks = Vec::new();

    let mut cur_page = PageLayout::default();
    let mut cur_y = 0.0f32;
    let mut cur_page_start_char = 0usize;
    let mut cur_page_start_byte = 0usize;
    let mut cur_block_idx = 0usize;
    // Furthest text position placed so far — resolves pending page starts
    // and end_chars for pages that end at non-text blocks.
    let mut last_char_pos = 0usize;
    let mut last_byte_pos = 0usize;
    let mut cur_byte_offset = 0usize;
    let mut cur_char_offset = 0usize;

    let target_lang = if config.hyphenate { lang } else { None };

    for (b_idx, block) in chapter.blocks.iter().enumerate() {
        match block {
            Block::Paragraph { runs, indent, align, left_margin_em, bullet_prefix, is_quote } => {
                let em_px = config.font_size * (300.0 / 72.0);
                let left_margin_px = *left_margin_em * em_px;

                let (bullet_shaped, is_circle_bullet, bullet_adv) = if let Some(ref bullet_str) = bullet_prefix {
                    if bullet_str.contains('•') {
                        (None, true, em_px * 0.9)
                    } else {
                        let shaped = cache.shape_word(bullet_str, FontStyle::Bold, config.font_size, fonts);
                        let adv = shaped.advance.max(em_px * 0.9);
                        (Some(shaped), false, adv)
                    }
                } else {
                    (None, false, 0.0)
                };

                let avail_w = (content_w - left_margin_px - bullet_adv).max(100.0);
                let first_indent = if *indent && bullet_prefix.is_none() { indent_px } else { 0.0 };

                let lines = break_paragraph_lines_streaming(
                    &chapter.text,
                    runs,
                    first_indent,
                    avail_w,
                    config.font_size,
                    config.line_spacing,
                    *align,
                    fonts,
                    cache,
                    target_lang,
                    &mut cur_byte_offset,
                    &mut cur_char_offset,
                );

                let n_lines = lines.len();

                // Widow & Orphan control (2-line rule)
                let mut max_lines_for_cur_page = usize::MAX;
                if !cur_page.elements.is_empty() && n_lines >= 2 {
                    let mut lines_that_fit = 0;
                    let mut test_y = cur_y;
                    for line in &lines {
                        if test_y + line.height <= content_h {
                            lines_that_fit += 1;
                            test_y += line.height;
                        } else {
                            break;
                        }
                    }

                    // 1. Orphan suppression: single line at bottom of page -> push whole paragraph to next page
                    if lines_that_fit == 1 {
                        max_lines_for_cur_page = 0;
                    }
                    // 2. Widow suppression: single line at top of next page -> pull 1 extra line to next page
                    else if lines_that_fit == n_lines - 1 && n_lines >= 3 {
                        max_lines_for_cur_page = n_lines - 2;
                    }
                }

                let block_start_y = cur_y;

                for (l_idx, line) in lines.into_iter().enumerate() {
                    let line_h = line.height;
                    let force_break = l_idx >= max_lines_for_cur_page;

                    // Does this line fit on current page?
                    if (cur_y + line_h > content_h || force_break) && !cur_page.elements.is_empty() {
                        // Finish current page
                        cur_page.end_char = line.start_char;
                        finish_page(
                            &mut pages,
                            &mut page_breaks,
                            std::mem::take(&mut cur_page),
                            cur_block_idx,
                            cur_page_start_char,
                            cur_page_start_byte,
                            last_char_pos,
                            last_byte_pos,
                        );

                        // Start new page
                        cur_page = PageLayout::default();
                        cur_page.page_idx = pages.len();
                        cur_y = 0.0;
                        cur_page_start_char = line.start_char;
                        cur_page_start_byte = line.start_byte;
                        cur_block_idx = b_idx;
                        max_lines_for_cur_page = usize::MAX;
                    }

                    // First text on a page that began at a non-text block
                    // claims the pending start.
                    if cur_page_start_char == usize::MAX {
                        cur_page_start_char = line.start_char;
                        cur_page_start_byte = line.start_byte;
                    }
                    last_char_pos = last_char_pos.max(line.end_char);
                    last_byte_pos = last_byte_pos.max(line.end_byte);

                    let baseline = cur_y + line.ascender;

                    if l_idx == 0 {
                        if is_circle_bullet {
                            cur_page.elements.push(PageElement::CircleBullet {
                                x: left_margin_px + em_px * 0.25,
                                y: baseline - em_px * 0.28,
                                radius: (em_px * 0.12).max(2.5),
                            });
                        } else if let Some(ref b_shaped) = bullet_shaped {
                            cur_page.elements.push(PageElement::Bullet {
                                shaped: std::sync::Arc::clone(b_shaped),
                                x: left_margin_px,
                                y: baseline,
                                size_pt: config.font_size,
                            });
                        }
                    }

                    let x_offset = left_margin_px + bullet_adv + if l_idx == 0 { first_indent } else { 0.0 };

                    cur_page.elements.push(PageElement::Line {
                        line,
                        x: x_offset,
                        y: baseline,
                    });
                    cur_y += line_h;
                }

                if *is_quote && cur_y > block_start_y {
                    cur_page.elements.push(PageElement::QuoteBar {
                        x: (left_margin_px - em_px * 0.4).max(0.0),
                        y0: block_start_y + 2.0,
                        y1: cur_y - 2.0,
                    });
                }

                // Paragraph spacing
                cur_y += config.font_size * config.paragraph_spacing;
            }
            Block::CodeBlock { code } => {
                let em_px = config.font_size * (300.0 / 72.0);
                let left_margin_px = em_px * 0.8;
                let code_size = config.font_size * 0.82;
                let line_h = code_size * (300.0 / 72.0) * 1.25;
                let block_start_y = cur_y;

                cur_y += config.font_size * 0.3; // Space before code block

                for c_line in code.lines() {
                    if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                        cur_page.end_char = last_char_pos;
                        finish_page(
                            &mut pages,
                            &mut page_breaks,
                            std::mem::take(&mut cur_page),
                            cur_block_idx,
                            cur_page_start_char,
                            cur_page_start_byte,
                            last_char_pos,
                            last_byte_pos,
                        );
                        cur_page = PageLayout::default();
                        cur_page.page_idx = pages.len();
                        cur_y = 0.0;
                        cur_page_start_char = usize::MAX;
                        cur_page_start_byte = usize::MAX;
                        cur_block_idx = b_idx;
                    }

                    if !c_line.trim().is_empty() {
                        let shaped = cache.shape_code_word(c_line, code_size, fonts);
                        cur_page.elements.push(PageElement::CodeLine {
                            shaped,
                            x: left_margin_px + 8.0,
                            y: cur_y + line_h * 0.75,
                            size_pt: code_size,
                        });
                    }
                    cur_y += line_h;
                }

                // Add subtle left vertical line to delineate code block cleanly
                if cur_y > block_start_y {
                    cur_page.elements.push(PageElement::QuoteBar {
                        x: (left_margin_px - 2.0).max(0.0),
                        y0: block_start_y + 4.0,
                        y1: cur_y - 2.0,
                    });
                }

                cur_y += config.font_size * 0.5; // Space after code block
            }
            Block::Heading { level: _, runs } => {
                let lines = break_paragraph_lines_streaming(
                    &chapter.text,
                    runs,
                    0.0,
                    content_w,
                    config.font_size,
                    1.2,
                    TextAlign::Center,
                    fonts,
                    cache,
                    None,
                    &mut cur_byte_offset,
                    &mut cur_char_offset,
                );

                let em_px = config.font_size * (300.0 / 72.0);
                let body_line_h = em_px * config.line_spacing;
                let heading_h: f32 = lines.iter().map(|l| l.height).sum::<f32>() + config.font_size * 0.9;
                let min_heading_room = heading_h + 2.0 * body_line_h;

                // Heading keep_with_next: must have room for heading + at least 2 body lines
                if cur_y + min_heading_room > content_h && !cur_page.elements.is_empty() {
                    cur_page.end_char = lines.first().map(|l| l.start_char).unwrap_or(last_char_pos);
                    finish_page(
                        &mut pages,
                        &mut page_breaks,
                        std::mem::take(&mut cur_page),
                        cur_block_idx,
                        cur_page_start_char,
                        cur_page_start_byte,
                        last_char_pos,
                        last_byte_pos,
                    );

                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
                    cur_page_start_char = lines.first().map(|l| l.start_char).unwrap_or(usize::MAX);
                    cur_page_start_byte = lines.first().map(|l| l.start_byte).unwrap_or(usize::MAX);
                    cur_block_idx = b_idx;
                }

                cur_y += config.font_size * 0.5; // Space before heading
                for line in lines {
                    if cur_page_start_char == usize::MAX {
                        cur_page_start_char = line.start_char;
                        cur_page_start_byte = line.start_byte;
                    }
                    last_char_pos = last_char_pos.max(line.end_char);
                    last_byte_pos = last_byte_pos.max(line.end_byte);
                    let baseline = cur_y + line.ascender;
                    cur_y += line.height;
                    cur_page.elements.push(PageElement::Line {
                        line,
                        x: 0.0,
                        y: baseline,
                    });
                }
                cur_y += config.font_size * 0.4; // Space after heading
            }
            Block::Spacer(px) => {
                if cur_y > 0.0 && cur_y + (*px as f32) < content_h {
                    cur_y += *px as f32;
                }
            }
            Block::Rule => {
                if cur_y + 10.0 > content_h && !cur_page.elements.is_empty() {
                    cur_page.end_char = last_char_pos;
                    finish_page(
                        &mut pages,
                        &mut page_breaks,
                        std::mem::take(&mut cur_page),
                        cur_block_idx,
                        cur_page_start_char,
                        cur_page_start_byte,
                        last_char_pos,
                        last_byte_pos,
                    );
                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
                    cur_page_start_char = usize::MAX;
                    cur_page_start_byte = usize::MAX;
                    cur_block_idx = b_idx;
                }
                cur_page.elements.push(PageElement::Rule {
                    x: 0.0,
                    y: cur_y + 5.0,
                    width: content_w,
                });
                cur_y += 15.0;
            }
            Block::Image { id, width, height, .. } => {
                let (orig_w, orig_h) = image_sizes
                    .and_then(|m| m.get(id))
                    .copied()
                    .unwrap_or_else(|| (width.unwrap_or(content_w as u32), height.unwrap_or(400)));

                let max_w = content_w;
                let max_h = content_h * 0.80; // Keep within page limits
                let scale = (max_w / orig_w as f32).min(max_h / orig_h as f32).min(1.0);
                let draw_w = (orig_w as f32 * scale).round();
                let draw_h = (orig_h as f32 * scale).round();

                if cur_y + draw_h > content_h && !cur_page.elements.is_empty() {
                    cur_page.end_char = last_char_pos;
                    finish_page(
                        &mut pages,
                        &mut page_breaks,
                        std::mem::take(&mut cur_page),
                        cur_block_idx,
                        cur_page_start_char,
                        cur_page_start_byte,
                        last_char_pos,
                        last_byte_pos,
                    );
                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
                    cur_page_start_char = usize::MAX;
                    cur_page_start_byte = usize::MAX;
                    cur_block_idx = b_idx;
                }

                let x = ((content_w - draw_w) / 2.0).max(0.0);
                cur_page.elements.push(PageElement::Image {
                    id: id.clone(),
                    x,
                    y: cur_y,
                    width: draw_w,
                    height: draw_h,
                });
                cur_y += draw_h + config.font_size * 0.5;
            }
        }
    }

    if !cur_page.elements.is_empty() || pages.is_empty() {
        cur_page.end_char = chapter.char_count();
        finish_page(
            &mut pages,
            &mut page_breaks,
            cur_page,
            cur_block_idx,
            cur_page_start_char,
            cur_page_start_byte,
            last_char_pos,
            last_byte_pos,
        );
    }

    (ChapterPageTable { pages: page_breaks }, pages)
}
