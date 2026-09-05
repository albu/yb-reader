//! Multi-page layout calculation and PageTable construction.

use crate::font::FontSystem;
use crate::line::{break_paragraph_lines_streaming, LayoutLine};
use crate::model::{Block, Chapter, ChapterPageTable, PageBreak, TextAlign};
use crate::shape::{ShapeCache, ShapedWord};
use hypher::Lang;

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
    /// Body-paragraph alignment override (Justify or Left). Book-set
    /// alignments (centered poetry, flush list items, headings) are kept.
    pub body_align: TextAlign,
    /// Word-spacing multiplier on the base space advance (1.0 = font
    /// default). Applied at tokenize time so the breaker, rasterizer and
    /// word-rect extraction all see the same final advance.
    pub word_spacing_mult: f32,
    /// Letter-spacing tracking in pixels added to every glyph advance
    /// (0.0 = none). Applied to a per-use copy — the shape cache stays
    /// valid.
    pub letter_spacing_px: f32,
}

impl LayoutConfig {
    /// The reader-app layout, parameterized exactly like the app's
    /// settings — the ONE place the margins/spacing recipe lives so the
    /// app backend and the profiling tools cannot drift apart (they did:
    /// rendertest measured 54/146/104-px margins while the app rendered
    /// 72/164/122).
    pub fn reader(
        vw: u32,
        vh: u32,
        margin_pad: u32,
        font_size: f32,
        line_spacing: f32,
        show_header: bool,
        paragraph_spacing: f32,
        indent_em: f32,
        hyphenate: bool,
        body_align: TextAlign,
        word_spacing_mult: f32,
        letter_spacing_px: f32,
    ) -> Self {
        Self {
            page_width: vw,
            page_height: vh,
            margin_left: margin_pad,
            margin_right: margin_pad,
            margin_top: margin_pad + if show_header { 98 } else { 0 },
            margin_bottom: margin_pad + 72,
            font_size,
            line_spacing,
            paragraph_spacing,
            indent_em,
            hyphenate,
            body_align,
            word_spacing_mult,
            letter_spacing_px,
        }
    }
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
            body_align: TextAlign::Justify,
            word_spacing_mult: 1.0,
            letter_spacing_px: 0.0,
        }
    }
}

impl LayoutConfig {
    pub fn content_width(&self) -> f32 {
        self.page_width
            .saturating_sub(self.margin_left + self.margin_right) as f32
    }

