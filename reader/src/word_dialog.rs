use crate::dictionary::WordResult;
use ybdev::input::Gesture;
use yui::painter::{pt, Painter, PX, Rect};
use yui::screen::{Action, Screen};

/// Word-card sheet geometry: the translation row keeps its space first
/// (capped), then the WordNet definition fills the rest.
const MAX_CARD_PT: f32 = 210.0;
const MIN_CARD_PT: f32 = 85.0;
const TITLE_H_PT: f32 = 45.0;
const SEC_H_PT: f32 = 16.0;
const LINE_H_PT: f32 = 13.0;
const MAX_TR_LINES: usize = 3;

/// How many definition/translation lines fit, and the resulting card
/// height, given the full wrapped line counts. With the translation row
/// off, the definition gets the whole budget.
fn budget_lines(def_lines: usize, tr_lines: usize) -> (usize, usize, i32) {
    let card_max_h = pt(MAX_CARD_PT);
    let title_h = pt(TITLE_H_PT);
    let sec_h = pt(SEC_H_PT);
    let line_h = pt(LINE_H_PT);
    let tr_shown = if tr_lines == 0 {
        0
    } else {
        tr_lines.min(MAX_TR_LINES)
    };
    let tr_h = if tr_shown == 0 {
        0
    } else {
        sec_h + tr_shown as i32 * line_h
    };
    let def_budget = (card_max_h - title_h - tr_h - sec_h).max(line_h);
    let def_shown = def_lines.min(((def_budget / line_h) as usize).max(1));
    let def_h = if def_shown == 0 {
        0
    } else {
        sec_h + def_shown as i32 * line_h
    };
    let card_h = (title_h + def_h + tr_h).clamp(pt(MIN_CARD_PT), card_max_h);
    (def_shown, tr_shown, card_h)
}

/// The dictionary card: the word, the WordNet definition row (always
/// attempted) and the translation row (the active dictionary). Tapping
/// the translation source cycles the book's translation dictionary and
/// remembers the pick.
pub struct WordDialog {
    result: WordResult,
    is_learning: bool,
    on_action: Option<Box<dyn FnOnce(WordAction) -> Action>>,
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    chk_rect: Rect,
    dict_rect: Rect,
    trans_header_rect: Rect,
    dicts: std::rc::Rc<crate::dictionary::ActiveDicts>,
    book: String,
    /// Wrapped definition lines, cached so a redraw (e.g. cycling the
    /// translation dictionary) never re-wraps the unchanged definition.
    def_lines: Vec<WrapLine>,
    last_def_meaning: Option<String>,
}

pub enum WordAction {
    StarLearning,
    Close,
}

impl WordDialog {
    pub fn new<F>(
        result: WordResult,
        is_learning: bool,
        bg: Option<Vec<u8>>,
        dicts: std::rc::Rc<crate::dictionary::ActiveDicts>,
        book: String,
        on_action: F,
    ) -> Self
    where
        F: FnOnce(WordAction) -> Action + 'static,
    {
        WordDialog {
            result,
            is_learning,
            on_action: Some(Box::new(on_action)),
            bg,
            dims: (1236, 1648),
            dicts,
            book,
            chk_rect: Rect::new(0, 0, 0, 0),
            dict_rect: Rect::new(0, 0, 0, 0),
            trans_header_rect: Rect::new(0, 0, 0, 0),
            def_lines: Vec::new(),
            last_def_meaning: None,
        }
    }

    /// Cycle the book's translation dictionary override forward and
    /// re-resolve the translation row. WordNet stays as the definition.
    fn cycle_dict(&mut self) -> Action {
        let active = self.dicts.active_bases();
        let current = crate::dictionary::book_override(&self.book);
        let next = crate::dictionary::next_translation_choice(current.as_deref(), &active);
        crate::dictionary::set_book_override(&self.book, next.as_deref());
        self.result.translation = self.dicts.translation(self.result.word(), next.as_deref());
        Action::Redraw
    }

    fn dispatch(&mut self, action: WordAction) -> Action {
        if let Some(cb) = self.on_action.take() {
            cb(action)
        } else {
            Action::Pop
        }
    }
}

/// One wrapped physical line plus the paragraph kind it belongs to.
/// WordNet marks example sentences with a leading quote, and the card
/// dims/indents them — a flag, not a fresh decision per physical line,
/// so a wrapped example's continuation stays ghosted instead of
/// turning into definition ink (the "replication" bug).
#[derive(Debug)]
pub(crate) struct WrapLine {
    pub text: String,
    pub example: bool,
}

