//! Boox-style bottom navigation bar: icon-over-label tabs, a rule above,
//! and a thick underline marking the active tab. Pure drawing +
//! hit-testing; the screen hosting it decides what a tab means.
//!
//! Line weights matter at 300 dpi: 1px strokes are ~0.09mm and read as
//! faint gray smudges on e-ink — icons use 2px strokes, rules 3px.

use crate::painter::{pt, Painter, Rect};

pub const BAR_H_PT: f32 = 46.0;
const LABEL_SIZE_PT: f32 = 7.0;
const LABEL_BASE_PT: f32 = 35.0;
const ICON_Y_OFF_PT: f32 = 8.0;
const ICON_W_PT: f32 = 17.0;
const ICON_H_PT: f32 = 14.0;
const UNDERLINE_PT: f32 = 3.0;


/// Line-art tab icons (drawn with Painter primitives, no font needed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Pitched-roof house.
    Home,
    /// Open book.
    Books,
}

#[derive(Clone, Copy, Debug)]
pub struct NavTab {
    pub icon: Icon,
    pub label: &'static str,
}

impl NavTab {
    pub const fn new(icon: Icon, label: &'static str) -> NavTab {
        NavTab { icon, label }
    }
}

pub fn bar_h_px() -> i32 {
    pt(BAR_H_PT)
}

/// Is this y (px) inside the bar strip at the bottom of an h-tall screen?
pub fn in_bar(y: i32, h: i32) -> bool {
    y >= h - bar_h_px()
}

/// Which tab does a tap at (x, y) hit? None when above the bar.
pub fn hit(x: i32, y: i32, w: i32, h: i32, n_tabs: usize) -> Option<usize> {
    if n_tabs == 0 || !in_bar(y, h) {
        return None;
    }
    let tw = (w / n_tabs as i32).max(1);
    Some(((x.clamp(0, w - 1) / tw) as usize).min(n_tabs - 1))
}

/// Draw the bar pinned to the bottom of the screen.
pub fn draw_nav(p: &mut Painter, tabs: &[NavTab], selected: usize) {
    let (w, h) = p.size();
    let top = h - bar_h_px();
    // Rule separating content from the bar.
    p.rect(Rect::new(0, top, w, 3), 120);

    let n = tabs.len().max(1) as i32;
    let tw = w / n;
    for (i, t) in tabs.iter().enumerate() {
        let active = i == selected;
        let color = if active { 0u8 } else { 120 };
        let left = i as i32 * tw;
        let cx = left + tw / 2;
        draw_icon(p, t.icon, cx - pt(ICON_W_PT) / 2, top + pt(ICON_Y_OFF_PT), color);
        // Centered within THIS tab's cell — a panel-wide center would
        // stack every label on top of the others.
        p.text_center_in(left, left + tw, top + pt(LABEL_BASE_PT), LABEL_SIZE_PT, color, t.label);
        if active {
            p.rect(Rect::new(left, h - pt(UNDERLINE_PT), tw, pt(UNDERLINE_PT)), 0);
        }
    }
}

fn draw_icon(p: &mut Painter, icon: Icon, x: i32, y: i32, color: u8) {
    let w = pt(ICON_W_PT);
    let h = pt(ICON_H_PT);
    match icon {
        Icon::Home => {
            let mid = x + w / 2;
            let roof_top = y + pt(1.0);
            let eaves_y = y + h / 2 - pt(1.0);
            // Chimney
            p.rect(Rect::new(x + w - pt(5.0), roof_top + pt(2.0), pt(2.5), pt(5.0)), color);
            // Roof peak lines
            p.line_w(x + pt(1.0), eaves_y, mid, roof_top, 3, color);
            p.line_w(mid, roof_top, x + w - pt(1.0), eaves_y, 3, color);
            // House body
            let body_x = x + pt(3.0);
            let body_w = w - pt(6.0);
            let body_y = eaves_y;
            let body_h = h - (eaves_y - y) - pt(1.0);
            p.rect_outline_t(Rect::new(body_x, body_y, body_w, body_h), 2, color);
            // Centered Door
            let door_w = pt(4.5);
            let door_h = pt(6.5);
            let door_x = x + (w - door_w) / 2;
            let door_y = body_y + body_h - door_h;
            p.rect(Rect::new(door_x, door_y, door_w, door_h), color);
        }
        Icon::Books => {
            let mid = x + w / 2;
            let pad_y = pt(2.0);
            let book_h = h - 2 * pad_y;
            // Central spine
            p.line_w(mid, y + pad_y, mid, y + pad_y + book_h, 3, color);
            // Left page curve
            p.line_w(mid, y + pad_y, x + pt(2.0), y + pad_y + pt(2.0), 2, color);
            p.line_w(x + pt(2.0), y + pad_y + pt(2.0), x + pt(2.0), y + pad_y + book_h - pt(1.0), 2, color);
            p.line_w(x + pt(2.0), y + pad_y + book_h - pt(1.0), mid, y + pad_y + book_h, 2, color);
            // Right page curve
            p.line_w(mid, y + pad_y, x + w - pt(2.0), y + pad_y + pt(2.0), 2, color);
            p.line_w(x + w - pt(2.0), y + pad_y + pt(2.0), x + w - pt(2.0), y + pad_y + book_h - pt(1.0), 2, color);
            p.line_w(x + w - pt(2.0), y + pad_y + book_h - pt(1.0), mid, y + pad_y + book_h, 2, color);
            // Text line hints on pages
            p.hline_t(y + pad_y + pt(5.0), x + pt(4.5), mid - pt(3.0), 1, color);
            p.hline_t(y + pad_y + pt(8.0), x + pt(4.5), mid - pt(3.0), 1, color);
            p.hline_t(y + pad_y + pt(5.0), mid + pt(3.0), x + w - pt(4.5), 1, color);
            p.hline_t(y + pad_y + pt(8.0), mid + pt(3.0), x + w - pt(4.5), 1, color);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    const W: i32 = 1236;
    const H: i32 = 1648;

    #[test]
    fn hit_maps_tabs_and_rejects_content_area() {
        let bar = bar_h_px();
        // Above the bar: content, never a tab hit.
        assert_eq!(hit(600, H - bar - 1, W, H, 2), None);
        // Inside the bar: left half = tab 0, right half = tab 1.
        assert_eq!(hit(0, H - bar + 5, W, H, 2), Some(0));
        assert_eq!(hit(600, H - 10, W, H, 2), Some(0));
        assert_eq!(hit(619, H - 10, W, H, 2), Some(1));
        assert_eq!(hit(W - 1, H - 1, W, H, 2), Some(1));
        // Clamped on the far edges.
        assert_eq!(hit(-50, H - 5, W, H, 2), Some(0));
    }

    #[test]
    fn bar_height_is_46pt_of_screen() {
        assert_eq!(bar_h_px(), pt(46.0));
        assert!(in_bar(H - 1, H));
        assert!(!in_bar(0, H));
    }
}
