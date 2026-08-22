//! Line breaking, word tokenization, hyphenation, and line layout.

use std::sync::Arc;
use hypher::{hyphenate, Lang};
use crate::font::FontSystem;
use crate::model::{Run, Style, TextAlign};
use crate::shape::{ShapeCache, ShapedWord};

#[derive(Debug, Clone)]
pub enum LineItem {
    Word {
        byte_start: usize,
        byte_end: usize,
        char_start: usize,
        char_end: usize,
        shaped: Arc<ShapedWord>,
        style: Style,
    },
    HyphenatedPrefix {
        byte_start: usize,
        byte_end: usize,
        char_start: usize,
        char_end: usize,
        prefix_shaped: Arc<ShapedWord>,
        hyphen_adv: f32,
        style: Style,
    },
    Space {
        adv: f32,
        style: Style,
    },
    /// Explicit <br/> — force-ends the current line. Consumed by the
    /// breaker; never part of a rendered line.
    HardBreak,
}

impl LineItem {
    pub fn advance(&self) -> f32 {
        match self {
            LineItem::Word { shaped, .. } => shaped.advance,
            LineItem::HyphenatedPrefix { prefix_shaped, hyphen_adv, .. } => {
                prefix_shaped.advance + hyphen_adv
            }
            LineItem::Space { adv, .. } => *adv,
            LineItem::HardBreak => 0.0,
        }
    }

    pub fn is_space(&self) -> bool {
        matches!(self, LineItem::Space { .. })
    }

    pub fn byte_range(&self) -> Option<(usize, usize)> {
        match self {
            LineItem::Word { byte_start, byte_end, .. } => Some((*byte_start, *byte_end)),
            LineItem::HyphenatedPrefix { byte_start, byte_end, .. } => Some((*byte_start, *byte_end)),
            _ => None,
        }
    }

