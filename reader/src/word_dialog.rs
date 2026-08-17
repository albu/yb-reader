use crate::vocab::WordEntry;
use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub struct WordDialog {
    entry: WordEntry,
    on_action: Option<Box<dyn FnOnce(WordAction) -> Action>>,
}

pub enum WordAction {
    StarLearning,
    MarkKnown,
    Close,
}

impl WordDialog {
    pub fn new<F>(entry: WordEntry, on_action: F) -> Self
    where
        F: FnOnce(WordAction) -> Action + 'static,
    {
        WordDialog {
            entry,
            on_action: Some(Box::new(on_action)),
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
        false
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();

        // Card dimensions (centered modal)
        let card_w = pt(250.0).min(w - pt(24.0));
        let card_h = pt(190.0);
        let card_x = (w - card_w) / 2;
        let card_y = (h - card_h) / 2;

        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Backdrop
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 3, 0);

        // Header: Word Title + CEFR Badge
        let title_y = card_y + pt(24.0);
        let title = p.truncate(14.0, &self.entry.word, (card_w - pt(70.0)) as f32);
        p.text(card_x + pt(14.0), title_y, 14.0, 0, &title);

        let badge_text = format!("{} (lvl {})", self.entry.cefr_str(), self.entry.difficulty);
        p.text_right(card_x + card_w - pt(14.0), title_y - pt(2.0), 8.0, 90, &badge_text);

        p.hline_t(title_y + pt(8.0), card_x + pt(12.0), card_x + card_w - pt(12.0), 1, 200);

        // Body: English Gloss
        let mut text_y = title_y + pt(26.0);
        if !self.entry.gloss_en.is_empty() {
            p.text(card_x + pt(14.0), text_y, 8.0, 110, "DEFINITION");
            text_y += pt(14.0);

            let max_chars = 38;
            let words: Vec<&str> = self.entry.gloss_en.split_whitespace().collect();
            let mut line = String::new();

            for word in words {
                if line.len() + word.len() + 1 > max_chars {
                    p.text(card_x + pt(14.0), text_y, 9.5, 0, &line);
                    text_y += pt(14.0);
                    line = word.to_string();
                } else if line.is_empty() {
                    line = word.to_string();
                } else {
                    line.push(' ');
                    line.push_str(word);
                }
            }
            if !line.is_empty() {
                p.text(card_x + pt(14.0), text_y, 9.5, 0, &line);
                text_y += pt(16.0);
            }
        }


        // Translation (if available)
        if !self.entry.gloss_tr.is_empty() {
            text_y += pt(4.0);
            p.text(card_x + pt(14.0), text_y, 8.0, 110, "TRANSLATION");
            text_y += pt(14.0);
            p.text(card_x + pt(14.0), text_y, 10.0, 0, &self.entry.gloss_tr);
        }

        // Action Buttons Row at bottom
        let btn_y = card_y + card_h - pt(38.0);
        let btn_h = pt(28.0);
        let btn_w = (card_w - pt(36.0)) / 2;

        // Button 1: [ ★ Learning ]
        let btn1_x = card_x + pt(12.0);
        let btn1_rect = Rect::new(btn1_x, btn_y, btn_w, btn_h);
        p.rect(btn1_rect, 245);
        p.rect_outline_t(btn1_rect, 1, 0);
        p.text_center(btn_y + pt(18.0), 8.5, 0, "★ Star / Learn");

        // Button 2: [ ✓ Mark Known ]
        let btn2_x = btn1_x + btn_w + pt(12.0);
        let btn2_rect = Rect::new(btn2_x, btn_y, btn_w, btn_h);
        p.rect(btn2_rect, 245);
        p.rect_outline_t(btn2_rect, 1, 0);
        p.text_center(btn_y + pt(18.0), 8.5, 0, "✓ Mark Known");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { x, y } => {
                let (w, h) = (1236, 1648); // Screen bounds
                let card_w = pt(250.0).min(w - pt(24.0));
                let card_h = pt(190.0);
                let card_x = (w - card_w) / 2;
                let card_y = (h - card_h) / 2;

                let btn_y = card_y + card_h - pt(38.0);
                let btn_h = pt(28.0);
                let btn_w = (card_w - pt(36.0)) / 2;

                let btn1_x = card_x + pt(12.0);
                let btn1_rect = Rect::new(btn1_x, btn_y, btn_w, btn_h);

                let btn2_x = btn1_x + btn_w + pt(12.0);
                let btn2_rect = Rect::new(btn2_x, btn_y, btn_w, btn_h);

                let card_rect = Rect::new(card_x, card_y, card_w, card_h);

                let px = x as i32;
                let py = y as i32;

                if btn1_rect.contains(px, py) {
                    return self.dispatch(WordAction::StarLearning);
                }
                if btn2_rect.contains(px, py) {
                    return self.dispatch(WordAction::MarkKnown);
                }

                // Tapping outside card closes it
                if !card_rect.contains(px, py) {
                    return self.dispatch(WordAction::Close);
                }

                Action::Keep
            }
            Gesture::Swipe { .. } => self.dispatch(WordAction::Close),
            _ => Action::Keep,
        }
    }
}
