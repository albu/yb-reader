//! Home — the tabbed root screen with a Boox-style bottom nav bar.
//! Tab 0 "home": the Continue card (last-read book) above the action
//! rows with icons and chevrons. Tab 1 "library": the full two-line
//! list — the Continue card lives on home only, where it is the
//! dashboard; the recent sort already floats the last-read book to
//! the top here. Brightness lives on the edge gestures (top-edge
//! swipe / two-finger tap) everywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;
use ybdev::sysinfo;

use crate::books::ReaderScreen;
use crate::library::{self, list_books};
use crate::mirror::MirrorScreen;
use crate::positions::{self, Pos};
use yui::nav::{self, Icon, NavTab};
use yui::painter::{pt, Painter, Rect};
use yui::Orientation;
use yui::screen::{Action, Screen};

const TABS: [NavTab; 2] = [
    NavTab::new(Icon::Home, "home"),
    NavTab::new(Icon::Books, "library"),
];

/// --- Home tab layout (pt) ---
const PAD_PT: f32 = 20.0;
const KICKER_BASE_PT: f32 = 30.0;
const KICKER_SIZE_PT: f32 = 7.0;
const HEADER_RULE_PT: f32 = 40.0;
/// Action rows start under the header rule when no Continue card
/// exists, below the card's divider otherwise.
const ROWS_TOP_PT: f32 = 56.0;
const ROWS_TOP_CONT_PT: f32 = 122.0;
const ROW_H_PT: f32 = 34.0;
const ROW_LABEL_PT: f32 = 10.0;
const ROW_LABEL_BASE_PT: f32 = 21.0;
const CHEV_PT: f32 = 12.0;
/// Icon column: 13pt box, 8pt gap.
const ICON_BOX_PT: f32 = 13.0;
const ICON_GAP_PT: f32 = 8.0;

/// --- Library tab layout (pt) ---
const LIB_TITLE_BASE_PT: f32 = 30.0;
const LIB_TITLE_SIZE_PT: f32 = 11.0;
const LIB_COUNT_PT: f32 = 7.5;
const KICKER2_SIZE_PT: f32 = 7.0;

/// Continue block (only drawn when a last-read book exists).
const CONT_TOP_PT: f32 = 62.0;
const CONT_H_PT: f32 = 44.0;
const CONT_TITLE_PT: f32 = 10.0;
const CONT_SUB_PT: f32 = 7.5;
const CONT_ICON_PT: f32 = 16.0;
/// Divider under the Continue card, before the home action rows.
const CONT_DIV_PT: f32 = 112.0;

/// All-books list: kicker + rows.
const LIST_TOP_PT: f32 = 62.0;
/// Two-line rows: title over author, format label in its own left
/// column, progress status on the right edge.
const LIB_ROW_PT: f32 = 28.0;
const LIB_ITEM_PT: f32 = 9.5;
const LIB_ITEM_BASE_PT: f32 = 12.0;
const LIB_AUTHOR_BASE_PT: f32 = 23.0;
const FMT_COL_PT: f32 = 24.0;
const LIB_FOOT_PT: f32 = 7.0;
const LIB_FOOT_OFF_PT: f32 = 14.0;

const ROW_LABELS: [&str; 5] = [
    "Flashcards Deck",
    "Receive over Wi-Fi",
    "Mirror to Mac",
    "System",
    "Exit",
];

/// Takeover mode: we are the whole UI, so "exit" means handing the device
/// back to the stock framework (exit 42, boot.sh's cue), not returning to
/// a library that isn't running. Guarded by a confirm dialog everywhere.
fn takeover() -> bool {
    std::path::Path::new("/mnt/us/DONT_START_FRAMEWORK").exists()
}

fn confirm_exit_to_stock(bg: Option<Vec<u8>>) -> Action {
    Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
        "Exit to Kindle?",
        "The stock Kindle UI returns.\nReboot brings yb-reader back.",
        "Exit",
        bg,
        move |_act| Action::Quit,
    )))
}

/// Library ordering — a view concern, not a filesystem one. Cycles on a
/// footer/header tap. `Recent` (the default) ranks by last interaction:
/// the position timestamp when the book was read, the file's arrival
/// when it never was.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SortMode {
    Recent,
    Title,
}

