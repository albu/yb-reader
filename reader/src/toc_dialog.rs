use mupdf::Outline;
use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

#[derive(Debug, Clone)]
pub struct TocItem {
    pub title: String,
    pub chapter_idx: usize,
    pub char_offset: usize,
    pub page: usize,
    pub level: usize,
}

pub enum TocAction {
    #[allow(dead_code)]
    JumpTo(usize),
    JumpToYRead {
        chapter_idx: usize,
        char_offset: usize,
        page: usize,
    },
    /// Return to the previous reading location (jump-history undo). The
    /// reader applies it on resume — this dialog only reports it.
    Back {
        page: usize,
        sub: usize,
    },
    Close,
}

pub struct TocDialog<F: FnMut(TocAction) -> Action> {
    items: Vec<TocItem>,
    /// Per-item expansion state (tree rows with children). Rows without
    /// children ignore their flag; collapsing a parent hides its subtree
    /// but keeps child flags, so re-expanding restores the exact view.
    expanded: Vec<bool>,
    current_page: usize,
    active_idx: usize,
    /// Window start, in VISIBLE-row coordinates (collapsed rows hidden).
    offset: usize,
    per_page: usize,
    dims: (i32, i32),
    /// Previous reading position from the jump history, when one exists —
    /// rendered as a pinned "Back" row above the chapter list.
    back_target: Option<(usize, usize)>,
    on_action: F,
}

impl<F: FnMut(TocAction) -> Action> TocDialog<F> {
    pub fn from_outlines(outlines: &[Outline], current_page: usize, on_action: F) -> Self {
        let mut items = Vec::new();
        Self::flatten_outlines(outlines, 0, &mut items);
        Self::with_items(items, current_page, on_action)
    }

    /// Arm the "Back to p.N" row (previous position from the reader's jump
    /// history). Hidden when the history is empty.
    pub fn with_back(mut self, page: usize, sub: usize) -> Self {
        self.back_target = Some((page, sub));
        self
    }

    pub fn from_yread_toc(
        toc: &[yread::model::TocEntry],
        cur_chapter: usize,
        cur_char: usize,
        offsets: &[usize],
        on_action: F,
    ) -> Self {
        let mut items = Vec::new();
        for entry in toc {
            let base_page = offsets.get(entry.chapter_idx).copied().unwrap_or(0);
            items.push(TocItem {
                title: entry.title.clone(),
                chapter_idx: entry.chapter_idx,
                char_offset: entry.char_offset,
                page: base_page,
                level: entry.level,
            });
        }

        // Find the entry containing the current chapter and character offset
        let mut best_idx = 0;
        for (i, item) in items.iter().enumerate() {
            if item.chapter_idx < cur_chapter
                || (item.chapter_idx == cur_chapter && item.char_offset <= cur_char)
            {
                best_idx = i;
            } else if item.chapter_idx > cur_chapter {
                break;
            }
        }

        Self::with_items_and_active(items, cur_chapter, best_idx, on_action)
    }

    #[allow(dead_code)]
    pub fn from_chapters(
        chapters: &[yread::model::Chapter],
        current_chap: usize,
        on_action: F,
    ) -> Self {
        let mut items = Vec::new();
        for (idx, ch) in chapters.iter().enumerate() {
            let title = if ch.title.trim().is_empty() {
                format!("Chapter {}", idx + 1)
            } else {
                ch.title.trim().to_string()
            };
            items.push(TocItem {
                title,
                chapter_idx: idx,
                char_offset: 0,
                page: idx,
                level: 0,
            });
        }
        Self::with_items(items, current_chap, on_action)
    }

    /// Common init: everything collapsed except the ancestor chain of the
    /// entry containing `current_page`, and pre-scrolled to it.
    fn with_items(items: Vec<TocItem>, current_page: usize, on_action: F) -> Self {
        // Find the entry containing the current page
        let mut best_idx = 0;
        for (i, item) in items.iter().enumerate() {
            if item.page <= current_page {
                best_idx = i;
            } else {
                break;
            }
        }
        Self::with_items_and_active(items, current_page, best_idx, on_action)
    }

    fn with_items_and_active(
        items: Vec<TocItem>,
        current_page: usize,
        best_idx: usize,
        on_action: F,
    ) -> Self {
        let mut expanded = vec![false; items.len()];
        // Expand the ancestors (strictly shallower entries) above it so
        // "where am I" is visible on open; the entry itself stays as-is.
        let mut needed = items.get(best_idx).map(|it| it.level).unwrap_or(0);
        for j in (0..best_idx).rev() {
            if items[j].level < needed {
                expanded[j] = true;
                needed = items[j].level;
                if needed == 0 {
                    break;
                }
            }
        }
        let mut dlg = TocDialog {
            items,
            expanded,
            current_page,
            active_idx: best_idx,
            offset: 0,
            per_page: 8,
            dims: (1236, 1648),
            back_target: None,
            on_action,
        };
        let vis = dlg.visible_indices();
        let vis_pos = vis
            .iter()
            .position(|&i| i >= best_idx)
            .unwrap_or(0);
        dlg.offset = vis_pos.saturating_sub(2);
        dlg
    }

