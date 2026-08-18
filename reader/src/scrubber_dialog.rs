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

        let card_w = w - pt(16.0);
        let card_h = pt(145.0);
        let card_x = pt(8.0);
        let card_y = h - card_h - pt(10.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        // Docked bottom scrubber card
        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        // Row 1: [ TOC ] and [ Highlights ] — the card is the
        // reader's navigation hub. (No ✕ Done button: tapping outside the
        // card or any swipe already commits the target page.)
        let row1_y = card_y + pt(10.0);
        let row1_h = pt(26.0);
        let btn_w = (card_w - pt(42.0)) / 2;
        let toc_x = card_x + pt(14.0);
        p.rect_outline_t(Rect::new(toc_x, row1_y, btn_w, row1_h), 1, 100);
        p.text_center_in(toc_x, toc_x + btn_w, row1_y + pt(17.0), 8.5, 0, "TOC");

        let hl_x = toc_x + btn_w + pt(14.0);
        p.rect_outline_t(Rect::new(hl_x, row1_y, btn_w, row1_h), 1, 100);
        p.text_center_in(hl_x, hl_x + btn_w, row1_y + pt(17.0), 8.5, 0, "Highlights");

        // Row 2: Page Progress Text
        let pct = (self.target_page + 1) * 100 / self.total_pages;
        let prog_text = format!("Page {} of {}  ({}%)", self.target_page + 1, self.total_pages, pct);
        p.text_center(card_y + pt(52.0), 11.5, 0, &prog_text);

        // Row 3: Interactive Slider Track
        let track_x = card_x + pt(24.0);
        let track_w = card_w - pt(48.0);
        let track_y = card_y + pt(74.0);
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
        let thumb_w = pt(18.0);
        let thumb_h = pt(18.0);
        let thumb_rect = Rect::new(thumb_x - thumb_w / 2, track_y + track_h / 2 - thumb_h / 2, thumb_w, thumb_h);
        p.rect(thumb_rect, 0);

        // Row 4: 4 Instant Step Buttons [-10] [-1] [+1] [+10]
        let btn_y = card_y + pt(98.0);
        let btn_h = pt(32.0);
        let step_w = (card_w - pt(48.0) - pt(24.0)) / 4;

        let steps = [("-10", -10), ("-1", -1), ("+1", 1), ("+10", 10)];
        for (i, (label, _)) in steps.iter().enumerate() {
            let bx = card_x + pt(24.0) + i as i32 * (step_w + pt(8.0));
            let brect = Rect::new(bx, btn_y, step_w, btn_h);
            p.rect_outline_t(brect, 1, 120);
            p.text_center_in(bx, bx + step_w, btn_y + pt(20.0), 9.5, 0, label);
        }
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

                // Tapping outside card closes scrubber directly on current page
                if !card_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::Done(self.target_page));
                }

                // Row 1: [ TOC ] / [ Highlights ] taps
                let row1_y = card_y + pt(10.0);
                let row1_h = pt(26.0);
                let btn_w = (card_w - pt(42.0)) / 2;
                let toc_x = card_x + pt(14.0);
                let toc_rect =
                    Rect::new(toc_x - pt(4.0), row1_y - pt(4.0), btn_w + pt(8.0), row1_h + pt(8.0));
                if toc_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::OpenToc(self.target_page));
                }
                let hl_x = toc_x + btn_w + pt(14.0);
                let hl_rect =
                    Rect::new(hl_x - pt(4.0), row1_y - pt(4.0), btn_w + pt(8.0), row1_h + pt(8.0));
                if hl_rect.contains(px, py) {
                    return (self.on_action)(ScrubberAction::OpenHighlights(self.target_page));
                }

                // Row 3: Slider track tap
                let track_x = card_x + pt(24.0);
                let track_w = card_w - pt(48.0);
                let track_y = card_y + pt(62.0);
                let track_rect = Rect::new(track_x - pt(15.0), track_y, track_w + pt(30.0), pt(32.0));

                if track_rect.contains(px, py) {
                    let rel_x = (px - track_x).clamp(0, track_w);
                    let frac = rel_x as f32 / track_w as f32;
                    let target = ((frac * (self.total_pages - 1) as f32).round() as usize)
                        .min(self.total_pages.saturating_sub(1));
                    self.set_page(target);
                    return Action::Redraw;
                }

                // Row 4: 4 Jump Buttons [-10] [-1] [+1] [+10]
                let btn_y = card_y + pt(94.0);
                let btn_h = pt(40.0);
                let step_w = (card_w - pt(48.0) - pt(24.0)) / 4;
                let steps = [("-10", -10), ("-1", -1), ("+1", 1), ("+10", 10)];

                for (i, (_, delta)) in steps.iter().enumerate() {
                    let bx = card_x + pt(24.0) + i as i32 * (step_w + pt(8.0));
                    let brect = Rect::new(bx - pt(2.0), btn_y, step_w + pt(4.0), btn_h);
                    if brect.contains(px, py) {
                        let new_page = (self.target_page as i32 + delta)
                            .clamp(0, (self.total_pages as i32).saturating_sub(1)) as usize;
                        self.set_page(new_page);
                        return Action::Redraw;
                    }
                }

                Action::Keep
            }
            Gesture::Swipe { dir: SwipeDir::East, .. } => {
                // Swipe right -> +10 pages
                let new_page = (self.target_page + 10).min(self.total_pages.saturating_sub(1));
                self.set_page(new_page);
                Action::Redraw
            }
            Gesture::Swipe { dir: SwipeDir::West, .. } => {
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

