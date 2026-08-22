//! Knuth-Plass dynamic programming line breaking, word tokenization, hyphenation, and line layout.

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
            *cur_char_offset += text[*cur_byte_offset..run.start].chars().count();
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

/// Break a single segment (free of hard breaks) using Knuth-Plass dynamic programming.
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

    // Knuth-Plass optimal line breaking is strictly for fully-justified text.
    // For centered, left-aligned, or right-aligned text (like headings and lists),
    // use greedy ragged line breaking to preserve natural word spacing.
    if align == TextAlign::Justify {
        if let Some(lines) = knuth_plass(
            text,
            &tokens,
            first_line_indent_px,
            max_width,
            base_font_size,
            line_spacing_mult,
            align,
            fonts,
            cache,
            lang,
            is_last_paragraph_segment,
        ) {
            if !lines.is_empty() {
                return lines;
            }
        }
    }

    // Fallback: Greedy breaker if Knuth-Plass could not find a solution (e.g. single giant token)
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
// Knuth-Plass Optimal Line Breaker
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitnessClass {
    Tight,     // r in [-1.0, -0.5)
    Normal,    // r in [-0.5, 0.5]
    Loose,     // r in (0.5, 1.0]
    VeryLoose, // r > 1.0
}

impl FitnessClass {
    pub fn from_ratio(r: f32) -> Self {
        if r < -0.5 {
            FitnessClass::Tight
        } else if r <= 0.5 {
            FitnessClass::Normal
        } else if r <= 1.0 {
            FitnessClass::Loose
        } else {
            FitnessClass::VeryLoose
        }
    }

    pub fn index(self) -> usize {
        match self {
            FitnessClass::Tight => 0,
            FitnessClass::Normal => 1,
            FitnessClass::Loose => 2,
            FitnessClass::VeryLoose => 3,
        }
    }

    pub fn diff(self, other: FitnessClass) -> usize {
        (self.index() as isize - other.index() as isize).unsigned_abs()
    }
}

/// A candidate break point in the paragraph stream.
#[derive(Clone, Debug)]
enum BreakPoint {
    /// Space between word `word_idx` and `word_idx + 1`
    SpaceAfterWord {
        word_idx: usize,
    },
    /// Hyphenation point inside word `word_idx` after `prefix_bytes`
    HyphenInsideWord {
        word_idx: usize,
        prefix_bytes: usize,
        prefix_chars: usize,
        prefix_adv: f32,
        hyphen_adv: f32,
    },
    /// End of paragraph
    EndOfParagraph,
}

#[derive(Clone, Copy, Debug)]
struct ActiveNode {
    break_idx: usize,
    line_number: usize,
    fitness: FitnessClass,
    demerits: u64,
    is_hyphen: bool,
    parent_idx: usize,
}

