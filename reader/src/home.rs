//! Home — the tabbed root screen with a Boox-style bottom nav bar.
//! Tab 0 "home": status line + action rows (Mirror / Fetch / Exit) with
//! icons and chevrons. Tab 1 "library": a Continue section resuming the
//! last-read book, then the full list. Brightness lives on the edge
//! gestures (top-edge swipe / two-finger tap) everywhere.

use std::path::PathBuf;

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;

use crate::books::{list_books, ReaderScreen};
use crate::fetch;
use crate::mirror::MirrorScreen;
use crate::positions::{self, Pos};
use yui::nav::{self, Icon, NavTab};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::MessageScreen;

const TABS: [NavTab; 2] = [
    NavTab::new(Icon::Home, "home"),
    NavTab::new(Icon::Books, "library"),
];

/// --- Home tab layout (pt) ---
const PAD_PT: f32 = 20.0;
const KICKER_BASE_PT: f32 = 26.0;
const KICKER_SIZE_PT: f32 = 7.0;
const HEADER_RULE_PT: f32 = 40.0;
const ROWS_TOP_PT: f32 = 56.0;
const ROW_H_PT: f32 = 34.0;
const ROW_LABEL_PT: f32 = 10.0;
const ROW_LABEL_BASE_PT: f32 = 21.0;
const CHEV_PT: f32 = 12.0;
/// Icon column: 13pt box, 8pt gap.
const ICON_BOX_PT: f32 = 13.0;
const ICON_GAP_PT: f32 = 8.0;

/// --- Library tab layout (pt) ---
const LIB_TITLE_BASE_PT: f32 = 28.0;
const LIB_TITLE_SIZE_PT: f32 = 11.0;
const LIB_COUNT_PT: f32 = 7.5;
const KICKER2_SIZE_PT: f32 = 7.0;

/// Continue block (only drawn when a last-read book exists).
const CONT_TOP_PT: f32 = 62.0;
const CONT_H_PT: f32 = 44.0;
const CONT_TITLE_PT: f32 = 10.0;
const CONT_SUB_PT: f32 = 7.5;
const CONT_ICON_PT: f32 = 16.0;

/// All-books list: kicker + rows; the list starts lower when the
/// Continue block is present.
const LIST_TOP_WITH_CONT_PT: f32 = 132.0;
const LIST_TOP_PT: f32 = 62.0;
const LIB_ROW_PT: f32 = 24.0;
const LIB_ITEM_PT: f32 = 9.5;
const LIB_ITEM_BASE_PT: f32 = 15.0;
const LIB_FOOT_PT: f32 = 7.0;
const LIB_FOOT_OFF_PT: f32 = 14.0;

const ROW_LABELS: [&str; 4] = ["Mirror to Mac", "Fetch book from Mac", "ssh", "Exit"];

/// The ssh row's label reflects the live state (scanned in draw).
fn ssh_label() -> &'static str {
    if ybdev::ssh::running() {
        "ssh on · :2222"
    } else {
        "ssh off"
    }
}

pub struct HomeScreen {
    w: u32,
    h: u32,
    tab: usize,
    books: Vec<PathBuf>,
    names: Vec<String>,
    offset: usize,
    /// The last-read book (path + saved position), if it still exists.
    cont: Option<(PathBuf, Pos)>,
    /// Set in draw (depends on panel height); draw always runs first.
    per_page: usize,
}

impl HomeScreen {
    pub fn new(w: u32, h: u32) -> HomeScreen {
        HomeScreen {
            w,
            h,
            tab: 0,
            books: vec![],
            names: vec![],
            offset: 0,
            cont: None,
            per_page: 1,
        }
    }

    fn scan(&mut self) {
        self.books = list_books();
        self.names = self
            .books
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        self.offset = 0;
        self.cont = None;
        // Continue = freshest stored position whose file still exists.
        if let Some((name, pos)) = positions::last_read() {
            if let Some(i) = self.names.iter().position(|n| *n == name) {
                self.cont = Some((self.books[i].clone(), pos));
            }
        }
    }

    /// Pure hit-test for the home tab's action rows.
    fn hit_home_row(y: i32) -> Option<usize> {
        let top = pt(ROWS_TOP_PT);
        let row_h = pt(ROW_H_PT);
        if y < top || y >= top + ROW_LABELS.len() as i32 * row_h {
            return None;
        }
        Some(((y - top) / row_h) as usize)
    }

