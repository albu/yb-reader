//! Per-book highlights list — the index the capture flow was missing.
//! Tap a row to jump back to the highlight's page (the same in-memory
//! record_pos + unwind path the TOC uses); long-press to delete through
//! the confirm popup (deletion lives here, deliberately — never inside
//! the reading flow). Reloads itself on resume so deletes show instantly.

use ybdev::input::{Gesture, SwipeDir};

use crate::notes::{self, Highlight};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum HighlightsAction {
    /// Jump to this page (0-based, as stored).
    JumpTo(usize),
    Close,
}

pub struct HighlightsDialog<F: FnMut(HighlightsAction) -> Action> {
    book: String,
    /// Page order — it's a navigation index, not a journal.
    items: Vec<Highlight>,
    current_page: usize,
    offset: usize,
    per_page: usize,
    dims: (i32, i32),
    /// Last painted frame, so the delete confirm floats over the list.
    snap: Option<Vec<u8>>,
    on_action: F,
}

impl<F: FnMut(HighlightsAction) -> Action> HighlightsDialog<F> {
    pub fn from_book(book: &str, current_page: usize, on_action: F) -> Self {
        let mut items = notes::load(book);
        items.sort_by_key(|h| h.page);

        // Pre-scroll to the nearest highlight at/before the current page.
        let mut best_idx = 0;
        for (i, h) in items.iter().enumerate() {
            if h.page <= current_page {
                best_idx = i;
            } else {
                break;
            }
        }
        let initial_offset = best_idx.saturating_sub(2);

        HighlightsDialog {
            book: book.to_string(),
            items,
            current_page,
            offset: initial_offset,
            per_page: 8,
            dims: (1236, 1648),
            snap: None,
            on_action,
        }
    }
}

impl<F: FnMut(HighlightsAction) -> Action> Screen for HighlightsDialog<F> {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    /// A delete confirm above us popped: reload and re-present, so the
    /// removed row vanishes immediately.
    fn on_resume(&mut self) -> Action {
        self.items = notes::load(&self.book);
        self.items.sort_by_key(|h| h.page);
        if self.offset >= self.items.len() {
            self.offset = self.items.len().saturating_sub(1);
        }
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        // Header bar
        let bar_h = pt(40.0);
        p.rect(Rect::new(0, 0, w, bar_h), 245);
        p.hline_t(bar_h, 0, w, 1, 200);
        p.text(pt(16.0), pt(24.0), 10.5, 0, "Highlights");

        let close_w = pt(55.0);
        let close_h = pt(24.0);
        let close_x = w - close_w - pt(12.0);
        let close_y = pt(8.0);
        let close_rect = Rect::new(close_x, close_y, close_w, close_h);
        p.rect_outline_t(close_rect, 1, 100);
        p.text_center_in(
            close_x,
            close_x + close_w,
            close_y + pt(16.0),
            8.5,
            0,
            "Close",
        );

        if self.items.is_empty() {
            p.text_center(h / 2, 11.0, 0, "No highlights in this book yet");
            p.text_center(
                h / 2 + pt(16.0),
                8.5,
                130,
                "toggle the bookmark (top-right), then hold a word and drag",
            );
            self.snap = Some(p.snapshot());
            return;
        }

        // Rows: truncated text + page number
        let list_top = pt(50.0);
        let row_h = pt(42.0);
        let content_h = h - list_top - pt(35.0);
        self.per_page = ((content_h / row_h) as usize).max(1);
        let visible = self.items.len().min(self.offset + self.per_page);

        for (i, idx) in (self.offset..visible).enumerate() {
            let item = &self.items[idx];
            let ry = list_top + i as i32 * row_h;
            let pad = pt(14.0);

            let near = item.page <= self.current_page
                && self
                    .items
                    .get(idx + 1)
                    .map(|n| self.current_page < n.page)
                    .unwrap_or(true);
            let row_rect = Rect::new(pad, ry, w - 2 * pad, row_h - pt(4.0));
            if near {
                p.rect(row_rect, 240);
                p.rect_outline_t(row_rect, 1, 0);
            } else {
                p.rect_outline_t(row_rect, 1, 220);
            }

            let max_w = (w - 2 * pad - pt(90.0)) as f32 / pt(1.0) as f32;
            let text = p.truncate(8.5, &item.text, max_w);
            p.text(pad + pt(10.0), ry + pt(24.0), 8.5, 0, &text);
            let page_str = format!("p. {}", item.page + 1);
            p.text_right(w - pad - pt(12.0), ry + pt(24.0), 8.0, 100, &page_str);
        }

        let footer = format!(
            "{} of {} · tap: jump · hold: delete",
            if visible == 0 { 0 } else { self.offset + 1 },
            self.items.len()
        );
        p.text_center(h - pt(12.0), 8.0, 120, &footer);
        self.snap = Some(p.snapshot());
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, _h) = self.dims;

