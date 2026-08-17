use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum ScrubberAction {
    JumpTo(usize),
    Close,
}

pub struct ScrubberDialog<F: FnMut(ScrubberAction) -> Action> {
    _current_page: usize,
    target_page: usize,
    total_pages: usize,
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    on_action: F,
}

impl<F: FnMut(ScrubberAction) -> Action> ScrubberDialog<F> {
    pub fn new(current_page: usize, total_pages: usize, bg: Option<Vec<u8>>, on_action: F) -> Self {
        ScrubberDialog {
            _current_page: current_page,
            target_page: current_page,
            total_pages: total_pages.max(1),
            bg,
            dims: (1236, 1648),
            on_action,
        }
    }
}


impl<F: FnMut(ScrubberAction) -> Action> Screen for ScrubberDialog<F> {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // Blit background page snapshot for zero-flash partial popup
        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
            // Slight dimming overlay over upper screen

            let dim_h = h - pt(160.0);
            for y in (0..dim_h).step_by(2) {
                p.hline_t(y, 0, w, 1, 230);
            }
        } else {
            p.clear(255);
        }

        let card_w = w - pt(16.0);
        let card_h = pt(145.0);
        let card_x = pt(8.0);
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Docked bottom card
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        // Header: Page progress
        let pct = (self.target_page + 1) * 100 / self.total_pages;
        let prog_text = format!("Page {} of {}  ({}%)", self.target_page + 1, self.total_pages, pct);
        p.text_center(card_y + pt(22.0), 12.0, 0, &prog_text);

        // Interactive Slider Track
        let track_x = card_x + pt(24.0);
        let track_w = card_w - pt(48.0);
        let track_y = card_y + pt(48.0);
        let track_h = pt(6.0);

        p.rect(Rect::new(track_x, track_y, track_w, track_h), 220);
        p.rect_outline_t(Rect::new(track_x, track_y, track_w, track_h), 1, 100);

        // Slider Thumb
        let frac = if self.total_pages > 1 {
            (self.target_page as f32) / ((self.total_pages - 1) as f32)
        } else {
            0.0
        };
        let thumb_x = track_x + (frac * track_w as f32) as i32;
        let thumb_w = pt(16.0);
        let thumb_h = pt(16.0);
        let thumb_rect = Rect::new(thumb_x - thumb_w / 2, track_y + track_h / 2 - thumb_h / 2, thumb_w, thumb_h);
        p.rect(thumb_rect, 0);

        // 4 Jump Buttons: [-10] [-1] [+1] [+10]
        let btn_y = card_y + pt(68.0);
        let btn_h = pt(28.0);
        let step_w = (card_w - pt(48.0) - pt(24.0)) / 4;

        let steps = [("-10", -10), ("-1", -1), ("+1", 1), ("+10", 10)];
        for (i, (label, _)) in steps.iter().enumerate() {
            let bx = card_x + pt(24.0) + i as i32 * (step_w + pt(8.0));
            let brect = Rect::new(bx, btn_y, step_w, btn_h);
            p.rect_outline_t(brect, 1, 120);
            p.text_center_in(bx, bx + step_w, btn_y + pt(18.0), 9.0, 0, label);
        }

        // Action Row: [ Cancel ] and [ Jump to Page ]
        let act_y = card_y + pt(105.0);
        let act_h = pt(30.0);
        let act_w = (card_w - pt(48.0) - pt(12.0)) / 2;

        let cancel_x = card_x + pt(24.0);
        let cancel_rect = Rect::new(cancel_x, act_y, act_w, act_h);
        p.rect_outline_t(cancel_rect, 1, 100);
        p.text_center_in(cancel_x, cancel_x + act_w, act_y + pt(19.0), 9.5, 50, "Cancel");

        let jump_x = cancel_x + act_w + pt(12.0);
        let jump_rect = Rect::new(jump_x, act_y, act_w, act_h);
        p.rect(jump_rect, 0);
        p.text_center_in(jump_x, jump_x + act_w, act_y + pt(19.0), 9.5, 255, "Jump to Page");
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let card_w = w - pt(16.0);
        let card_h = pt(145.0);
        let card_x = pt(8.0);
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Tapping outside dismisses
                if !card_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::Close);
                }

                // Slider track tap
                let track_x = card_x + pt(24.0);
                let track_w = card_w - pt(48.0);
                let track_y = card_y + pt(34.0);
                let track_rect = Rect::new(track_x - pt(15.0), track_y, track_w + pt(30.0), pt(32.0));

                if track_rect.contains(px, py) {
                    let rel_x = (px - track_x).clamp(0, track_w);
                    let frac = rel_x as f32 / track_w as f32;
                    self.target_page = ((frac * (self.total_pages - 1) as f32).round() as usize)
                        .min(self.total_pages.saturating_sub(1));
                    return Action::Redraw;
                }

                // 4 Jump Buttons: [-10] [-1] [+1] [+10]
                let btn_y = card_y + pt(64.0);
                let btn_h = pt(34.0);
                let step_w = (card_w - pt(48.0) - pt(24.0)) / 4;
                let steps = [("-10", -10), ("-1", -1), ("+1", 1), ("+10", 10)];

                for (i, (_, delta)) in steps.iter().enumerate() {
                    let bx = card_x + pt(24.0) + i as i32 * (step_w + pt(8.0));
                    let brect = Rect::new(bx - pt(2.0), btn_y, step_w + pt(4.0), btn_h);
                    if brect.contains(px, py) {
                        let new_page = (self.target_page as i32 + delta)
                            .clamp(0, (self.total_pages as i32).saturating_sub(1)) as usize;
                        self.target_page = new_page;
                        return Action::Redraw;
                    }
                }

                // Action Row: Cancel and Jump
                let act_y = card_y + pt(102.0);
                let act_h = pt(36.0);
                let act_w = (card_w - pt(48.0) - pt(12.0)) / 2;

                let cancel_x = card_x + pt(24.0);
                let cancel_rect = Rect::new(cancel_x, act_y, act_w, act_h);
                if cancel_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::Close);
                }

                let jump_x = cancel_x + act_w + pt(12.0);
                let jump_rect = Rect::new(jump_x, act_y, act_w, act_h);
                if jump_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::JumpTo(self.target_page));
                }

                Action::Keep
            }
            Gesture::Swipe { .. } => (self.on_action)(ScrubberAction::Close),
            _ => Action::Keep,
        }
    }

}
