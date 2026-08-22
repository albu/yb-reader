use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum FootnoteAction {
    JumpTo(usize),
    JumpToYRead {
        chapter_idx: usize,
        char_offset: usize,
        page: usize,
    },
    Close,
}

pub struct FootnoteDialog<F: FnMut(FootnoteAction) -> Action> {
    title: String,
    content: String,
    target_page: Option<usize>,
    target_yread: Option<(usize, usize, usize)>, // (chapter_idx, char_offset, page)
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    scroll_line: usize,
    max_scroll: usize,
    visible_lines: usize,
    on_action: F,
}

impl<F: FnMut(FootnoteAction) -> Action> FootnoteDialog<F> {
    pub fn new(
        title: &str,
        content: &str,
        target_page: Option<usize>,
        bg: Option<Vec<u8>>,
        on_action: F,
    ) -> Self {
        FootnoteDialog {
            title: title.to_string(),
            content: content.to_string(),
            target_page,
            target_yread: None,
            bg,
            dims: (1236, 1648),
            scroll_line: 0,
            max_scroll: 0,
            visible_lines: 6,
            on_action,
        }
    }

    pub fn new_yread(
        title: &str,
        content: &str,
        target_yread: Option<(usize, usize, usize)>,
        bg: Option<Vec<u8>>,
        on_action: F,
    ) -> Self {
        FootnoteDialog {
            title: title.to_string(),
            content: content.to_string(),
            target_page: None,
            target_yread,
            bg,
            dims: (1236, 1648),
            scroll_line: 0,
            max_scroll: 0,
            visible_lines: 6,
            on_action,
        }
    }
}