fn knuth_plass(
    text: &str,
    tokens: &[LineItem],
    first_line_indent_px: f32,
    max_width: f32,
    base_font_size: f32,
    line_spacing_mult: f32,
    align: TextAlign,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
    lang: Option<Lang>,
    is_last_paragraph_segment: bool,
) -> Option<Vec<LayoutLine>> {
    // Extract words and break points
    let mut words: Vec<LineItem> = Vec::new();
    let mut break_points: Vec<BreakPoint> = Vec::new();

    for it in tokens {
        match it {
            LineItem::Word { .. } => {
                let word_idx = words.len();
                words.push(it.clone());

                // Check for potential hyphenation break points inside this word
                if let (Some(target_lang), LineItem::Word { byte_start, byte_end, shaped: word_shaped, style, .. }) = (lang, it) {
                    if let Some(word_text) = text.get(*byte_start..*byte_end) {
                        if word_text.chars().count() >= 5 {
                            let syllables: Vec<&str> = hyphenate(word_text, target_lang).collect();
                            if syllables.len() >= 2 {
                                let run_size = base_font_size * style.size_mult;
                                let hyp_adv = cache.hyphen_advance(style.font_style, run_size, fonts);
                                let mut prefix = String::new();
                                for &syl in &syllables[..syllables.len() - 1] {
                                    prefix.push_str(syl);
                                    if let Some(p_adv) = advance_to_boundary(&word_shaped.glyphs, prefix.len()) {
                                        break_points.push(BreakPoint::HyphenInsideWord {
                                            word_idx,
                                            prefix_bytes: prefix.len(),
                                            prefix_chars: prefix.chars().count(),
                                            prefix_adv: p_adv,
                                            hyphen_adv: hyp_adv,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            LineItem::Space { .. } => {
                if !words.is_empty() {
                    break_points.push(BreakPoint::SpaceAfterWord {
                        word_idx: words.len() - 1,
                    });
                }
            }
            _ => {}
        }
    }

    if words.is_empty() {
        return Some(Vec::new());
    }

    // Prefix sums so each DP (break × active) measurement is O(1): the
    // naive re-sum of every candidate slice made paragraph breaking
    // quadratic, and is_orphan_punctuation re-scanned text per pair.
    // cum_word[i] = Σ advance of words[0..i]; cum_space[i] = Σ of the
    // inter-word space advances after words[0..i).
    let n_words = words.len();
    let mut cum_word = Vec::with_capacity(n_words + 1);
    let mut cum_space = Vec::with_capacity(n_words + 1);
    let mut orphan = Vec::with_capacity(n_words);
    let mut w_acc = 0.0f32;
    let mut s_acc = 0.0f32;
    cum_word.push(0.0);
    cum_space.push(0.0);
    for (i, w) in words.iter().enumerate() {
        orphan.push(is_orphan_punctuation(text, w));
        w_acc += w.advance();
        cum_word.push(w_acc);
        if i + 1 < n_words {
            if let LineItem::Word { style, .. } = w {
                let run_size = base_font_size * style.size_mult;
                s_acc += cache.space_advance(style.font_style, run_size, fonts);
            }
        }
        cum_space.push(s_acc);
    }
    let sums = SliceSums { cum_word, cum_space, orphan };

    // Append terminal EndOfParagraph break
    break_points.push(BreakPoint::EndOfParagraph);

    // Dynamic programming active nodes list
    let mut all_nodes: Vec<ActiveNode> = Vec::with_capacity(break_points.len() * 4);
    let mut active_indices: Vec<usize> = Vec::with_capacity(32);

    // Initial node at paragraph start (break index 0 representing before first word)
    let start_node = ActiveNode {
        break_idx: 0,
        line_number: 0,
        fitness: FitnessClass::Normal,
        demerits: 0,
        is_hyphen: false,
        parent_idx: usize::MAX,
    };
    all_nodes.push(start_node);
    active_indices.push(0);

    for (b_idx, bp) in break_points.iter().enumerate().map(|(i, b)| (i + 1, b)) {
        let mut best_for_class: [Option<(u64, ActiveNode)>; 4] = [None, None, None, None];
        let is_terminal = matches!(bp, BreakPoint::EndOfParagraph);

        let mut next_active: Vec<usize> = Vec::with_capacity(active_indices.len());

        for &act_idx in &active_indices {
            let act = all_nodes[act_idx];
            let target_w = if act.line_number == 0 {
                max_width - first_line_indent_px
            } else {
                max_width
            };

            // Calculate content width from `act.break_idx` to `b_idx`
            let (content_w, space_total, is_hyphen_break, valid) =
                measure_slice(&sums, n_words, &break_points, act.break_idx, b_idx);

            if !valid {
                continue;
            }

            let slack = target_w - content_w;
            // Capacity from the REAL space advance the line contains —
            // the old count × hardcoded 0.25 em judged a different line
            // than the one measured (and rendered).
            let stretch_capacity = (space_total * 0.50).max(1.0);
            let shrink_capacity = (space_total * 0.33).max(1.0);

            let ratio = if slack >= 0.0 {
                if is_terminal {
                    0.0 // Last line has no looseness penalty
                } else {
                    slack / stretch_capacity
                }
            } else {
                slack / shrink_capacity
            };

            // Can line stay active for future breaks?
            if ratio >= -1.0 {
                next_active.push(act_idx);
            }

            // Is this a feasible line break?
            if ratio < -1.0 || (ratio > 1.6 && !is_terminal) {
                continue;
            }

            let fitness = FitnessClass::from_ratio(ratio);
            let penalty: f32 = if is_hyphen_break { 50.0 } else { 0.0 };
            let r_term = 1.0 + 100.0 * ratio.abs().powi(3) + penalty;
            let line_demerits = (r_term * r_term) as u64;

            // Incompatible fitness penalty
            let fit_penalty: u64 = if fitness.diff(act.fitness) > 1 { 100 } else { 0 };

            // Consecutive hyphen penalty
            let flag_penalty: u64 = if is_hyphen_break && act.is_hyphen { 3000 } else { 0 };

            let total_dem = act.demerits.saturating_add(line_demerits).saturating_add(fit_penalty).saturating_add(flag_penalty);

            let candidate_node = ActiveNode {
                break_idx: b_idx,
                line_number: act.line_number + 1,
                fitness,
                demerits: total_dem,
                is_hyphen: is_hyphen_break,
                parent_idx: act_idx,
            };

            let c_idx = fitness.index();
            match best_for_class[c_idx] {
                Some((best_dem, _)) if total_dem < best_dem => {
                    best_for_class[c_idx] = Some((total_dem, candidate_node));
                }
                None => {
                    best_for_class[c_idx] = Some((total_dem, candidate_node));
                }
                _ => {}
            }
        }

        // Add newly formed best nodes to active list
        for slot in &best_for_class {
            if let Some((_, node)) = slot {
                let new_idx = all_nodes.len();
                all_nodes.push(*node);
                next_active.push(new_idx);
            }
        }

        // Cap active list to 64 to prevent any pathological compute spike
        if next_active.len() > 64 {
            next_active.sort_by_key(|&idx| all_nodes[idx].demerits);
            next_active.truncate(32);
        }

        active_indices = next_active;
    }

    // Find the terminal node with lowest demerits
    let terminal_nodes: Vec<&ActiveNode> = all_nodes
        .iter()
        .filter(|n| n.break_idx == break_points.len())
        .collect();

    let best_terminal = terminal_nodes.into_iter().min_by_key(|n| n.demerits)?;

    // Backtrack to assemble line ranges
    let mut plan: Vec<ActiveNode> = Vec::new();
    let mut curr = *best_terminal;
    while curr.parent_idx != usize::MAX {
        plan.push(curr);
        curr = all_nodes[curr.parent_idx];
    }
    plan.reverse();

    // Reconstruct LayoutLines from plan
    let mut lines: Vec<LayoutLine> = Vec::with_capacity(plan.len());
    let mut prev_break_idx = 0;

    for (l_idx, node) in plan.iter().enumerate() {
        let is_last_line = l_idx + 1 == plan.len() && is_last_paragraph_segment;
        let target_w = if l_idx == 0 {
            max_width - first_line_indent_px
        } else {
            max_width
        };

        let items = extract_line_items(
            text,
            &words,
            &break_points,
            prev_break_idx,
            node.break_idx,
            base_font_size,
            fonts,
            cache,
        );

        let line = build_line(
            items,
            target_w,
            align,
            is_last_line,
            base_font_size,
            line_spacing_mult,
            fonts,
        );
        lines.push(line);
        prev_break_idx = node.break_idx;
    }

    Some(lines)
}

fn is_orphan_punctuation(text: &str, item: &LineItem) -> bool {
    match item {
        LineItem::Word { byte_start, byte_end, .. } => {
            if let Some(s) = text.get(*byte_start..*byte_end) {
                let trimmed = s.trim();
                !trimmed.is_empty()
                    && trimmed.chars().all(|c| {
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
                    })
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Per-paragraph prefix sums for O(1) DP slice measurement.
struct SliceSums {
    cum_word: Vec<f32>,
    cum_space: Vec<f32>,
    /// Word i may not START a line (punctuation-only token).
    orphan: Vec<bool>,
}

/// O(1) measurement between two break candidates via prefix sums:
/// (word width incl. hyphen suffix, total REAL inter-word space advance,
/// is_hyphen, valid).
fn measure_slice(
    sums: &SliceSums,
    words_len: usize,
    break_points: &[BreakPoint],
    from_break: usize,
    to_break: usize,
) -> (f32, f32, bool, bool) {
    let (start_word, start_prefix_adv) = if from_break == 0 {
        (0, 0.0f32)
    } else {
        match &break_points[from_break - 1] {
            BreakPoint::SpaceAfterWord { word_idx } => (word_idx + 1, 0.0),
            BreakPoint::HyphenInsideWord { word_idx, prefix_adv, .. } => (*word_idx, *prefix_adv),
            BreakPoint::EndOfParagraph => return (0.0, 0.0, false, false),
        }
    };

    // A line can never start with orphan punctuation (comma, period, etc.)
    if start_word < words_len && start_prefix_adv == 0.0 && sums.orphan[start_word] {
        return (0.0, 0.0, false, false);
    }

    let (end_word, end_suffix_adv, is_hyphen) = if to_break == break_points.len() {
        (words_len, 0.0f32, false)
    } else {
        match &break_points[to_break - 1] {
            BreakPoint::SpaceAfterWord { word_idx } => (word_idx + 1, 0.0, false),
            BreakPoint::HyphenInsideWord { word_idx, prefix_adv, hyphen_adv, .. } => {
                (*word_idx, *prefix_adv + *hyphen_adv, true)
            }
            BreakPoint::EndOfParagraph => (words_len, 0.0, false),
        }
    };

    if start_word > end_word || (start_word == end_word && is_hyphen) {
        return (0.0, 0.0, false, false);
    }

    let mut total_w = sums.cum_word[end_word] - sums.cum_word[start_word];
    if start_prefix_adv > 0.0 {
        total_w = (total_w - start_prefix_adv).max(0.0);
    }
    if is_hyphen {
        total_w += end_suffix_adv;
    }
    let space_total = if end_word > start_word {
        sums.cum_space[end_word - 1] - sums.cum_space[start_word]
    } else {
        0.0
    };
    (total_w, space_total, is_hyphen, true)
}

/// Extract materialized `LineItem`s for a finalized line slice.
fn extract_line_items(
    text: &str,
    words: &[LineItem],
    break_points: &[BreakPoint],
    from_break: usize,
    to_break: usize,
    base_font_size: f32,
    fonts: &FontSystem,
    cache: &mut ShapeCache,
) -> Vec<LineItem> {
    let (start_word, start_split) = if from_break == 0 {
        (0, None)
    } else {
        match &break_points[from_break - 1] {
            BreakPoint::SpaceAfterWord { word_idx } => (word_idx + 1, None),
            BreakPoint::HyphenInsideWord { word_idx, prefix_bytes, prefix_chars, .. } => {
                (*word_idx, Some((*prefix_bytes, *prefix_chars)))
            }
            BreakPoint::EndOfParagraph => (words.len(), None),
        }
    };

    let (end_word, end_split) = if to_break == break_points.len() {
        (words.len(), None)
    } else {
        match &break_points[to_break - 1] {
            BreakPoint::SpaceAfterWord { word_idx } => (word_idx + 1, None),
            BreakPoint::HyphenInsideWord { word_idx, prefix_bytes, prefix_chars, hyphen_adv, .. } => {
                (*word_idx, Some((*prefix_bytes, *prefix_chars, *hyphen_adv)))
            }
            BreakPoint::EndOfParagraph => (words.len(), None),
        }
    };

    let mut items = Vec::new();

    for w_idx in start_word..=end_word {
        if w_idx >= words.len() {
            break;
        }
        let base_word = &words[w_idx];

        if w_idx == start_word && start_split.is_some() {
            let (p_bytes, p_chars) = start_split.unwrap();
            if let LineItem::Word { byte_start, byte_end, char_start, style, .. } = base_word {
                if let Some(w_text) = text.get(*byte_start..*byte_end) {
                    let suffix_str = &w_text[p_bytes..];
                    let run_size = base_font_size * style.size_mult;
                    let suffix_shaped = cache.shape_word(suffix_str, style.font_style, run_size, fonts);
                    items.push(LineItem::Word {
                        byte_start: *byte_start + p_bytes,
                        byte_end: *byte_end,
                        char_start: *char_start + p_chars,
                        char_end: *char_start + w_text.chars().count(),
                        shaped: suffix_shaped,
                        style: style.clone(),
                    });
                }
            }
        } else if w_idx == end_word && end_split.is_some() {
            let (p_bytes, p_chars, hyp_adv) = end_split.unwrap();
            if let LineItem::Word { byte_start, char_start, style, .. } = base_word {
                if let Some(w_text) = text.get(*byte_start..*byte_start + p_bytes) {
                    let run_size = base_font_size * style.size_mult;
                    let prefix_shaped = cache.shape_word(w_text, style.font_style, run_size, fonts);
                    items.push(LineItem::HyphenatedPrefix {
                        byte_start: *byte_start,
                        byte_end: *byte_start + p_bytes,
                        char_start: *char_start,
                        char_end: *char_start + p_chars,
                        prefix_shaped,
                        hyphen_adv: hyp_adv,
                        style: style.clone(),
                    });
                }
            }
            break; // Stop at end hyphen
        } else if w_idx < end_word {
            items.push(base_word.clone());
        }

        if w_idx + 1 < end_word {
            if let LineItem::Word { style, .. } = base_word {
                let run_size = base_font_size * style.size_mult;
                let sp_adv = cache.space_advance(style.font_style, run_size, fonts);
                items.push(LineItem::Space {
                    adv: sp_adv,
                    style: style.clone(),
                });
            }
        }
    }

    items
}

// ---------------------------------------------------------------------------
// Greedy Breaker Fallback
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

                            for &syl in &syllables[..syllables.len() - 1] {
                                prefix.push_str(syl);
                                if let Some(p_adv) = advance_to_boundary(&word_shaped.glyphs, prefix.len()) {
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

/// Advance width of the byte-prefix [0, boundary) of an already-shaped
/// word, read off its glyph clusters. None when the boundary splits a
/// cluster (ligature / attached mark) — that hyphenation point is
/// invalid. Clusters are monotonic for the scripts we hyphenate.
fn advance_to_boundary(glyphs: &[crate::shape::ShapedGlyph], boundary: usize) -> Option<f32> {
    let mut sum = 0.0f32;
    for g in glyphs {
        if (g.cluster as usize) < boundary {
            sum += g.x_advance;
        } else {
            return if g.cluster as usize == boundary { Some(sum) } else { None };
        }
    }
    None
}
