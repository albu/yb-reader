//! Dialog construction for the reader: pure builders that gather book
//! state and wire the dialog callbacks. ReaderScreen methods are thin
//! wrappers around these — they take value snapshots, never &mut self,
//! so the callback wiring reads linearly instead of interleaved with
//! screen state juggling.

use std::rc::Rc;

use mupdf::Document;
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
    back: Option<(usize, usize)>,
    path_name: String,
    total: usize,
    settings: ReaderSettings,
) -> Action {
    let mut dlg =
        crate::toc_dialog::TocDialog::from_outlines(outlines, cur_page, move |act| match act {
            crate::toc_dialog::TocAction::JumpToYRead { page, .. } => {
                record(&path_name, page, total, settings);
                Action::Pop
            }
            crate::toc_dialog::TocAction::Back { page, sub } => {
                positions::record_pos(&path_name, page, total, sub, Some(settings));
                Action::Pop
            }
            crate::toc_dialog::TocAction::Close => Action::Pop,
        });
    if let Some((page, sub)) = back {
        dlg = dlg.with_back(page, sub);
    }
    Action::Push(Box::new(dlg))
}

pub fn yread_toc_dialog(
    toc: &[yread::model::TocEntry],
    cur_chapter: usize,
    cur_char: usize,
    offsets: &[usize],
    chars: &[usize],
    total_pages: usize,
    back: Option<(usize, usize)>,
    path_name: String,
    settings: ReaderSettings,
) -> Action {
    let mut dlg = crate::toc_dialog::TocDialog::from_yread_toc(
        toc,
        cur_chapter,
        cur_char,
        offsets,
        chars,
        total_pages,
        move |act| match act {
            crate::toc_dialog::TocAction::JumpToYRead {
                chapter_idx,
                char_offset,
                page,
            } => {
                let encoded_sub = crate::backend_yread::pack_yread_sub(chapter_idx, char_offset);
                positions::record_pos(&path_name, page, total_pages, encoded_sub, Some(settings));
                Action::Pop
            }
            crate::toc_dialog::TocAction::Back { page, sub } => {
                positions::record_pos(&path_name, page, total_pages, sub, Some(settings));
                Action::Pop
            }
            crate::toc_dialog::TocAction::Close => Action::Pop,
        },
    );
    if let Some((page, sub)) = back {
        dlg = dlg.with_back(page, sub);
    }
    Action::Push(Box::new(dlg))
}

pub fn scrubber_dialog(
    doc: &Rc<Document>,
    cur_page: usize,
    total: usize,
    bg: Option<Vec<u8>>,
    back: Option<(usize, usize)>,
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
        move |target_page| render_page(doc_for_renderer.as_ref(), target_page, 0, &settings, w, h),
        move |act| match act {
            crate::scrubber_dialog::ScrubberAction::Done(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::scrubber_dialog::ScrubberAction::OpenToc(target) => {
                if let Ok(ol) = doc_for_toc.outlines() {
                    let path_cl = path_name.clone();
                    let mut dlg =
                        crate::toc_dialog::TocDialog::from_outlines(&ol, target, move |act| {
                            match act {
                                crate::toc_dialog::TocAction::JumpToYRead { page, .. } => {
                                    record(&path_cl, page, total, settings);
                                    Action::PopN(2)
                                }
                                crate::toc_dialog::TocAction::Back { page, sub } => {
                                    positions::record_pos(
                                        &path_cl,
                                        page,
                                        total,
                                        sub,
                                        Some(settings),
                                    );
                                    Action::PopN(2)
                                }
                                crate::toc_dialog::TocAction::Close => Action::Pop,
                            }
                        });
                    if let Some((page, sub)) = back {
                        dlg = dlg.with_back(page, sub);
                    }
                    return Action::Push(Box::new(dlg));
                }
                Action::Pop
            }
            crate::scrubber_dialog::ScrubberAction::OpenHighlights(target) => {
                let path_hl = path_name.clone();
                let path_jump = path_hl.clone();
                Action::Push(Box::new(
                    crate::highlights_dialog::HighlightsDialog::from_book(
                        &path_hl,
                        target,
                        move |act| match act {
                            crate::highlights_dialog::HighlightsAction::JumpTo(p) => {
                                // The stored page can predate a reflow — clamp.
                                let page = p.min(total.saturating_sub(1));
                                record(&path_jump, page, total, settings);
                                // Same unwind as the TOC jump: pop list + scrubber.
                                Action::PopN(2)
                            }
                            crate::highlights_dialog::HighlightsAction::Close => Action::Pop,
                        },
                    ),
                ))
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
                    }
                }
                snippet = lines.join("\n");
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
            crate::footnote_dialog::FootnoteAction::JumpToYRead {
                chapter_idx,
                char_offset,
                page,
            } => {
                let encoded_sub = chapter_idx * 1_000_000 + (char_offset % 1_000_000);
                positions::record_pos(&path_name, page, total, encoded_sub, Some(settings));
                Action::Pop
            }
            crate::footnote_dialog::FootnoteAction::Close => Action::Pop,
        },
    )))
}

