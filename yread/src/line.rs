//! Greedy line breaking with hyphenation, word tokenization, and line layout.

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
    let mut cur_byte = 0;
    let mut cur_char = 0;
    break_paragraph_lines_streaming(
        text,
        runs,
        first_line_indent_px,
        max_width,
        base_font_size,
        line_spacing_mult,
        align,
        fonts,
        cache,
        lang,
        &mut cur_byte,
        &mut cur_char,
    )
}

/// Tokenize and wrap runs with monotonic offset tracking across chapter blocks.
pub fn break_paragraph_lines_streaming(
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
    cur_byte_offset: &mut usize,
    cur_char_offset: &mut usize,
) -> Vec<LayoutLine> {
    if runs.is_empty() {
        return Vec::new();
    }

    // Step 1: Flatten runs into raw tokens (words, spaces, hard breaks)
    let mut tokens: Vec<LineItem> = Vec::new();

    for run in runs {
        let run_text = match text.get(run.start..run.end) {
            Some(t) => t,
            None => continue,
        };

        if run.start > *cur_byte_offset {
            // Same .get() guard as the run slice above: safe only while
            // runs arrive in text order at char boundaries — degrade to a
            // skip instead of panicking if that invariant is ever broken.
            if let Some(gap) = text.get(*cur_byte_offset..run.start) {
                *cur_char_offset += gap.chars().count();
            }
            *cur_byte_offset = run.start;
        }

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
            let word_char_start = *cur_char_offset;
            let word_char_end = word_char_start + word_char_count;

            if !word_str.is_empty() {
                let is_pure_punct = word_str.chars().all(|c| {
                    matches!(
                        c,
                        ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '”' | '’' | '»' | '…' | '%' | '°'
                    )
                });
                let prev_is_word = matches!(tokens.last(), Some(LineItem::Word { .. }));

                if is_pure_punct && prev_is_word {
                    if let Some(LineItem::Word { byte_start, byte_end, char_end, shaped, style, .. }) = tokens.last_mut() {
                        *byte_end = word_byte_end;
                        *char_end = word_char_end;
                        if let Some(full_str) = text.get(*byte_start..word_byte_end) {
                            let s_size = base_font_size * style.size_mult;
                            *shaped = cache.shape_word(full_str, style.font_style, s_size, fonts);
                        }
                    }
                } else {
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
            }

            *cur_char_offset += word_char_count;
            byte_offset += word_str.len();

            if trailing_nl {
                tokens.push(LineItem::HardBreak);
                *cur_char_offset += 1;
                byte_offset += 1;
            } else if has_trailing_space {
                let prev_is_space = matches!(tokens.last(), Some(LineItem::Space { .. }));
                if !prev_is_space {
                    let sp_adv = cache.space_advance(run.style.font_style, run_size, fonts);
                    tokens.push(LineItem::Space {
                        adv: sp_adv,
                        style: run.style.clone(),
                    });
                }
                *cur_char_offset += 1;
                byte_offset += 1;
            }
        }
        *cur_byte_offset = run.end;
    }

    if tokens.is_empty() {
        return Vec::new();
    }

    // Step 2: Split tokens by HardBreak (<br/>) segments and layout each segment
    let mut all_lines: Vec<LayoutLine> = Vec::new();
    let mut segment_tokens: Vec<LineItem> = Vec::new();
    let mut is_first_segment_line = true;

    for item in tokens {
        if matches!(item, LineItem::HardBreak) {
            let was_empty = segment_tokens.is_empty();
            let seg_lines = break_segment(
                text,
                std::mem::take(&mut segment_tokens),
                if is_first_segment_line { first_line_indent_px } else { 0.0 },
                max_width,
                base_font_size,
                line_spacing_mult,
                align,
                fonts,
                cache,
                lang,
                false,
            );

            if was_empty {
                let mut blank = build_line(
                    Vec::new(),
                    max_width,
                    align,
                    false,
                    base_font_size,
                    line_spacing_mult,
                    fonts,
                );
                if let Some(prev) = all_lines.last() {
                    blank.start_byte = prev.end_byte;
                    blank.end_byte = prev.end_byte;
                    blank.start_char = prev.end_char;
                    blank.end_char = prev.end_char;
                }
                all_lines.push(blank);
            } else {
                all_lines.extend(seg_lines);
            }
            is_first_segment_line = false;
        } else {
            segment_tokens.push(item);
        }
    }

    if !segment_tokens.is_empty() {
        let seg_lines = break_segment(
            text,
            segment_tokens,
            if is_first_segment_line { first_line_indent_px } else { 0.0 },
            max_width,
            base_font_size,
            line_spacing_mult,
            align,
            fonts,
            cache,
            lang,
            true,
        );
        all_lines.extend(seg_lines);
    }

    all_lines
}

/// Break a single segment (free of hard breaks) using greedy packing
/// with hyphenation.
fn break_segment(
    text: &str,
    mut tokens: Vec<LineItem>,
    first_line_indent_px: f32,
    max_width: f32,
    base_font_size: f32,
    line_spacing_mult: f32,
    align: TextAlign,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
    is_last_paragraph_segment: bool,
) -> Vec<LayoutLine> {
    // Strip leading and trailing spaces from the segment
    while tokens.first().map(|it| it.is_space()).unwrap_or(false) {
        tokens.remove(0);
    }
    while tokens.last().map(|it| it.is_space()).unwrap_or(false) {
        tokens.pop();
    }
    if tokens.is_empty() {
        return Vec::new();
    }

    // Knuth-Plass optimal breaking lived here for exactly one day
    // (2026-08-23). It accepts tight lines expecting the renderer to
    // squeeze spaces, and its width model drifted from the renderer's
    // at the edges — lines past the right margin, dropped spaces at
    // hyphen ends, then a squeeze fix that collapsed spaces at other
    // font sizes. Removed rather than parked behind a flag: a path no
    // run exercises cannot be kept correct, only assumed correct.
    // Greedy packs each line near-full, so justified spacing stays
    // small and uniform. Revive from history (git log -S knuth_plass)
    // only against per-font-size real-book verification.
    greedy_break(
        text,
        tokens,
        first_line_indent_px,
        max_width,
        base_font_size,
        line_spacing_mult,
        align,
        fonts,
        cache,
        lang,
        is_last_paragraph_segment,
    )
}

