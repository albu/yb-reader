//! Dialog construction for the reader: pure builders that gather book
//! state and wire the dialog callbacks. ReaderScreen methods are thin
//! wrappers around these — they take value snapshots, never &mut self,
//! so the callback wiring reads linearly instead of interleaved with
//! screen state juggling.

use std::rc::Rc;

use mupdf::{Colorspace, Document, Matrix};
use yui::screen::Action;

use crate::positions;
use crate::render::render_page;
use crate::split::ReaderSettings;

/// Jump-to-a-fresh-place record: sub-box 0 (a TOC/scrubber jump lands at
/// the top of the target view).
fn record(path: &str, page: usize, total: usize, settings: ReaderSettings) {
    positions::record_pos(path, page, total, 0, Some(settings));
}

/// Settings-change record: preserves the CURRENT sub-box so adjusting
/// rotation or crops never yanks the reader back to the top of the page.
/// The single writer both settings entry points (the dialog's on_change
/// and the curtain's ROTATE card) go through — one persistence path, no
/// way for them to disagree.
pub fn record_sub(path: &str, page: usize, sub: usize, total: usize, settings: ReaderSettings) {
    positions::record_pos(path, page, total, sub, Some(settings));
}

pub fn toc_dialog(
    outlines: &[mupdf::Outline],
    cur_page: usize,
    path_name: String,
    total: usize,
    settings: ReaderSettings,
) -> Action {
    Action::Push(Box::new(crate::toc_dialog::TocDialog::from_outlines(
        outlines,
        cur_page,
        move |act| match act {
            crate::toc_dialog::TocAction::JumpTo(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::toc_dialog::TocAction::Close => Action::Pop,
        },
    )))
}

pub fn scrubber_dialog(
    doc: &Rc<Document>,
    cur_page: usize,
    total: usize,
    bg: Option<Vec<u8>>,
    path_name: String,
    settings: ReaderSettings,
    w: u32,
    h: u32,
) -> Action {
    let doc_for_renderer = Rc::clone(doc);
    let doc_for_toc = Rc::clone(doc);

    Action::Push(Box::new(crate::scrubber_dialog::ScrubberDialog::new(
        cur_page,
        total,
        bg,
        move |target_page| {
            render_page(doc_for_renderer.as_ref(), target_page, 0, &settings, w, h)
        },
        move |act| {
            match act {
                crate::scrubber_dialog::ScrubberAction::Done(target) => {
                    record(&path_name, target, total, settings);
                    Action::Pop
                }
                crate::scrubber_dialog::ScrubberAction::OpenToc(target) => {
                    if let Ok(ol) = doc_for_toc.outlines() {
                        let path_cl = path_name.clone();
                        return Action::Push(Box::new(crate::toc_dialog::TocDialog::from_outlines(
                            &ol,
                            target,
                            move |act| {
                                match act {
                                    crate::toc_dialog::TocAction::JumpTo(t) => {
                                        record(&path_cl, t, total, settings);
                                        // Unwind BOTH the TOC and this scrubber:
                                        // a single Pop would reveal the scrubber,
                                        // whose Done would overwrite this position
                                        // and the reader would never jump.
                                        Action::PopN(2)
                                    }
                                    crate::toc_dialog::TocAction::Close => Action::Pop,
                                }
                            },
                        )));
                    }
                    Action::Pop
                }
                crate::scrubber_dialog::ScrubberAction::OpenHighlights(target) => {
                    let path_hl = path_name.clone();
                    let path_jump = path_hl.clone();
                    Action::Push(Box::new(crate::highlights_dialog::HighlightsDialog::from_book(
                        &path_hl,
                        target,
                        move |act| {
                            match act {
                                crate::highlights_dialog::HighlightsAction::JumpTo(p) => {
                                    // The stored page can predate a reflow — clamp.
                                    let page = p.min(total.saturating_sub(1));
                                    record(&path_jump, page, total, settings);
                                    // Same unwind as the TOC jump: pop list + scrubber.
                                    Action::PopN(2)
                                }
                                crate::highlights_dialog::HighlightsAction::Close => Action::Pop,
                            }
                        },
                    )))
                }
            }
        },
    )))
}

pub fn footnote_dialog(
    doc: &Document,
    uri: &str,
    bg: Option<Vec<u8>>,
    path_name: String,
    total: usize,
    settings: ReaderSettings,
) -> Action {
    let dest = doc.resolve_link(uri).ok().flatten();
    let target_page = dest.as_ref().map(|d| d.loc.page_number as usize);

    let mut snippet = String::new();
    if let Some(target) = target_page {
        if let Ok(p) = doc.load_page(target as i32) {
            if let Ok(tp) = p.to_text_page(mupdf::TextPageFlags::empty()) {
                let mut lines = Vec::new();
                for block in tp.blocks() {
                    for line in block.lines() {
                        let mut line_str = String::new();
                        for ch in line.chars() {
                            if let Some(c) = ch.char() {
                                line_str.push(c);
                            }
                        }
                        let text = line_str.trim().to_string();
                        if !text.is_empty() {
                            lines.push(text);
                        }
                        if lines.len() >= 6 {
                            break;
                        }
                    }
                    if lines.len() >= 6 {
                        break;
                    }
                }
                snippet = lines.join(" ");
            }
        }
    }

    if snippet.is_empty() {
        snippet = format!("Link target: {}", uri);
    }

    Action::Push(Box::new(crate::footnote_dialog::FootnoteDialog::new(
        "Footnote / Note",
        &snippet,
        target_page,
        bg,
        move |act| match act {
            crate::footnote_dialog::FootnoteAction::JumpTo(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::footnote_dialog::FootnoteAction::Close => Action::Pop,
        },
    )))
}

pub fn settings_dialog(
    doc: Option<&Rc<Document>>,
    page_no: usize,
    sub_idx: usize,
    settings: ReaderSettings,
    is_pdf: bool,
    path_name: String,
    total: usize,
) -> Action {
    let samples = doc.and_then(|doc| {
        let page = doc.load_page(page_no as i32).ok()?;
        let m = Matrix::new_scale(1.0, 1.0);
        let pm = page.to_pixmap(&m, &Colorspace::device_gray(), false, true).ok()?;
        Some((
            pm.samples().to_vec(),
            pm.width() as usize,
            pm.height() as usize,
            pm.stride() as usize,
        ))
    });

    Action::Push(Box::new(crate::settings_dialog::ReaderSettingsDialog::new(
        settings,
        is_pdf,
        samples,
        move |new_settings| {
            record_sub(&path_name, page_no, sub_idx, total, new_settings);
            Action::Pop
        },
    )))
}

pub fn word_dialog(
    entry: crate::vocab::WordEntry,
    mut prof: crate::vocab::VocabProfile,
    bg: Option<Vec<u8>>,
) -> Action {
    let word = entry.word.clone();
    let diff = entry.difficulty;
    let is_learning = prof.learning_words.contains(&word);

    Action::Push(Box::new(crate::word_dialog::WordDialog::new(
        entry,
        is_learning,
        bg,
        move |action| {
            match action {
                crate::word_dialog::WordAction::StarLearning => {
                    prof.record_lookup(&word, diff);
                    let mut deck = crate::flashcards::FlashcardDeck::load();
                    deck.add_word(&word);
                }
                crate::word_dialog::WordAction::MarkKnown => {
                    prof.mark_known(&word, diff);
                }
                crate::word_dialog::WordAction::Close => {}
            }
            Action::Pop
        },
    )))
}

/// The dictionary-miss card: same bottom-card form as the word dialog.
pub fn dict_miss(word: &str, bg: Option<Vec<u8>>) -> Action {
    Action::Push(Box::new(crate::footnote_dialog::FootnoteDialog::new(
        "Dictionary",
        &format!("«{}» — no entry in the dictionary", word),
        None,
        bg,
        |_act| Action::Pop,
    )))
}