/// Greedy wrap that preserves hard breaks (newlines): WordNet meanings
/// arrive as sense blocks ("1. gloss" / example line), and flattening
/// them back into one paragraph would wreck that structure. Shared with
/// the flashcard trainer's card back, which renders at its own size.
pub(crate) fn wrap_lines(p: &Painter, text: &str, max_w: f32, font_pt: f32) -> Vec<WrapLine> {
    let mut lines = Vec::new();
    for hard in text.split('\n') {
        let example = hard.trim_start().starts_with('"');
        let mut cur = String::new();
        for word in hard.split_whitespace() {
            let test = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{} {}", cur, word)
            };
            if p.text_width(font_pt, &test) > max_w {
                if !cur.is_empty() {
                    lines.push(WrapLine { text: cur, example });
                }
                cur = word.to_string();
            } else {
                cur = test;
            }
        }
        if !cur.is_empty() {
            lines.push(WrapLine { text: cur, example });
        }
    }
    lines
}

impl Screen for WordDialog {
    fn default_edges(&self) -> bool {
        true
    }

    fn on_enter(&mut self) -> Action {
        // Fast partial refresh - NEVER flash full screen when opening dictionary sheet!
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
        } else {
            p.clear(255);
        }

        let card_w = w - pt(16.0);
        let card_x = pt(8.0);
        let max_text_w = (card_w - pt(28.0)) as f32;

        // Wrap the definition once and keep it across redraws: cycling the
        // translation dictionary (the only action that repaints this card)
        // must not re-wrap the unchanged definition.
        let def_meaning: Option<&str> = self.result.definition.as_ref().map(|e| e.meaning.as_str());
        if self.last_def_meaning.as_deref() != def_meaning {
            let wrapped = self
                .result
                .definition
                .as_ref()
                .map(|e| wrap_lines(p, &e.meaning, max_text_w, 9.5))
                .unwrap_or_default();
            self.last_def_meaning = def_meaning.map(str::to_string);
            self.def_lines = wrapped;
        }
        let def_lines = &self.def_lines;

        let tr_lines = self
            .result
            .translation
            .as_ref()
            .map(|e| wrap_lines(p, &e.meaning, max_text_w, 9.5))
            .unwrap_or_default();

        let (def_shown, tr_shown, card_h) = budget_lines(def_lines.len(), tr_lines.len());
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        let title_y = card_y + pt(17.0);

        // Right side buttons: [ eng-rus > ] [ Learn ]
        // 1. Learn Toggle Checkbox Button (far right)
        let chk_w = pt(52.0);
        let btn_h = pt(16.0);
        let btn_y = card_y + pt(6.0);
        let chk_x = card_x + card_w - chk_w - pt(10.0);
        let chk_rect = Rect::new(chk_x, btn_y, chk_w, btn_h);
        self.chk_rect = Rect::new(
            chk_x - pt(4.0),
            card_y,
            chk_w + pt(14.0),
            pt(TITLE_H_PT),
        );

        if self.is_learning {
            p.rect(chk_rect, 0);
            p.text_center_in(chk_x, chk_x + chk_w, btn_y + pt(11.5), 7.5, 255, "Learning");
        } else {
            p.rect_outline_t(chk_rect, 1, 130);
            p.text_center_in(chk_x, chk_x + chk_w, btn_y + pt(11.5), 7.5, 60, "Learn");
        }

        // 2. Translation source selector pill button (to the left of Learn)
        let tr_source = self
            .result
            .translation
            .as_ref()
            .map(|e| e.source.as_str())
            .unwrap_or("off");
        let dict_label = format!("{tr_source} >");
        let lw = p.text_width(7.5, &dict_label).round() as i32;
        let pill_w = lw + pt(14.0);
        let pill_gap = pt(8.0);
        let pill_x = chk_x - pill_w - pill_gap;
        let pill_rect = Rect::new(pill_x, btn_y, pill_w, btn_h);
        p.rect_outline_t(pill_rect, 1, 130);
        p.text_center_in(
            pill_rect.x,
            pill_rect.x + pill_rect.w,
            btn_y + pt(11.5),
            7.5,
            60,
            &dict_label,
        );

        // Generous touch hitbox for dictionary toggle button:
        // Spans the full height of the header and has generous horizontal reach.
        self.dict_rect = Rect::new(
            pill_x - pt(6.0),
            card_y,
            pill_w + pt(10.0),
            pt(TITLE_H_PT),
        );

