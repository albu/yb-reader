use crate::vocab::WordEntry;
use ybdev::input::Gesture;
use yui::painter::{pt, Painter, PX, Rect};
use yui::screen::{Action, Screen};

pub struct WordDialog {
    entry: WordEntry,
    is_learning: bool,
    on_action: Option<Box<dyn FnOnce(WordAction) -> Action>>,
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    chk_rect: Rect,
}

pub enum WordAction {
    StarLearning,
    #[allow(dead_code)]
    MarkKnown,
    Close,
}

impl WordDialog {
    pub fn new<F>(entry: WordEntry, is_learning: bool, bg: Option<Vec<u8>>, on_action: F) -> Self
    where
        F: FnOnce(WordAction) -> Action + 'static,
    {
        WordDialog {
            entry,
            is_learning,
            on_action: Some(Box::new(on_action)),
            bg,
            dims: (1236, 1648),
            chk_rect: Rect::new(0, 0, 0, 0),
        }
    }

    fn dispatch(&mut self, action: WordAction) -> Action {
        if let Some(cb) = self.on_action.take() {
            cb(action)
        } else {
            Action::Pop
        }
    }
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

        // Bottom docked card
        let card_w = w - pt(16.0);
        let card_x = pt(8.0);

        // Wrap text to calculate required height
        let max_text_w = (card_w - pt(28.0)) as f32;
        let mut lines = Vec::new();

        if !self.entry.gloss_en.is_empty() {
            let words: Vec<&str> = self.entry.gloss_en.split_whitespace().collect();
            let mut cur_line = String::new();
            for word in words {
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

        let ru_h = if !self.entry.gloss_ru.is_empty() {
            pt(18.0)
        } else {
            0
        };
        let body_h = (lines.len().clamp(1, 3) as i32) * pt(13.0);
        let card_h = (pt(45.0) + ru_h + body_h).clamp(pt(85.0), pt(150.0));
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Backdrop Card
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        // Header Row: Word Title + CEFR badge + Learn Checkbox Pill
        let title_y = card_y + pt(18.0);
        let title = p.truncate(13.0, &self.entry.word, (card_w - pt(120.0)) as f32 / PX);
        p.text(card_x + pt(12.0), title_y, 13.0, 0, &title);

        let badge_text = format!("{} · lvl {}", self.entry.cefr_str(), self.entry.difficulty);
        let badge_x = card_x + pt(18.0) + p.text_width(13.0, &title).round() as i32;
        p.text(badge_x, title_y - pt(1.0), 8.0, 100, &badge_text);

        // Learn Toggle Checkbox Button on top-right
        let chk_w = pt(72.0);
        let chk_h = pt(22.0);
        let chk_x = card_x + card_w - chk_w - pt(10.0);
        let chk_y = card_y + pt(6.0);
        let chk_rect = Rect::new(chk_x, chk_y, chk_w, chk_h);
        self.chk_rect = chk_rect;

        if self.is_learning {
            p.rect(chk_rect, 0);
            p.text_center_in(chk_x, chk_x + chk_w, chk_y + pt(15.0), 8.0, 255, "Learning");
        } else {
            p.rect_outline_t(chk_rect, 1, 100);
            p.text_center_in(chk_x, chk_x + chk_w, chk_y + pt(15.0), 8.0, 50, "Learn");
        }

        p.hline_t(
            title_y + pt(6.0),
            card_x + pt(10.0),
            card_x + card_w - pt(10.0),
            1,
            220,
        );

        // Body: Russian translation + English Definition lines
        let mut text_y = title_y + pt(20.0);
        if !self.entry.gloss_ru.is_empty() {
            let ru_trunc = p.truncate(10.5, &self.entry.gloss_ru, max_text_w / PX);
            p.text(card_x + pt(12.0), text_y, 10.5, 0, &ru_trunc);
            text_y += pt(15.0);
        }

        for line in &lines {
            p.text(card_x + pt(12.0), text_y, 9.0, 50, line);
            text_y += pt(13.0);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

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