    pub fn char_range(&self) -> Option<(usize, usize)> {
        match self {
            LineItem::Word { char_start, char_end, .. } => Some((*char_start, *char_end)),
            LineItem::HyphenatedPrefix { char_start, char_end, .. } => Some((*char_start, *char_end)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayoutLine {
    pub items: Vec<LineItem>,
    pub width: f32,
    pub max_width: f32,
    pub height: f32,
    pub ascender: f32,
    pub descender: f32,
    pub align: TextAlign,
    pub is_last_in_paragraph: bool,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_char: usize,
    pub end_char: usize,
}

/// Tokenize and wrap runs into lines fitting `max_width`.
pub fn break_paragraph_lines(
    text: &str,
    runs: &[Run],
    first_line_indent_px: f32,
    max_width: f32,
    base_font_size: f32,
    line_spacing_mult: f32,
    align: TextAlign,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
) -> Vec<LayoutLine> {
    if runs.is_empty() {
        return Vec::new();
    }

    // Step 1: Flatten runs into token list (words and spaces)
    let mut tokens = Vec::new();
    let mut cur_char_idx = if let Some(first_run) = runs.first() {
        text[..first_run.start].chars().count()
    } else {
        0
    };
    let mut last_byte_offset = runs.first().map(|r| r.start).unwrap_or(0);

    for run in runs {
        let run_text = match text.get(run.start..run.end) {
            Some(t) => t,
            None => continue,
        };

        if run.start > last_byte_offset {
            cur_char_idx += text[last_byte_offset..run.start].chars().count();
        }
        last_byte_offset = run.end;

        let run_size = base_font_size * run.style.size_mult;
        let mut byte_offset = run.start;

        for part in run_text.split_inclusive(|c: char| c == ' ' || c == '\n') {
            let trailing_nl = part.ends_with('\n');
            let has_trailing_space = part.ends_with(' ') || trailing_nl;
            let word_str = if has_trailing_space {
                &part[..part.len() - 1]
            } else {
                part
            };

            let word_byte_start = byte_offset;
            let word_byte_end = word_byte_start + word_str.len();
            let word_char_count = word_str.chars().count();
            let word_char_start = cur_char_idx;
            let word_char_end = word_char_start + word_char_count;

            if !word_str.is_empty() {
                let shaped = cache.shape_word(word_str, run.style.font_style, run_size, fonts);
                tokens.push(LineItem::Word {
                    byte_start: word_byte_start,
                    byte_end: word_byte_end,
                    char_start: word_char_start,
                    char_end: word_char_end,
                    shaped,
                    style: run.style.clone(),
                });
            }

            cur_char_idx += word_char_count;
            byte_offset += word_str.len();

            if trailing_nl {
                tokens.push(LineItem::HardBreak);
                cur_char_idx += 1;
                byte_offset += 1;
            } else if has_trailing_space {
                // Runs whose text nodes each normalize to edge spaces produce
                // adjacent Space tokens ("text  more") — collapse them so
                // justify doesn't count phantom gaps.
                let prev_is_space = matches!(tokens.last(), Some(LineItem::Space { .. }));
                if !prev_is_space {
                    let sp_adv = cache.space_advance(run.style.font_style, run_size, fonts);
                    tokens.push(LineItem::Space {
                        adv: sp_adv,
                        style: run.style.clone(),
                    });
                }
                cur_char_idx += 1;
                byte_offset += 1;
            }
        }
    }

    if tokens.is_empty() {
        return Vec::new();
    }

    // Step 2: Greedy line breaking
    let mut lines: Vec<LayoutLine> = Vec::new();
    let mut current_line_items: Vec<LineItem> = Vec::new();
    let mut current_line_width = 0.0f32;
    let mut is_first_line = true;

    for item in tokens {
        let allowed_width = if is_first_line {
            max_width - first_line_indent_px
        } else {
            max_width
        };

        // <br/>: force-end the current line. An empty item list here means
        // a consecutive break — emit a blank line that inherits the previous
        // line's text range so char offsets stay monotonic.
        if matches!(item, LineItem::HardBreak) {
            let was_empty = current_line_items.is_empty();
            while current_line_items.last().map(|it| it.is_space()).unwrap_or(false) {
                current_line_items.pop();
            }
            let mut line = build_line(
                std::mem::take(&mut current_line_items),
                allowed_width,
                align,
                false,
                base_font_size,
                line_spacing_mult,
                fonts,
            );
            if was_empty {
                if let Some(prev) = lines.last() {
                    line.start_byte = prev.end_byte;
                    line.end_byte = prev.end_byte;
                    line.start_char = prev.end_char;
                    line.end_char = prev.end_char;
                }
            }
            lines.push(line);
            is_first_line = false;
            current_line_width = 0.0;
            continue;
        }

        let item_adv = item.advance();

        // If line is empty and item is a space, skip leading space
        if current_line_items.is_empty() && item.is_space() {
            continue;
        }

        if current_line_width + item_adv <= allowed_width || current_line_items.is_empty() {
            current_line_width += item_adv;
            current_line_items.push(item);
        } else {
            // Check if we can hyphenate the word before breaking
            let mut hyphenated = false;
            if let (Some(target_lang), LineItem::Word { byte_start, byte_end, char_start, style, .. }) = (lang, &item) {
                if let Some(word_text) = text.get(*byte_start..*byte_end) {
                    if word_text.chars().count() >= 5 {
                        let syllables: Vec<&str> = hyphenate(word_text, target_lang).collect();
                        if syllables.len() >= 2 {
                            let run_size = base_font_size * style.size_mult;
                            let hyp_adv = cache.hyphen_advance(style.font_style, run_size, fonts);
                            let mut prefix = String::new();
                            let mut best_break: Option<(usize, usize, Arc<crate::shape::ShapedWord>)> = None;

                            for &syl in &syllables[..syllables.len() - 1] {
                                prefix.push_str(syl);
                                let prefix_shaped = cache.shape_word(&prefix, style.font_style, run_size, fonts);
                                if current_line_width + prefix_shaped.advance + hyp_adv <= allowed_width {
                                    best_break = Some((prefix.len(), prefix.chars().count(), prefix_shaped));
                                }
                            }

                            if let Some((pref_bytes, pref_chars, prefix_shaped)) = best_break {
                                current_line_items.push(LineItem::HyphenatedPrefix {
                                    byte_start: *byte_start,
                                    byte_end: *byte_start + pref_bytes,
                                    char_start: *char_start,
                                    char_end: *char_start + pref_chars,
                                    prefix_shaped: Arc::clone(&prefix_shaped),
                                    hyphen_adv: hyp_adv,
                                    style: style.clone(),
                                });

                                // Remainder becomes the start of the next line
                                let suffix = &word_text[pref_bytes..];
                                let suffix_shaped = cache.shape_word(suffix, style.font_style, run_size, fonts);
                                let suffix_item = LineItem::Word {
                                    byte_start: *byte_start + pref_bytes,
                                    byte_end: *byte_end,
                                    char_start: *char_start + pref_chars,
                                    char_end: *char_start + word_text.chars().count(),
                                    shaped: Arc::clone(&suffix_shaped),
                                    style: style.clone(),
                                };

                                // Finish current line
                                let line = build_line(
                                    std::mem::take(&mut current_line_items),
                                    allowed_width,
                                    align,
                                    false,
                                    base_font_size,
                                    line_spacing_mult,
                                    fonts,
                                );
                                lines.push(line);

                                // Start new line with the suffix
                                current_line_items.push(suffix_item);
                                current_line_width = suffix_shaped.advance;
                                hyphenated = true;
                            }
                        }
                    }
                }
            }

            if !hyphenated {
                // Break line without hyphenation
                // Trim trailing space from finished line
                while current_line_items.last().map(|it| it.is_space()).unwrap_or(false) {
                    current_line_items.pop();
                }

                let line = build_line(
                    std::mem::take(&mut current_line_items),
                    allowed_width,
                    align,
                    false,
                    base_font_size,
                    line_spacing_mult,
                    fonts,
                );
                lines.push(line);
                is_first_line = false;

                if !item.is_space() {
                    current_line_width = item.advance();
                    current_line_items.push(item);
                } else {
                    current_line_width = 0.0;
                }
            }
        }
    }

    if !current_line_items.is_empty() {
        while current_line_items.last().map(|it| it.is_space()).unwrap_or(false) {
            current_line_items.pop();
        }
        let allowed_width = if is_first_line {
            max_width - first_line_indent_px
        } else {
            max_width
        };
        let line = build_line(
            current_line_items,
            allowed_width,
            align,
            true,
            base_font_size,
            line_spacing_mult,
            fonts,
        );
        lines.push(line);
    }

    lines
}

fn build_line(
    items: Vec<LineItem>,
    max_width: f32,
    align: TextAlign,
    is_last: bool,
    base_font_size: f32,
    line_spacing_mult: f32,
    fonts: &FontSystem,
) -> LayoutLine {
    let width: f32 = items.iter().map(|it| it.advance()).sum();
    let mut start_byte = usize::MAX;
    let mut end_byte = 0;
    let mut start_char = usize::MAX;
    let mut end_char = 0;

    let mut max_ascender = 0.0f32;
    let mut max_descender = 0.0f32;

    for it in &items {
        if let Some((b0, b1)) = it.byte_range() {
            start_byte = start_byte.min(b0);
            end_byte = end_byte.max(b1);
        }
        if let Some((c0, c1)) = it.char_range() {
            start_char = start_char.min(c0);
            end_char = end_char.max(c1);
        }

        let style = match it {
            LineItem::Word { style, .. } | LineItem::HyphenatedPrefix { style, .. } | LineItem::Space { style, .. } => style,
            LineItem::HardBreak => continue,
        };
        let m = fonts.metrics(style.font_style, base_font_size * style.size_mult);
        max_ascender = max_ascender.max(m.ascender);
        max_descender = max_descender.max(m.descender);
    }

    if start_byte == usize::MAX {
        start_byte = 0;
    }
    if start_char == usize::MAX {
        start_char = 0;
    }

    let natural_height = (max_ascender + max_descender).max(base_font_size * (300.0 / 72.0));
    let height = natural_height * line_spacing_mult;

    LayoutLine {
        items,
        width,
        max_width,
        height,
        ascender: max_ascender,
        descender: max_descender,
        align,
        is_last_in_paragraph: is_last,
        start_byte,
        end_byte,
        start_char,
        end_char,
    }
}
