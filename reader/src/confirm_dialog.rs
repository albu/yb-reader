//! Destructive-action confirm: a floating card centered over a dimmed
//! snapshot of the screen behind it (Windows-style popup, not a screen
//! replacement). Tap outside (or any swipe) cancels; the confirm button
//! is the only path to [`ConfirmAction::Yes`]. Generic on the callback
//! like the other dialogs — home uses it for library deletes.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum ConfirmAction {
    Yes,
    No,
}

pub struct ConfirmDialog<F: FnMut(ConfirmAction) -> Action> {
    title: String,
    /// May contain '\n' for a second line (e.g. the filename).
    message: String,
    confirm_label: String,
    /// Snapshot of the screen we float over; None falls back to white.
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    on_action: F,
}

impl<F: FnMut(ConfirmAction) -> Action> ConfirmDialog<F> {
    pub fn new(
        title: &str,
        message: &str,
        confirm_label: &str,
        bg: Option<Vec<u8>>,
        on_action: F,
    ) -> Self {
        ConfirmDialog {
            title: title.to_string(),
            message: message.to_string(),
            confirm_label: confirm_label.to_string(),
            bg,
            dims: (1236, 1648),
            on_action,
        }
    }

    fn card_rect(&self) -> Rect {
        let (w, h) = self.dims;
        let card_w = w - 2 * pt(60.0);
        let card_h = pt(170.0);
        Rect::new(pt(60.0), (h - card_h) / 2, card_w, card_h)
    }

    fn buttons(&self) -> (Rect, Rect) {
        let card = self.card_rect();
        let (w, _) = self.dims;
        let btn_h = pt(36.0);
        let btn_w = (card.w - pt(48.0) - pt(16.0)) / 2;
        let y = card.y + card.h - btn_h - pt(20.0);
        let cx = (w - card.w) / 2 + pt(24.0);
        (
            Rect::new(cx, y, btn_w, btn_h),
            Rect::new(cx + btn_w + pt(16.0), y, btn_w, btn_h),
        )
    }
}

impl<F: FnMut(ConfirmAction) -> Action> Screen for ConfirmDialog<F> {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);

        // The screen behind, dimmed by scanlines — the popup pattern the
        // reader dialogs use, so the list stays visibly in place.
        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
        } else {
            p.clear(255);
        }
        for y in (0..h).step_by(3) {
            p.hline_t(y, 0, w, 1, 235);
        }

        let card = self.card_rect();
        // Drop shadow first so the card reads as floating above the dim.
        p.rect(
            Rect::new(card.x + pt(4.0), card.y + pt(4.0), card.w, card.h),
            205,
        );
        p.rect(card, 255);
        p.rect_outline_t(card, 2, 0);

        p.text_center(card.y + pt(34.0), 11.0, 0, &self.title);
        for (i, line) in self.message.lines().enumerate() {
            let trunc = p.truncate(9.0, line, p.width_pt() - 2.0 * 60.0 - 12.0);
            p.text_center(card.y + pt(62.0) + i as i32 * pt(16.0), 9.0, 130, &trunc);
        }

        let (cancel, confirm) = self.buttons();
        p.rect_outline_t(cancel, 2, 100);
        p.text_center_in(
            cancel.x,
            cancel.x + cancel.w,
            cancel.y + pt(23.0),
            9.5,
            50,
            "Cancel",
        );
        p.rect(confirm, 0);
        p.text_center_in(
            confirm.x,
            confirm.x + confirm.w,
            confirm.y + pt(23.0),
            9.5,
            255,
            &self.confirm_label,
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (px, py) = match g {
            Gesture::Tap { x, y } => (x as i32, y as i32),
            Gesture::Swipe { .. } => return (self.on_action)(ConfirmAction::No),
            _ => return Action::Keep,
        };

        let (cancel, confirm) = self.buttons();
        if cancel.contains(px, py) {
            return (self.on_action)(ConfirmAction::No);
        }
        if confirm.contains(px, py) {
            return (self.on_action)(ConfirmAction::Yes);
        }
        // Anywhere else on the card is a no-op; outside dismisses as cancel.
        if !self.card_rect().contains(px, py) {
            return (self.on_action)(ConfirmAction::No);
        }
        Action::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> ConfirmDialog<fn(ConfirmAction) -> Action> {
        fn noop(_: ConfirmAction) -> Action {
            Action::Keep
        }
        ConfirmDialog::new("t", "m", "Delete", None, noop)
    }

    #[test]
    fn card_is_centered_and_buttons_live_inside_it() {
        let d = dialog();
        let card = d.card_rect();
        assert_eq!(card.x, (1236 - card.w) / 2);
        assert_eq!(card.y, (1648 - card.h) / 2);
        let (cancel, confirm) = d.buttons();
        for b in [cancel, confirm] {
            assert!(b.x >= card.x && b.x + b.w <= card.x + card.w);
            assert!(b.y >= card.y && b.y + b.h <= card.y + card.h);
            assert!(cancel.x + cancel.w < confirm.x);
        }
    }
}