impl SortMode {
    fn next(self) -> SortMode {
        match self {
            SortMode::Recent => SortMode::Title,
            SortMode::Title => SortMode::Recent,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::Recent => "recent",
            SortMode::Title => "title",
        }
    }
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

/// Last interaction with a book: read-timestamp vs file arrival,
/// whichever is fresher. A book read yesterday outranks one uploaded an
/// hour ago but never opened; a freshly received book still floats above
/// stale ones.
fn recency_key(name: &str, pos_map: &HashMap<String, Pos>, p: &Path) -> u64 {
    let read = pos_map.get(name).map(|pos| pos.ts).unwrap_or(0);
    let file = mtime(p)
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    read.max(file)
}

/// Row progress label: "38%" when the total is known, nothing when it
/// isn't. Clamped at 100 for the last page.
fn progress_str(pos: &Pos) -> Option<String> {
    if pos.total == 0 {
        return None;
    }
    let pct = (((pos.page + 1) as f32 / pos.total as f32) * 100.0).round() as i32;
    Some(format!("{}%", pct.min(100)))
}

/// Filename without its extension — the display fallback when a file
/// carries no metadata. Separators filenames carry in place of spaces
/// (`_`, URL-style `%20`) read as spaces.
fn sans_ext(name: &str) -> String {
    let base = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    base.replace("%20", " ").replace('_', " ")
}

/// Right-edge row status: "38%" when the total is known, "Unread" when
/// the book was never opened, nothing when a position exists but the
/// total doesn't. The format badge lives in its own left column now —
/// two files sharing a title stay distinguishable there.
fn progress_tag(pos: Option<&Pos>) -> Option<String> {
    match pos {
        None => Some("Unread".to_string()),
        Some(p) => progress_str(p),
    }
}


pub struct HomeScreen {
    w: u32,
    h: u32,
    tab: usize,
    books: Vec<PathBuf>,
    names: Vec<String>,
    /// What rows actually show: metadata title or filename-sans-extension.
    /// `names` stays the identity (positions/highlights are keyed by it).
    disp: Vec<String>,
    authors: Vec<String>,
    /// Positions captured at scan, so draw/sort don't re-read the file.
    pos_map: HashMap<String, Pos>,
    offset: usize,
    sort: SortMode,
    /// The last-read book (path + saved position), if it still exists.
    cont: Option<(PathBuf, Pos)>,
    /// Set in draw (depends on panel height); draw always runs first.
    per_page: usize,
    /// Last painted frame, handed to popups so they float over the list.
    snap: Option<Vec<u8>>,
    /// Header clock as painted — on_tick compares a fresh reading
    /// against it and redraws when the minute flips.
    hdr_time: String,
    /// Flashcards due, cached at scan() so draw never touches flash.
    due_count: usize,
}

impl HomeScreen {
    pub fn new(w: u32, h: u32) -> HomeScreen {
        HomeScreen {
            w,
            h,
            tab: 0,
            books: vec![],
            names: vec![],
            disp: vec![],
            authors: vec![],
            pos_map: HashMap::new(),
            offset: 0,
            sort: SortMode::Recent,
            cont: None,
            per_page: 1,
            snap: None,
            hdr_time: String::new(),
            due_count: 0,
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
        // Deleted books' position entries die here — the store never
        // self-cleans otherwise. (Refuses empty scans: an unreadable
        // documents/ directory must not wipe every position.)
        positions::prune(&self.names);
        let metas = library::meta_for(&self.books);
        self.disp = self
            .names
            .iter()
            .zip(metas.iter())
            .map(|(n, m)| {
                m.as_ref()
                    .map(|(t, _)| t.clone())
                    .unwrap_or_else(|| sans_ext(n))
            })
            .collect();
        self.authors = self
            .names
            .iter()
            .zip(metas.iter())
            .map(|(_, m)| {
                m.as_ref()
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default()
            })
            .collect();
        self.pos_map = positions::all();
        self.offset = 0;
        self.cont = None;
        // Continue = freshest stored position whose file still exists.
        if let Some((name, pos)) = positions::last_read() {
            if let Some(i) = self.names.iter().position(|n| *n == name) {
                self.cont = Some((self.books[i].clone(), pos));
            }
        }
        self.apply_sort();
        // Due count for the Flashcards row, refreshed per scan (enter +
        // resume) instead of re-parsing flashcards.json on every draw.
        self.due_count = crate::flashcards::FlashcardDeck::load().due_count();
    }

    /// Order the in-memory list by the current mode, keeping the parallel
    /// display vectors aligned. Title lowercases the key — the old
    /// byte-order sort put "Zoo" before "apple" and every accented title
    /// at the end.
    fn apply_sort(&mut self) {
        // The parallel vectors must match the book count before zipping —
        // zip truncates to the shortest, and a short one would silently
        // empty the whole list. Reconcile instead of trusting.
        let n = self.books.len();
        self.names.resize(n, String::new());
        if self.disp.len() != n {
            self.disp = self.names.iter().map(|s| sans_ext(s)).collect();
        }
        self.authors.resize(n, String::new());
        let mut items: Vec<(PathBuf, String, String, String)> = self
            .books
            .iter()
            .cloned()
            .zip(self.names.iter().cloned())
            .zip(self.disp.iter().cloned())
            .zip(self.authors.iter().cloned())
            .map(|(((a, b), c), d)| (a, b, c, d))
            .collect();
        match self.sort {
            SortMode::Title => items.sort_by(|a, b| a.2.to_lowercase().cmp(&b.2.to_lowercase())),
            SortMode::Recent => items.sort_by(|a, b| {
                recency_key(&b.1, &self.pos_map, &b.0)
                    .cmp(&recency_key(&a.1, &self.pos_map, &a.0))
                    .then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase()))
            }),
        }
        self.books = items.iter().map(|i| i.0.clone()).collect();
        self.names = items.iter().map(|i| i.1.clone()).collect();
        self.disp = items.iter().map(|i| i.2.clone()).collect();
        self.authors = items.iter().map(|i| i.3.clone()).collect();
        self.offset = 0;
    }

    /// The footer band (range + sort control) — the tappable strip just
    /// above the nav bar on the library tab.
    fn in_footer(y: i32, h: u32) -> bool {
        let content_h = h as i32 - nav::bar_h_px();
        y >= content_h - pt(26.0) && y < content_h
    }

