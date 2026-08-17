//! The generic screens every yb-* app needs: a launcher menu and a
//! tap-to-dismiss message overlay. Geometry is ported verbatim from the
//! ad-hoc ui.rs screens (pt-authored).

use ybdev::input::{Gesture, SwipeDir};

use crate::painter::{pt, Painter};
use crate::screen::{Action, Screen};

/// --- MenuScreen layout (pt): ported from ui.rs::menu_loop ---
const TITLE_BASE_PT: f32 = 32.0;
const TITLE_SIZE_PT: f32 = 15.0;
const ITEM_SIZE_PT: f32 = 11.5;
const ITEM_BASE_OFF_PT: f32 = 8.0;
const TOP_PT: f32 = 54.0;
const ITEM_H_PT: f32 = 26.0;
const FOOTER_BASE_OFF_PT: f32 = 30.0;
const FOOTER_SIZE_PT: f32 = 7.0;

pub struct MenuScreen {
    title: String,
    items: Vec<String>,
    on_select: Box<dyn FnMut(usize) -> Action>,
}

impl MenuScreen {
    /// `on_select` maps the tapped item index to the next action.
    pub fn new(
        title: &str,
        items: &[&str],
        on_select: impl FnMut(usize) -> Action + 'static,
    ) -> MenuScreen {
        MenuScreen {
            title: title.to_string(),
            items: items.iter().map(|s| s.to_string()).collect(),
            on_select: Box::new(on_select),
        }
    }

    /// Pure hit-test (px in, item index out) — shared by draw and tests.
    pub fn hit(y_px: i32, n_items: usize) -> Option<usize> {
        let top = pt(TOP_PT);
        let item_h = pt(ITEM_H_PT);
        if y_px < top || y_px >= top + n_items as i32 * item_h {
            return None;
        }
        let idx = ((y_px - top) / item_h) as usize;
        (idx < n_items).then_some(idx)
    }
}

impl Screen for MenuScreen {
    fn draw(&mut self, p: &mut Painter) {
        let h = p.size().1;
        p.clear(255);
        p.text_center(pt(TITLE_BASE_PT), TITLE_SIZE_PT, 0, &self.title);
        for (i, item) in self.items.iter().enumerate() {
            let y = pt(TOP_PT) + i as i32 * pt(ITEM_H_PT) + pt(ITEM_BASE_OFF_PT);
            p.text_center(y, ITEM_SIZE_PT, 0, item);
        }
        p.text_center(
            h - pt(FOOTER_BASE_OFF_PT),
            FOOTER_SIZE_PT,
            120,
            "swipe down: exit · swipe from top: brightness",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { y, .. } => match MenuScreen::hit(y as i32, self.items.len()) {
                Some(idx) => (self.on_select)(idx),
                None => Action::Keep,
            },
            // Vertical swipe on the root = leave the app (the old
            // menu_loop returned None on the same gestures).
            Gesture::Swipe { dir: SwipeDir::North, .. }
            | Gesture::Swipe { dir: SwipeDir::South, .. } => Action::Quit,
            Gesture::Swipe { .. } => Action::Keep,
            Gesture::TwoFingerTap => Action::Keep,
        }
    }
}

/// --- MessageScreen layout (pt): ported from ui.rs::message ---
const MSG_SIZE_PT: f32 = 10.0;
const MSG_LINE_PT: f32 = 14.0;
const MSG_FIRST_OFF_PT: f32 = 10.0;

/// Fullscreen message overlay; any gesture dismisses it.
pub struct MessageScreen {
    lines: Vec<String>,
}

impl MessageScreen {
    pub fn new(lines: &[&str]) -> MessageScreen {
        MessageScreen {
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn from_strings(lines: Vec<String>) -> MessageScreen {
        MessageScreen { lines }
    }
}

impl Screen for MessageScreen {
    fn draw(&mut self, p: &mut Painter) {
        let h = p.size().1;
        p.clear(255);
        let line_h = pt(MSG_LINE_PT);
        let total = self.lines.len() as i32 * line_h;
        let mut y = (h - total) / 2 + pt(MSG_FIRST_OFF_PT);
        for l in &self.lines {
            p.text_center(y, MSG_SIZE_PT, 0, l);
            y += line_h;
        }
    }

    fn on_gesture(&mut self, _g: Gesture) -> Action {
        Action::Pop
    }

    fn default_edges(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_hit_maps_rows_and_rejects_outside() {
        let top = pt(TOP_PT);
        let item_h = pt(ITEM_H_PT);
        // First row: anywhere from top to top+item_h.
        assert_eq!(MenuScreen::hit(top + 1, 5), Some(0));
        assert_eq!(MenuScreen::hit(top + item_h - 1, 5), Some(0));
        // Boundary lands in the second row.
        assert_eq!(MenuScreen::hit(top + item_h, 5), Some(1));
        assert_eq!(MenuScreen::hit(top + 4 * item_h + 5, 5), Some(4));
        // Above the list (title area) and below it: nothing.
        assert_eq!(MenuScreen::hit(top - 1, 5), None);
        assert_eq!(MenuScreen::hit(top + 5 * item_h, 5), None);
        // Trailing rows of a short menu don't exist.
        assert_eq!(MenuScreen::hit(top + 2 * item_h, 2), None);
    }

    #[test]
    fn menu_layout_uses_real_rows() {
        // 26pt at 300dpi = 108.33px -> 108. 54pt = 225.0 mathematically,
        // but PX (300/72) isn't exact in binary, so the product truncates
        // to 224 — same value the old ad-hoc screens computed. Pin both.
        assert_eq!(pt(26.0), 108);
        assert_eq!(pt(54.0), 224);
    }
}