        match g {
            Gesture::Tap { x, y } => {
                let (px, py) = (x as i32, y as i32);

                let close_w = pt(55.0);
                let close_h = pt(24.0);
                let close_x = w - close_w - pt(12.0);
                let close_rect = Rect::new(close_x, pt(8.0), close_w, close_h);
                if close_rect.contains(px, py) {
                    return (self.on_action)(HighlightsAction::Close);
                }

                if self.items.is_empty() {
                    return (self.on_action)(HighlightsAction::Close);
                }

                let list_top = pt(50.0);
                let row_h = pt(42.0);
                if py >= list_top && py < list_top + self.per_page as i32 * row_h {
                    let idx = self.offset + ((py - list_top) / row_h) as usize;
                    if idx < self.items.len() {
                        return (self.on_action)(HighlightsAction::JumpTo(self.items[idx].page));
                    }
                }
                Action::Keep
            }

            Gesture::LongPress { x, y } => {
                let (_px, py) = (x as i32, y as i32);
                let list_top = pt(50.0);
                let row_h = pt(42.0);
                if py >= list_top && py < list_top + self.per_page as i32 * row_h {
                    let idx = self.offset + ((py - list_top) / row_h) as usize;
                    if idx < self.items.len() {
                        let book = self.book.clone();
                        let text = self.items[idx].text.clone();
                        let bg = self.snap.clone();
                        let preview: String = text.chars().take(60).collect();
                        return Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                            "Delete highlight?",
                            &preview,
                            "Delete",
                            bg,
                            move |act| {
                                if matches!(act, crate::confirm_dialog::ConfirmAction::Yes) {
                                    notes::remove(&book, &text);
                                }
                                Action::Pop
                            },
                        )));
                    }
                }
                Action::Keep
            }

            Gesture::Swipe {
                dir: SwipeDir::North,
                ..
            } => {
                self.offset =
                    (self.offset + self.per_page.max(1)).min(self.items.len().saturating_sub(1));
                Action::RedrawFull
            }
            Gesture::Swipe {
                dir: SwipeDir::South,
                ..
            } => {
                if self.offset > 0 {
                    self.offset = self.offset.saturating_sub(self.per_page);
                    Action::RedrawFull
                } else {
                    Action::Keep
                }
            }
            Gesture::Swipe { .. } => (self.on_action)(HighlightsAction::Close),
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_scroll_and_reload_reflect_store() {
        // YB_NOTES_DIR is process-global — serialize with notes' own tests.
        let _g = crate::notes::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("yb-hl-dialog-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_NOTES_DIR", dir.to_str().unwrap());

        for p in [10, 40, 41, 90] {
            assert!(notes::add("t.epub", p, &format!("span number {}", p)));
        }

        let noop = |_: HighlightsAction| Action::Keep;
        let d = HighlightsDialog::from_book("t.epub", 55, noop);
        // Page order; nearest-at/before page 55 is index 2 (page 41), so
        // the pre-scroll puts it two rows down (one lead-in row visible).
        let pages: Vec<usize> = d.items.iter().map(|h| h.page).collect();
        assert_eq!(pages, vec![10, 40, 41, 90]);
        assert_eq!(d.offset, 0);
        assert_eq!(d.items.len(), 4);

        notes::remove("t.epub", "span number 41");
        let mut d2 = HighlightsDialog::from_book("t.epub", 55, noop);
        d2.on_resume(); // reload path used after a delete-confirm pops
        assert_eq!(d2.items.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
