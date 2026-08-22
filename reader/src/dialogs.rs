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
    let mut dlg = crate::toc_dialog::TocDialog::from_outlines(
        outlines,
        cur_page,
        move |act| match act {
            crate::toc_dialog::TocAction::JumpTo(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::toc_dialog::TocAction::JumpToYRead { page, .. } => {
                record(&path_name, page, total, settings);
                Action::Pop
            }
            crate::toc_dialog::TocAction::Back { page, sub } => {
                positions::record_pos(&path_name, page, total, sub, Some(settings));
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

pub fn yread_toc_dialog(
    toc: &[yread::model::TocEntry],
    offsets: &[usize],
    chars_per_page: f32,
    cur_page: usize,
    back: Option<(usize, usize)>,
    path_name: String,
    total_pages: usize,
    settings: ReaderSettings,
) -> Action {
    let mut dlg = crate::toc_dialog::TocDialog::from_yread_toc(
        toc,
        offsets,
        chars_per_page,
        cur_page,
        move |act| match act {
            crate::toc_dialog::TocAction::JumpTo(target) => {
                record(&path_name, target, total_pages, settings);
                Action::Pop
            }
            crate::toc_dialog::TocAction::JumpToYRead { chapter_idx, char_offset, page } => {
                let encoded_sub = chapter_idx * 1_000_000 + (char_offset % 1_000_000);
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
        move |target_page| {
            render_page(doc_for_renderer.as_ref(), target_page, 0, &settings, w, h)
        },
        move |act| match act {
            crate::scrubber_dialog::ScrubberAction::Done(target) => {
                record(&path_name, target, total, settings);
                Action::Pop
            }
            crate::scrubber_dialog::ScrubberAction::OpenToc(target) => {
                if let Ok(ol) = doc_for_toc.outlines() {
                    let path_cl = path_name.clone();
                    let mut dlg = crate::toc_dialog::TocDialog::from_outlines(
                        &ol,
                        target,
                        move |act| match act {
                            crate::toc_dialog::TocAction::JumpTo(t) => {
                                record(&path_cl, t, total, settings);
                                Action::PopN(2)
                            }
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
                        },
                    );
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
                Action::Push(Box::new(crate::highlights_dialog::HighlightsDialog::from_book(
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
                )))
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
            crate::footnote_dialog::FootnoteAction::JumpToYRead { chapter_idx, char_offset, page } => {
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
            crate::footnote_dialog::FootnoteAction::JumpToYRead { chapter_idx, char_offset, page } => {
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
        book,
        page_no,
        sub_idx,
        total,
        settings,
        is_pdf,
        doc,
        page_gray,
        on_change,
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
