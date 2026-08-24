use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;

use crate::backend::{create_backend, BusyPhase, PageTurnResult, ReaderBackend};
use crate::chrome;
use crate::curtain::CurtainScreen;
use crate::dialogs;
use crate::positions;
use crate::selection::{self, SelState};
use crate::split::{ReaderSettings, RectF};

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::Orientation;

pub struct ReaderScreen {
    pw: u32,
    ph: u32,
    backend: Rc<RefCell<Box<dyn ReaderBackend>>>,
    settings: ReaderSettings,
    page_gray: Option<Vec<u8>>,
    dims: (i32, i32),
    time_str: String,
    vocab_db: Option<&'static crate::vocab::VocabDb>,
    vocab_prof: crate::vocab::VocabProfile,
    page_words: Vec<(String, RectF)>,
    page_links: Vec<(RectF, String)>,
    /// Saved highlights for this book (notes store), for underlining.
    highlights: Vec<crate::notes::Highlight>,
    /// (page, sub) the current `page_gray` snapshot was rendered for, when
    /// it is a disk-cached page — used to keep it on screen through the
    /// instant-open handoff instead of blanking to "Opening…".
    snap_pos: Option<(usize, usize)>,
    /// Poll reported a content change (open/layout landed, queued turns
    /// applied): re-render on the next draw WITHOUT dropping the cached
    /// bitmap — if the new state is still loading, the snapshot stays on
    /// screen instead of blanking to a loading message.
    render_pending: bool,
    sel_mode: bool,
    sel: Option<SelState>,
    jump_history: Vec<(usize, usize)>,
    turns_since_full: usize,
}

impl ReaderScreen {
    pub fn new(path: PathBuf, resume: usize, w: u32, h: u32) -> ReaderScreen {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pos = positions::resume_pos(&name);
        let settings = pos.settings.unwrap_or_default();
        let sub_idx = if pos.page == resume { pos.sub_idx } else { 0 };

        let (vw, vh) = Orientation::from_rotation(settings.split.rotation).visual_dims(w, h);
        let backend = create_backend(path.clone(), resume, sub_idx, vw, vh, &settings);
        let is_pdf = backend.is_pdf();
        let engine = if is_pdf { 0 } else { 1 };

        let cached_snap =
            crate::cache::load_snapshot(&name, resume, sub_idx, &settings, vw, vh, engine);
        let has_snap = cached_snap.is_some();
        if cached_snap.is_some() {
            plog(&format!("loaded instant page snapshot for {}", name));
        }

        let vocab_db = crate::vocab::VocabDb::open();
        let vocab_prof = crate::vocab::VocabProfile::load();

        ReaderScreen {
            pw: w,
            ph: h,
            backend: Rc::new(RefCell::new(backend)),
            settings,
            page_gray: cached_snap,
            dims: (w as i32, h as i32),
            time_str: chrome::current_time_str(),
            vocab_db,
            vocab_prof,
            page_words: Vec::new(),
            page_links: Vec::new(),
            highlights: crate::notes::load(&name),
            snap_pos: if has_snap {
                Some((resume, sub_idx))
            } else {
                None
            },
            render_pending: false,
            sel_mode: false,
            sel: None,
            jump_history: Vec::new(),
            turns_since_full: 0,
        }
    }

    fn book_name(&self) -> String {
        self.backend.borrow().book_name()
    }

    fn visual_dims(&self) -> (u32, u32) {
        Orientation::from_rotation(self.settings.split.rotation).visual_dims(self.pw, self.ph)
    }

    fn is_pdf(&self) -> bool {
        self.backend.borrow().is_pdf()
    }

    fn save_progress(&self) {
        let b = self.backend.borrow();
        // is_paginated, not just is_ready: between "book parsed" and
        // "landing chapter laid out" the yread backend's total is still
        // the placeholder 1, and a save in that window (quick exit, the
        // poll right after open) records "page 0 of 1" over the real
        // position.
        if !b.is_ready() || !b.is_paginated() {
            return;
        }
        let name = self.book_name();
        positions::record_pos(
            &name,
            b.current_page(),
            b.total_pages(),
            b.current_sub_idx(),
            Some(self.settings),
        );
    }

