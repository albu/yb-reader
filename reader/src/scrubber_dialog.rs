use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub enum ScrubberAction {
    Done(usize),
    OpenToc(usize),
    OpenHighlights(usize),
}

pub struct ScrubberDialog<F: FnMut(ScrubberAction) -> Action> {
    target_page: usize,
    total_pages: usize,
    bg: Option<Vec<u8>>,
    dims: (i32, i32),
    renderer: Box<dyn Fn(usize) -> Option<Vec<u8>>>,
    on_action: F,
}

// ---- geometry ------------------------------------------------------------
//
// One source of truth for the card layout, shared by draw and hit-testing.
// These values used to live twice and had drifted 4-12pt apart (track
// painted at +74 but hit at +62), masked by generous hit padding.

/// The docked bottom card.
pub fn card_rect(w: i32, h: i32) -> Rect {
    let card_w = w - pt(16.0);
    let card_h = pt(145.0);
    Rect::new(pt(8.0), h - card_h - pt(10.0), card_w, card_h)
}

/// Row 1: the [ TOC ] and [ Highlights ] buttons.
fn row1_rects(card: Rect) -> (Rect, Rect) {
    let y = card.y + pt(10.0);
    let h = pt(26.0);
    let btn_w = (card.w - pt(42.0)) / 2;
    let toc = Rect::new(card.x + pt(14.0), y, btn_w, h);
    let hl = Rect::new(toc.x + btn_w + pt(14.0), y, btn_w, h);
    (toc, hl)
}

/// Row 3: the slider track as painted.
fn track_rect(card: Rect) -> Rect {
    Rect::new(
        card.x + pt(24.0),
        card.y + pt(74.0),
        card.w - pt(48.0),
        pt(6.0),
    )
}

/// Row 4: the four step buttons [-10] [-1] [+1] [+10].
fn step_rects(card: Rect) -> Vec<Rect> {
    let y = card.y + pt(98.0);
    let h = pt(32.0);
    let w = (card.w - pt(48.0) - pt(24.0)) / 4;
    (0..4)
        .map(|i| Rect::new(card.x + pt(24.0) + i as i32 * (w + pt(8.0)), y, w, h))
        .collect()
}

/// Grow a rect by (dx, dy) on each side — hit padding around painted
/// controls, so a tap on a visual edge still lands.
fn inflate(r: Rect, dx: i32, dy: i32) -> Rect {
    Rect::new(r.x - dx, r.y - dy, r.w + 2 * dx, r.h + 2 * dy)
}

// --------------------------------------------------------------------------

impl<F: FnMut(ScrubberAction) -> Action> ScrubberDialog<F> {
    pub fn new(
        current_page: usize,
        total_pages: usize,
        bg: Option<Vec<u8>>,
        renderer: impl Fn(usize) -> Option<Vec<u8>> + 'static,
        on_action: F,
    ) -> Self {
        ScrubberDialog {
            target_page: current_page,
            total_pages: total_pages.max(1),
            bg,
            dims: (1236, 1648),
            renderer: Box::new(renderer),
            on_action,
        }
    }