    pub fn content_height(&self) -> f32 {
        self.page_height
            .saturating_sub(self.margin_top + self.margin_bottom) as f32
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

use crate::model::FontStyle;
use std::collections::HashMap;

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
    let sc = if start_char == usize::MAX {
        last_char
    } else {
        start_char
    };
    let sb = if start_byte == usize::MAX {
        last_byte
    } else {
        start_byte
    };
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
    let em_px = config.font_size * (300.0 / 72.0);
    let indent_px = config.indent_em * em_px;

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
            Block::Paragraph {
                runs,
                indent,
                align,
                left_margin_em,
                bullet_prefix,
                is_quote,
            } => {
                let left_margin_px = *left_margin_em * em_px;

                let (bullet_shaped, is_circle_bullet, bullet_adv) = if let Some(ref bullet_str) =
                    bullet_prefix
                {
                    if bullet_str.contains('•') {
                        (None, true, em_px * 0.9)
                    } else {
                        let shaped =
                            cache.shape_word(bullet_str, FontStyle::Bold, config.font_size, fonts);
                        let adv = shaped.advance.max(em_px * 0.9);
                        (Some(shaped), false, adv)
                    }
                } else {
                    (None, false, 0.0)
                };

                let avail_w = (content_w - left_margin_px - bullet_adv).max(100.0);
                // The paragraph that directly follows a heading opens the
                // section and is set flush-left (no first-line indent).
                let follows_heading = matches!(
                    chapter.blocks.get(b_idx.wrapping_sub(1)),
                    Some(Block::Heading { .. })
                );
                let first_indent =
                    if *indent && bullet_prefix.is_none() && !follows_heading {
                        indent_px
                    } else {
                        0.0
                    };

                // The reader's alignment choice drives default (justified)
                // body paragraphs. Book-set alignments — centered poetry,
                // flush list items, right-aligned epigraphs — are kept, so
                // a ragged-left preference never destroys book grammar.
                let align = if *align == TextAlign::Justify
                    && matches!(config.body_align, TextAlign::Justify | TextAlign::Left)
                {
                    config.body_align
                } else {
                    *align
                };

                let lines = break_paragraph_lines_streaming(
                    &chapter.text,
                    runs,
                    first_indent,
                    avail_w,
                    config.font_size,
                    config.line_spacing,
                    align,
                    fonts,
                    cache,
                    target_lang,
                    config.word_spacing_mult,
                    config.letter_spacing_px,
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
                    // 2. Widow suppression: single line at top of next page -> pull 1 extra line to next page.
                    //    For a 3-line paragraph that pull would strand ONE line at the bottom — the very
                    //    orphan rule 1 exists to prevent — so push the whole paragraph instead.
                    else if lines_that_fit == n_lines - 1 && n_lines >= 3 {
                        max_lines_for_cur_page = if n_lines - 2 == 1 { 0 } else { n_lines - 2 };
                    }
                }

                let block_start_y = cur_y;

                for (l_idx, line) in lines.into_iter().enumerate() {
                    let line_h = line.height;
                    let force_break = l_idx >= max_lines_for_cur_page;

                    // Does this line fit on current page?
                    if (cur_y + line_h > content_h || force_break) && !cur_page.elements.is_empty()
                    {
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

                    let x_offset =
                        left_margin_px + bullet_adv + if l_idx == 0 { first_indent } else { 0.0 };

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
                cur_y += em_px * config.paragraph_spacing;
            }
            Block::CodeBlock { code } => {
                let left_margin_px = em_px * 0.8;
                let code_size = config.font_size * 0.82;
                let line_h = code_size * (300.0 / 72.0) * 1.25;
                let mut page_bar_start_y = cur_y;

                cur_y += em_px * 0.35; // Space before code block

                let code_x = left_margin_px + 8.0;
                let max_w = (content_w - code_x - 8.0).max(60.0);

                for c_line in code.lines() {
                    let expanded = c_line.replace('\t', "    ");

                    if expanded.trim().is_empty() {
                        if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                            if cur_y > page_bar_start_y {
                                cur_page.elements.push(PageElement::QuoteBar {
                                    x: (left_margin_px - 2.0).max(0.0),
                                    y0: page_bar_start_y + 4.0,
                                    y1: cur_y - 2.0,
                                });
                            }
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
                            page_bar_start_y = 0.0;
                        }
                        cur_y += line_h;
                        continue;
                    }

                    // Measure leading indentation of the line
                    let leading_spaces = expanded.chars().take_while(|c| *c == ' ').count();
                    let indent_px = if leading_spaces > 0 {
                        cache
                            .shape_code_word(&expanded[..leading_spaces], code_size, fonts)
                            .advance
                    } else {
                        0.0
                    };
                    let cont_indent = (indent_px + em_px * 0.5).min(max_w * 0.45);
                    let cont_x = code_x + cont_indent;
                    let cont_max_w = (content_w - cont_x - 8.0).max(60.0);

                    let shaped = cache.shape_code_word(&expanded, code_size, fonts);
                    if shaped.advance <= max_w {
                        if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                            if cur_y > page_bar_start_y {
                                cur_page.elements.push(PageElement::QuoteBar {
                                    x: (left_margin_px - 2.0).max(0.0),
                                    y0: page_bar_start_y + 4.0,
                                    y1: cur_y - 2.0,
                                });
                            }
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
                            page_bar_start_y = 0.0;
                        }
                        cur_page.elements.push(PageElement::CodeLine {
                            shaped,
                            x: code_x,
                            y: cur_y + line_h * 0.75,
                            size_pt: code_size,
                        });
                        cur_y += line_h;
                    } else {
                        // Wrap long code line into multiple segments that fit within max_w
                        let mut rem = expanded.as_str();
                        let mut is_first = true;

                        while !rem.is_empty() {
                            let (cur_x_pos, cur_avail_w) = if is_first {
                                (code_x, max_w)
                            } else {
                                (cont_x, cont_max_w)
                            };

                            let piece_shaped = cache.shape_code_word(rem, code_size, fonts);
                            if piece_shaped.advance <= cur_avail_w {
                                if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                                    if cur_y > page_bar_start_y {
                                        cur_page.elements.push(PageElement::QuoteBar {
                                            x: (left_margin_px - 2.0).max(0.0),
                                            y0: page_bar_start_y + 4.0,
                                            y1: cur_y - 2.0,
                                        });
                                    }
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
                                    page_bar_start_y = 0.0;
                                }
                                cur_page.elements.push(PageElement::CodeLine {
                                    shaped: piece_shaped,
                                    x: cur_x_pos,
                                    y: cur_y + line_h * 0.75,
                                    size_pt: code_size,
                                });
                                cur_y += line_h;
                                break;
                            }

                            let split_idx = find_code_break_point(
                                rem,
                                &piece_shaped,
                                cur_avail_w,
                                code_size,
                                fonts,
                                cache,
                            );

                            let head = &rem[..split_idx];
                            let tail = &rem[split_idx..];

                            let head_shaped = cache.shape_code_word(head, code_size, fonts);
                            if cur_y + line_h > content_h && !cur_page.elements.is_empty() {
                                if cur_y > page_bar_start_y {
                                    cur_page.elements.push(PageElement::QuoteBar {
                                        x: (left_margin_px - 2.0).max(0.0),
                                        y0: page_bar_start_y + 4.0,
                                        y1: cur_y - 2.0,
                                    });
                                }
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
                                page_bar_start_y = 0.0;
                            }
                            cur_page.elements.push(PageElement::CodeLine {
                                shaped: head_shaped,
                                x: cur_x_pos,
                                y: cur_y + line_h * 0.75,
                                size_pt: code_size,
                            });
                            cur_y += line_h;

                            is_first = false;
                            rem = tail.trim_start();
                        }
                    }
                }

                // Add subtle left vertical line to delineate code block cleanly
                if cur_y > page_bar_start_y {
                    cur_page.elements.push(PageElement::QuoteBar {
                        x: (left_margin_px - 2.0).max(0.0),
                        y0: page_bar_start_y + 4.0,
                        y1: cur_y - 2.0,
                    });
                }

                cur_y += em_px * 0.45; // Space after code block
            }
            Block::Heading { level, runs } => {
                // Reader-side heading hierarchy: chapter titles (<h1>)
                // centered, section headings flush-left. Overrides the
                // book's own heading alignment — the reader ignores source
                // CSS, so this is closer to typical book intent anyway.
                let heading_align = if *level <= 1 {
                    TextAlign::Center
                } else {
                    TextAlign::Left
                };
                let lines = break_paragraph_lines_streaming(
                    &chapter.text,
                    runs,
                    0.0,
                    content_w,
                    config.font_size,
                    1.2,
                    heading_align,
                    fonts,
                    cache,
                    None,
                    1.0,
                    0.0,
                    &mut cur_byte_offset,
                    &mut cur_char_offset,
                );

                let body_line_h = em_px * config.line_spacing;
                let heading_h: f32 =
                    lines.iter().map(|l| l.height).sum::<f32>() + em_px * 0.9;
                let min_heading_room = heading_h + 2.0 * body_line_h;

                // Heading keep_with_next: must have room for heading + at least 2 body lines
                if cur_y + min_heading_room > content_h && !cur_page.elements.is_empty() {
                    cur_page.end_char =
                        lines.first().map(|l| l.start_char).unwrap_or(last_char_pos);
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

                // Space before heading: generous (1.5em) to detach it from
                // the section above, but only mid-page — a heading that opens
                // a page or the chapter sits flush at the top margin.
                if cur_y > 0.0 {
                    let space_before = match *level {
                        1 => em_px * 1.8,
                        2 => em_px * 1.5,
                        _ => em_px * 1.2,
                    };
                    cur_y += space_before;
                }
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
                cur_y += em_px * 0.35; // Space after heading
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
            Block::Image {
                id, width, height, ..
            } => {
                let (orig_w, orig_h) = image_sizes
                    .and_then(|m| m.get(id))
                    .copied()
                    .unwrap_or_else(|| (width.unwrap_or(content_w as u32), height.unwrap_or(400)));

                // A sniffed/crafted header declaring a zero dimension
                // would turn the scale into inf (survived only by the
                // .min(1.0) ordering luck) and emit a 0×0 element — skip
                // the image outright.
                if orig_w == 0 || orig_h == 0 {
                    continue;
                }

                let max_w = content_w;
                let max_h = content_h * 0.85; // Keep within page limits

                let fit_scale = (max_w / orig_w as f32).min(max_h / orig_h as f32);

                // On e-ink screens (typically 300 DPI), images authored for standard 96 DPI displays
                // appear tiny (1/3 of intended physical size) if clamped to 1.0 hardware pixel scale.
                // - Small icons/bullets (orig < 100px) or horizontal divider ornaments (height < 40px)
                //   are scaled by the DPI factor (~3.0x) so they match the text size without blowing up.
                // - Substantive illustrations, diagrams, charts, photos, and book covers are scaled
                //   to fill the available content width/height (up to 5.0x max upscale to keep fidelity).
                let max_upscale = if (orig_w < 100 && orig_h < 100) || orig_h < 40 {
                    3.0
                } else {
                    5.0
                };
                let scale = fit_scale.min(max_upscale);

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
                cur_y += draw_h + em_px * 0.40;
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

/// Find the best character index to break a long code line so it fits within `max_w`.
///
/// Scans shaped glyph clusters to identify the slice that fits within `max_w`,
/// then prefers clean syntactic break points (whitespace, comma/semicolon, brackets, operators)
/// before falling back to a character boundary. Always takes at least 1 character to guarantee progress.
fn find_code_break_point(
    text: &str,
    shaped: &ShapedWord,
    max_w: f32,
    code_size: f32,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
) -> usize {
    let mut cum_x = 0.0;
    let mut last_valid_byte = 0;

    for g in &shaped.glyphs {
        cum_x += g.x_advance;
        let cluster = g.cluster as usize;
        if cum_x <= max_w && cluster <= text.len() {
            if text.is_char_boundary(cluster) {
                last_valid_byte = last_valid_byte.max(cluster);
            }
        } else {
            break;
        }
    }

    let first_char_end = text.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    if last_valid_byte < first_char_end {
        last_valid_byte = first_char_end;
    }

    let mut space_break = None;
    let mut punct_break = None;

    for (idx, c) in text[..last_valid_byte].char_indices() {
        let next_idx = idx + c.len_utf8();
        if c == ' ' {
            space_break = Some(next_idx);
        } else if matches!(c, ',' | ';' | ')' | ']' | '}' | '>' | ':' | '.' | '/' | '\\') {
            punct_break = Some(next_idx);
        } else if matches!(
            c,
            '(' | '[' | '{' | '<' | '=' | '+' | '-' | '*' | '&' | '|' | '!' | '?' | '%'
        ) {
            punct_break = Some(idx);
        }
    }

    let chosen = if let Some(sb) = space_break {
        if let Some(pb) = punct_break {
            if pb > sb && pb - sb > 8 {
                pb
            } else {
                sb
            }
        } else {
            sb
        }
    } else if let Some(pb) = punct_break {
        pb
    } else {
        last_valid_byte
    };

    let mut split_idx = chosen.max(first_char_end);
    if !text.is_char_boundary(split_idx) {
        while split_idx > first_char_end && !text.is_char_boundary(split_idx) {
            split_idx -= 1;
        }
    }

    // Verify head advance against max_w and retreat if kerning/shaping added width
    while split_idx > first_char_end {
        let head = &text[..split_idx];
        let w = cache.shape_code_word(head, code_size, fonts).advance;
        if w <= max_w {
            break;
        }
        let prev_len = text[..split_idx]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        split_idx -= prev_len;
    }

    split_idx.max(first_char_end)
}