    fn find_word_at_pos(&self, vx: f32, vy: f32) -> Option<(String, RectF)> {
        let pad = 12.0f32;
        let mut best: Option<(String, RectF, f32)> = None;

        for (word, r) in &self.page_words {
            if vx >= r.x0 - pad && vx <= r.x1 + pad && vy >= r.y0 - pad && vy <= r.y1 + pad {
                let cx = (r.x0 + r.x1) / 2.0;
                let cy = (r.y0 + r.y1) / 2.0;
                let dist = (vx - cx).powi(2) + (vy - cy).powi(2);
                if best.as_ref().is_none_or(|(_, _, d)| dist < *d) {
                    best = Some((word.clone(), *r, dist));
                }
            }
        }
        best.map(|(w, r, _)| (w, r))
    }

    /// Index of the word under (vx, vy) — the selection primitive: spans
    /// are word indices, so finger imprecision disappears into snapping.
    fn word_idx_at_pos(&self, vx: f32, vy: f32) -> Option<usize> {
        self.page_words.iter().position(|(_, r)| {
            vx >= r.x0 - 12.0 && vx <= r.x1 + 12.0 && vy >= r.y0 - 12.0 && vy <= r.y1 + 12.0
        })
    }

    fn find_link_at_pos(&self, vx: f32, vy: f32) -> Option<(RectF, String)> {
        let pad = 12.0f32;
        let mut best: Option<(RectF, String, f32)> = None;

        for (r, uri) in &self.page_links {
            if vx >= r.x0 - pad && vx <= r.x1 + pad && vy >= r.y0 - pad && vy <= r.y1 + pad {
                let cx = (r.x0 + r.x1) / 2.0;
                let cy = (r.y0 + r.y1) / 2.0;
                let dist = (vx - cx).powi(2) + (vy - cy).powi(2);
                if best.as_ref().is_none_or(|(_, _, d)| dist < *d) {
                    best = Some((*r, uri.clone(), dist));
                }
            }
        }
        best.map(|(r, u, _)| (r, u))
    }

    fn open_footnote_or_link(&self, uri: &str) -> Action {
        self.backend.borrow().resolve_link_or_footnote(
            uri,
            self.page_gray.clone(),
            self.book_name(),
            self.settings,
        )
    }

    fn open_toc_dialog(&mut self) -> Action {
        let cur_page = self.backend.borrow().current_page();
        let back = self.jump_history.last().copied();
        self.backend
            .borrow()
            .open_toc_dialog(cur_page, back, self.book_name(), self.settings)
    }

    fn open_scrubber_dialog(&mut self) -> Action {
        let cur_page = self.backend.borrow().current_page();
        let (vw, vh) = self.visual_dims();
        let back = self.jump_history.last().copied();
        self.backend.borrow().open_scrubber_dialog(
            cur_page,
            self.page_gray.clone(),
            back,
            self.book_name(),
            self.settings,
            vw,
            vh,
        )
    }

    fn open_quick_settings_sheet(&mut self) -> Action {
        let path_name = self.book_name();
        let total = self.backend.borrow().total_pages();
        let page_no = self.backend.borrow().current_page();
        let sub_idx = self.backend.borrow().current_sub_idx();
        let settings = self.settings;
        let is_pdf = self.is_pdf();
        // PDF crop dialog needs the live document — without it the row
        // opens a non-functional dialog.
        let doc = self.backend.borrow().mupdf_doc();
        let base_gray = self.page_gray.clone();
        let backend_rc = Rc::clone(&self.backend);
        let (vw, vh) = self.visual_dims();

        dialogs::quick_settings_sheet(
            path_name,
            page_no,
            sub_idx,
            total,
            settings,
            is_pdf,
            doc,
            base_gray,
            move |new_settings| {
                backend_rc
                    .borrow_mut()
                    .interactive_preview(&new_settings, vw, vh)
            },
        )
    }