        // Left side: Headword Title (truncated before buttons)
        let title_x = card_x + pt(12.0);
        let title_budget = (pill_x - title_x - pt(10.0)).max(pt(40.0));
        let title = p.truncate(13.0, self.result.word(), title_budget as f32 / PX);
        p.text(title_x, title_y, 13.0, 0, &title);

        // Header divider rule (comfortably below the buttons)
        p.hline_t(
            card_y + pt(26.0),
            card_x + pt(10.0),
            card_x + card_w - pt(10.0),
            1,
            225,
        );

        let mut text_y = card_y + pt(38.0);

        if let Some(e) = &self.result.definition {
            p.text(
                card_x + pt(12.0),
                text_y,
                7.5,
                110,
                &format!("DEFINITION \u{00b7} {}", e.source),
            );
            text_y += pt(12.0);
            let def_truncated = def_shown < def_lines.len();
            for (i, line) in def_lines.iter().take(def_shown).enumerate() {
                let text = if def_truncated && i + 1 == def_shown {
                    "..." // the e-ink font has no ellipsis glyph
                } else {
                    line.text.as_str()
                };
                // Example sentences (and their wrapped continuations —
                // the flag rides through wrap_lines) indent and dim so
                // the gloss reads as the definition block.
                p.text(
                    card_x + pt(12.0) + if line.example { pt(10.0) } else { 0 },
                    text_y,
                    9.0,
                    if line.example { 110 } else { 30 },
                    text,
                );
                text_y += pt(13.0);
            }
            text_y += pt(4.0);
        }

        if let Some(e) = &self.result.translation {
            p.text(
                card_x + pt(12.0),
                text_y,
                7.5,
                110,
                &format!("TRANSLATION \u{00b7} {}", e.source),
            );
            // Also allow tapping on the TRANSLATION header row to cycle dictionaries
            self.trans_header_rect = Rect::new(
                card_x,
                text_y - pt(10.0),
                card_w,
                pt(16.0),
            );
            text_y += pt(12.0);
            let tr_truncated = tr_shown < tr_lines.len();
            for (i, line) in tr_lines.iter().take(tr_shown).enumerate() {
                let text = if tr_truncated && i + 1 == tr_shown {
                    "..."
                } else {
                    line.text.as_str()
                };
                p.text(card_x + pt(12.0), text_y, 9.0, 50, text);
                text_y += pt(13.0);
            }
        } else {
            self.trans_header_rect = Rect::new(0, 0, 0, 0);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Tapping the dictionary selector pill or the translation header row cycles dictionaries
                if self.dict_rect.contains(px, py) || self.trans_header_rect.contains(px, py) {
                    return self.cycle_dict();
                }

                // Tapping the Learn checkbox toggles learning
                if self.chk_rect.contains(px, py) {
                    return self.dispatch(WordAction::StarLearning);
                }

                // Tapping anywhere else on screen closes the popup
                self.dispatch(WordAction::Close)
            }
            Gesture::Swipe { .. } => self.dispatch(WordAction::Close),
            Gesture::LongPress { .. } => self.dispatch(WordAction::Close),
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_budget_adapts_to_translation_row() {
        let max_h = pt(MAX_CARD_PT);
        let min_h = pt(MIN_CARD_PT);

        // Translation off: the definition gets the whole budget.
        let (def_only, tr_off, card_off) = budget_lines(12, 0);
        assert_eq!(tr_off, 0);
        assert!(def_only >= 8, "definition should get the budget: {def_only}");
        assert!(card_off <= max_h);

        // One translation line: it keeps its line, definition shrinks.
        let (def_one, tr_one, card_one) = budget_lines(12, 1);
        assert_eq!(tr_one, 1);
        assert!(def_one < def_only, "def must yield space to translation");
        assert!(card_one <= max_h);

        // A long translation is capped; the definition still fits.
        let (def_long, tr_long, card_long) = budget_lines(12, 5);
        assert_eq!(tr_long, MAX_TR_LINES, "translation lines must cap");
        assert!(def_long >= 1);
        assert!(card_long <= max_h);

        // Nothing at all: the sheet floors at the minimum.
        let (_, _, card_empty) = budget_lines(0, 0);
        assert_eq!(card_empty, min_h);

        // Definition only, short: card shrinks below the max.
        let (_, _, card_short) = budget_lines(2, 0);
        assert!(card_short < max_h && card_short >= min_h);
    }

