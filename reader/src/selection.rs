//! Text selection: the word-index state machine and the confirm-bar
//! visuals. Selection spans are word indices into the page's word list
//! (reading order), so finger imprecision disappears into snapping.

use crate::split::RectF;
use yui::painter::{pt, Painter, Rect};

/// A pending selection. `end` moves — by drag while holding, by tap
/// after lifting — while `anchor` only moves by another long-press
/// (re-anchor).
pub struct SelState {
    pub anchor: usize,
    pub end: usize,
}

impl SelState {
    pub fn new(word: usize) -> Self {
        SelState {
            anchor: word,
            end: word,
        }
    }

    /// Ordered span, regardless of which end the finger dragged to.
    pub fn span(&self) -> (usize, usize) {
        (self.anchor.min(self.end), self.anchor.max(self.end))
    }
}

/// Confirm bar geometry (bar + Save + Cancel), shared by draw and hit-testing.
pub fn sel_bar_rects(w: i32, h: i32) -> (Rect, Rect, Rect) {
    let bar_h = pt(48.0);
    let bar_y = h - pt(76.0);
    let bar_x = pt(24.0);
    let bar_w = w - 2 * pt(24.0);
    let btn_w = pt(64.0);
    let btn_h = pt(32.0);
    let by = bar_y + (bar_h - btn_h) / 2;
    let x_btn = Rect::new(bar_x + bar_w - btn_w - pt(12.0), by, btn_w, btn_h);
    let ok_btn = Rect::new(x_btn.x - btn_w - pt(10.0), by, btn_w, btn_h);
    (Rect::new(bar_x, bar_y, bar_w, bar_h), ok_btn, x_btn)
}

/// Caret triangle pointing into the span at one of its end words.
fn draw_sel_caret(p: &mut Painter, r: &RectF, start: bool) {
    let cy = ((r.y0 + r.y1) / 2.0).round() as i32;
    let hh = pt(8.0);
    let cx = if start {
        (r.x0 - 10.0).round() as i32
    } else {
        (r.x1 + 10.0).round() as i32
    };
    for dy in 0..hh {
        let wdt = dy + 2;
        let x = if start { cx - wdt } else { cx };
        p.rect(Rect::new(x, cy - dy, wdt.max(1), 1), 0);
        p.rect(Rect::new(x, cy + dy, wdt.max(1), 1), 0);
    }
}

/// The pending selection: inverted span + end carets + confirm bar with
/// Save / Cancel. No-op (draws nothing) when the span exceeds the word
/// list, which can briefly happen right after a page change.
pub fn draw_selection(p: &mut Painter, sel: &SelState, words: &[(String, RectF)]) {
    let (w, h) = p.size();
    let (lo, hi) = sel.span();
    if hi >= words.len() {
        return;
    }
    for (_, r) in &words[lo..=hi] {
        let x0 = (r.x0 - 2.0).round() as i32;
        let y0 = (r.y0 - 1.0).round() as i32;
        let rw = ((r.x1 - r.x0) + 4.0).round().max(2.0) as i32;
        let rh = ((r.y1 - r.y0) + 2.0).round().max(2.0) as i32;
        p.invert(Rect::new(x0, y0, rw, rh));
    }
    draw_sel_caret(p, &words[lo].1, true);
    draw_sel_caret(p, &words[hi].1, false);

    let (bar, ok_btn, x_btn) = sel_bar_rects(w, h);
    p.rect(bar, 255);
    p.rect_outline_t(bar, 2, 0);
    let text = words[lo..=hi]
        .iter()
        .map(|(w, _)| w.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let trunc = p.truncate(8.0, &text, (ok_btn.x - bar.x - pt(24.0)) as f32 / pt(1.0) as f32);
    p.text(bar.x + pt(12.0), bar.y + pt(19.0), 8.0, 0, &trunc);
    p.text(bar.x + pt(12.0), bar.y + pt(37.0), 6.5, 130, "tap: end · hold: start");
    p.rect(ok_btn, 0);
    p.text_center_in(ok_btn.x, ok_btn.x + ok_btn.w, ok_btn.y + pt(21.0), 9.0, 255, "Save");
    p.rect_outline_t(x_btn, 2, 100);
    p.text_center_in(x_btn.x, x_btn.x + x_btn.w, x_btn.y + pt(21.0), 9.0, 50, "Cancel");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_orders_and_flips() {
        // Drag/tap "backwards" past the anchor: the span is simply ordered.
        let s = SelState { anchor: 5, end: 2 };
        assert_eq!(s.span(), (2, 5));
        let s = SelState { anchor: 5, end: 9 };
        assert_eq!(s.span(), (5, 9));
        let s = SelState { anchor: 5, end: 5 };
        assert_eq!(s.span(), (5, 5));
    }
}