    /// The curtain from the reader carries the book's rotation context,
    /// so the ORIENTATION pill can cycle the book's split rotation.
    fn open_curtain(&mut self) -> Action {
        let (page, sub, total) = {
            let b = self.backend.borrow();
            (b.current_page(), b.current_sub_idx(), b.total_pages())
        };
        Action::Push(Box::new(CurtainScreen::new_with_rotation(
            crate::curtain::RotateCtx {
                book: self.book_name(),
                page,
                sub,
                total,
                settings: self.settings,
            },
        )))
    }

    fn open_word_dialog(&mut self, entry: crate::vocab::WordEntry) -> Action {
        dialogs::word_dialog(entry, self.vocab_prof.clone(), self.page_gray.clone())
    }

    /// Commit the pending selection to the notes store and underline it.
    fn sel_save(&mut self) -> Action {
        let Some(sel) = self.sel.take() else {
            return Action::Keep;
        };
        let (lo, hi) = sel.span();
        // A same-page re-render (render_pending) swaps `page_words` without
        // a page turn; a stale span must not slice out of bounds. Empty
        // text just skips the save.
        let text = if hi < self.page_words.len() {
            self.page_words[lo..=hi]
                .iter()
                .map(|(w, _)| w.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        };
        if text.is_empty() {
            return Action::Keep;
        }
        let page = self.backend.borrow().current_page();
        if crate::notes::add(&self.book_name(), page, &text) {
            let head: String = text.chars().take(48).collect();
            plog(&format!("highlight: p{} '{}…'", page + 1, head));
            self.highlights = crate::notes::load(&self.book_name());
        }
        Action::Redraw
    }

    fn handle_page_turn_result(&mut self, res: PageTurnResult) -> Action {
        match res {
            PageTurnResult::Changed { redraw_full } => {
                // A pending selection spans words that no longer exist.
                self.sel = None;
                self.page_gray = None;
                self.snap_pos = None;
                self.save_progress();
                self.turns_since_full += 1;
                let interval = positions::global_refresh_interval();
                let force_full = redraw_full || (interval > 0 && self.turns_since_full >= interval);
                if force_full {
                    self.turns_since_full = 0;
                    Action::RedrawFull
                } else {
                    Action::Redraw
                }
            }
            _ => Action::Keep,
        }
    }
}

impl Screen for ReaderScreen {
    fn default_edges(&self) -> bool {
        false
    }

    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::from_rotation(self.settings.split.rotation))
    }

    fn on_enter(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        let pos = positions::resume_pos(&self.book_name());

        // The cached page (if any) was rendered for exactly this resume
        // position — keep it on screen so the book opens instantly while
        // parse + layout catch up in the background.
        let mut snapshot_valid = self.snap_pos == Some((pos.page, pos.sub_idx));
        if let Some(s) = pos.settings {
            if s != self.settings {
                let old = self.settings;
                self.settings = s;
                // Dims must be taken AFTER adopting: visual_dims reads
                // self.settings, so capturing earlier would paginate the
                // incoming rotation with the stale viewport.
                let (vw, vh) = self.visual_dims();
                self.backend
                    .borrow_mut()
                    .apply_settings_change(&old, &s, vw, vh);
                // The cached bitmap was rendered with the old settings.
                snapshot_valid = false;
            }
        }
        let (vw, vh) = self.visual_dims();

        if pos.sub_idx > 0 || !self.is_pdf() {
            self.backend
                .borrow_mut()
                .jump_to_sub(pos.sub_idx, vw, vh, &self.settings);
        } else {
            self.backend
                .borrow_mut()
                .jump_to_page(pos.page, vw, vh, &self.settings);
        }
        if !snapshot_valid {
            self.page_gray = None;
            self.snap_pos = None;
        }
        self.save_progress();

        Action::RedrawFull
    }

    fn on_resume(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        let pos = positions::resume_pos(&self.book_name());
        // Highlights can change underneath us (the highlights dialog),
        // and a jump leaves a pending selection pointing at a dead page.
        self.highlights = crate::notes::load(&self.book_name());
        self.sel = None;

        let mut pos_changed = false;
        let mut pos_moved = false;
        if let Some(s) = pos.settings {
            if s != self.settings {
                let old = self.settings;
                self.settings = s;
                // Dims AFTER adoption — see on_enter. Capturing before the
                // swap laid out the whole visible book for the stale
                // rotation and cached those pages.
                let (vw, vh) = self.visual_dims();
                self.backend
                    .borrow_mut()
                    .apply_settings_change(&old, &s, vw, vh);
                pos_changed = true;
            }
        }
        let (vw, vh) = self.visual_dims();

        let cur_sub = self.backend.borrow().current_sub_idx();
        let cur_page = self.backend.borrow().current_page();
        if self.is_pdf() {
            if pos.page != cur_page || pos.sub_idx != cur_sub {
                self.backend
                    .borrow_mut()
                    .jump_to_page(pos.page, vw, vh, &self.settings);
                // Restore the split sub-box too (e.g. Back to a mid-page
                // position); jump_to_page resets it to 0.
                if pos.sub_idx < 100 {
                    self.backend
                        .borrow_mut()
                        .jump_to_sub(pos.sub_idx, vw, vh, &self.settings);
                }
                pos_changed = true;
                pos_moved = true;
            }
        } else {
            if pos.sub_idx != cur_sub {
                self.backend
                    .borrow_mut()
                    .jump_to_sub(pos.sub_idx, vw, vh, &self.settings);
                pos_changed = true;
                pos_moved = true;
            }
        }

        if pos_changed {
            if pos_moved {
                // Jump-history undo chain: the TOC dialog's "Back" wrote
                // the previous reading position to the store — landing on
                // the top entry means an undo, so consume it. Any other
                // jump (TOC row, link/footnote, scrubber) remembers where
                // we came from so a later "Back" can return (one Back per
                // jump, no toggling).
                if self.jump_history.last() == Some(&(pos.page, pos.sub_idx)) {
                    self.jump_history.pop();
                } else {
                    self.jump_history.push((cur_page, cur_sub));
                    if self.jump_history.len() > 16 {
                        self.jump_history.remove(0);
                    }
                }
            }
            self.page_gray = None;
            self.snap_pos = None;
            self.save_progress();
            Action::RedrawFull
        } else {
            Action::Redraw
        }
    }

    fn tick_interval(&self) -> std::time::Duration {
        if !self.backend.borrow().is_ready() || self.backend.borrow().has_pending_work() {
            std::time::Duration::from_millis(100)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    fn on_tick(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        let (vw, vh) = self.visual_dims();

        if self.backend.borrow_mut().poll(vw, vh, &self.settings) {
            self.render_pending = true;
            self.save_progress();
            Action::Redraw
        } else {
            Action::Keep
        }
    }

    fn on_leave(&mut self) {
        self.save_progress();
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        let (vw, vh) = self.visual_dims();
        let book_name = self.book_name();

        let mut backend = self.backend.borrow_mut();
        if let Some(err) = backend.error() {
            p.clear(255);
            let box_w = (w - pt(48.0)).min(pt(320.0));
            let box_h = pt(140.0);
            let box_x = (w - box_w) / 2;
            let box_y = (h - box_h) / 2;
            let r = Rect::new(box_x, box_y, box_w, box_h);
            p.rect(r, 255);
            p.rect_outline_t(r, 2, 0);
            p.text_center_in(
                box_x,
                box_x + box_w,
                box_y + pt(28.0),
                12.0,
                0,
                "Failed to Open Book",
            );
            let err_trunc = p.truncate(8.0, err, (box_w - pt(24.0)) as f32);
            p.text_center_in(box_x, box_x + box_w, box_y + pt(55.0), 8.0, 100, &err_trunc);
            return;
        }

        // Re-render when the cached bitmap is stale (page_gray cleared by a
        // turn/jump/settings change — the old bitmap is the WRONG page) or
        // when a poll reported a transition (render_pending — same page,
        // engine catching up). Anything else — selection drags, chrome
        // ticks, dialog returns — blits the cache instead of
        // re-rasterizing (and re-writing a 2 MB snapshot) per redraw. A
        // render that is still loading behind a valid cached page keeps
        // the snapshot on screen: the async handoff must never blank to a
        // loading message.
        let mut rendered_new = false;
        if self.page_gray.is_none() || self.render_pending {
            self.render_pending = false;
            let render_output = backend.render_page(vw, vh, &self.settings);
            match render_output.gray {
                Some(gray) => {
                    p.blit_gray(0, 0, vw as i32, vh as i32, &gray, vw as usize);
                    // Persist the rendered page so a reopen shows it
                    // instantly while parse + layout catch up in the
                    // background. One write per position: consecutive
                    // redraws of the same page skip it.
                    let (snap_page, snap_sub) = (backend.current_page(), backend.current_sub_idx());
                    if self.snap_pos != Some((snap_page, snap_sub)) {
                        let engine = if backend.is_pdf() { 0 } else { 1 };
                        crate::cache::save_snapshot(
                            &book_name,
                            snap_page,
                            snap_sub,
                            &self.settings,
                            vw,
                            vh,
                            &gray,
                            engine,
                        );
                        self.snap_pos = Some((snap_page, snap_sub));
                    }
                    self.page_gray = Some(gray);
                    self.page_words = render_output.words;
                    self.page_links = render_output.links;
                    // Re-render replaces the word layout; a pending
                    // selection's span may reference indices that no
                    // longer exist (same rule as page turns, :283).
                    self.sel = None;
                    rendered_new = true;
                }
                None => {
                    if self.page_gray.is_none() {
                        // No cached page to show: the honest full-screen
                        // loading state.
                        p.clear(255);
                        if render_output.is_loading {
                            let msg = if backend.is_ready() {
                                "Laying out…"
                            } else {
                                "Opening…"
                            };
                            p.text_center(h / 2, 12.0, 100, msg);
                        }
                        self.page_words = render_output.words;
                        self.page_links = render_output.links;
                        self.sel = None;
                        return;
                    }
                    // Loading behind a valid cached page: the snapshot
                    // blits below; words/links still describe it.
                }
            }
        }
        if !rendered_new {
            if let Some(cached) = &self.page_gray {
                p.blit_gray(0, 0, vw as i32, vh as i32, cached, vw as usize);
            }
        }

        // Saved highlights: solid light underlines, page-gated and matched
        // by word sequence within the page.
        let page_no = backend.current_page();
        if !self.page_words.is_empty() {
            for (lo, hi) in crate::notes::matched_spans(&self.highlights, page_no, &self.page_words)
            {
                for (_, r) in &self.page_words[lo..=hi] {
                    let y = (r.y1 + 1.0).round() as i32;
                    let x0 = r.x0.round() as i32;
                    let x1 = r.x1.round() as i32;
                    if x1 > x0 {
                        p.rect(Rect::new(x0, y, x1 - x0, 2), 170);
                    }
                }
            }
        }

        // Header & Footer Chrome
        let (mut footer_text, footer_page, footer_total) = backend.footer_info();
        // While the engine catches up on the SHOWN page, say so in the
        // footer instead of blanking to a loading screen. Neighbor
        // prefetch doesn't count — the page is final (busy_phase, not
        // has_pending_work).
        if let Some(phase) = backend.busy_phase() {
            footer_text.push_str(match phase {
                BusyPhase::Opening => " · Opening…",
                BusyPhase::LayingOut => " · Laying out…",
                BusyPhase::Turning => " · Turning…",
            });
        }
        let chap_title = backend.chapter_title();
        let is_pdf = backend.is_pdf();
        let toc_pages = backend.toc_chapter_pages();
        drop(backend);

        let top_title = if is_pdf {
            book_name.as_str()
        } else {
            chap_title.as_deref().unwrap_or(book_name.as_str())
        };

        chrome::draw_header(
            p,
            &self.time_str,
            top_title,
            self.settings.invert,
            self.sel_mode,
        );

        chrome::draw_footer(
            p,
            &footer_text,
            footer_page,
            footer_total,
            &toc_pages,
            self.settings.invert,
        );

        if let Some(sel) = &self.sel {
            selection::draw_selection(p, sel, &self.page_words);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (vis_w, vis_h) = self.dims;
        let (vw, vh) = self.visual_dims();

        // 1. Edge Gestures: Curtain, Back, Brightness
        if g.top_edge_swipe() || g.top_edge_swipe_in(vis_h.max(vh as i32) as u32) {
            return self.open_curtain();
        }
        if g.corner_back()
            || g.corner_back_in(vis_w.max(vw as i32) as u32, vis_h.max(vh as i32) as u32)
        {
            return Action::Pop;
        }

        match g {
            Gesture::TwoFingerTap => {
                self.turns_since_full = 0;
                self.page_gray = None;
                self.snap_pos = None;
                Action::RedrawFull
            }
            Gesture::Swipe { dir, .. } => {
                // Any swipe cancels a pending selection (mode stays on).
                let had_sel = self.sel.take().is_some();
                let act = match dir {
                    SwipeDir::East => {
                        let res = self
                            .backend
                            .borrow_mut()
                            .turn_page(-1, vw, vh, &self.settings);
                        self.handle_page_turn_result(res)
                    }
                    SwipeDir::West => {
                        let res = self
                            .backend
                            .borrow_mut()
                            .turn_page(1, vw, vh, &self.settings);
                        self.handle_page_turn_result(res)
                    }
                    SwipeDir::North => self.open_quick_settings_sheet(),
                    _ => Action::Keep,
                };
                if had_sel && matches!(act, Action::Keep) {
                    return Action::Redraw;
                }
                act
            }
            Gesture::Tap { x, y } => {
                let (vx, vy) = (x as i32, y as i32);

                // 0a. Bookmark (leftmost mark of the header's right
                // cluster: bookmark · wifi · battery) toggles selection
                // mode. Checked first so the ribbon's own strip stays
                // carved out of the header zones.
                if vx > vis_w - pt(70.0) && vy < pt(40.0) {
                    self.sel_mode = !self.sel_mode;
                    self.sel = None;
                    return Action::RedrawFull;
                }

                // 0b. Pending selection: bar buttons, or a tap sets the end
                // (extend / shrink / flip — one operation). Everything else
                // is inert so a stray tap can't turn the page and eat the
                // selection.
                if self.sel.is_some() {
                    let (bar, ok_btn, x_btn) = selection::sel_bar_rects(vis_w, vis_h);
                    if ok_btn.contains(vx, vy) {
                        return self.sel_save();
                    }
                    if x_btn.contains(vx, vy) {
                        self.sel = None;
                        return Action::Redraw;
                    }
                    if bar.contains(vx, vy) {
                        return Action::Keep;
                    }
                    if let Some(i) = self.word_idx_at_pos(vx as f32, vy as f32) {
                        if let Some(sel) = &mut self.sel {
                            if i != sel.end {
                                sel.end = i;
                                return Action::Redraw;
                            }
                        }
                        return Action::Keep;
                    }
                    return Action::Keep;
                }

                // Header TOC icon in top left
                if vx < pt(70.0) && vy < pt(40.0) {
                    return self.open_toc_dialog();
                }

                // Bottom Left -> Quick Settings Sheet
                if vx < pt(100.0) && vy > vis_h - pt(45.0) {
                    return self.open_quick_settings_sheet();
                }

                // Bottom Center / Right -> Scrubber / Seek
                if vx >= pt(100.0) && vy > vis_h - pt(45.0) {
                    return self.open_scrubber_dialog();
                }

                // Page Turn tap zones: left third = back, everything else
                // = forward (center-tap advance is e-reader muscle memory;
                // a dead middle zone reads as an ignored tap).
                if vx < vis_w / 3 {
                    let res = self
                        .backend
                        .borrow_mut()
                        .turn_page(-1, vw, vh, &self.settings);
                    self.handle_page_turn_result(res)
                } else {
                    let res = self
                        .backend
                        .borrow_mut()
                        .turn_page(1, vw, vh, &self.settings);
                    self.handle_page_turn_result(res)
                }
            }
            Gesture::LongPress { x, y } => {
                let (vx, vy) = (x as i32, y as i32);
                // Selection mode (bookmark on): a long-press anchors (or
                // re-anchors) a selection.
                if self.sel.is_some() || self.sel_mode {
                    if let Some(i) = self.word_idx_at_pos(vx as f32, vy as f32) {
                        self.sel = Some(SelState::new(i));
                        return Action::Redraw;
                    }
                }
                // Links and footnotes follow the dictionary gesture —
                // long-press, not tap (a plain tap must keep turning the
                // page). A link wins over the word lookup so a footnote
                // reference opens its note rather than the dictionary.
                if let Some((_rect, uri)) = self.find_link_at_pos(vx as f32, vy as f32) {
                    return self.open_footnote_or_link(&uri);
                }
                if let Some((word_text, _rect)) = self.find_word_at_pos(vx as f32, vy as f32) {
                    let found = self.vocab_db.as_ref().and_then(|db| db.lookup(&word_text));
                    if let Some(entry) = found {
                        return self.open_word_dialog(entry);
                    }
                    return dialogs::dict_miss(&word_text, self.page_gray.clone());
                }
                Action::Keep
            }
            Gesture::Drag { x, y } => {
                // Finger still down after the anchoring long-press: extend
                // the span word by word (reading order; backtracking
                // shrinks, crossing the anchor flips).
                let idx = self.word_idx_at_pos(x as f32, y as f32);
                if let (Some(sel), Some(i)) = (&mut self.sel, idx) {
                    if i != sel.end {
                        sel.end = i;
                        return Action::Redraw;
                    }
                }
                Action::Keep
            }
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toc_resume_jumps_and_back_consumes_history() {
        let (w, h) = (1236, 1648);
        let pid = std::process::id();

        // Part 1 — a TOC/link jump records a new position; the reader
        // remembers where it came from so the TOC "Back to p.N" row can
        // offer undo.
        let jump_path = std::path::PathBuf::from(format!("/tmp/test_toc_{}_jump.fb2", pid));
        let mut screen = ReaderScreen::new(jump_path, 0, w, h);
        let jump_book = screen.book_name();
        positions::record_pos(&jump_book, 10, 50, 3 * 1_000_000 + 42, None);

        let action = screen.on_resume();
        assert!(matches!(action, Action::RedrawFull));
        assert_eq!(screen.jump_history.last(), Some(&(0, 0)));

        // Part 2 — the "Back" button (which writes the stored top entry
        // back): the reader consumes it instead of pushing a new one.
        let back_path = std::path::PathBuf::from(format!("/tmp/test_toc_{}_back.fb2", pid));
        let mut screen = ReaderScreen::new(back_path, 0, w, h);
        let back_book = screen.book_name();
        screen.jump_history.push((10, 2));
        positions::record_pos(&back_book, 10, 50, 2, None);

        let action = screen.on_resume();
        assert!(matches!(action, Action::RedrawFull));
        assert!(screen.jump_history.is_empty());
    }

    #[test]
    fn bookmark_toggle_and_selection_anchor() {
        let path = std::path::PathBuf::from(format!("/tmp/test_sel_{}.fb2", std::process::id()));
        let (w, h) = (1236, 1648);
        let mut screen = ReaderScreen::new(path, 0, w, h);

        // Tapping the ribbon (top-right, left of the battery) toggles
        // selection mode on.
        let act = screen.on_gesture(Gesture::Tap {
            x: (w - 30) as u32,
            y: 10,
        });
        assert!(matches!(act, Action::RedrawFull));
        assert!(screen.sel_mode);

        // In selection mode, a long-press on a word anchors a selection.
        screen.page_words = vec![("hello".to_string(), RectF::new(10.0, 10.0, 50.0, 30.0))];
        let act = screen.on_gesture(Gesture::LongPress { x: 20, y: 20 });
        assert!(matches!(act, Action::Redraw));
        assert!(screen.sel.is_some());

        // A drag extends the span to a second word.
        screen
            .page_words
            .push(("world".to_string(), RectF::new(60.0, 10.0, 110.0, 30.0)));
        let act = screen.on_gesture(Gesture::Drag { x: 80, y: 20 });
        assert!(matches!(act, Action::Redraw));
        assert_eq!(screen.sel.as_ref().map(|s| s.end), Some(1));

        // A swipe cancels the pending selection (mode stays on).
        let act = screen.on_gesture(Gesture::Swipe {
            dir: SwipeDir::West,
            x: 500,
            y: 800,
            ex: 300,
            ey: 800,
        });
        assert!(matches!(act, Action::Redraw));
        assert!(screen.sel.is_none());
        assert!(screen.sel_mode);
    }
}
