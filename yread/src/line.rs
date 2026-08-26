//! Greedy line breaking with hyphenation, word tokenization, and line layout.

use crate::font::FontSystem;
use crate::model::{FontStyle, Run, Style, TextAlign};
use crate::shape::{ShapeCache, ShapedWord};
use hypher::{hyphenate, Lang};
use std::sync::Arc;

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
        /// True for U+00A0 (non-breaking space): renders as a space but is
        /// never a legal line break ("Mr. Smith", "10 km" stay whole).
        nobreak: bool,
        /// TeX-style interword glue tolerance: how far this gap may grow
        /// (stretch) or shrink beyond its natural advance on a justified
        /// line. Defaults to ~space/2 and ~space/3. The breaker stores the
        /// solved per-gap adjustment on the LayoutLine; these fields keep
        /// the width model self-describing and ready for optimal
        /// (Knuth-Plass) breaking, which needs per-gap glue values.
        stretch: f32,
        shrink: f32,
    },
    /// A soft hyphen (U+00AD) between two word fragments: a legal break
    /// opportunity that draws a hyphen only when the line actually breaks
    /// there, and is invisible otherwise.
    SoftHyphen {
        style: Style,
        hyphen_adv: f32,
    },
    /// A hyphen glyph materialized at a line end that broke at a soft
    /// hyphen. Advance matches the hyphen glyph so later items on the
    /// next line keep x positions aligned with ink.
    Hyphen {
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
            LineItem::HyphenatedPrefix {
                prefix_shaped,
                hyphen_adv,
                ..
            } => prefix_shaped.advance + hyphen_adv,
            LineItem::Space { adv, .. } => *adv,
            LineItem::SoftHyphen { .. } => 0.0,
            LineItem::Hyphen { adv, .. } => *adv,
            LineItem::HardBreak => 0.0,
        }
    }

    pub fn is_space(&self) -> bool {
        matches!(self, LineItem::Space { .. })
    }

    pub fn byte_range(&self) -> Option<(usize, usize)> {
        match self {
            LineItem::Word {
                byte_start,
                byte_end,
                ..
            } => Some((*byte_start, *byte_end)),
            LineItem::HyphenatedPrefix {
                byte_start,
                byte_end,
                ..
            } => Some((*byte_start, *byte_end)),
            _ => None,
        }
    }

    pub fn char_range(&self) -> Option<(usize, usize)> {
        match self {
            LineItem::Word {
                char_start,
                char_end,
                ..
            } => Some((*char_start, *char_end)),
            LineItem::HyphenatedPrefix {
                char_start,
                char_end,
                ..
            } => Some((*char_start, *char_end)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayoutLine {
    pub items: Vec<LineItem>,
    pub width: f32,
    pub max_width: f32,
    /// Solved leading pen offset (centered/right-aligned lines). Computed
    /// once at build time — the single source of truth that both the
    /// rasterizer and the app's word-rect extraction consume.
    pub start_offset: f32,
    /// Solved justification: extra advance added to every inter-word gap
    /// of a justified line, 0 otherwise.
    pub extra_space: f32,
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

impl LayoutLine {
    /// The solved alignment as (leading offset, per-gap extra advance).
    /// Raster and hit-testing must both read these stored values — the
    /// historical drift between independently computed versions broke
    /// dictionary taps on justified lines (see `solve_alignment`).
    pub fn alignment_adjust(&self) -> (f32, f32) {
        (self.start_offset, self.extra_space)
    }
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
    break_paragraph_lines_with_spacing(
        text, runs, first_line_indent_px, max_width, base_font_size, line_spacing_mult, align,
        fonts, cache, lang, 1.0, 0.0,
    )
}

/// Like `break_paragraph_lines`, with explicit word-spacing multiplier and
/// letter-spacing tracking (px). Both are applied to the final advances at
/// tokenize time, so the breaker, rasterizer and word-rect extraction all
/// see the same numbers.
pub fn break_paragraph_lines_with_spacing(
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
    word_spacing_mult: f32,
    letter_spacing_px: f32,
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
        word_spacing_mult,
        letter_spacing_px,
        &mut cur_byte,
        &mut cur_char,
    )
}

/// Push a word token, gluing pure punctuation to the previous word so a
/// line never starts with a comma/period/quote (orphan-punctuation guard).
fn push_word_token(
    tokens: &mut Vec<LineItem>,
    text: &str,
    run: &Run,
    run_size: f32,
    base_font_size: f32,
    word_byte_start: usize,
    word_byte_end: usize,
    word_char_start: usize,
    word_char_end: usize,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    letter_spacing_px: f32,
) {
    if word_byte_start >= word_byte_end {
        return;
    }
    let word_str = match text.get(word_byte_start..word_byte_end) {
        Some(s) => s,
        None => return,
    };
    let is_pure_punct = word_str.chars().all(|c| {
        matches!(
            c,
            ',' | '.'
                | '!'
                | '?'
                | ';'
                | ':'
                | ')'
                | ']'
                | '}'
                | '”'
                | '’'
                | '»'
                | '…'
                | '%'
                | '°'
        )
    });
    if is_pure_punct {
        // A soft hyphen between a word and its punctuation is absorbed so
        // the punctuation glues to the word — breaking between a word and
        // its comma is never desirable.
        if matches!(tokens.last(), Some(LineItem::SoftHyphen { .. })) {
            tokens.pop();
        }
        if let Some(LineItem::Word {
            byte_start,
            byte_end,
            char_end,
            shaped,
            style,
            ..
        }) = tokens.last_mut()
        {
            *byte_end = word_byte_end;
            *char_end = word_char_end;
            if let Some(full_str) = text.get(*byte_start..word_byte_end) {
                let s_size = base_font_size * style.size_mult;
                *shaped = shape_word_with_spacing(
                    cache,
                    full_str,
                    style.font_style,
                    s_size,
                    fonts,
                    letter_spacing_px,
                );
            }
            return;
        }
    }
    let shaped = shape_word_with_spacing(
        cache,
        word_str,
        run.style.font_style,
        run_size,
        fonts,
        letter_spacing_px,
    );
    tokens.push(LineItem::Word {
        byte_start: word_byte_start,
        byte_end: word_byte_end,
        char_start: word_char_start,
        char_end: word_char_end,
        shaped,
        style: run.style.clone(),
    });
}

/// Push a space token, collapsing consecutive spaces into one. A NBSP in a
/// run upgrades a preceding breakable space so the whole run is
/// non-breaking ("Mr.  Smith" intended as non-breaking keeps that intent).
fn push_space_token(
    tokens: &mut Vec<LineItem>,
    run: &Run,
    run_size: f32,
    nobreak: bool,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    word_spacing_mult: f32,
    letter_spacing_px: f32,
) {
    match tokens.last_mut() {
        Some(LineItem::Space { nobreak: n, .. }) => {
            if nobreak {
                *n = true;
            }
        }
        _ => {
            let adv = cache.space_advance(run.style.font_style, run_size, fonts)
                * word_spacing_mult
                + letter_spacing_px;
            tokens.push(LineItem::Space {
                adv,
                style: run.style.clone(),
                nobreak,
                stretch: (adv - letter_spacing_px) * 0.5,
                shrink: (adv - letter_spacing_px) * (1.0 / 3.0),
            });
        }
    }
}

/// Shape a word and apply letter-spacing tracking to a per-use copy (the
/// shape cache keeps the untracked original).
fn shape_word_with_spacing(
    cache: &mut ShapeCache,
    word: &str,
    style: FontStyle,
    size: f32,
    fonts: &FontSystem,
    letter_spacing_px: f32,
) -> Arc<ShapedWord> {
    let shaped = cache.shape_word(word, style, size, fonts);
    if letter_spacing_px == 0.0 {
        shaped
    } else {
        shaped.tracked(letter_spacing_px)
    }
}

/// Tokenize one run into words / spaces / soft hyphens / hard breaks.
/// A word is any run of non-space, non-newline characters. U+00A0 becomes
/// a non-breaking Space token; U+00AD splits a word into fragments with a
/// SoftHyphen marker between them.
fn tokenize_run(
    tokens: &mut Vec<LineItem>,
    text: &str,
    run: &Run,
    run_text: &str,
    run_start_byte: usize,
    run_start_char: usize,
    base_font_size: f32,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    word_spacing_mult: f32,
    letter_spacing_px: f32,
) {
    let run_size = base_font_size * run.style.size_mult;

    // Absolute offsets of the word currently being accumulated, if any.
    let mut word_byte_start: Option<usize> = None;
    let mut word_char_start = run_start_char;
    let mut rel_char = 0usize;

    for (rel_byte, ch) in run_text.char_indices() {
        let abs_byte = run_start_byte + rel_byte;
        let abs_char = run_start_char + rel_char;
        match ch {
            '\n' => {
                if let Some(wbs) = word_byte_start.take() {
                    push_word_token(
                        tokens, text, run, run_size, base_font_size, wbs, abs_byte,
                        word_char_start, abs_char, fonts, cache, letter_spacing_px,
                    );
                }
                tokens.push(LineItem::HardBreak);
            }
            ' ' => {
                if let Some(wbs) = word_byte_start.take() {
                    push_word_token(
                        tokens, text, run, run_size, base_font_size, wbs, abs_byte,
                        word_char_start, abs_char, fonts, cache, letter_spacing_px,
                    );
                }
                push_space_token(
                    tokens,
                    run,
                    run_size,
                    false,
                    fonts,
                    cache,
                    word_spacing_mult,
                    letter_spacing_px,
                );
            }
            '\u{00A0}' => {
                if let Some(wbs) = word_byte_start.take() {
                    push_word_token(
                        tokens, text, run, run_size, base_font_size, wbs, abs_byte,
                        word_char_start, abs_char, fonts, cache, letter_spacing_px,
                    );
                }
                push_space_token(
                    tokens,
                    run,
                    run_size,
                    true,
                    fonts,
                    cache,
                    word_spacing_mult,
                    letter_spacing_px,
                );
            }
            '\u{00AD}' => {
                if let Some(wbs) = word_byte_start.take() {
                    push_word_token(
                        tokens, text, run, run_size, base_font_size, wbs, abs_byte,
                        word_char_start, abs_char, fonts, cache, letter_spacing_px,
                    );
                }
                let hyp_adv = cache.hyphen_advance(run.style.font_style, run_size, fonts);
                tokens.push(LineItem::SoftHyphen {
                    style: run.style.clone(),
                    hyphen_adv: hyp_adv,
                });
            }
            _ => {
                if word_byte_start.is_none() {
                    word_byte_start = Some(abs_byte);
                    word_char_start = abs_char;
                }
            }
        }
        rel_char += 1;
    }

    // Trailing word extends to the end of the run.
    if let Some(wbs) = word_byte_start {
        push_word_token(
            tokens, text, run, run_size, base_font_size, wbs, run_start_byte + run_text.len(),
            word_char_start, run_start_char + rel_char, fonts, cache, letter_spacing_px,
        );
    }
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
    word_spacing_mult: f32,
    letter_spacing_px: f32,
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

        let run_start_char = *cur_char_offset;
        tokenize_run(
            &mut tokens,
            text,
            run,
            run_text,
            run.start,
            run_start_char,
            base_font_size,
            fonts,
            cache,
            word_spacing_mult,
            letter_spacing_px,
        );
        *cur_char_offset = run_start_char + run_text.chars().count();
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
                if is_first_segment_line {
                    first_line_indent_px
                } else {
                    0.0
                },
                max_width,
                base_font_size,
                line_spacing_mult,
                align,
                fonts,
                cache,
                lang,
                letter_spacing_px,
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
            if is_first_segment_line {
                first_line_indent_px
            } else {
                0.0
            },
            max_width,
            base_font_size,
            line_spacing_mult,
            align,
            fonts,
            cache,
            lang,
            letter_spacing_px,
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
    letter_spacing_px: f32,
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
        letter_spacing_px,
        is_last_paragraph_segment,
    )
}

// ---------------------------------------------------------------------------
// Greedy Breaker
// ---------------------------------------------------------------------------

/// A hyphen-like character: hyphenation must never break next to one, or
/// the line-end dash doubles an existing dash ("so--called") or strands it
/// at an edge.
fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2013}' | '\u{2014}')
}

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
    letter_spacing_px: f32,
    is_last_paragraph_segment: bool,
) -> Vec<LayoutLine> {
    let mut lines = Vec::new();
    let mut current_line_items: Vec<LineItem> = Vec::new();
    let mut current_line_width = 0.0f32;
    let mut is_first_line = true;
    // Consecutive line ends that broke on a hyphen (the "ladder").
    // Hygiene: capped at 2 — a third consecutive hyphenated end must move
    // the whole word instead, or the right margin grows a staircase.
    // Note: this counts AUTHORIAL syllable breaks only. Soft-hyphen (U+00AD)
    // materializations are the author's explicit intent and are deliberately
    // neither suppressed nor counted here — but they ARE visual hyphens, so
    // the gallery's ladder column (which counts both kinds) can read higher
    // than 2 on shy-rich books. That is expected, not a hygiene violation.
    let mut hyphen_ladder = 0u32;

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
            // Check hyphenation (hygiene-gated: ladder <= 2).
            let mut hyphenated = false;
            if hyphen_ladder < 2 {
                if let (
                    Some(target_lang),
                    LineItem::Word {
                        byte_start,
                        byte_end,
                        char_start,
                        style,
                        shaped: word_shaped,
                        ..
                    },
                ) = (lang, &item)
                {
                    if let Some(word_text) = text.get(*byte_start..*byte_end) {
                        let word_char_count = word_text.chars().count();
                        if word_char_count >= 5 {
                            let syllables: Vec<&str> =
                                hyphenate(word_text, target_lang).collect();
                            if syllables.len() >= 2 {
                                let run_size = base_font_size * style.size_mult;
                                let hyp_adv =
                                    cache.hyphen_advance(style.font_style, run_size, fonts);
                                let mut prefix = String::new();
                                let mut best_break: Option<(usize, usize, f32)> = None;
                                let boundaries = BoundaryAdvances::new(&word_shaped.glyphs);

                                for &syl in &syllables[..syllables.len() - 1] {
                                    prefix.push_str(syl);
                                    // Hygiene: never strand a 1-2 char
                                    // prefix or suffix, and never break
                                    // right next to an existing dash — the
                                    // "so--called" double-hyphen artifact.
                                    let pref_chars = prefix.chars().count();
                                    let ends_with_dash = prefix
                                        .chars()
                                        .next_back()
                                        .map_or(false, is_dash);
                                    let suffix_starts_with_dash = word_text
                                        [prefix.len()..]
                                        .chars()
                                        .next()
                                        .map_or(false, is_dash);
                                    if pref_chars < 3
                                        || word_char_count - pref_chars < 2
                                        || ends_with_dash
                                        || suffix_starts_with_dash
                                    {
                                        continue;
                                    }
                                    if let Some(p_adv) = boundaries.advance(prefix.len()) {
                                        if current_line_width + p_adv + hyp_adv <= allowed_width {
                                            best_break = Some((prefix.len(), pref_chars, p_adv));
                                        }
                                    }
                                }

                                if let Some((pref_bytes, pref_chars, _p_adv)) = best_break {
                                    let prefix_str = &word_text[..pref_bytes];
                                    let prefix_shaped = shape_word_with_spacing(
                                        cache,
                                        prefix_str,
                                        style.font_style,
                                        run_size,
                                        fonts,
                                        letter_spacing_px,
                                    );
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
                                    let suffix_shaped = shape_word_with_spacing(
                                        cache,
                                        suffix,
                                        style.font_style,
                                        run_size,
                                        fonts,
                                        letter_spacing_px,
                                    );
                                    let suffix_item = LineItem::Word {
                                        byte_start: *byte_start + pref_bytes,
                                        byte_end: *byte_end,
                                        char_start: *char_start + pref_chars,
                                        char_end: *char_start + word_char_count,
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
                                    hyphen_ladder += 1;
                                }
                            }
                        }
                    }
                }
            }

            if !hyphenated {
                // Trim trailing breakable spaces — a line never ends with
                // the gap that the break itself provides.
                while let Some(LineItem::Space { nobreak: false, .. }) =
                    current_line_items.last()
                {
                    current_line_items.pop();
                }

                // Non-breaking spaces never split from their word. If the
                // item that did not fit IS a NBSP, its preceding word must
                // come along too (a lone NBSP at line start is a lost
                // space). Then pull any trailing "word NBSP" groups so
                // "Mr. Smith" / "10 km" never split.
                let mut carry: Vec<LineItem> = Vec::new();
                if matches!(item, LineItem::Space { nobreak: true, .. }) {
                    if let Some(w) = current_line_items.pop() {
                        carry.push(w);
                    }
                }
                while let Some(LineItem::Space { nobreak: true, .. }) =
                    current_line_items.last()
                {
                    let sp = current_line_items.pop().unwrap();
                    if let Some(w) = current_line_items.pop() {
                        carry.push(w);
                    }
                    carry.push(sp);
                }
                carry.reverse();

                // A line ending at a soft hyphen materializes the hyphen
                // ("cus-"); the break is the reason it becomes visible.
                let materialize_hyphen = matches!(
                    item,
                    LineItem::Word { .. } | LineItem::Space { nobreak: true, .. }
                );
                let soft_hyphen_break = match current_line_items.last() {
                    Some(LineItem::SoftHyphen { style, hyphen_adv })
                        if materialize_hyphen =>
                    {
                        Some((style.clone(), *hyphen_adv))
                    }
                    _ => None,
                };
                if let Some((style, hyphen_adv)) = soft_hyphen_break {
                    // The fit check ran before the break, so materializing
                    // the hyphen here could push ink past the measure when
                    // the line is within one hyphen-width of full — the
                    // exact "lines past the right margin" drift that killed
                    // Knuth-Plass. Recompute the true line width (trailing
                    // spaces may have been trimmed, so the running width is
                    // stale) and only break with a hyphen if it still fits.
                    let line_width: f32 =
                        current_line_items.iter().map(|it| it.advance()).sum();
                    if line_width + hyphen_adv <= allowed_width {
                        current_line_items.pop();
                        current_line_items.push(LineItem::Hyphen {
                            adv: hyphen_adv,
                            style,
                        });
                    } else if current_line_items.len() > 2 {
                        // No room for the hyphen: move the fragment + soft
                        // hyphen onto the next line instead, so the word
                        // can fit whole there or break at the soft hyphen
                        // with a fresh-line fit check.
                        current_line_items.pop(); // SoftHyphen
                        if let Some(w) = current_line_items.pop() {
                            carry.push(w);
                            carry.push(LineItem::SoftHyphen { style, hyphen_adv });
                        }
                    }
                    // else: the fragment alone fills this line (an
                    // absurdly narrow measure). Leave it dash-less and let
                    // the next word start the next line.
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

                // Start the next line with the carried NBSP group, then
                // the item that did not fit.
                let mut carry_width = 0.0f32;
                for c in carry {
                    carry_width += c.advance();
                    current_line_items.push(c);
                }
                match item {
                    LineItem::Space { nobreak: true, .. } => {
                        // A non-breaking space that did not fit is still a
                        // required space: keep it attached to the group.
                        carry_width += item.advance();
                        current_line_items.push(item);
                    }
                    LineItem::Space { .. } => {
                        // Breakable space that did not fit: the line break
                        // is the gap.
                    }
                    _ => {
                        carry_width += item.advance();
                        current_line_items.push(item);
                    }
                }
                // Reset the pen to the new line's width — the built line's
                // width must not leak into the next line's fitting checks.
                current_line_width = carry_width;
                // A non-hyphenated line end breaks the ladder.
                hyphen_ladder = 0;
            }
        }
    }

    if !current_line_items.is_empty() {
        while current_line_items
            .last()
            .map(|it| it.is_space())
            .unwrap_or(false)
        {
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
    let (start_offset, extra_space) = solve_alignment(align, width, max_width, is_last, &items);
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
            LineItem::Word { style, .. }
            | LineItem::HyphenatedPrefix { style, .. }
            | LineItem::Space { style, .. }
            | LineItem::SoftHyphen { style, .. }
            | LineItem::Hyphen { style, .. } => style,
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
        start_offset,
        extra_space,
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

/// Solve a line's alignment ONCE, at build time. Returns (leading pen
/// offset, per-gap extra advance). Both the rasterizer and the app's
/// word-rect extraction consume the stored values — the historical bug
/// this exists to prevent: two independent width models drifting apart
/// at the edges, so dictionary taps hit the wrong word on justified
/// lines (see `alignment_adjust` in raster.rs).
///
/// Justification policy: fill the measure with uniform per-gap stretch
/// when the slack is under 40% of the measure, matching the long-standing
/// device rule. Greedy lines can otherwise need per-gap stretches of
/// several spaces (long words, narrow measures), which strict TeX
/// tolerances (~space/2) would refuse — leaving most of a paragraph
/// visibly ragged. Tighter tolerance-based gating belongs to optimal
/// breaking, which can choose breakpoints that keep slack within the
/// per-space stretch/shrink stored on each gap.
fn solve_alignment(
    align: TextAlign,
    width: f32,
    max_width: f32,
    is_last: bool,
    items: &[LineItem],
) -> (f32, f32) {
    match align {
        TextAlign::Center => {
            let slack = (max_width - width).max(0.0);
            (slack / 2.0, 0.0)
        }
        TextAlign::Right => {
            let slack = (max_width - width).max(0.0);
            (slack, 0.0)
        }
        TextAlign::Justify => {
            if is_last {
                return (0.0, 0.0);
            }
            let slack = max_width - width;
            let space_count = items.iter().filter(|it| it.is_space()).count();
            if space_count == 0 {
                return (0.0, 0.0);
            }
            if slack > 0.0 {
                // Underfull: fill the measure with uniform per-gap stretch
                // when the slack is within the generous 40%-of-measure
                // bound (see the policy note above).
                if slack < max_width * 0.40 {
                    return (0.0, slack / space_count as f32);
                }
            } else if slack < 0.0 {
                // Overfull by a hair — hyphenation width drift (the
                // boundary-lookup advance vs the reshaped prefix differs
                // by sub-pixels) or rounding. Shrink the gaps back onto
                // the measure, bounded by the per-space shrink tolerance
                // so word spacing never collapses.
                let min_shrink = items
                    .iter()
                    .filter_map(|it| match it {
                        LineItem::Space { shrink, .. } => Some(*shrink),
                        _ => None,
                    })
                    .fold(f32::MAX, f32::min);
                let per_gap = -slack / space_count as f32;
                if per_gap <= min_shrink {
                    return (0.0, -per_gap);
                }
            }
            (0.0, 0.0)
        }
        TextAlign::Left => (0.0, 0.0),
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