    fn in_continue(y: i32) -> bool {
        y >= pt(CONT_TOP_PT) && y < pt(CONT_TOP_PT) + pt(CONT_H_PT)
    }

    fn list_top(has_cont: bool) -> i32 {
        pt(if has_cont {
            LIST_TOP_WITH_CONT_PT
        } else {
            LIST_TOP_PT
        })
    }

    /// Tab switch from a horizontal swipe; RedrawFull only on a change.
    fn switch(&mut self, delta: i32) -> Action {
        let next = (self.tab as i32 + delta).clamp(0, TABS.len() as i32 - 1) as usize;
        if next == self.tab {
            return Action::Keep;
        }
        self.tab = next;
        if next == 1 {
            self.scan();
        }
        Action::RedrawFull
    }
}

impl Screen for HomeScreen {
    fn on_enter(&mut self) -> Action {
        self.scan();
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        let content_h = h - nav::bar_h_px();
        let pad = pt(PAD_PT);
        p.clear(255);

        match self.tab {
            0 => {
                p.text(pad, pt(KICKER_BASE_PT), KICKER_SIZE_PT, 130, "YB READER");
                p.hline_t(pt(HEADER_RULE_PT), pad, w - pad, 3, 140);
                for (i, label) in ROW_LABELS.iter().enumerate() {
                    let top = pt(ROWS_TOP_PT) + i as i32 * pt(ROW_H_PT);
                    let label = if *label == "ssh" { ssh_label() } else { label };
                    draw_row_icon(p, i, pad, top + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2);
                    p.text(
                        pad + pt(ICON_BOX_PT) + pt(ICON_GAP_PT),
                        top + pt(ROW_LABEL_BASE_PT),
                        ROW_LABEL_PT,
                        0,
                        label,
                    );
                    p.text_right(w - pad, top + pt(ROW_LABEL_BASE_PT), CHEV_PT, 160, ">");
                    if i + 1 < ROW_LABELS.len() {
                        p.hline_t(top + pt(ROW_H_PT), pad, w - pad, 2, 180);
                    }
                }
            }
            _ => {
                let has_cont = self.cont.is_some();
                p.text(pad, pt(LIB_TITLE_BASE_PT), LIB_TITLE_SIZE_PT, 0, "Library");
                p.text_right(
                    w - pad,
                    pt(LIB_TITLE_BASE_PT),
                    LIB_COUNT_PT,
                    130,
                    &format!("{} books", self.names.len()),
                );
                p.hline_t(pt(HEADER_RULE_PT), pad, w - pad, 3, 140);

                if has_cont {
                    let (path, pos) = self.cont.as_ref().unwrap();
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    p.text(pad, pt(54.0), KICKER2_SIZE_PT, 130, "CONTINUE");
                    let top = pt(CONT_TOP_PT);
                    draw_book_icon(p, pad, top + (pt(CONT_H_PT) - pt(CONT_ICON_PT)) / 2);
                    let label =
                        p.truncate(CONT_TITLE_PT, &name, p.width_pt() - 2.0 * PAD_PT - 18.0);
                    p.text(
                        pad + pt(CONT_ICON_PT) + pt(8.0),
                        top + pt(20.0),
                        CONT_TITLE_PT,
                        0,
                        &label,
                    );
                    let sub = if pos.total > 0 {
                        format!("page {} of {}", pos.page + 1, pos.total)
                    } else {
                        format!("page {}", pos.page + 1)
                    };
                    p.text(
                        pad + pt(CONT_ICON_PT) + pt(8.0),
                        top + pt(36.0),
                        CONT_SUB_PT,
                        130,
                        &sub,
                    );
                    p.text_right(w - pad, top + pt(26.0), CHEV_PT, 160, ">");
                    p.hline_t(pt(112.0), pad, w - pad, 2, 180);
                    p.text(pad, pt(124.0), KICKER2_SIZE_PT, 130, "ALL BOOKS");
                } else if !self.names.is_empty() {
                    p.text(pad, pt(54.0), KICKER2_SIZE_PT, 130, "ALL BOOKS");
                }

                if self.names.is_empty() {
                    p.text_center(content_h / 2, 10.0, 0, "Library is empty");
                    p.text_center(
                        content_h / 2 + pt(14.0),
                        8.0,
                        130,
                        "add books to /mnt/us/documents",
                    );
                } else {
                    let rows_top = HomeScreen::list_top(has_cont);
                    self.per_page =
                        ((content_h - rows_top - pt(LIB_FOOT_OFF_PT)) / pt(LIB_ROW_PT))
                            .max(1) as usize;
                    let visible = self.names.len().min(self.offset + self.per_page);
                    for (i, idx) in (self.offset..visible).enumerate() {
                        let top = rows_top + i as i32 * pt(LIB_ROW_PT);
                        let label = p.truncate(
                            LIB_ITEM_PT,
                            &self.names[idx],
                            p.width_pt() - 2.0 * PAD_PT - 2.0,
                        );
                        p.text(pad, top + pt(LIB_ITEM_BASE_PT), LIB_ITEM_PT, 0, &label);
                    }
                    let footer = format!("{}-{} · swipe: scroll", self.offset + 1, visible);
                    p.text_center(
                        content_h - pt(LIB_FOOT_OFF_PT),
                        LIB_FOOT_PT,
                        130,
                        &footer,
                    );
                }
            }
        }

        nav::draw_nav(p, &TABS, self.tab);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w as i32, self.h as i32);
        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                if let Some(t) = nav::hit(x, y, w, h, TABS.len()) {
                    if t == self.tab {
                        return Action::Keep;
                    }
                    self.tab = t;
                    if t == 1 {
                        self.scan();
                    }
                    return Action::RedrawFull;
                }
                match self.tab {
                    0 => match HomeScreen::hit_home_row(y) {
                        Some(0) => Action::Push(Box::new(MirrorScreen::new(self.w, self.h))),
                        Some(1) => {
                            let msg = fetch::fetch_book();
                            let lines: Vec<String> = match msg {
                                Ok(m) => m.lines().map(|l| l.to_string()).collect(),
                                Err(e) => e.lines().map(|l| l.to_string()).collect(),
                            };
                            Action::Push(Box::new(MessageScreen::from_strings(lines)))
                        }
                        Some(2) => {
                            // Toggle. Note: turning ssh off from the app
                            // while deployed-in over ssh severs that
                            // session — that's the point of a switch.
                            let on = ybdev::ssh::running();
                            if on {
                                ybdev::ssh::disable();
                            } else {
                                ybdev::ssh::enable();
                            }
                            plog(&format!("ssh {}", if on { "off" } else { "on" }));
                            Action::RedrawFull
                        }
                        Some(_) => Action::Quit,
                        None => Action::Keep,
                    },
                    _ => {
                        // Continue row: open the last-read book where it
                        // was left.
                        if self.cont.is_some() && HomeScreen::in_continue(y) {
                            let (path, pos) = self.cont.as_ref().unwrap();
                            let path = path.clone();
                            let page = pos.page as usize;
                            let (w, h) = (self.w, self.h);
                            return Action::Push(Box::new(ReaderScreen::new(path, page, w, h)));
                        }
                        let rows_top = HomeScreen::list_top(self.cont.is_some());
                        let row_h = pt(LIB_ROW_PT);
                        if y >= rows_top && y < rows_top + self.per_page as i32 * row_h {
                            let idx = self.offset + ((y - rows_top) / row_h) as usize;
                            if idx < self.books.len() {
                                let path = self.books[idx].clone();
                                let page = positions::resume_page(&self.names[idx]);
                                let (w, h) = (self.w, self.h);
                                return Action::Push(Box::new(ReaderScreen::new(path, page, w, h)));
                            }
                        }
                        Action::Keep
                    }
                }
            }
            // Library tab: vertical swipes scroll the list.
            Gesture::Swipe { dir: SwipeDir::North, .. } if self.tab == 1 => {
                self.offset = (self.offset + self.per_page.max(1))
                    .min(self.names.len().saturating_sub(1));
                Action::RedrawFull
            }
            Gesture::Swipe { dir: SwipeDir::South, .. } if self.tab == 1 => {
                if self.offset > 0 {
                    self.offset -= self.per_page.min(self.offset);
                    Action::RedrawFull
                } else {
                    Action::Keep
                }
            }
            // Home tab: swipe down/up still exits (old muscle memory).
            Gesture::Swipe { dir: SwipeDir::North, .. }
            | Gesture::Swipe { dir: SwipeDir::South, .. } => Action::Quit,
            // Horizontal swipes flip tabs, Boox-style.
            Gesture::Swipe { dir: SwipeDir::East, .. } => self.switch(-1),
            Gesture::Swipe { dir: SwipeDir::West, .. } => self.switch(1),
            Gesture::TwoFingerTap => Action::Keep,
        }
    }
}