// ---------------------------------------------------------------------------
// Greedy Breaker
// ---------------------------------------------------------------------------

fn greedy_break(
    text: &str,
    tokens: Vec<LineItem>,
    first_line_indent_px: f32,
    max_width: f32,
    base_font_size: f32,
    line_spacing_mult: f32,
    align: TextAlign,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
    is_last_paragraph_segment: bool,
) -> Vec<LayoutLine> {
    let mut lines = Vec::new();
    let mut current_line_items: Vec<LineItem> = Vec::new();
    let mut current_line_width = 0.0f32;
    let mut is_first_line = true;

    for item in tokens {
        let allowed_width = if is_first_line {
            max_width - first_line_indent_px
        } else {
            max_width
        };

        let item_adv = item.advance();
        if current_line_items.is_empty() && item.is_space() {
            continue;
        }

        if current_line_width + item_adv <= allowed_width || current_line_items.is_empty() {
            current_line_width += item_adv;
            current_line_items.push(item);
        } else {
            // Check hyphenation
            let mut hyphenated = false;
            if let (Some(target_lang), LineItem::Word { byte_start, byte_end, char_start, style, shaped: word_shaped, .. }) = (lang, &item) {
                if let Some(word_text) = text.get(*byte_start..*byte_end) {
                    if word_text.chars().count() >= 5 {
                        let syllables: Vec<&str> = hyphenate(word_text, target_lang).collect();
                        if syllables.len() >= 2 {
                            let run_size = base_font_size * style.size_mult;
                            let hyp_adv = cache.hyphen_advance(style.font_style, run_size, fonts);
                            let mut prefix = String::new();
                            let mut best_break: Option<(usize, usize, f32)> = None;
                            let boundaries = BoundaryAdvances::new(&word_shaped.glyphs);

                            for &syl in &syllables[..syllables.len() - 1] {
                                prefix.push_str(syl);
                                if let Some(p_adv) = boundaries.advance(prefix.len()) {
                                    if current_line_width + p_adv + hyp_adv <= allowed_width {
                                        best_break = Some((prefix.len(), prefix.chars().count(), p_adv));
                                    }
                                }
                            }

                            if let Some((pref_bytes, pref_chars, _p_adv)) = best_break {
                                let prefix_str = &word_text[..pref_bytes];
                                let prefix_shaped = cache.shape_word(prefix_str, style.font_style, run_size, fonts);
                                current_line_items.push(LineItem::HyphenatedPrefix {
                                    byte_start: *byte_start,
                                    byte_end: *byte_start + pref_bytes,
                                    char_start: *char_start,
                                    char_end: *char_start + pref_chars,
                                    prefix_shaped,
                                    hyphen_adv: hyp_adv,
                                    style: style.clone(),
                                });

                                let suffix = &word_text[pref_bytes..];
                                let suffix_shaped = cache.shape_word(suffix, style.font_style, run_size, fonts);
                                let suffix_item = LineItem::Word {
                                    byte_start: *byte_start + pref_bytes,
                                    byte_end: *byte_end,
                                    char_start: *char_start + pref_chars,
                                    char_end: *char_start + word_text.chars().count(),
                                    shaped: suffix_shaped.clone(),
                                    style: style.clone(),
                                };

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

                                current_line_items.push(suffix_item);
                                current_line_width = suffix_shaped.advance;
                                hyphenated = true;
                            }
                        }
                    }
                }
            }

            if !hyphenated {
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
            is_last_paragraph_segment,
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

/// Prefix-advance lookup over one shaped word's glyph clusters. Built once
/// per hyphenated word so each syllable-boundary query is O(log G); the old
/// per-syllable rescan from glyph 0 made an oversized word O(syllables ×
/// glyphs) — effectively quadratic, enough to hang pagination on a huge
/// unbroken token.
struct BoundaryAdvances {
    /// cum[i] = total x_advance of glyphs[..i]
    cum: Vec<f32>,
    clusters: Vec<u32>,
}

impl BoundaryAdvances {
    fn new(glyphs: &[crate::shape::ShapedGlyph]) -> Self {
        let mut cum = Vec::with_capacity(glyphs.len() + 1);
        let mut clusters = Vec::with_capacity(glyphs.len());
        cum.push(0.0);
        for g in glyphs {
            cum.push(cum.last().copied().unwrap_or(0.0) + g.x_advance);
            clusters.push(g.cluster);
        }
        Self { cum, clusters }
    }

    /// Advance width of the byte-prefix [0, boundary) of the word, read off
    /// its glyph clusters. None when the boundary splits a cluster
    /// (ligature / attached mark) — that hyphenation point is invalid.
    /// Clusters are monotonic for the scripts we hyphenate.
    fn advance(&self, boundary: usize) -> Option<f32> {
        let i = self.clusters.partition_point(|&c| (c as usize) < boundary);
        if i < self.clusters.len() && self.clusters[i] as usize == boundary {
            Some(self.cum[i])
        } else {
            None
        }
    }
}
