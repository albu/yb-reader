use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum FootnoteAction {
    JumpTo(usize),
    Close,
}

pub struct FootnoteDialog<F: FnMut(FootnoteAction) -> Action> {
    title: String,
    content: String,
    target_page: Option<usize>,
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
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
            bg,
            dims: (1236, 1648),
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
        let max_text_w = (card_w - pt(24.0)) as f32;

        // Wrap footnote content lines
        let mut lines = Vec::new();
        let mut cur_line = String::new();
        for word in self.content.split_whitespace() {
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

        let body_h = (lines.len().clamp(1, 4) as i32) * pt(14.0);
        let card_h = (pt(52.0) + body_h).clamp(pt(95.0), pt(160.0));
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Backdrop Card
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        // Title row
        let title_y = card_y + pt(18.0);
        let title = p.truncate(11.5, &self.title, (card_w - pt(100.0)) as f32);
        p.text(card_x + pt(12.0), title_y, 11.5, 0, &title);

        // Action button on top-right: [ ↗ Jump ] if target_page exists, else [ ✕ Close ]
        let btn_w = pt(65.0);
        let btn_h = pt(22.0);
        let btn_x = card_x + card_w - btn_w - pt(10.0);
        let btn_y = card_y + pt(6.0);
        let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);

        if let Some(target) = self.target_page {
            p.rect(btn_rect, 0);
            let jump_lbl = format!("↗ p.{}", target + 1);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(15.0), 8.0, 255, &jump_lbl);
        } else {
            p.rect_outline_t(btn_rect, 1, 100);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(15.0), 8.0, 50, "✕ Close");
        }

        p.hline_t(title_y + pt(6.0), card_x + pt(10.0), card_x + card_w - pt(10.0), 1, 220);

        // Body: Content lines
        let mut text_y = title_y + pt(20.0);
        for line in &lines {
            p.text(card_x + pt(12.0), text_y, 9.5, 0, line);
            text_y += pt(14.0);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let card_w = w - pt(16.0);
        let card_h = pt(120.0);
        let card_x = pt(8.0);
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
                    if let Some(target) = self.target_page {
                        return (self.on_action)(FootnoteAction::JumpTo(target));
                    } else {
                        return (self.on_action)(FootnoteAction::Close);
                    }
                }

                Action::Keep
            }
            Gesture::Swipe { .. } => (self.on_action)(FootnoteAction::Close),
            _ => Action::Keep,
        }
    }
}
