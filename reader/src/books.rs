//! The reader screen: page state, the gesture grammar, and the Screen
//! lifecycle. Rendering and text geometry live in render.rs, document
//! open/warm-cache in document.rs, dialog construction in dialogs.rs,
//! selection visuals in selection.rs, page chrome in chrome.rs, library
//! listing in library.rs.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use mupdf::Document;
use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;

use crate::chrome;
use crate::curtain::CurtainScreen;
use crate::dialogs;
use crate::document::{self as doc_store, BookReady, SendDoc, WARM};
use crate::positions;
use crate::selection::{self, SelState, sel_bar_rects};
use crate::split::{RectF, ReaderSettings};

use crate::render::render_page;

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

pub struct ReaderScreen {
    path: PathBuf,
    w: u32,
    h: u32,
    loading: Option<std::sync::mpsc::Receiver<Result<BookReady, String>>>,
    doc: Option<Rc<Document>>,
    err: Option<String>,
    total: usize,
    page_no: usize,
    sub_idx: usize,
    settings: ReaderSettings,
    page_gray: Option<Vec<u8>>,
    dims: (i32, i32),
    turns_since_full: usize,
    pending_turns: i32,
    time_str: String,
    /// &'static: the 14 MB dictionary is read once per process
    /// (vocab::open caches it) and shared by every ReaderScreen.
    vocab_db: Option<&'static crate::vocab::VocabDb>,
    vocab_prof: crate::vocab::VocabProfile,
    page_words: Vec<(String, RectF)>,
    page_annotations: Vec<(RectF, crate::vocab::WordEntry)>,
    page_links: Vec<(RectF, String)>,
    page_start_time: Instant,
    avg_secs_per_page: f32,
    toc_chapters: Vec<usize>,
    /// Selection mode (bookmark toggled): long-press selects instead of
    /// opening the dictionary. Taps keep turning pages in every mode.
    sel_mode: bool,
    sel: Option<SelState>,
    /// Cached highlights for this book (notes store), for underlining.
    highlights: Vec<crate::notes::Highlight>,
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

        // Attempt instant frame 0 snapshot load from disk cache
        let cached_snap = crate::cache::load_snapshot(&name, resume, sub_idx, &settings, w, h);
        if cached_snap.is_some() {
            plog(&format!("loaded instant page snapshot for {}", name));
        }

        let vocab_db = crate::vocab::VocabDb::open();
        let vocab_prof = crate::vocab::VocabProfile::load();

