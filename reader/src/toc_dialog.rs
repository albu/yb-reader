use mupdf::Outline;
use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

#[derive(Debug, Clone)]
pub struct TocItem {
    pub title: String,
    pub page: usize,
    pub level: usize,
}

pub enum TocAction {
    JumpTo(usize),
    Close,
}

pub struct TocDialog<F: FnMut(TocAction) -> Action> {
    items: Vec<TocItem>,
    current_page: usize,
    offset: usize,
    per_page: usize,
    dims: (i32, i32),
    on_action: F,
}

impl<F: FnMut(TocAction) -> Action> TocDialog<F> {
    pub fn from_outlines(outlines: &[Outline], current_page: usize, on_action: F) -> Self {
        let mut items = Vec::new();
        Self::flatten_outlines(outlines, 0, &mut items);

        // Find closest chapter to current page to pre-scroll
        let mut best_idx = 0;
        for (i, item) in items.iter().enumerate() {
            if item.page <= current_page {
                best_idx = i;
            } else {
                break;
            }
        }
        let initial_offset = best_idx.saturating_sub(2);

        TocDialog {
            items,
            current_page,
            offset: initial_offset,
            per_page: 8,
            dims: (1236, 1648),
            on_action,
        }
    }

    pub fn from_chapters(chapters: &[yread::model::Chapter], current_chap: usize, on_action: F) -> Self {
        let mut items = Vec::new();
        for (idx, ch) in chapters.iter().enumerate() {
            let title = if ch.title.trim().is_empty() {
                format!("Chapter {}", idx + 1)
            } else {
                ch.title.trim().to_string()
            };
            items.push(TocItem {
                title,
                page: idx,
                level: 0,
            });
        }
        let initial_offset = current_chap.saturating_sub(2);
        TocDialog {
            items,
            current_page: current_chap,
            offset: initial_offset,
            per_page: 8,
            dims: (1236, 1648),
            on_action,
        }
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

        if self.items.is_empty() {
            p.text_center(h / 2, 11.0, 0, "No Table of Contents available in this book");
            return;
        }

        // List of Chapters
        let list_top = bar_h + pt(10.0);
        let row_h = pt(42.0);
        let pad = pt(14.0);
        let content_h = h - list_top - pt(35.0);
        self.per_page = ((content_h / row_h) as usize).max(1);

        let visible = self.items.len().min(self.offset + self.per_page);
        for (i, idx) in (self.offset..visible).enumerate() {
            let item = &self.items[idx];
            let ry = list_top + i as i32 * row_h;
            let is_current = idx + 1 < self.items.len()
                && self.current_page >= item.page
                && self.current_page < self.items[idx + 1].page;

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

            let max_w = (w - text_x - pt(70.0)) as f32;
            let title = p.truncate(9.5, &item.title, max_w);

            let marker = if is_current { "● " } else { "" };
            let display_title = format!("{}{}", marker, title);
            p.text(text_x, text_y, 9.5, 0, &display_title);

            // Page number on right
            let page_str = format!("p. {}", item.page + 1);
            p.text_right(w - pad - pt(12.0), text_y, 8.5, 100, &page_str);
        }

        // Footer Pagination Info
        let footer_text = format!("{}-{} of {} chapters · swipe to scroll", self.offset + 1, visible, self.items.len());
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

                if self.items.is_empty() {
                    return (self.on_action)(TocAction::Close);
                }

                // Row tap
                let bar_h = pt(40.0);
                let list_top = bar_h + pt(10.0);
                let row_h = pt(42.0);
                let _pad = pt(14.0);

                if py >= list_top && py < list_top + self.per_page as i32 * row_h {
                    let idx = self.offset + ((py - list_top) / row_h) as usize;
                    if idx < self.items.len() {
                        let target_page = self.items[idx].page;
                        return (self.on_action)(TocAction::JumpTo(target_page));
                    }
                }

                Action::Keep
            }

            Gesture::Swipe { dir: SwipeDir::North, .. } => {
                self.offset = (self.offset + self.per_page.max(1)).min(self.items.len().saturating_sub(1));
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