    fn set_page(&mut self, page: usize) {
        let new_page = page.min(self.total_pages.saturating_sub(1));
        if new_page != self.target_page || self.bg.is_none() {
            self.target_page = new_page;
            if let Some(new_bg) = (self.renderer)(new_page) {
                self.bg = Some(new_bg);
            }
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

        // Blit live background page preview underneath
        if let Some(bg) = &self.bg {
            p.blit_gray(0, 0, w, h, bg, w as usize);
            // Subtle dimming lines over upper screen
            let dim_h = h - pt(160.0);
            for y in (0..dim_h).step_by(3) {
                p.hline_t(y, 0, w, 1, 235);
            }
        } else {
            p.clear(255);
        }

        let card = card_rect(w, h);

        // Docked bottom scrubber card
        p.rect(card, 255);
        p.rect_outline_t(card, 2, 0);

        // Row 1: [ TOC ] and [ Highlights ] — the card is the
        // reader's navigation hub. (No ✕ Done button: tapping outside the
        // card or any swipe already commits the target page.)
        let (toc, hl) = row1_rects(card);
        p.rect_outline_t(toc, 1, 100);
        p.text_center_in(toc.x, toc.x + toc.w, toc.y + pt(17.0), 8.5, 0, "TOC");
        p.rect_outline_t(hl, 1, 100);
        p.text_center_in(hl.x, hl.x + hl.w, hl.y + pt(17.0), 8.5, 0, "Highlights");

        // Row 2: Page Progress Text
        let pct = (self.target_page + 1) * 100 / self.total_pages;
        let prog_text = format!(
            "Page {} of {}  ({}%)",
            self.target_page + 1,
            self.total_pages,
            pct
        );
        p.text_center(card.y + pt(52.0), 11.5, 0, &prog_text);

        // Row 3: Interactive Slider Track
        let track = track_rect(card);
        p.rect(track, 220);
        p.rect_outline_t(track, 1, 100);

        // Slider Thumb
        let frac = if self.total_pages > 1 {
            (self.target_page as f32) / ((self.total_pages - 1) as f32)
        } else {
            0.0
        };
        let thumb_x = track.x + (frac * track.w as f32) as i32;
        let thumb_w = pt(18.0);
        let thumb_h = pt(18.0);
        let thumb_rect = Rect::new(
            thumb_x - thumb_w / 2,
            track.y + track.h / 2 - thumb_h / 2,
            thumb_w,
            thumb_h,
        );
        p.rect(thumb_rect, 0);

        // Row 4: 4 Instant Step Buttons [-10] [-1] [+1] [+10]
        let steps = step_rects(card);
        let labels = ["-10", "-1", "+1", "+10"];
        for (brect, label) in steps.iter().zip(labels) {
            p.rect_outline_t(*brect, 1, 120);
            p.text_center_in(
                brect.x,
                brect.x + brect.w,
                brect.y + pt(20.0),
                9.5,
                0,
                label,
            );
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;
        let card = card_rect(w, h);

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Tapping outside card closes scrubber directly on current page
                if !card.contains(px, py) {
                    return (self.on_action)(ScrubberAction::Done(self.target_page));
                }

                // Row 1: [ TOC ] / [ Highlights ] taps
                let (toc, hl) = row1_rects(card);
                if inflate(toc, 4, 4).contains(px, py) {
                    return (self.on_action)(ScrubberAction::OpenToc(self.target_page));
                }
                if inflate(hl, 4, 4).contains(px, py) {
                    return (self.on_action)(ScrubberAction::OpenHighlights(self.target_page));
                }

                // Row 3: Slider track tap (padded to cover the thumb)
                let track = track_rect(card);
                if inflate(track, 16, 13).contains(px, py) {
                    let rel_x = (px - track.x).clamp(0, track.w);
                    let frac = rel_x as f32 / track.w as f32;
                    let target = ((frac * (self.total_pages - 1) as f32).round() as usize)
                        .min(self.total_pages.saturating_sub(1));
                    self.set_page(target);
                    return Action::Redraw;
                }

                // Row 4: 4 Jump Buttons [-10] [-1] [+1] [+10]
                let deltas = [-10, -1, 1, 10];
                for (brect, delta) in step_rects(card).iter().zip(deltas) {
                    if inflate(*brect, 2, 5).contains(px, py) {
                        let new_page = (self.target_page as i32 + delta)
                            .clamp(0, (self.total_pages as i32).saturating_sub(1))
                            as usize;
                        self.set_page(new_page);
                        return Action::Redraw;
                    }
                }

                Action::Keep
            }
            Gesture::Swipe {
                dir: SwipeDir::East,
                ..
            } => {
                // Swipe right -> +10 pages
                let new_page = (self.target_page + 10).min(self.total_pages.saturating_sub(1));
                self.set_page(new_page);
                Action::Redraw
            }
            Gesture::Swipe {
                dir: SwipeDir::West,
                ..
            } => {
                // Swipe left -> -10 pages
                let new_page = self.target_page.saturating_sub(10);
                self.set_page(new_page);
                Action::Redraw
            }
            Gesture::Swipe { .. } => (self.on_action)(ScrubberAction::Done(self.target_page)),
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every painted control's center must hit-test to that control —
    /// the invariant the old duplicated constants broke.
    #[test]
    fn painted_centers_hit_their_controls() {
        let (w, h) = (1236, 1648);
        let card = card_rect(w, h);
        let center = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);

        let (toc, hl) = row1_rects(card);
        assert!(inflate(toc, 4, 4).contains(center(toc).0, center(toc).1));
        assert!(inflate(hl, 4, 4).contains(center(hl).0, center(hl).1));
        // The two buttons don't leak into each other.
        assert!(!inflate(toc, 4, 4).contains(center(hl).0, center(hl).1));

        let track = track_rect(card);
        assert!(inflate(track, 16, 13).contains(center(track).0, center(track).1));

        for brect in step_rects(card) {
            let (cx, cy) = center(brect);
            assert!(inflate(brect, 2, 5).contains(cx, cy));
            // A step button must not reach the track above it.
            assert!(!inflate(track, 16, 13).contains(cx, cy));
        }

        // The card sits in the lower part of the panel, rows stack in order.
        assert!(card.y > h / 2);
        let (toc, _) = row1_rects(card);
        let track = track_rect(card);
        let steps = step_rects(card);
        assert!(toc.y < track.y && track.y < steps[0].y);
    }
}