pub fn footnote_dialog_yread(
    book: &yread::Book,
    cur_chap_idx: usize,
    uri: &str,
    bg: Option<Vec<u8>>,
    path_name: String,
    total: usize,
    settings: ReaderSettings,
    chap_offsets: &[usize],
    chars_per_page: f32,
) -> Action {
    let res = book.resolve_footnote_or_link(cur_chap_idx, uri);
    let target_yread = res.target.map(|(ch, off)| {
        let base_page = chap_offsets.get(ch).copied().unwrap_or(0);
        let sec_page = (off as f32 / chars_per_page.max(100.0)).floor() as usize;
        let page = base_page + sec_page;
        (ch, off, page)
    });

    Action::Push(Box::new(crate::footnote_dialog::FootnoteDialog::new_yread(
        &res.title,
        &res.text,
        target_yread,
        bg,
        move |act| match act {
            crate::footnote_dialog::FootnoteAction::JumpTo(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::footnote_dialog::FootnoteAction::JumpToYRead {
                chapter_idx,
                char_offset,
                page,
            } => {
                let encoded_sub = chapter_idx * 1_000_000 + (char_offset % 1_000_000);
                positions::record_pos(&path_name, page, total, encoded_sub, Some(settings));
                Action::Pop
            }
            crate::footnote_dialog::FootnoteAction::Close => Action::Pop,
        },
    )))
}

pub fn quick_settings_sheet(
    book: String,
    page_no: usize,
    sub_idx: usize,
    total: usize,
    settings: ReaderSettings,
    is_pdf: bool,
    doc: Option<std::rc::Rc<mupdf::Document>>,
    page_gray: Option<Vec<u8>>,
    on_change: impl FnMut(ReaderSettings) -> Option<Vec<u8>> + 'static,
) -> Action {
    Action::Push(Box::new(crate::quick_settings::QuickSettingsSheet::new(
        book, page_no, sub_idx, total, settings, is_pdf, doc, page_gray, on_change,
    )))
}

/// The Learning pill matches the profile's stored form: record_lookup
/// stores clean_word(), so the dictionary's display headword must be
/// cleaned the same way before comparing ("power plant" → "powerplant").
fn is_learning_word(prof: &crate::vocab::VocabProfile, word: &str) -> bool {
    prof.learning_words.contains(&crate::vocab::clean_word(word))
}

pub fn word_dialog(
    result: crate::dictionary::WordResult,
    mut prof: crate::vocab::VocabProfile,
    bg: Option<Vec<u8>>,
    dicts: std::rc::Rc<crate::dictionary::ActiveDicts>,
    book: String,
) -> Action {
    let word = result.word().to_string();
    let is_learning = is_learning_word(&prof, &word);

    Action::Push(Box::new(crate::word_dialog::WordDialog::new(
        result,
        is_learning,
        bg,
        dicts,
        book,
        move |action| {
            match action {
                crate::word_dialog::WordAction::StarLearning => {
                    // User dictionaries carry no difficulty score; keep the
                    // profile's default learning weight.
                    prof.record_lookup(&word, 50);
                    let mut deck = crate::flashcards::FlashcardDeck::load();
                    deck.add_word(&word);
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
        &format!(
            "«{}» — no entry in the active dictionaries.\n\n\
             Add one over Wi-Fi (receive page → Dictionaries) or pick one \
             in System → Dictionaries.",
            word
        ),
        None,
        bg,
        |_act| Action::Pop,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The F7 regression: the Learning pill used to compare the raw
    // display headword against the stored set, so multi-word entries
    // ("power plant", stored by record_lookup as "powerplant") never
    // matched. Both sides of that contract must go through clean_word.
    #[test]
    fn learning_pill_matches_multi_word_headwords() {
        let mut prof = crate::vocab::VocabProfile::default();
        assert!(!is_learning_word(&prof, "Power Plant"));

        // Marking it learning stores the clean form…
        prof.record_lookup("Power Plant", 50);
        assert!(prof.learning_words.contains("powerplant"));

        // …and the pill matches the display form the dictionary reports.
        assert!(is_learning_word(&prof, "Power Plant"));
        assert!(is_learning_word(&prof, "power plant"));
    }
}