        ReaderScreen {
            path,
            w,
            h,
            loading: None,
            doc: None,
            err: None,
            total: pos.total.max(1),
            page_no: resume,
            sub_idx,
            settings,
            page_gray: cached_snap,
            dims: (w as i32, h as i32),
            turns_since_full: 0,
            pending_turns: 0,
            time_str: chrome::current_time_str(),
            vocab_db,
            vocab_prof,
            page_words: Vec::new(),
            page_annotations: Vec::new(),
            page_links: Vec::new(),
            page_start_time: Instant::now(),
            avg_secs_per_page: 45.0,
            toc_chapters: Vec::new(),
            sel_mode: false,
            sel: None,
            highlights: Vec::new(),
        }
    }

    fn book_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn is_pdf(&self) -> bool {
        self.path
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
    }

    fn save_progress(&self) {
        positions::record_pos(
            &self.book_name(),
            self.page_no,
            self.total,
            self.sub_idx,
            Some(self.settings),
        );
    }

    fn turn(&mut self, forward: bool) -> Action {
        let total_steps = self.settings.split.total_steps(self.total);
        let cur_step = self
            .settings
            .split
            .page_sub_to_step(self.page_no, self.sub_idx);

        let next_step = if forward {
            (cur_step + 1).min(total_steps.saturating_sub(1))
        } else {
            cur_step.saturating_sub(1)
        };

        if next_step == cur_step {
            return Action::Keep;
        }

        let elapsed = self.page_start_time.elapsed().as_secs_f32();
        if elapsed >= 3.0 && elapsed <= 300.0 {
            self.avg_secs_per_page = self.avg_secs_per_page * 0.7 + elapsed * 0.3;
        }
        self.page_start_time = Instant::now();

        let (new_page, new_sub) = self.settings.split.step_to_page_sub(next_step);

        if self.doc.is_none() {
            // If background-loading, check if the neighbor page is already in snapshot
            // Cache hit: instant page swap!
            if let Some(snap) = crate::cache::load_snapshot(
                &self.book_name(),
                new_page,
                new_sub,
                &self.settings,
                self.w,
                self.h,
            ) {
                self.page_no = new_page;
                self.sub_idx = new_sub;
                self.page_gray = Some(snap);
                self.page_words.clear();
                self.page_annotations.clear();
                self.pending_turns = 0;
                self.save_progress();
                return Action::Redraw;
            }

            // Not yet cached: queue the turn for when background layout finishes
            if forward {
                self.pending_turns += 1;
            } else {
                self.pending_turns = (self.pending_turns - 1).max(-(self.page_no as i32));
            }
            return Action::Redraw;
        }

        self.page_no = new_page;
        self.sub_idx = new_sub;
        self.page_gray = None;
        self.page_words.clear();
        self.page_annotations.clear();
        self.save_progress();

        self.turns_since_full += 1;
        let global_interval = positions::global_refresh_interval();
        if global_interval > 0 && self.turns_since_full >= global_interval {
            self.turns_since_full = 0;
            Action::RedrawFull
        } else {
            Action::Redraw
        }
    }

    fn scan_toc_chapters(&mut self) {
        if let Some(doc) = &self.doc {
            if let Ok(ref outlines) = doc.outlines() {
                let mut chapters = Vec::new();
                Self::collect_outline_pages(outlines, &mut chapters);
                chapters.sort_unstable();
                chapters.dedup();
                self.toc_chapters = chapters;
            }
        }
    }

    fn collect_outline_pages(outlines: &[mupdf::Outline], out: &mut Vec<usize>) {
        for o in outlines {
            if let Some(dest) = &o.dest {
                out.push(dest.loc.page_number as usize);
            }
            if !o.down.is_empty() {
                Self::collect_outline_pages(&o.down, out);
            }
        }
    }

    fn open_toc_dialog(&mut self) -> Action {
        let Some(doc) = &self.doc else { return Action::Keep };
        let Some(outlines) = doc.outlines().ok() else { return Action::Keep };
        dialogs::toc_dialog(
            &outlines,
            self.page_no,
            self.book_name(),
            self.total,
            self.settings,
        )
    }

    fn open_scrubber_dialog(&mut self) -> Action {
        let Some(doc) = &self.doc else { return Action::Keep };
        dialogs::scrubber_dialog(
            doc,
            self.page_no,
            self.total,
            self.page_gray.clone(),
            self.book_name(),
            self.settings,
            self.w,
            self.h,
        )
    }

    fn open_footnote_or_link(&mut self, uri: &str) -> Action {
        let Some(doc) = &self.doc else { return Action::Keep };
        dialogs::footnote_dialog(
            doc,
            uri,
            self.page_gray.clone(),
            self.book_name(),
            self.total,
            self.settings,
        )
    }

    fn open_settings_dialog(&mut self) -> Action {
        dialogs::settings_dialog(
            self.doc.as_ref(),
            self.page_no,
            self.settings,
            self.is_pdf(),
            self.book_name(),
            self.total,
        )
    }

    fn open_curtain(&mut self) -> Action {
        Action::Push(Box::new(CurtainScreen::new()))
    }

    /// Map physical touch/swipe event to visual orientation coordinates.
    /// Returns (visual_x, visual_y, visual_swipe_dir).
    fn map_gesture(&self, g: Gesture) -> (i32, i32, Option<SwipeDir>) {
        let (w, h) = self.dims; // w=1236, h=1648
        match g {
            Gesture::Tap { x, y } | Gesture::LongPress { x, y } | Gesture::Drag { x, y } => {
                let (px, py) = (x as i32, y as i32);
                match self.settings.split.rotation {
                    270 => {
                        let vx = (h - 1).saturating_sub(py);
                        let vy = px;
                        (vx, vy, None)
                    }
                    90 => {
                        let vx = py;
                        let vy = (w - 1).saturating_sub(px);
                        (vx, vy, None)
                    }
                    _ => (px, py, None),
                }
            }
            Gesture::Swipe { dir, x, y, .. } => {
                let (vx, vy, _) = self.map_gesture(Gesture::Tap { x, y });
                let v_dir = match self.settings.split.rotation {
                    270 => match dir {
                        SwipeDir::North => SwipeDir::West,
                        SwipeDir::South => SwipeDir::East,
                        SwipeDir::East => SwipeDir::South,
                        SwipeDir::West => SwipeDir::North,
                    },
                    90 => match dir {
                        SwipeDir::North => SwipeDir::East,
                        SwipeDir::South => SwipeDir::West,
                        SwipeDir::East => SwipeDir::North,
                        SwipeDir::West => SwipeDir::South,
                    },
                    _ => dir,
                };
                (vx, vy, Some(v_dir))
            }
            _ => (0, 0, None),
        }
    }

    fn pre_cache_neighbors(&mut self, w: u32, h: u32) {
        let total_steps = self.settings.split.total_steps(self.total);
        let cur_step = self.settings.split.page_sub_to_step(self.page_no, self.sub_idx);
        let book_name = self.book_name();
        let settings = self.settings;
        let Some(doc) = &mut self.doc else { return };

        // Next page
        if cur_step + 1 < total_steps {
            let (next_p, next_s) = settings.split.step_to_page_sub(cur_step + 1);
            if crate::cache::load_snapshot(&book_name, next_p, next_s, &settings, w, h).is_none() {
                if let Some(gray) = render_page(doc, next_p, next_s, &settings, w, h) {
                    crate::cache::save_snapshot(&book_name, next_p, next_s, &settings, w, h, &gray);
                }
            }
        }

        // Previous page
        if cur_step > 0 {
            let (prev_p, prev_s) = settings.split.step_to_page_sub(cur_step - 1);
            if crate::cache::load_snapshot(&book_name, prev_p, prev_s, &settings, w, h).is_none() {
                if let Some(gray) = render_page(doc, prev_p, prev_s, &settings, w, h) {
                    crate::cache::save_snapshot(&book_name, prev_p, prev_s, &settings, w, h, &gray);
                }
            }
        }
    }

    fn compute_annotations(&mut self) {
        self.page_words.clear();
        self.page_annotations.clear();
        self.page_links.clear();

        let Some(doc) = &self.doc else { return };
        let Ok(page) = doc.load_page(self.page_no as i32) else { return };
        let Some(geom) = crate::render::LayoutGeom::new(
            &self.settings,
            page.bounds().unwrap_or_default(),
            self.sub_idx,
            self.w,
            self.h,
        ) else {
            return;
        };
        let Ok(tp) = page.to_text_page(mupdf::TextPageFlags::empty()) else { return };

        self.page_links = crate::render::links_from_page(&page, &geom);
        self.page_words = crate::render::words_from_text_page(&tp, &geom);

        // Budget annotations: take highest-difficulty words up to max_per_page
        let mut candidate_entries: Vec<(RectF, crate::vocab::WordEntry)> = Vec::new();
        if let Some(db) = &self.vocab_db {
            for (word, r) in &self.page_words {
                if let Some(entry) = db.lookup(word) {
                    if self.vocab_prof.should_annotate(&entry) {
                        candidate_entries.push((*r, entry));
                    }
                }
            }
        }
        candidate_entries.sort_by(|a, b| b.1.difficulty.cmp(&a.1.difficulty));
        self.page_annotations = candidate_entries
            .into_iter()
            .take(self.vocab_prof.max_per_page)
            .collect();
    }

    fn find_word_at_pos(&self, vx: f32, vy: f32) -> Option<(String, RectF)> {
        self.word_idx_at_pos(vx, vy)
            .map(|i| (self.page_words[i].0.clone(), self.page_words[i].1))
    }

    /// Index of the word under (vx, vy) — the selection primitive: spans
    /// are word indices, so finger imprecision disappears into snapping.
    fn word_idx_at_pos(&self, vx: f32, vy: f32) -> Option<usize> {
        self.page_words.iter().position(|(_, r)| {
            vx >= r.x0 - 12.0 && vx <= r.x1 + 12.0 && vy >= r.y0 - 12.0 && vy <= r.y1 + 12.0
        })
    }

    /// Commit the pending selection to the notes store and underline it.
    fn sel_save(&mut self) -> Action {
        let Some(sel) = self.sel.take() else {
            return Action::Keep;
        };
        let (lo, hi) = sel.span();
        let text = self.page_words[lo..=hi]
            .iter()
            .map(|(w, _)| w.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if crate::notes::add(&self.book_name(), self.page_no, &text) {
            let head: String = text.chars().take(48).collect();
            plog(&format!("highlight: p{} '{}…'", self.page_no + 1, head));
            self.highlights = crate::notes::load(&self.book_name());
        }
        Action::Redraw
    }

    fn find_link_at_pos(&self, vx: f32, vy: f32) -> Option<&(RectF, String)> {
        self.page_links.iter().find(|(r, _)| {
            vx >= r.x0 - 15.0 && vx <= r.x1 + 15.0 && vy >= r.y0 - 15.0 && vy <= r.y1 + 15.0
        })
    }

    fn open_word_dialog(&mut self, entry: crate::vocab::WordEntry) -> Action {
        dialogs::word_dialog(entry, self.vocab_prof.clone(), self.page_gray.clone())
    }
}