/// Small open-book glyph for the Continue row.
fn draw_book_icon(p: &mut Painter, x: i32, y: i32) {
    let w = pt(CONT_ICON_PT);
    let h = pt(CONT_ICON_PT) - pt(2.0);
    let spine = x + w / 2;
    const T: i32 = 2;
    p.line_w(spine, y + 2, spine, y + h - 2, T, 0);
    p.rect_outline_t(Rect::new(x + 1, y + 2, w / 2 - 2, h - 5), T, 0);
    p.rect_outline_t(Rect::new(spine + 1, y + 2, w / 2 - 2, h - 5), T, 0);
}

/// Row icons in a 13pt box: 0 screen+arrow (mirror), 1 down-into-tray
/// (fetch), 2 terminal prompt (ssh), 3 X (exit).
fn draw_row_icon(p: &mut Painter, row: usize, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    let mid = x + s / 2;
    const T: i32 = 2;
    match row {
        0 => {
            p.rect_outline_t(Rect::new(x, y, s, s - 2), T, 0);
            p.line_w(mid - 3, y + s / 2, mid + 4, y + s / 2 - 1, T, 0);
            p.line_w(mid + 1, y + s / 2 - 4, mid + 4, y + s / 2 - 1, T, 0);
            p.line_w(mid + 1, y + s / 2 + 2, mid + 4, y + s / 2 - 1, T, 0);
        }
        1 => {
            p.line_w(mid, y + 1, mid, y + s - 5, T, 0);
            p.line_w(mid - 3, y + s - 8, mid, y + s - 5, T, 0);
            p.line_w(mid + 3, y + s - 8, mid, y + s - 5, T, 0);
            p.rect_outline_t(Rect::new(x + 1, y + s - 3, s - 2, 2), T, 0);
        }
        2 => {
            // A little terminal: window frame with a ">_" prompt.
            p.rect_outline_t(Rect::new(x, y + 1, s, s - 4), T, 0);
            p.hline_t(y + 5, x + 2, x + s - 3, 1, 0);
            p.line_w(mid - 4, y + 8, mid - 1, y + s / 2 + 1, T, 0);
            p.line_w(mid - 4, y + s - 7, mid - 1, y + s / 2 + 1, T, 0);
            p.line_w(mid + 1, y + s - 6, mid + 5, y + s - 6, T, 0);
        }
        _ => {
            p.line_w(x + 2, y + 2, x + s - 3, y + s - 3, T, 0);
            p.line_w(x + s - 3, y + 2, x + 2, y + s - 3, T, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_rows_hit_exact_bounds() {
        let top = pt(ROWS_TOP_PT);
        let row_h = pt(ROW_H_PT);
        assert_eq!(HomeScreen::hit_home_row(top + 2), Some(0));
        assert_eq!(HomeScreen::hit_home_row(top + row_h - 1), Some(0));
        assert_eq!(HomeScreen::hit_home_row(top + row_h + 4), Some(1));
        assert_eq!(HomeScreen::hit_home_row(top + 2 * row_h + 4), Some(2));
        assert_eq!(HomeScreen::hit_home_row(top + 3 * row_h + 4), Some(3));
        // Header and below the last row are not rows.
        assert_eq!(HomeScreen::hit_home_row(top - 1), None);
        assert_eq!(HomeScreen::hit_home_row(top + 4 * row_h), None);
    }

    #[test]
    fn continue_row_bounds_and_list_top() {
        let cont_top = pt(CONT_TOP_PT);
        assert!(HomeScreen::in_continue(cont_top + 1));
        assert!(HomeScreen::in_continue(cont_top + pt(CONT_H_PT) - 1));
        assert!(!HomeScreen::in_continue(cont_top - 1));
        assert!(!HomeScreen::in_continue(cont_top + pt(CONT_H_PT)));
        // With Continue present the list starts strictly below it.
        assert!(HomeScreen::list_top(true) > cont_top + pt(CONT_H_PT));
        assert!(HomeScreen::list_top(false) < HomeScreen::list_top(true));
    }
}