    /// Resolve a tap/long-press y on the library tab to the book under
    /// it. Long-press entry point for deletes.
    fn book_at(&self, y: i32) -> Option<(PathBuf, String)> {
        let rows_top = pt(LIST_TOP_PT);
        let row_h = pt(LIB_ROW_PT);
        if y >= rows_top && y < rows_top + self.per_page as i32 * row_h {
            let idx = self.offset + ((y - rows_top) / row_h) as usize;
            if idx < self.books.len() {
                return Some((self.books[idx].clone(), self.names[idx].clone()));
            }
        }
        None
    }

    /// Pure hit-test for the home tab's action rows.
    fn hit_home_row(y: i32, has_cont: bool) -> Option<usize> {
        let top = HomeScreen::rows_top(has_cont);
        let row_h = pt(ROW_H_PT);
        if y < top || y >= top + ROW_LABELS.len() as i32 * row_h {
            return None;
        }
        Some(((y - top) / row_h) as usize)
    }

    /// Where the home tab's action rows start: below the Continue card
    /// when one exists, under the header rule otherwise.
    fn rows_top(has_cont: bool) -> i32 {
        pt(if has_cont { ROWS_TOP_CONT_PT } else { ROWS_TOP_PT })
    }

    /// Open the Continue book where it was left — the body both tabs'
    /// Continue taps share.
    fn open_continue(&self) -> Action {
        let (path, pos) = self.cont.as_ref().unwrap();
        Action::Push(Box::new(ReaderScreen::new(
            path.clone(),
            pos.page as usize,
            self.w,
            self.h,
        )))
    }

    fn in_continue(y: i32) -> bool {
        y >= pt(CONT_TOP_PT) && y < pt(CONT_TOP_PT) + pt(CONT_H_PT)
    }

    /// Tab switch from a horizontal swipe; Redraw on a change.
    fn switch(&mut self, delta: i32) -> Action {
        let next = (self.tab as i32 + delta).clamp(0, TABS.len() as i32 - 1) as usize;
        if next == self.tab {
            return Action::Keep;
        }
        self.tab = next;
        if self.tab == 1 {
            self.scan();
        }
        Action::Redraw
    }

    /// The Continue card — kicker, open-book icon, title, author · page,
    /// progress bar, chevron. Drawn on the home tab under the header;
    /// the action rows follow its divider.
    fn draw_continue_block(&self, p: &mut Painter) {
        let w = p.size().0;
        let pad = pt(PAD_PT);
        let (path, pos) = self.cont.as_ref().unwrap();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Show what the row shows: metadata title + author (by filename
        // lookup — identity, not display).
        let i = self.names.iter().position(|n| *n == name);
        let disp = i
            .map(|i| self.disp[i].clone())
            .unwrap_or_else(|| sans_ext(&name));
        let author = i.map(|i| self.authors[i].clone()).unwrap_or_default();
        p.text(pad, pt(54.0), KICKER2_SIZE_PT, 130, "CONTINUE");
        let top = pt(CONT_TOP_PT);
        draw_book_icon(p, pad, top + (pt(CONT_H_PT) - pt(CONT_ICON_PT)) / 2);
        let label = p.truncate(CONT_TITLE_PT, &disp, p.width_pt() - 2.0 * PAD_PT - 18.0);
        p.text(
            pad + pt(CONT_ICON_PT) + pt(8.0),
            top + pt(20.0),
            CONT_TITLE_PT,
            0,
            &label,
        );
        let page = if pos.total > 0 {
            format!("page {} of {}", pos.page + 1, pos.total)
        } else {
            format!("page {}", pos.page + 1)
        };
        let sub = if author.is_empty() {
            page
        } else {
            format!("{} · {}", author, page)
        };
        p.text(
            pad + pt(CONT_ICON_PT) + pt(8.0),
            top + pt(36.0),
            CONT_SUB_PT,
            130,
            &sub,
        );
        // Thin progress bar along the block's bottom edge.
        if pos.total > 0 {
            let bx = pad + pt(CONT_ICON_PT) + pt(8.0);
            let bw = (w - pad - bx).max(1);
            let frac = ((pos.page + 1) as f32 / pos.total as f32).clamp(0.0, 1.0);
            p.rect(Rect::new(bx, top + pt(41.0), bw, pt(2.0)), 230);
            let fw = ((bw as f32) * frac).round() as i32;
            if fw > 0 {
                p.rect(Rect::new(bx, top + pt(41.0), fw.min(bw), pt(2.0)), 90);
            }
        }
        p.text_right(w - pad, top + pt(26.0), CHEV_PT, 160, ">");
    }
}

impl Screen for HomeScreen {
    fn default_edges(&self) -> bool {
        true
    }

    /// The launcher is authored portrait — coming back from a landscape
    /// book must reset the grip, not inherit it.
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn on_enter(&mut self) -> Action {
        self.scan();
        Action::Redraw
    }

    fn on_resume(&mut self) -> Action {
        self.scan();
        Action::Redraw
    }



    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        let content_h = h - nav::bar_h_px();
        let pad = pt(PAD_PT);
        p.clear(255);

        // Ambient status, same slot the book header uses (chrome.rs):
        // clock left, battery right (+ = charging). Deliberately smaller
        // and lighter than the kicker below — metadata, not a peer row.
        // on_tick keeps the clock honest while the screen idles.
        let t = crate::chrome::current_time_str();
        self.hdr_time = t.clone();
        let (cap, plugged) = sysinfo::battery();
        let bat = if plugged {
            format!("+{}%", cap)
        } else {
            format!("{}%", cap)
        };
        p.text(pad, pt(12.0), 6.5, 165, &t);
        p.text_right(w - pad, pt(12.0), 6.5, 165, &bat);