impl Screen for ReaderScreen {
    fn default_edges(&self) -> bool {
        // Handle edges internally to seamlessly support landscape + custom bottom-left settings
        false
    }

    fn on_enter(&mut self) -> Action {
        // Deliberately no keep_awake hold: input-idle suspend while
        // reading is the approved policy (page turns reset powerd's
        // t2; stillness means the reader put the device down). The
        // App's resume hook makes the wake seamless.
        self.time_str = chrome::current_time_str();
        self.save_progress();

        let pos = positions::resume_pos(&self.book_name());
        if let Some(s) = pos.settings {
            if s != self.settings {
                self.settings = s;
                self.sub_idx = 0;
                self.page_gray = None;
            }
        }

        // Warm cache check
        if let Ok(mut warm) = WARM.lock() {
            if let Some((p, SendDoc(doc), total, font_sz)) = warm.take() {
                if p == self.path && (font_sz - self.settings.font_size).abs() < 0.01 {
                    plog(&format!(
                        "book warm: instant open (rss={})",
                        doc_store::rss_mib()
                    ));
                    self.doc = Some(Rc::new(doc));
                    self.total = total;
                    self.scan_toc_chapters();
                    self.highlights = crate::notes::load(&self.book_name());
                    self.save_progress();
                    return Action::Redraw;
                }
            }
        }
        self.loading = Some(doc_store::open_async(
            self.path.clone(),
            self.w,
            self.h,
            self.settings.font_size,
            self.settings.margin_pad,
        ));
        Action::Redraw
    }