    /// Does this row own a subtree (next entry is deeper)?
    fn has_children(&self, idx: usize) -> bool {
        self.items
            .get(idx + 1)
            .map(|next| next.level > self.items[idx].level)
            .unwrap_or(false)
    }

    /// The pinned "Back to p.N" row just under the header bar, when jump
    /// history exists. Same geometry in draw and tap handling.
    fn back_row_rect(&self) -> Option<Rect> {
        self.back_target.map(|_| {
            let (w, _h) = self.dims;
            let bar_h = pt(40.0);
            let pad = pt(14.0);
            Rect::new(pad, bar_h + pt(6.0), w - 2 * pad, pt(34.0))
        })
    }

    /// Indices of rows currently visible: an entry shows only while every
    /// ancestor above it is expanded. Collapsed parents stay in the
    /// ancestor chain (as closed doors), so grandchildren stay hidden too.
    fn visible_indices(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut chain: Vec<usize> = Vec::new();
        for i in 0..self.items.len() {
            while let Some(&top) = chain.last() {
                if self.items[top].level < self.items[i].level {
                    break;
                }
                chain.pop();
            }
            let visible = chain.iter().all(|&a| self.expanded[a]);
            if visible {
                out.push(i);
                if self.has_children(i) {
                    chain.push(i);
                }
            } else if self.has_children(i) {
                // Keep closed doors in the chain even when this row itself
                // is hidden — its deeper relatives must see the closure.
                chain.push(i);
            }
        }
        out
    }

    fn flatten_outlines(outlines: &[Outline], level: usize, out: &mut Vec<TocItem>) {
        for o in outlines {
            let page_num = if let Some(dest) = &o.dest {
                dest.loc.page_number as usize
            } else if let Some(uri) = &o.uri {
                if let Some(pos) = uri.find("#page=") {
                    let s = &uri[pos + 6..];
                    let p_str = s.split('&').next().unwrap_or("1");
                    p_str.parse::<usize>().unwrap_or(1).saturating_sub(1)
                } else {
                    0
                }
            } else {
                0
            };


            let clean_title = o.title.trim().to_string();
            if !clean_title.is_empty() {
                out.push(TocItem {
                    title: clean_title,
                    chapter_idx: 0,
                    char_offset: 0,
                    page: page_num,
                    level,
                });
            }

            if !o.down.is_empty() {
                Self::flatten_outlines(&o.down, level + 1, out);
            }
        }
    }
}

impl<F: FnMut(TocAction) -> Action> Screen for TocDialog<F> {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        // Header Navigation Bar
        let bar_h = pt(40.0);
        p.rect(Rect::new(0, 0, w, bar_h), 245);
        p.hline_t(bar_h, 0, w, 1, 200);

        p.text(pt(16.0), pt(24.0), 10.5, 0, "Table of Contents");

        // Close Button on top-right
        let close_w = pt(55.0);
        let close_h = pt(24.0);
        let close_x = w - close_w - pt(12.0);
        let close_y = pt(8.0);
        let close_rect = Rect::new(close_x, close_y, close_w, close_h);
        p.rect_outline_t(close_rect, 1, 100);
        p.text_center_in(close_x, close_x + close_w, close_y + pt(16.0), 8.5, 0, "Close");

        // Back row — pinned above the list, only when the reader has a
        // previous jump to return to. The arrow is DRAWN (two lines + a
        // stem), same policy as the tree chevrons: no glyph-coverage
        // gamble on exotic arrow codepoints.
        let pad = pt(14.0);
        let back_h = pt(34.0);
        if let Some((back_page, _)) = self.back_target {
            let back_rect = self.back_row_rect().unwrap();
            p.rect(back_rect, 244);
            p.rect_outline_t(back_rect, 1, 120);
            // ← arrow: tip at (ax, cy), stem to (ax + s*2, cy)
            let cy = back_rect.y + back_rect.h / 2;
            let ax = pad + pt(3.0);
            let s = pt(4.0);
            p.line_w(ax, cy, ax + 2 * s, cy, 2, 0);
            p.line_w(ax, cy, ax + s, cy - s, 2, 0);
            p.line_w(ax, cy, ax + s, cy + s, 2, 0);
            let back_text = format!("Back to p.{}", back_page + 1);
            p.text(pad + pt(20.0), back_rect.y + pt(20.0), 9.0, 0, &back_text);
        }