        match self.tab {
            0 => {
                p.text(pad, pt(KICKER_BASE_PT), KICKER_SIZE_PT, 130, "YB READER");
                // Build stamp, top right: the on-device answer to "did the
                // deploy land?" (* = dirty tree when built).
                p.text_right(
                    w - pad,
                    pt(KICKER_BASE_PT),
                    KICKER_SIZE_PT,
                    180,
                    concat!("v", env!("YB_BUILD")),
                );
                p.hline_t(pt(HEADER_RULE_PT), pad, w - pad, 3, 140);
                // The dashboard card: what's being read, front and center.
                let has_cont = self.cont.is_some();
                if has_cont {
                    self.draw_continue_block(p);
                    p.hline_t(pt(CONT_DIV_PT), pad, w - pad, 2, 180);
                }
                let rows_top = HomeScreen::rows_top(has_cont);
                for (i, default_label) in ROW_LABELS.iter().enumerate() {
                    let top = rows_top + i as i32 * pt(ROW_H_PT);
                    draw_row_icon(p, i, pad, top + (pt(ROW_H_PT) - pt(ICON_BOX_PT)) / 2);

                    let label = if i == 0 {
                        if self.due_count > 0 {
                            format!("Flashcards ({} due)", self.due_count)
                        } else {
                            "Flashcards · all caught up".to_string()
                        }
                    } else if i == ROW_LABELS.len() - 1 && takeover() {
                        "Exit to Kindle".to_string()
                    } else {
                        default_label.to_string()
                    };

                    p.text(
                        pad + pt(ICON_BOX_PT) + pt(ICON_GAP_PT),
                        top + pt(ROW_LABEL_BASE_PT),
                        ROW_LABEL_PT,
                        0,
                        &label,
                    );
                    p.text_right(w - pad, top + pt(ROW_LABEL_BASE_PT), CHEV_PT, 160, ">");
                    if i + 1 < ROW_LABELS.len() {
                        p.hline_t(top + pt(ROW_H_PT), pad, w - pad, 2, 180);
                    }
                }
            }
            _ => {
                p.text(pad, pt(LIB_TITLE_BASE_PT), LIB_TITLE_SIZE_PT, 0, "Library");
                p.text_right(
                    w - pad,
                    pt(LIB_TITLE_BASE_PT),
                    LIB_COUNT_PT,
                    130,
                    &format!("{} books · {}", self.names.len(), self.sort.label()),
                );
                p.hline_t(pt(HEADER_RULE_PT), pad, w - pad, 3, 140);

                if !self.names.is_empty() {
                    p.text(pad, pt(54.0), KICKER2_SIZE_PT, 130, "ALL BOOKS");
                }

                if self.names.is_empty() {
                    // The empty state is the wireless CTA — the receive
                    // loop is the fastest first-book path, and tapping
                    // anywhere on the content opens it.
                    p.text_center(content_h / 2 - pt(4.0), 10.0, 0, "Library is empty");
                    p.text_center(
                        content_h / 2 + pt(14.0),
                        8.5,
                        130,
                        "tap to receive books over Wi-Fi",
                    );
                    p.text_center(
                        content_h / 2 + pt(28.0),
                        8.0,
                        160,
                        "or copy files to /mnt/us/documents",
                    );
                } else {
                    let rows_top = pt(LIST_TOP_PT);
                    self.per_page =
                        ((content_h - rows_top - pt(LIB_FOOT_OFF_PT)) / pt(LIB_ROW_PT))
                            .max(1) as usize;
                    let visible = self.names.len().min(self.offset + self.per_page);
                    for (i, idx) in (self.offset..visible).enumerate() {
                        let top = rows_top + i as i32 * pt(LIB_ROW_PT);
                        let ext = self.books[idx]
                            .extension()
                            .map(|e| e.to_string_lossy().to_uppercase())
                            .unwrap_or_default();
                        if !ext.is_empty() {
                            p.text(pad, top + pt(LIB_ITEM_BASE_PT), 7.0, 130, &ext);
                        }
                        let tag = progress_tag(self.pos_map.get(&self.names[idx]));
                        // Reserve the right edge for the status when
                        // present; rows without one use the full width.
                        let reserve = if tag.is_some() { 16.0 } else { 2.0 };
                        let budget = p.width_pt() - 2.0 * PAD_PT - FMT_COL_PT - reserve;
                        let label = p.truncate(LIB_ITEM_PT, &self.disp[idx], budget);
                        p.text(
                            pad + pt(FMT_COL_PT),
                            top + pt(LIB_ITEM_BASE_PT),
                            LIB_ITEM_PT,
                            0,
                            &label,
                        );
                        let author = &self.authors[idx];
                        if !author.is_empty() {
                            let a = p.truncate(7.0, author, budget);
                            p.text(
                                pad + pt(FMT_COL_PT),
                                top + pt(LIB_AUTHOR_BASE_PT),
                                7.0,
                                130,
                                &a,
                            );
                        }
                        if let Some(s) = &tag {
                            p.text_right(w - pad, top + pt(LIB_ITEM_BASE_PT), 7.0, 130, s);
                        }
                    }
                    // Footer doubles as the sort control: the label with a
                    // ▾ marker reads as tappable, and the whole band is.
                    let footer = format!(
                        "{}-{} of {} · sort: {} (tap)",
                        self.offset + 1,
                        visible,
                        self.names.len(),
                        self.sort.label()
                    );
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
        self.snap = Some(p.snapshot());
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_secs(20)
    }

    fn on_tick(&mut self) -> Action {
        // Keep the header clock honest while the screen idles: a
        // flash-less partial refresh roughly once a minute — the same
        // cadence the stock status bar keeps.
        if crate::chrome::current_time_str() != self.hdr_time {
            Action::Redraw
        } else {
            Action::Keep
        }
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
                    return Action::Redraw;
                }

                match self.tab {
                    0 => {
                        // Continue card first — it sits above the rows and
                        // must win the hit before any row math.
                        if self.cont.is_some() && HomeScreen::in_continue(y) {
                            return self.open_continue();
                        }
                        match HomeScreen::hit_home_row(y, self.cont.is_some()) {
                            Some(0) => Action::Push(Box::new(crate::flashcards::FlashcardsScreen::new())),
                            Some(1) => {
                                Action::Push(Box::new(crate::receive::ReceiveScreen::new()))
                            }
                            Some(2) => Action::Push(Box::new(MirrorScreen::new(self.w, self.h))),
                            Some(3) => Action::Push(Box::new(crate::system::SystemScreen::new())),
                            // Exit: in takeover mode this hands the device to
                            // the stock framework — confirm first.
                            Some(_) => {
                                if takeover() {
                                    confirm_exit_to_stock(self.snap.clone())
                                } else {
                                    Action::Quit
                                }
                            }
                            None => Action::Keep,
                        }
                    }

                    _ => {
                        // Empty library: the whole content area is the
                        // receive CTA (see draw).
                        if self.names.is_empty() {
                            return Action::Push(Box::new(
                                crate::receive::ReceiveScreen::new(),
                            ));
                        }

                        // Sort control: the footer band (primary, the hint
                        // lives there) or the header (secondary).
                        if HomeScreen::in_footer(y, self.h) || y < pt(HEADER_RULE_PT) {
                            self.sort = self.sort.next();
                            self.apply_sort();
                            return Action::RedrawFull;
                        }

                        let rows_top = pt(LIST_TOP_PT);
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
            // Home tab: swipe down/up still exits (old muscle memory) —
            // but leaving takeover mode deserves a confirm like the Exit
            // row gets.
            Gesture::Swipe { dir: SwipeDir::North, .. }
            | Gesture::Swipe { dir: SwipeDir::South, .. } => {
                if takeover() {
                    confirm_exit_to_stock(self.snap.clone())
                } else {
                    Action::Quit
                }
            }
            // Horizontal swipes flip tabs, Boox-style.
            Gesture::Swipe { dir: SwipeDir::East, .. } => self.switch(-1),
            Gesture::Swipe { dir: SwipeDir::West, .. } => self.switch(1),
            Gesture::TwoFingerTap => Action::Keep,
            // Library tab: long-press a row (Continue included) to delete
            // the book — the wireless loop needs cable-free removal too.
            Gesture::LongPress { x: _, y } if self.tab == 1 => {
                let Some((path, name)) = self.book_at(y as i32) else {
                    return Action::Keep;
                };
                let bg = self.snap.clone();
                Action::Push(Box::new(crate::confirm_dialog::ConfirmDialog::new(
                    "Delete book?",
                    &format!("{}\n\nThe file is removed from documents/.", name),
                    "Delete",
                    bg,
                    move |act| {
                        match act {
                            crate::confirm_dialog::ConfirmAction::Yes => {
                                match std::fs::remove_file(&path) {
                                    Ok(()) => plog(&format!("library: deleted {}", name)),
                                    Err(e) => {
                                        plog(&format!("library: delete {} failed: {}", name, e))
                                    }
                                }
                            }
                            crate::confirm_dialog::ConfirmAction::No => {}
                        }
                        Action::Pop
                    },
                )))
            }
            _ => Action::Keep,
        }
    }

}

/// Small open-book glyph for the Continue row.
fn draw_book_icon(p: &mut Painter, x: i32, y: i32) {
    let w = pt(CONT_ICON_PT);
    let h = pt(CONT_ICON_PT) - pt(1.0);
    let mid = x + w / 2;
    let pad_y = pt(1.5);
    let book_h = h - 2 * pad_y;

    // Central spine
    p.line_w(mid, y + pad_y, mid, y + pad_y + book_h, 3, 0);

    // Left page (curved top and bottom)
    p.line_w(mid, y + pad_y, x + pt(4.0), y + pad_y - pt(1.5), 2, 0);
    p.line_w(x + pt(4.0), y + pad_y - pt(1.5), x + pt(1.5), y + pad_y + pt(0.5), 2, 0);
    p.line_w(x + pt(1.5), y + pad_y + pt(0.5), x + pt(1.5), y + pad_y + book_h - pt(0.5), 2, 0);
    p.line_w(x + pt(1.5), y + pad_y + book_h - pt(0.5), x + pt(4.0), y + pad_y + book_h - pt(2.0), 2, 0);
    p.line_w(x + pt(4.0), y + pad_y + book_h - pt(2.0), mid, y + pad_y + book_h, 2, 0);

    // Right page (curved top and bottom)
    p.line_w(mid, y + pad_y, x + w - pt(4.0), y + pad_y - pt(1.5), 2, 0);
    p.line_w(x + w - pt(4.0), y + pad_y - pt(1.5), x + w - pt(1.5), y + pad_y + pt(0.5), 2, 0);
    p.line_w(x + w - pt(1.5), y + pad_y + pt(0.5), x + w - pt(1.5), y + pad_y + book_h - pt(0.5), 2, 0);
    p.line_w(x + w - pt(1.5), y + pad_y + book_h - pt(0.5), x + w - pt(4.0), y + pad_y + book_h - pt(2.0), 2, 0);
    p.line_w(x + w - pt(4.0), y + pad_y + book_h - pt(2.0), mid, y + pad_y + book_h, 2, 0);
}

/// Row icons in a 13pt box:
/// 0: Flashcards Stack
/// 1: Wi-Fi broadcast (Receive over Wi-Fi)
/// 2: Display Monitor + Cast (Mirror to Mac)
/// 3: Control Sliders (System)
/// 4: Power / Exit button (Exit)
fn draw_row_icon(p: &mut Painter, row: usize, x: i32, y: i32) {
    let s = pt(ICON_BOX_PT);
    let mid_x = x + s / 2;
    let mid_y = y + s / 2;
    const T: i32 = 2;

    match row {
        0 => {
            // Flashcards deck (clean offset cards with 2px stroke and vocab lines)
            p.rect_outline_t(Rect::new(x + pt(2.5), y, s - pt(2.5), s - pt(3.0)), T, 140);
            p.rect(Rect::new(x, y + pt(3.0), s - pt(2.5), s - pt(3.0)), 255);
            p.rect_outline_t(Rect::new(x, y + pt(3.0), s - pt(2.5), s - pt(3.0)), T, 0);
            // Two clean lines inside front card
            p.hline_t(y + pt(6.5), x + pt(2.5), x + s - pt(5.5), 2, 0);
            p.hline_t(y + pt(9.5), x + pt(2.5), x + pt(6.5), 2, 130);
        }
        1 => {
            // Wi-Fi broadcast glyph: bottom dot + concentric signal arcs
            p.circle_fill(mid_x, y + s - pt(1.5), pt(1.5), 0);
            // Inner arc
            let r1 = pt(4.5) as f32;
            for angle in 225..=315 {
                let rad = (angle as f32).to_radians();
                let px = mid_x + (r1 * rad.cos()).round() as i32;
                let py = (y + s - pt(1.5)) + (r1 * rad.sin()).round() as i32;
                p.rect(Rect::new(px - 1, py - 1, 2, 2), 0);
            }
            // Outer arc
            let r2 = pt(8.0) as f32;
            for angle in 220..=320 {
                let rad = (angle as f32).to_radians();
                let px = mid_x + (r2 * rad.cos()).round() as i32;
                let py = (y + s - pt(1.5)) + (r2 * rad.sin()).round() as i32;
                p.rect(Rect::new(px - 1, py - 1, 2, 2), 0);
            }
        }
        2 => {
            // Monitor screen + stand + cast symbol
            let mon_h = s - pt(4.0);
            p.rect_outline_t(Rect::new(x, y, s, mon_h), T, 0);
            // Monitor stand
            p.rect(Rect::new(mid_x - 1, y + mon_h, 2, pt(3.0)), 0);
            p.hline_t(y + s - 1, mid_x - pt(3.5), mid_x + pt(3.5), T, 0);
            // Cast broadcast symbol in bottom-left corner of screen
            p.circle_fill(x + pt(2.5), y + mon_h - pt(2.5), pt(1.0), 0);
            let r_cast = pt(3.0) as f32;
            for angle in 270..=360 {
                let rad = (angle as f32).to_radians();
                let px = x + pt(2.5) + (r_cast * rad.cos()).round() as i32;
                let py = y + mon_h - pt(2.5) + (r_cast * rad.sin()).round() as i32;
                p.rect(Rect::new(px, py, 2, 2), 0);
            }
            let r_cast2 = pt(5.5) as f32;
            for angle in 270..=360 {
                let rad = (angle as f32).to_radians();
                let px = x + pt(2.5) + (r_cast2 * rad.cos()).round() as i32;
                let py = y + mon_h - pt(2.5) + (r_cast2 * rad.sin()).round() as i32;
                p.rect(Rect::new(px, py, 2, 2), 0);
            }
        }
        3 => {
            // Control Sliders: two parallel tracks with offset knobs
            p.hline_t(y + pt(3.5), x, x + s, 2, 150);
            p.circle_fill(x + pt(3.5), y + pt(3.5) + 1, pt(2.5), 0);
            p.hline_t(y + pt(9.5), x, x + s, 2, 150);
            p.circle_fill(x + s - pt(3.5), y + pt(9.5) + 1, pt(2.5), 0);
        }
        _ => {
            // Power / Exit glyph: circle with vertical top line
            let r = (s - pt(2.0)) / 2;
            p.circle_outline_t(mid_x, mid_y, r, T, 0);
            // White mask for top slot
            p.rect(Rect::new(mid_x - pt(2.0), y, pt(4.0), pt(4.5)), 255);
            // Vertical power bar
            p.rect(Rect::new(mid_x - 1, y + 1, 2, pt(6.0)), 0);
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
        assert_eq!(HomeScreen::hit_home_row(top + 2, false), Some(0));
        assert_eq!(HomeScreen::hit_home_row(top + row_h - 1, false), Some(0));
        assert_eq!(HomeScreen::hit_home_row(top + row_h + 4, false), Some(1));
        assert_eq!(HomeScreen::hit_home_row(top + 2 * row_h + 4, false), Some(2));
        assert_eq!(HomeScreen::hit_home_row(top + 3 * row_h + 4, false), Some(3));
        assert_eq!(HomeScreen::hit_home_row(top + 4 * row_h + 4, false), Some(4));
        // Header and below the last row are not rows.
        assert_eq!(HomeScreen::hit_home_row(top - 1, false), None);
        assert_eq!(HomeScreen::hit_home_row(top + 5 * row_h, false), None);
    }

    #[test]
    fn home_rows_shift_below_the_continue_card() {
        let top = HomeScreen::rows_top(true);
        let bare = HomeScreen::rows_top(false);
        // Strictly below the card (and its divider)…
        assert!(top > pt(CONT_TOP_PT + CONT_H_PT));
        assert!(top > bare);
        // …so the card's band hits no row, and the shifted rows still map.
        assert_eq!(HomeScreen::hit_home_row(bare + 2, true), None);
        assert_eq!(HomeScreen::hit_home_row(top + 2, true), Some(0));
        assert_eq!(HomeScreen::hit_home_row(top + 5 * pt(ROW_H_PT) - 1, true), Some(4));
        assert_eq!(HomeScreen::hit_home_row(top + 5 * pt(ROW_H_PT), true), None);
        // The card itself is the Continue tap zone, rows or not.
        assert!(HomeScreen::in_continue(pt(CONT_TOP_PT) + 1));
        assert!(HomeScreen::in_continue(pt(CONT_TOP_PT + CONT_H_PT) - 1));
    }



    #[test]
    fn continue_block_bounds() {
        let cont_top = pt(CONT_TOP_PT);
        assert!(HomeScreen::in_continue(cont_top + 1));
        assert!(HomeScreen::in_continue(cont_top + pt(CONT_H_PT) - 1));
        assert!(!HomeScreen::in_continue(cont_top - 1));
        assert!(!HomeScreen::in_continue(cont_top + pt(CONT_H_PT)));
    }

    #[test]
    fn sort_mode_cycles_and_labels() {
        assert_eq!(SortMode::Recent.next(), SortMode::Title);
        assert_eq!(SortMode::Title.next(), SortMode::Recent);
        // Recent is the default: it's what "open the library" means.
        assert_eq!(HomeScreen::new(1236, 1648).sort, SortMode::Recent);
        for m in [SortMode::Recent, SortMode::Title] {
            assert!(!m.label().is_empty());
        }
    }

    #[test]
    fn title_sort_lowercases_the_key() {
        let mut s = HomeScreen::new(1236, 1648);
        s.sort = SortMode::Title;
        s.books = vec![
            PathBuf::from("/x/zoo.epub"),
            PathBuf::from("/x/Apple.epub"),
            PathBuf::from("/x/banana.epub"),
        ];
        s.names = vec!["zoo.epub".into(), "Apple.epub".into(), "banana.epub".into()];
        s.disp = vec!["Zoo".into(), "Apple".into(), "banana".into()];
        s.apply_sort();
        assert_eq!(s.names, vec!["Apple.epub", "banana.epub", "zoo.epub"]);
        // Display vectors travel with the sort.
        assert_eq!(s.disp, vec!["Apple", "banana", "Zoo"]);
    }

    #[test]
    fn recent_sort_ranks_by_last_interaction() {
        // The /x paths don't exist on the host, so file mtime is the
        // epoch and the position timestamps decide the order.
        let mut s = HomeScreen::new(1236, 1648);
        s.sort = SortMode::Recent;
        s.books = vec![
            PathBuf::from("/x/old.epub"),
            PathBuf::from("/x/fresh.epub"),
            PathBuf::from("/x/never.epub"),
        ];
        s.names = vec!["old.epub".into(), "fresh.epub".into(), "never.epub".into()];
        s.disp = vec!["Old".into(), "Fresh".into(), "Never".into()];
        let mut pos = Pos::simple(3, 100, 500);
        s.pos_map.insert("old.epub".into(), pos);
        pos.ts = 900;
        s.pos_map.insert("fresh.epub".into(), pos);
        s.apply_sort();
        // Read most recently first; never-opened (ts 0) sinks.
        assert_eq!(s.names, vec!["fresh.epub", "old.epub", "never.epub"]);
    }

    #[test]
    fn recency_key_takes_the_max_of_read_and_arrival() {
        let mut map = HashMap::new();
        map.insert("read.epub".into(), Pos::simple(1, 10, 100));
        // A real file: its mtime is "now", far past any fixed read ts —
        // arrival must win for a never-opened book.
        let tmp = std::env::temp_dir().join("yb-reader-recency-test.epub");
        std::fs::write(&tmp, b"x").unwrap();
        let k = recency_key("read.epub", &map, &tmp);
        assert!(k >= 100);
        assert_eq!(recency_key("read.epub", &map, Path::new("/x/missing")), 100);
        // Never touched, missing file: zero.
        assert_eq!(recency_key("no.epub", &map, Path::new("/x/missing")), 0);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn progress_str_boundaries() {
        let mut pos = Pos::simple(0, 0, 123); // total unknown
        assert!(progress_str(&pos).is_none());
        pos.total = 200;
        assert_eq!(progress_str(&pos).unwrap(), "1%");
        pos.page = 99;
        assert_eq!(progress_str(&pos).unwrap(), "50%");
        pos.page = 199;
        assert_eq!(progress_str(&pos).unwrap(), "100%");
        pos.page = 500; // past the end (position from a bigger total)
        assert_eq!(progress_str(&pos).unwrap(), "100%");
    }

    #[test]
    fn sans_ext_strips_only_the_last_extension() {
        assert_eq!(sans_ext("SAMPLE.pdf"), "SAMPLE");
        assert_eq!(sans_ext("my.book.epub"), "my.book");
        assert_eq!(sans_ext("noext"), "noext");
        assert_eq!(sans_ext(".hidden"), ".hidden");
        // Space stand-ins read as spaces.
        assert_eq!(sans_ext("rust_notes_lifetimes.txt"), "rust notes lifetimes");
        assert_eq!(sans_ext("My%20Book.epub"), "My Book");
    }

    #[test]
    fn progress_tag_unread_percent_and_unknown() {
        let mut pos = Pos::simple(49, 100, 7); // 50%
        assert_eq!(progress_tag(Some(&pos)).unwrap(), "50%");
        // Position but unknown total: no honest number to show.
        pos.total = 0;
        assert_eq!(progress_tag(Some(&pos)), None);
        // Never opened.
        assert_eq!(progress_tag(None).unwrap(), "Unread");
    }

    #[test]
    fn empty_library_tap_opens_receive_and_full_list_does_not() {
        let mut s = HomeScreen::new(1236, 1648);
        s.tab = 1;
        s.books = vec![];
        s.names = vec![];
        // Anywhere in the content (not the nav bar, not the header band).
        assert!(matches!(
            s.on_gesture(Gesture::Tap { x: 600, y: 800 }),
            Action::Push(_)
        ));

        // With books present the same tap is a dead zone: no list row, no
        // sort band, nothing pushed.
        s.books = vec![PathBuf::from("/x/one.epub")];
        s.names = vec!["one.epub".into()];
        assert!(matches!(
            s.on_gesture(Gesture::Tap { x: 600, y: 800 }),
            Action::Keep
        ));
    }

    #[test]
    fn book_at_resolves_rows_and_misses() {
        let mut s = HomeScreen::new(1236, 1648);
        s.cont = None;
        s.per_page = 10;
        s.books = vec![PathBuf::from("/x/one.epub"), PathBuf::from("/x/two.epub")];
        s.names = vec!["one.epub".into(), "two.epub".into()];
        let top = pt(LIST_TOP_PT);
        let row_h = pt(LIB_ROW_PT);
        assert_eq!(s.book_at(top + 5).unwrap().1, "one.epub");
        assert_eq!(s.book_at(top + row_h + 5).unwrap().1, "two.epub");
        assert!(s.book_at(top - 1).is_none());
        assert!(s.book_at(top + 2 * row_h + 5).is_none());
    }

    #[test]
    fn footer_band_sits_above_nav_and_below_rows() {
        const H: u32 = 1648;
        let content_h = H as i32 - nav::bar_h_px();
        assert!(HomeScreen::in_footer(content_h - pt(14.0), H));
        assert!(HomeScreen::in_footer(content_h - 1, H));
        assert!(!HomeScreen::in_footer(content_h - pt(30.0), H));
        // The band must not reach into list territory by much: a full page
        // of rows still ends above it.
        let rows_top = pt(LIST_TOP_PT);
        assert!(rows_top + 20 * pt(LIB_ROW_PT) > content_h - pt(26.0));
    }

    /// Render both tabs (with a Continue card present) to PNGs under
    /// target/ui/ — the dev-machine stand-in for looking at the screen.
    /// The only assertion is "draws without panicking"; the PNG is the
    /// point.
    #[test]
    fn renders_both_tabs_to_png() {
        let font = yui::font::Font::load().unwrap();
        let mut s = HomeScreen::new(1236, 1648);
        s.books = vec![
            PathBuf::from("/x/sample.epub"),
            PathBuf::from("/x/clean_architecture.pdf"),
            PathBuf::from("/x/rust_notes_lifetimes.txt"),
            PathBuf::from("/x/berserk_v01.cbz"),
        ];
        s.names = vec![
            "sample.epub".into(),
            "clean_architecture.pdf".into(),
            "rust_notes_lifetimes.txt".into(),
            "berserk_v01.cbz".into(),
        ];
        s.disp = vec![
            "A Sample Book".into(),
            "Clean Architecture".into(),
            "rust notes lifetimes".into(),
            "Berserk v01".into(),
        ];
        s.authors = vec![
            "A. Author".into(),
            "Robert C. Martin".into(),
            String::new(),
            "Kentaro Miura".into(),
        ];
        let pos = Pos::simple(213, 412, 1_700_000_000);
        s.pos_map.insert("sample.epub".into(), pos);
        s.pos_map
            .insert("clean_architecture.pdf".into(), Pos::simple(30, 240, 1_690_000_000));
        s.cont = Some((PathBuf::from("/x/sample.epub"), pos));
        s.apply_sort();

        let dir = format!("{}/../target/ui", env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(&dir).unwrap();
        for (tab, name) in [(0usize, "home_tab0.png"), (1, "home_tab1.png")] {
            s.tab = tab;
            let mut canvas = vec![0u8; 1236 * 1648];
            let mut panel = vec![255u8; 1248 * 1648];
            let mut p = yui::Painter::new(
                &mut panel,
                1236,
                1648,
                1248,
                Orientation::Portrait,
                &mut canvas,
                &font,
            );
            s.draw(&mut p);
            // Portrait: the canvas IS the frame; flush would only copy it.
            let file = std::fs::File::create(format!("{dir}/{name}")).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&canvas).unwrap();
        }
    }
}