    fn on_leave(&mut self) {
        if let Some(doc_rc) = self.doc.take() {
            if let Ok(doc) = Rc::try_unwrap(doc_rc) {
                if let Ok(mut warm) = WARM.lock() {
                    *warm = Some((
                        self.path.clone(),
                        SendDoc(doc),
                        self.total,
                        self.settings.font_size,
                    ));
                }
            }
        }
        plog(&format!(
            "book closed rss={} avail={}",
            doc_store::rss_mib(),
            doc_store::avail_mib()
        ));
    }

    fn on_resume(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        self.vocab_prof = crate::vocab::VocabProfile::load();

        let pos = positions::resume_pos(&self.book_name());
        let page_changed = pos.page != self.page_no || pos.sub_idx != self.sub_idx;
        if page_changed {
            self.page_no = pos.page;
            self.sub_idx = pos.sub_idx;
            self.page_gray = None;
            self.page_words.clear();
            self.page_annotations.clear();
            self.page_start_time = Instant::now();
        }

        if let Some(s) = pos.settings {
            let font_changed = (s.font_size - self.settings.font_size).abs() > 0.01
                || s.margin_pad != self.settings.margin_pad;
            if font_changed {
                self.page_words.clear();
                self.page_annotations.clear();
            }
            self.settings = s;

            if font_changed && !self.is_pdf() {
                self.sub_idx = 0;
                self.page_gray = None;
                // In-memory instant reflow without re-reading/re-parsing ZIP archive from disk
                if let Some(doc_rc) = self.doc.take() {
                    if let Ok(doc) = Rc::try_unwrap(doc_rc) {
                        self.loading = Some(doc_store::reflow_async(
                            SendDoc(doc),
                            self.w,
                            self.h,
                            self.settings.font_size,
                            self.settings.margin_pad,
                        ));
                    } else {
                        self.loading = Some(doc_store::open_async(
                            self.path.clone(),
                            self.w,
                            self.h,
                            self.settings.font_size,
                            self.settings.margin_pad,
                        ));
                    }
                } else {
                    self.loading = Some(doc_store::open_async(
                        self.path.clone(),
                        self.w,
                        self.h,
                        self.settings.font_size,
                        self.settings.margin_pad,
                    ));
                }
                return Action::Redraw;
            }
        }
        if page_changed {
            Action::RedrawFull
        } else {
            Action::Redraw
        }
    }