        if self.items.is_empty() {
            p.text_center(h / 2, 11.0, 0, "No Table of Contents available in this book");
            return;
        }

        // List of Chapters
        let list_top = bar_h + pt(10.0) + if self.back_target.is_some() { back_h + pt(4.0) } else { 0 };
        let row_h = pt(42.0);
        let content_h = h - list_top - pt(35.0);
        self.per_page = ((content_h / row_h) as usize).max(1);

        let vis = self.visible_indices();
        self.offset = self.offset.min(vis.len().saturating_sub(1));
        let visible = vis.len().min(self.offset + self.per_page);
        for (i, vpos) in (self.offset..visible).enumerate() {
            let idx = vis[vpos];
            let item = &self.items[idx];
            let ry = list_top + i as i32 * row_h;
            let is_current = idx == self.active_idx;

            let row_rect = Rect::new(pad, ry, w - 2 * pad, row_h - pt(4.0));
            if is_current {
                p.rect(row_rect, 240);
                p.rect_outline_t(row_rect, 1, 0);
            } else {
                p.rect_outline_t(row_rect, 1, 220);
            }

            // Indentation by level
            let indent = pt((item.level as f32 * 10.0).min(40.0));
            let text_x = pad + pt(10.0) + indent;
            let text_y = ry + pt(22.0);

            // Chevron for expandable rows, drawn as two lines — no glyph
            // coverage gamble. Page number shifts left to make room.
            let expandable = self.has_children(idx);
            if expandable {
                let cy = ry + (row_h - pt(4.0)) / 2;
                let cx = w - pad - pt(34.0);
                let s = pt(4.0);
                if self.expanded[idx] {
                    // ▾
                    p.line_w(cx - s, cy - s, cx, cy + s, 2, 60);
                    p.line_w(cx + s, cy - s, cx, cy + s, 2, 60);
                } else {
                    // ▸
                    p.line_w(cx - s, cy - s, cx + s, cy, 2, 60);
                    p.line_w(cx - s, cy + s, cx + s, cy, 2, 60);
                }
            }

            let max_w = (w - text_x - pt(70.0) - if expandable { pt(34.0) } else { 0 }) as f32;
            let title = p.truncate(9.5, &item.title, max_w);

            let marker = if is_current { "● " } else { "" };
            let display_title = format!("{}{}", marker, title);
            p.text(text_x, text_y, 9.5, 0, &display_title);

            // Page number on right
            let page_str = format!("p. {}", item.page + 1);
            let page_right = if expandable { w - pad - pt(56.0) } else { w - pad - pt(12.0) };
            p.text_right(page_right, text_y, 8.5, 100, &page_str);
        }

