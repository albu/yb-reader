//! Multi-page layout calculation and PageTable construction.

use hypher::Lang;
use crate::font::FontSystem;
use crate::line::{break_paragraph_lines, LayoutLine};
use crate::model::{Block, Chapter, ChapterPageTable, PageBreak, TextAlign};
use crate::shape::ShapeCache;

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub page_width: u32,
    pub page_height: u32,
    pub margin_left: u32,
    pub margin_right: u32,
    pub margin_top: u32,
    pub margin_bottom: u32,
    pub font_size: f32,
    pub line_spacing: f32,
    pub indent_em: f32,
    pub hyphenate: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            page_width: 1236,
            page_height: 1648,
            margin_left: 72,
            margin_right: 72,
            margin_top: 72,
            margin_bottom: 72,
            font_size: 11.0,
            line_spacing: 1.2,
            indent_em: 1.5,
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

/// Paginate a single chapter and return both its PageTable and precalculated PageLayouts.
pub fn paginate_chapter(
    chapter: &Chapter,
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

    let target_lang = if config.hyphenate { lang } else { None };

    for (b_idx, block) in chapter.blocks.iter().enumerate() {
        match block {
            Block::Paragraph { runs, indent, align } => {
                let first_indent = if *indent { indent_px } else { 0.0 };
                let lines = break_paragraph_lines(
                    &chapter.text,
                    runs,
                    first_indent,
                    content_w,
                    config.font_size,
                    config.line_spacing,
                    *align,
                    fonts,
                    cache,
                    target_lang,
                );

                for (l_idx, line) in lines.into_iter().enumerate() {
                    let line_h = line.height;
                    // Does this line fit on current page?
                    if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                        // Finish current page
                        cur_page.end_char = line.start_char;
                        pages.push(cur_page);
                        page_breaks.push(PageBreak {
                            block_idx: cur_block_idx,
                            byte_offset: cur_page_start_byte,
                            char_offset: cur_page_start_char,
                        });

                        // Start new page
                        cur_page = PageLayout::default();
                        cur_page.page_idx = pages.len();
                        cur_y = 0.0;
                        cur_page_start_char = line.start_char;
                        cur_page_start_byte = line.start_byte;
                        cur_block_idx = b_idx;
                    }

                    let x_offset = if l_idx == 0 && *indent { first_indent } else { 0.0 };
                    let baseline = cur_y + line.ascender;

                    cur_page.elements.push(PageElement::Line {
                        line,
                        x: x_offset,
                        y: baseline,
                    });
                    cur_y += line_h;
                }

                // Paragraph spacing
                cur_y += config.font_size * 0.4;
            }
            Block::Heading { level, runs } => {
                let size_mult = match level {
                    1 => 1.5,
                    2 => 1.3,
                    _ => 1.15,
                };
                let lines = break_paragraph_lines(
                    &chapter.text,
                    runs,
                    0.0,
                    content_w,
                    config.font_size * size_mult,
                    config.line_spacing,
                    TextAlign::Center,
                    fonts,
                    cache,
                    None,
                );

                let heading_h: f32 = lines.iter().map(|l| l.height).sum::<f32>() + config.font_size * 1.5;

                // Orphan prevention: if heading + spacing doesn't leave room on page, break early
                if cur_y + heading_h > content_h && !cur_page.elements.is_empty() {
                    cur_page.end_char = lines.first().map(|l| l.start_char).unwrap_or(0);
                    pages.push(cur_page);
                    page_breaks.push(PageBreak {
                        block_idx: cur_block_idx,
                        byte_offset: cur_page_start_byte,
                        char_offset: cur_page_start_char,
                    });

                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
                    cur_page_start_char = lines.first().map(|l| l.start_char).unwrap_or(0);
                    cur_page_start_byte = lines.first().map(|l| l.start_byte).unwrap_or(0);
                    cur_block_idx = b_idx;
                }

                cur_y += config.font_size * 0.8; // Space before heading
                for line in lines {
                    let baseline = cur_y + line.ascender;
                    cur_y += line.height;
                    cur_page.elements.push(PageElement::Line {
                        line,
                        x: 0.0,
                        y: baseline,
                    });
                }
                cur_y += config.font_size * 0.8; // Space after heading
            }
            Block::Spacer(px) => {
                if cur_y > 0.0 && cur_y + (*px as f32) < content_h {
                    cur_y += *px as f32;
                }
            }
            Block::Rule => {
                if cur_y + 10.0 > content_h && !cur_page.elements.is_empty() {
                    pages.push(cur_page);
                    page_breaks.push(PageBreak {
                        block_idx: cur_block_idx,
                        byte_offset: cur_page_start_byte,
                        char_offset: cur_page_start_char,
                    });
                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
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
                let img_w = width.unwrap_or(content_w as u32) as f32;
                let img_h = height.unwrap_or(400) as f32;
                let scale = (content_w / img_w).min(1.0);
                let draw_w = img_w * scale;
                let draw_h = img_h * scale;

                if cur_y + draw_h > content_h && !cur_page.elements.is_empty() {
                    pages.push(cur_page);
                    page_breaks.push(PageBreak {
                        block_idx: cur_block_idx,
                        byte_offset: cur_page_start_byte,
                        char_offset: cur_page_start_char,
                    });
                    cur_page = PageLayout::default();
                    cur_page.page_idx = pages.len();
                    cur_y = 0.0;
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
                cur_y += draw_h + 10.0;
            }
        }
    }

    if !cur_page.elements.is_empty() || pages.is_empty() {
        cur_page.end_char = chapter.char_count();
        pages.push(cur_page);
        page_breaks.push(PageBreak {
            block_idx: cur_block_idx,
            byte_offset: cur_page_start_byte,
            char_offset: cur_page_start_char,
        });
    }

    (ChapterPageTable { pages: page_breaks }, pages)
}