    fn tick_interval(&self) -> std::time::Duration {
        if self.loading.is_some() {
            std::time::Duration::from_millis(150)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    fn on_tick(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        let Some(rx) = &self.loading else {
            return Action::Keep;
        };
        match rx.try_recv() {
            Ok(Ok(BookReady { doc, total })) => {
                self.doc = Some(Rc::new(doc.0));
                self.total = total;
                self.scan_toc_chapters();
                self.highlights = crate::notes::load(&self.book_name());
                self.loading = None;

                if self.pending_turns != 0 {
                    let steps = self.settings.split.total_steps(self.total);
                    let cur_step = self.settings.split.page_sub_to_step(self.page_no, self.sub_idx);
                    let target_step =
                        (cur_step as i32 + self.pending_turns).clamp(0, steps.saturating_sub(1) as i32)
                            as usize;
                    let (target_page, target_sub) = self.settings.split.step_to_page_sub(target_step);
                    self.page_no = target_page;
                    self.sub_idx = target_sub;
                    self.pending_turns = 0;
                }
                self.save_progress();
                Action::Redraw
            }
            Ok(Err(e)) => {
                self.err = Some(e);
                self.loading = None;
                Action::Redraw
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Action::Keep,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.err = Some("Loader thread disconnected".to_string());
                self.loading = None;
                Action::Redraw
            }
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        let is_night = self.settings.invert;
        let bg_color = if is_night { 0 } else { 255 };
        let fg_color = if is_night { 200 } else { 90 };
        p.clear(bg_color);

        if let Some(err) = &self.err {
            p.text_center(h / 2, 10.0, fg_color, err);
            return;
        }

        // If no cached snapshot exists AND doc is still loading:
        if self.page_gray.is_none() && self.doc.is_none() {
            p.text_center(h / 2, 10.0, fg_color, "Opening…");
            let name = p.truncate(8.0, &self.book_name(), p.width_pt() - 24.0);
            p.text_center(h / 2 + pt(16.0), 8.0, fg_color, &name);
            return;
        }

        if let Some(doc) = &mut self.doc {
            if self.page_gray.is_none() {
                let t0 = Instant::now();
                let page = render_page(
                    doc,
                    self.page_no,
                    self.sub_idx,
                    &self.settings,
                    w as u32,
                    h as u32,
                );
                plog(&format!(
                    "render page {}.{}: {}ms rss={}",
                    self.page_no,
                    self.sub_idx,
                    t0.elapsed().as_millis(),
                    doc_store::rss_mib()
                ));
                match page {
                    Some(gray) => {
                        // Persist snapshot to disk cache for instant resume
                        crate::cache::save_snapshot(
                            &self.book_name(),
                            self.page_no,
                            self.sub_idx,
                            &self.settings,
                            w as u32,
                            h as u32,
                            &gray,
                        );
                        self.page_gray = Some(gray);
                    }
                    None => {
                        self.err = Some("Render failed".to_string());
                        p.text_center(h / 2, 10.0, fg_color, "Render failed");
                        return;
                    }
                }
            }
        }

        if let Some(gray) = &self.page_gray {
            p.blit_gray(0, 0, w, h, gray, w as usize);
        }

        // Trigger neighbor pre-caching and Word Wise annotation extraction
        if self.doc.is_some() {
            self.pre_cache_neighbors(w as u32, h as u32);
            if self.page_words.is_empty() {
                self.compute_annotations();
            }
        }

        // Word Wise annotations in the profile's style
        crate::vocab::draw_annotations(
            p,
            &self.page_annotations,
            self.vocab_prof.style,
            is_night,
        );

        // Header status line + progress footer
        if self.settings.show_header {
            chrome::draw_header(p, &self.time_str, &self.book_name(), &self.settings, is_night);
        }
        let time_left = chrome::time_left_str(
            self.total,
            self.page_no,
            &self.toc_chapters,
            self.avg_secs_per_page,
        );
        let footer = chrome::footer_str(
            self.loading.is_some(),
            self.page_no,
            self.sub_idx,
            self.total,
            &self.settings,
            &time_left,
        );
        chrome::draw_footer(p, &footer, self.settings.split.rotation, is_night);

        // Saved highlights: solid light underlines, page-gated and matched
        // by word sequence within the page. (A span that crosses a
        // split-mode column boundary won't fully match either sub-page —
        // acceptable; it still exports from the store.)
        if !self.page_words.is_empty() {
            for (lo, hi) in crate::notes::matched_spans(
                &self.highlights,
                self.page_no,
                &self.page_words,
            ) {
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

        // Selection-mode ribbon + the pending selection itself
        chrome::draw_bookmark_ribbon(p, w, self.sel_mode);
        if let Some(sel) = &self.sel {
            selection::draw_selection(p, sel, &self.page_words);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        if self.err.is_some() {
            return Action::Pop;
        }

        let is_landscape = self.settings.split.is_landscape();

        let (vis_w, vis_h) = if is_landscape {
            (self.dims.1, self.dims.0) // 1648 x 1236
        } else {
            (self.dims.0, self.dims.1) // 1236 x 1648
        };

        let (vx, vy, v_dir) = self.map_gesture(g);

        if let Some(dir) = v_dir {
            // Any swipe cancels a pending selection (mode stays on).
            let had_sel = self.sel.take().is_some();
            // Visual Swipes
            let act = match dir {
                SwipeDir::West => self.turn(true),  // swipe left -> forward
                SwipeDir::East => self.turn(false), // swipe right -> back
                SwipeDir::South => {
                    // Top swipe down -> Curtain (brightness/control center)
                    if vy < vis_h * 20 / 100 {
                        self.open_curtain()
                    } else {
                        self.open_settings_dialog()
                    }
                }
                SwipeDir::North => {
                    // Bottom swipes:
                    // 1. Bottom-left swipe up -> Reader Settings (font size, margin, contrast, vocab)
                    if vx < vis_w * 35 / 100 && vy > vis_h * 70 / 100 {
                        self.open_settings_dialog()
                    } else if vx >= vis_w * 35 / 100 && vx <= vis_w * 65 / 100 && vy > vis_h * 70 / 100 {
                        // 2. Bottom-center swipe up -> Table of Contents (Chapters)!
                        self.open_toc_dialog()
                    } else if vx > vis_w * 65 / 100 && vy > vis_h * 70 / 100 {
                        // 3. Bottom-right swipe up -> Back to Library
                        Action::Pop
                    } else {
                        Action::Keep
                    }
                }
            };
            // A cancelled selection must repaint even when the swipe itself
            // was a no-op, or the bar lingers on screen.
            if had_sel && matches!(act, Action::Keep) {
                return Action::Redraw;
            }
            return act;
        }

        match g {
            Gesture::LongPress { .. } => {
                // Selection mode: a long-press anchors a selection (or
                // re-anchors a pending one — then dragging extends again).
                if let Some(i) = self.word_idx_at_pos(vx as f32, vy as f32) {
                    if self.sel.is_some() || self.sel_mode {
                        self.sel = Some(SelState::new(i));
                        return Action::Redraw;
                    }
                }

                // 1. Check if an interactive link or footnote was held
                if let Some((_, uri)) = self.find_link_at_pos(vx as f32, vy as f32) {
                    let uri_cl = uri.clone();
                    return self.open_footnote_or_link(&uri_cl);
                }

                // 2. Word Long Press: Check if user held on a word on the page for definition / translation
                if let Some((word_text, _rect)) = self.find_word_at_pos(vx as f32, vy as f32) {
                    // Check if word is an asterisk or footnote marker
                    if word_text.starts_with('*') || word_text.starts_with('[') {
                        if let Some((_, uri)) = self.find_link_at_pos(vx as f32, vy as f32) {
                            let uri_cl = uri.clone();
                            return self.open_footnote_or_link(&uri_cl);
                        }
                    }

                    let found = self
                        .vocab_db
                        .as_ref()
                        .and_then(|db| db.lookup(&word_text));
                    if let Some(entry) = found {
                        return self.open_word_dialog(entry);
                    }
                    // Not in the dictionary: say so — a silent no-op on a
                    // deliberate long-press reads as broken, not as "no
                    // entry".
                    return dialogs::dict_miss(&word_text, self.page_gray.clone());
                }
                Action::Keep
            }
            Gesture::Tap { .. } => {
                // 0a. Bookmark (top-right, left of the battery) toggles
                // selection mode. Checked before the clean-refresh corner
                // zone it sits inside; only the icon's own strip is
                // carved out.
                if vx > vis_w - pt(64.0) && vy < pt(34.0) {
                    self.sel_mode = !self.sel_mode;
                    self.sel = None;
                    return Action::RedrawFull;
                }

                // 0b. Pending selection: bar buttons, or tap sets the end
                // (extend / shrink / flip — one operation). Everything
                // else is inert so a stray tap can't turn the page and
                // eat the selection.
                if self.sel.is_some() {
                    let (bar, ok_btn, x_btn) = sel_bar_rects(self.dims.0, self.dims.1);
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

                // 1. Visual Top-Left corner -> Back to Library
                if vx < 240 && vy < 160 {
                    return Action::Pop;
                }

                // 2. Visual Top-Right corner -> Full Screen Refresh (clean flash)
                if vx > vis_w - 240 && vy < 160 {
                    return Action::RedrawFull;
                }

                // 3. Visual Bottom-Right corner -> Back to Library
                if vx > vis_w - 240 && vy > vis_h - 160 {
                    return Action::Pop;
                }

                // 4. Visual Bottom Footer Strip -> Open Interactive Page Scrubber & "Go to Page"
                if vy > vis_h - 140 && vx > 240 && vx < vis_w - 240 {
                    return self.open_scrubber_dialog();
                }

                // 5. Visual Top strip (middle) -> Curtain (Brightness / Network / Controls)
                if vy < 140 && vx > 240 && vx < vis_w - 240 {
                    return self.open_curtain();
                }

                // 6. Page turns (Left third = Back, Right two-thirds = Forward)
                if vx < vis_w / 3 {
                    self.turn(false)
                } else {
                    self.turn(true)
                }
            }
            Gesture::TwoFingerTap => {
                // Two-finger tap anywhere -> Quick Curtain / Brightness
                self.open_curtain()
            }
            Gesture::Drag { .. } => {
                // Finger still down after the anchoring long-press: extend
                // the span word by word (reading order; backtracking
                // shrinks, crossing the anchor flips).
                let idx = self.word_idx_at_pos(vx as f32, vy as f32);
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