    #[test]
    fn wrap_preserves_sense_blocks() {
        let font = yui::font::Font::load().unwrap();
        let (mut canvas, mut panel) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        let text = "1. move fast by using one's feet, with one foot off the ground\n\
                    \"Don't run--you'll be out of breath\" · \"The children ran to the store\"\n\
                    2. a score in baseball";
        let lines = wrap_lines(&p, text, 800.0, 9.5);
        assert!(lines.len() >= 4, "{lines:?}");
        // The gloss, example and next sense stay on their own lines.
        assert!(lines.iter().any(|l| l.text.starts_with("1. move fast")));
        assert!(lines.iter().any(|l| l.text.starts_with('"')));
        assert!(lines.iter().any(|l| l.text.starts_with("2. a score")));
        // Nothing re-glued the blocks into one paragraph.
        for l in &lines {
            assert!(!l.text.contains("2. a score") || l.text.starts_with("2. a score"));
        }
    }

    // The "replication" regression: an example sentence too long for one
    // card line must keep its ghosted style on the continuation line —
    // the example flag rides through wrap_lines instead of being
    // re-derived from each physical line's leading quote.
    #[test]
    fn wrapped_example_continuation_keeps_the_example_style() {
        let font = yui::font::Font::load().unwrap();
        let (mut canvas, mut panel) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );

        let meaning = "the act of making copies\n\
                       \"Gutenberg's reproduction of holy texts was far more efficient\"\n\
                       2. (genetics) the activity of reproducing nucleic acids";
        let lines = wrap_lines(&p, meaning, 400.0, 9.0);

        // The Gutenberg example is long enough to wrap at this width.
        let example_lines: Vec<&WrapLine> = lines
            .iter()
            .filter(|l| l.text.contains("Gutenberg") || l.text.contains("efficient"))
            .collect();
        assert!(example_lines.len() >= 2, "{lines:?}");
        assert!(
            example_lines.iter().all(|l| l.example),
            "every line of the wrapped example is an example: {example_lines:?}"
        );
        // The definition and the second sense stay full-contrast.
        assert!(lines
            .iter()
            .filter(|l| l.text.starts_with("the act of") || l.text.starts_with("2. "))
            .all(|l| !l.example));
    }

    #[test]
    fn hitboxes_are_generous_and_render_preview() {
        let font = yui::font::Font::load().unwrap();
        let (mut canvas, mut panel) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        let dicts = std::rc::Rc::new(crate::dictionary::open_active());
        let result = WordResult {
            definition: Some(crate::dictionary::DictEntry {
                // Real "replication" entry (definition + long example that
                // wraps) plus a fabricated second sense line, so the
                // artifact exercises both the continuation style and
                // multi-sense rendering.
                word: "replication".to_string(),
                source: "WordNet".to_string(),
                meaning: "the act of making copies\n\
                          \"Gutenberg's reproduction of holy texts was far more efficient\"\n\
                          2. (genetics) the activity of reproducing nucleic acids"
                    .to_string(),
            }),
            translation: Some(crate::dictionary::DictEntry {
                word: "replication".to_string(),
                source: "eng-rus".to_string(),
                meaning: "репликация, копирование, повторение".to_string(),
            }),
        };
        let mut dialog = WordDialog::new(
            result,
            false,
            None,
            dicts,
            "sample.epub".to_string(),
            |_| Action::Keep,
        );
        dialog.draw(&mut p);

        // Dictionary toggle hitbox must be tall (at least 40pt) and wide (at least 50pt)
        assert!(dialog.dict_rect.h >= pt(40.0), "dict_rect height too small: {}", dialog.dict_rect.h);
        assert!(dialog.dict_rect.w >= pt(50.0), "dict_rect width too small: {}", dialog.dict_rect.w);

        // Translation header hitbox must span the card width
        assert!(dialog.trans_header_rect.w >= pt(250.0), "trans_header_rect width too small: {}", dialog.trans_header_rect.w);

        // Learn button hitbox must be generous
        assert!(dialog.chk_rect.h >= pt(40.0), "chk_rect height too small: {}", dialog.chk_rect.h);
        assert!(dialog.chk_rect.w >= pt(60.0), "chk_rect width too small: {}", dialog.chk_rect.w);

        // Save preview PNG
        let artifact_dir = match std::env::var("YB_AI_PREVIEW_DIR")
            .or_else(|_| std::env::var("ARTIFACT_DIR"))
        {
            Ok(d) if !d.is_empty() => d,
            _ => return,
        };
        let path = std::path::Path::new(&artifact_dir).join("word_dialog_preview.png");
        if let Ok(file) = std::fs::File::create(&path) {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut w) = enc.write_header() {
                let _ = w.write_image_data(&canvas);
            }
        }
    }
}