impl<F: FnMut(FootnoteAction) -> Action> Screen for FootnoteDialog<F> {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // Blit background page snapshot
        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
        } else {
            p.clear(255);
        }

        let card_w = w - pt(16.0);
        let card_x = pt(8.0);
        let max_text_w = (card_w - pt(28.0)) as f32;

        // Wrap footnote content lines (respecting newlines in content)
        let mut lines = Vec::new();
        for para in self.content.lines() {
            let p_trimmed = para.trim();
            if p_trimmed.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut cur_line = String::new();
            for word in p_trimmed.split_whitespace() {
                let test = if cur_line.is_empty() {
                    word.to_string()
                } else {
                    format!("{} {}", cur_line, word)
                };
                if p.text_width(9.5, &test) > max_text_w {
                    if !cur_line.is_empty() {
                        lines.push(cur_line);
                    }
                    cur_line = word.to_string();
                } else {
                    cur_line = test;
                }
            }
            if !cur_line.is_empty() {
                lines.push(cur_line);
            }
        }
        if lines.is_empty() {
            lines.push(self.content.clone());
        }

        let line_height = pt(14.0);
        let header_h = pt(30.0);
        let card_h = if lines.len() <= 3 {
            (pt(50.0) + (lines.len() as i32) * line_height).clamp(pt(90.0), pt(140.0))
        } else {
            pt(180.0).min(h / 2)
        };
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Available lines inside card body
        let body_top = card_y + header_h;
        let body_bottom = card_y + card_h - pt(8.0);
        let vis_count = ((body_bottom - body_top) / line_height).max(1) as usize;
        self.visible_lines = vis_count;
        self.max_scroll = lines.len().saturating_sub(vis_count);
        self.scroll_line = self.scroll_line.min(self.max_scroll);

        // Backdrop Card
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        // Title row
        let title_y = card_y + pt(18.0);
        let title = p.truncate(11.5, &self.title, (card_w - pt(120.0)) as f32);
        p.text(card_x + pt(12.0), title_y, 11.5, 0, &title);

        // Scroll indicator if scrollable
        if lines.len() > vis_count {
            let indicator = format!("{}-{}/{}", self.scroll_line + 1, (self.scroll_line + vis_count).min(lines.len()), lines.len());
            let title_w = p.text_width(11.5, &title).round() as i32;
            let ind_x = card_x + pt(18.0) + title_w;
            if ind_x < card_x + card_w - pt(80.0) {
                p.text(ind_x, title_y - pt(1.0), 8.0, 120, &indicator);
            }
        }

        // Action button on top-right: [ ↗ Jump ] if target exists, else [ ✕ Close ]
        let btn_w = pt(65.0);
        let btn_h = pt(22.0);
        let btn_x = card_x + card_w - btn_w - pt(10.0);
        let btn_y = card_y + pt(6.0);
        let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);

        if let Some((_, _, page)) = self.target_yread {
            p.rect(btn_rect, 0);
            let jump_lbl = format!("↗ p.{}", page + 1);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(15.0), 8.0, 255, &jump_lbl);
        } else if let Some(target) = self.target_page {
            p.rect(btn_rect, 0);
            let jump_lbl = format!("↗ p.{}", target + 1);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(15.0), 8.0, 255, &jump_lbl);
        } else {
            p.rect_outline_t(btn_rect, 1, 100);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(15.0), 8.0, 50, "Close");
        }

        p.hline_t(title_y + pt(6.0), card_x + pt(10.0), card_x + card_w - pt(10.0), 1, 220);

        // Body: Content lines with scroll offset
        let mut text_y = body_top + pt(6.0);
        let end_idx = (self.scroll_line + vis_count).min(lines.len());
        for line in &lines[self.scroll_line..end_idx] {
            p.text(card_x + pt(12.0), text_y, 9.5, 0, line);
            text_y += line_height;
        }

        // Scrollbar on right edge if content exceeds visible area
        if lines.len() > vis_count {
            let track_x = card_x + card_w - pt(8.0);
            let track_y = body_top + pt(2.0);
            let track_h = body_bottom - body_top;
            let track_w = pt(3.0);

            // Track background
            p.rect(Rect::new(track_x, track_y, track_w, track_h), 230);

            // Thumb
            let thumb_h = ((vis_count as f32 / lines.len() as f32) * (track_h as f32)).clamp(pt(16.0) as f32, track_h as f32) as i32;
            let max_thumb_travel = (track_h - thumb_h).max(1);
            let thumb_y = track_y + (self.scroll_line as f32 / self.max_scroll.max(1) as f32 * max_thumb_travel as f32) as i32;
            p.rect(Rect::new(track_x, thumb_y, track_w, thumb_h), 60);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let card_w = w - pt(16.0);
        let card_x = pt(8.0);
        let card_h = pt(180.0).min(h / 2);
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Tap outside dismisses
                if !card_rect.contains(px, py) {
                    return (self.on_action)(FootnoteAction::Close);
                }

                // Top right button tap
                let btn_w = pt(65.0);
                let btn_h = pt(22.0);
                let btn_x = card_x + card_w - btn_w - pt(10.0);
                let btn_y = card_y + pt(6.0);
                let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);

                if btn_rect.contains(px, py) {
                    if let Some((ch, off, page)) = self.target_yread {
                        return (self.on_action)(FootnoteAction::JumpToYRead {
                            chapter_idx: ch,
                            char_offset: off,
                            page,
                        });
                    } else if let Some(target) = self.target_page {
                        return (self.on_action)(FootnoteAction::JumpTo(target));
                    } else {
                        return (self.on_action)(FootnoteAction::Close);
                    }
                }

                // Tap body top half / bottom half for scrolling
                let body_mid_y = card_y + (card_h / 2);
                if py < body_mid_y {
                    if self.scroll_line > 0 {
                        let step = self.visible_lines.saturating_sub(1).max(1);
                        self.scroll_line = self.scroll_line.saturating_sub(step);
                        return Action::Redraw;
                    }
                } else if self.scroll_line < self.max_scroll {
                    let step = self.visible_lines.saturating_sub(1).max(1);
                    self.scroll_line = (self.scroll_line + step).min(self.max_scroll);
                    return Action::Redraw;
                }

                Action::Keep
            }
            Gesture::Swipe { dir, .. } => {
                match dir {
                    SwipeDir::North => {
                        // Swipe up -> scroll down (reveal later text)
                        if self.scroll_line < self.max_scroll {
                            let step = self.visible_lines.saturating_sub(1).max(1);
                            self.scroll_line = (self.scroll_line + step).min(self.max_scroll);
                            Action::Redraw
                        } else {
                            Action::Keep
                        }
                    }
                    SwipeDir::South => {
                        // Swipe down -> scroll up (reveal earlier text) or close if at top
                        if self.scroll_line > 0 {
                            let step = self.visible_lines.saturating_sub(1).max(1);
                            self.scroll_line = self.scroll_line.saturating_sub(step);
                            Action::Redraw
                        } else {
                            (self.on_action)(FootnoteAction::Close)
                        }
                    }
                    SwipeDir::East | SwipeDir::West => {
                        (self.on_action)(FootnoteAction::Close)
                    }
                }
            }
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_footnote_dialog_creation_and_scrolling() {
        let long_content = (1..=50)
            .map(|i| format!("Footnote line number {}.", i))
            .collect::<Vec<_>>()
            .join(" ");

        let mut dialog = FootnoteDialog::new_yread(
            "Note [1]",
            &long_content,
            Some((2, 100, 15)),
            None,
            |_| Action::Pop,
        );

        dialog.dims = (1236, 1648);
        dialog.visible_lines = 5;
        dialog.max_scroll = 20;

        assert_eq!(dialog.scroll_line, 0);

        // Swipe North (up) -> scroll down
        let act = dialog.on_gesture(Gesture::Swipe {
            dir: SwipeDir::North,
            x: 500,
            y: 1500,
            ex: 500,
            ey: 1300,
        });
        assert!(matches!(act, Action::Redraw));
        assert!(dialog.scroll_line > 0);

        // Swipe South (down) -> scroll up
        let act = dialog.on_gesture(Gesture::Swipe {
            dir: SwipeDir::South,
            x: 500,
            y: 1300,
            ex: 500,
            ey: 1500,
        });
        assert!(matches!(act, Action::Redraw));
        assert_eq!(dialog.scroll_line, 0);

        // Swipe South when at top -> closes dialog
        let act = dialog.on_gesture(Gesture::Swipe {
            dir: SwipeDir::South,
            x: 500,
            y: 1300,
            ex: 500,
            ey: 1500,
        });
        assert!(matches!(act, Action::Pop));

        // Tap outside -> closes dialog
        let act = dialog.on_gesture(Gesture::Tap { x: 500, y: 100 });
        assert!(matches!(act, Action::Pop));
    }
}