        // Footer Pagination Info
        let footer_text = if vis.len() < self.items.len() {
            format!(
                "{}-{} of {} shown ({} total) · ▸ expands",
                self.offset + 1,
                visible,
                vis.len(),
                self.items.len()
            )
        } else {
            format!("{}-{} of {} chapters", self.offset + 1, visible, self.items.len())
        };
        p.text_center(h - pt(12.0), 8.0, 120, &footer_text);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, _h) = self.dims;

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Close Button tap
                let close_w = pt(55.0);
                let close_h = pt(24.0);
                let close_x = w - close_w - pt(12.0);
                let close_y = pt(8.0);
                let close_rect = Rect::new(close_x, close_y, close_w, close_h);
                if close_rect.contains(px, py) {
                    return (self.on_action)(TocAction::Close);
                }

                // Back row tap — checked before the empty-list guard so a
                // book without a TOC can still return from a jump.
                if let Some((back_page, back_sub)) = self.back_target {
                    if self
                        .back_row_rect()
                        .map(|r| r.contains(px, py))
                        .unwrap_or(false)
                    {
                        return (self.on_action)(TocAction::Back {
                            page: back_page,
                            sub: back_sub,
                        });
                    }
                }

                if self.items.is_empty() {
                    return (self.on_action)(TocAction::Close);
                }

                // Row tap
                let bar_h = pt(40.0);
                let list_top = bar_h
                    + pt(10.0)
                    + if self.back_target.is_some() {
                        pt(34.0) + pt(4.0)
                    } else {
                        0
                    };
                let row_h = pt(42.0);
                let pad = pt(14.0);

                if py >= list_top && py < list_top + self.per_page as i32 * row_h {
                    let vpos = self.offset + ((py - list_top) / row_h) as usize;
                    let vis = self.visible_indices();
                    if vpos < vis.len() {
                        let idx = vis[vpos];
                        // Chevron zone (expandable rows): toggle the subtree
                        if self.has_children(idx) {
                            let cx = w - pad - pt(34.0);
                            if px >= cx - pt(18.0) {
                                self.expanded[idx] = !self.expanded[idx];
                                return Action::RedrawFull;
                            }
                        }
                        let it = &self.items[idx];
                        return (self.on_action)(TocAction::JumpToYRead {
                            chapter_idx: it.chapter_idx,
                            char_offset: it.char_offset,
                            page: it.page,
                        });
                    }
                }

                Action::Keep
            }

            Gesture::Swipe { dir: SwipeDir::North, .. } => {
                let vis_len = self.visible_indices().len();
                self.offset = (self.offset + self.per_page.max(1)).min(vis_len.saturating_sub(1));
                Action::RedrawFull
            }
            Gesture::Swipe { dir: SwipeDir::South, .. } => {
                if self.offset > 0 {
                    self.offset = self.offset.saturating_sub(self.per_page);
                    Action::RedrawFull
                } else {
                    Action::Keep
                }
            }
            Gesture::Swipe { .. } => (self.on_action)(TocAction::Close),
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, char_off: usize, level: usize) -> yread::model::TocEntry {
        yread::model::TocEntry {
            title: title.to_string(),
            chapter_idx: 0,
            byte_offset: char_off,
            char_offset: char_off,
            level,
        }
    }

    fn book_toc() -> Vec<yread::model::TocEntry> {
        vec![
            entry("Part One", 0, 0),       // char 0
            entry("Ch 1", 5000, 1),        // char 5000
            entry("Sec 1.1", 9000, 2),     // char 9000
            entry("Part Two", 20000, 0),   // char 20000
            entry("Ch 2", 24000, 1),       // char 24000
        ]
    }

    fn dialog(current_char: usize) -> TocDialog<impl FnMut(TocAction) -> Action> {
        TocDialog::from_yread_toc(&book_toc(), 0, current_char, &[0], |_| Action::Keep)
    }

    fn titles(d: &TocDialog<impl FnMut(TocAction) -> Action>) -> Vec<&str> {
        d.visible_indices()
            .iter()
            .map(|&i| d.items[i].title.as_str())
            .collect()
    }

    #[test]
    fn opens_collapsed_except_current_ancestors() {
        // Reading inside Ch 1 (char 6000): Part One auto-expanded, Ch 1's own
        // subtree stays closed, Part Two collapsed.
        let d = dialog(6000);
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Part Two"]);
    }

    #[test]
    fn deep_position_expands_full_ancestor_chain() {
        // Inside Sec 1.1 (char 10000): both Part One and Ch 1 expanded.
        let d = dialog(10000);
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Sec 1.1", "Part Two"]);
    }

    #[test]
    fn chevron_toggle_collapses_and_restores() {
        let mut d = dialog(10000);
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Sec 1.1", "Part Two"]);
        // Collapse Part One (idx 0): subtree hidden, Ch 1 keeps its flag.
        d.expanded[0] = false;
        assert_eq!(titles(&d), vec!["Part One", "Part Two"]);
        d.expanded[0] = true;
        // Child expansion state survived the collapse cycle.
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Sec 1.1", "Part Two"]);
    }

    #[test]
    fn collapsed_parent_hides_grandchildren_even_if_child_open() {
        let mut d = dialog(6000); // [Part One, Ch 1, Part Two]
        d.expanded[1] = true; // open Ch 1 -> Sec 1.1 shows
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Sec 1.1", "Part Two"]);
        d.expanded[0] = false; // close Part One: everything below hides
        assert_eq!(titles(&d), vec!["Part One", "Part Two"]);
        d.expanded[0] = true; // reopen: Sec 1.1 still there (flag kept)
        assert_eq!(titles(&d), vec!["Part One", "Ch 1", "Sec 1.1", "Part Two"]);
    }

    #[test]
    fn has_children_boundaries() {
        let d = dialog(6000);
        assert!(d.has_children(0)); // Part One -> Ch 1
        assert!(d.has_children(1)); // Ch 1 -> Sec 1.1
        assert!(!d.has_children(2)); // leaf
        assert!(!d.has_children(4)); // last item
    }

    #[test]
    fn flat_toc_shows_everything() {
        // All level 0 (from_chapters shape): no tree, no behavior change.
        let flat: Vec<yread::model::TocEntry> = (0..5).map(|i| entry(&format!("Ch {i}"), i * 10, 0)).collect();
        let d = TocDialog::from_yread_toc(&flat, 0, 30, &[0], |_| Action::Keep);
        assert_eq!(d.visible_indices().len(), 5);
    }

    #[test]
    fn back_row_appears_only_when_armed() {
        // No jump history: no Back row.
        let d = dialog(6000);
        assert!(d.back_target.is_none());
        assert!(d.back_row_rect().is_none());

        // The reader arms it with the previous (page, sub).
        let d = d.with_back(5, 2);
        assert_eq!(d.back_target, Some((5, 2)));
        assert!(d.back_row_rect().is_some());
    }
}
