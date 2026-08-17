//! Local library + EPUB/PDF reader screens via MuPDF (the same engine
//! family KOReader uses). EPUB is reflowed to the panel width; pages
//! render in grayscale and are CACHED as pixels — re-presenting a page
//! after an overlay (frontlight/curtain) is a blit, never a MuPDF re-render.
//!
//! Includes Onyx Boox–style Article Mode & Multi-Split reading for PDFs,
//! instant 8-bit LUT Contrast Curves / Text Boldness, Paper Whitening,
//! Invert (Night Mode), Reflowable Font Size scaling, and Header Clock/Battery.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Mutex;
use std::time::Instant;

use mupdf::{Colorspace, Document, Matrix};
use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;
use ybdev::sysinfo;

use crate::curtain::CurtainScreen;
use crate::positions;
use crate::settings_dialog::ReaderSettingsDialog;
use crate::split::{RectF, ReaderSettings};

use crate::wifi;

use yui::painter::{pt, Painter};
use yui::screen::{Action, Screen};

const LIB_DIR: &str = "/mnt/us/documents";
const HEADER_H: u32 = 48; // px
const FOOTER_H: u32 = 72; // px


/// Files in documents that carry a book-ish extension but belong to the
/// framework (clippings ledger) or the jailbreak — not library entries.
const SYSTEM_FILES: [&str; 2] = ["My Clippings.txt", "JAILBROKEN.txt"];

pub fn list_books() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(LIB_DIR) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if SYSTEM_FILES.contains(&name.as_str()) {
                continue;
            }
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(
                ext.as_str(),
                "epub" | "pdf" | "mobi" | "azw3" | "fb2" | "txt" | "cbz"
            ) {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

// ---- async open ---------------------------------------------------------

struct BookReady {
    doc: SendDoc,
    total: usize,
}

/// mupdf-rs guards every call with the global BASE_CONTEXT mutex, so a
/// Document handle is safe to move across threads: all uses serialize.
struct SendDoc(Document);
unsafe impl Send for SendDoc {}

static WARM: Mutex<Option<(PathBuf, SendDoc, usize, f32)>> = Mutex::new(None);

fn reflow_async(
    mut send_doc: SendDoc,
    w: u32,
    h: u32,
    font_size: f32,
    margin_pad: u32,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let avail_w = (w - 2 * margin_pad) as f32 * 72.0 / 300.0;
        let avail_h = (h - 2 * margin_pad - FOOTER_H - HEADER_H) as f32 * 72.0 / 300.0;
        let _ = send_doc.0.layout(avail_w, avail_h, font_size);
        let total = send_doc.0.page_count().unwrap_or(1).max(1) as usize;
        plog(&format!(
            "book in-memory reflow in {}ms ({} pages, font={:.1}pt) rss={} avail={}",
            t0.elapsed().as_millis(),
            total,
            font_size,
            rss_mib(),
            avail_mib()
        ));
        let _ = tx.send(Ok(BookReady {
            doc: send_doc,
            total,
        }));
    });
    rx
}



fn open_async(
    path: PathBuf,
    w: u32,
    h: u32,
    font_size: f32,
    margin_pad: u32,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let r = (|| -> Result<BookReady, String> {
            let mut doc = Document::open(path.as_os_str()).map_err(|e| e.to_string())?;
            let open_ms = t0.elapsed().as_millis();
            // Reflow to the reading area (no-op for fixed-layout docs).
            let avail_w = (w - 2 * margin_pad) as f32 * 72.0 / 300.0;
            let avail_h = (h - 2 * margin_pad - FOOTER_H - HEADER_H) as f32 * 72.0 / 300.0;
            let _ = doc.layout(avail_w, avail_h, font_size);
            let total = doc.page_count().unwrap_or(1).max(1) as usize;
            plog(&format!(
                "book open {}ms + layout {}ms ({} pages, font={:.1}pt) rss={} avail={}",
                open_ms,
                t0.elapsed().as_millis() - open_ms,
                total,
                font_size,
                rss_mib(),
                avail_mib()
            ));
            Ok(BookReady {
                doc: SendDoc(doc),
                total,
            })
        })();
        let _ = tx.send(r);
    });
    rx
}


// ---- ReaderScreen -------------------------------------------------------

pub struct ReaderScreen {
    path: PathBuf,
    w: u32,
    h: u32,
    loading: Option<Receiver<Result<BookReady, String>>>,
    doc: Option<Document>,
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
    vocab_db: Option<crate::vocab::VocabDb>,
    vocab_prof: crate::vocab::VocabProfile,
    page_words: Vec<(String, RectF)>,
    page_annotations: Vec<(RectF, crate::vocab::WordEntry)>,
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
            time_str: current_time_str(),
            vocab_db,
            vocab_prof,
            page_words: Vec::new(),
            page_annotations: Vec::new(),
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

        let (new_page, new_sub) = self.settings.split.step_to_page_sub(next_step);

        if self.doc.is_none() {
            // If background-loading, check if the neighbor page is already in snapshot cache!
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




    fn open_settings_dialog(&mut self) -> Action {
        let settings = self.settings;
        let is_pdf = self.is_pdf();
        let samples = self.doc.as_ref().and_then(|doc| {
            let page = doc.load_page(self.page_no as i32).ok()?;
            let m = Matrix::new_scale(1.0, 1.0);
            let pm = page.to_pixmap(&m, &Colorspace::device_gray(), false, true).ok()?;
            Some((
                pm.samples().to_vec(),
                pm.width() as usize,
                pm.height() as usize,
                pm.stride() as usize,
            ))
        });

        let path_name = self.book_name();
        let page_no = self.page_no;
        let total = self.total;

        Action::Push(Box::new(ReaderSettingsDialog::new(
            settings,
            is_pdf,
            samples,
            move |new_settings| {
                positions::record_pos(&path_name, page_no, total, 0, Some(new_settings));
                Action::Pop
            },
        )))
    }

    fn open_curtain(&mut self) -> Action {
        Action::Push(Box::new(CurtainScreen::new()))
    }

    /// Map physical touch/swipe event to visual orientation coordinates.
    /// Returns (visual_x, visual_y, visual_swipe_dir).
    fn map_gesture(&self, g: Gesture) -> (i32, i32, Option<SwipeDir>) {
        let (w, h) = self.dims; // w=1236, h=1648
        match g {
            Gesture::Tap { x, y } => {
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

        let Some(doc) = &self.doc else { return };
        let Ok(page) = doc.load_page(self.page_no as i32) else { return };
        let Ok(tp) = page.to_text_page(mupdf::TextPageFlags::empty()) else { return };

        let bounds = page.bounds().unwrap_or_default();
        let scale_x = self.w as f32 / bounds.width().max(1.0);
        let scale_y = self.h as f32 / bounds.height().max(1.0);

        let mut candidate_entries: Vec<(RectF, crate::vocab::WordEntry)> = Vec::new();

        for block in tp.blocks() {
            for line in block.lines() {
                let mut cur_word = String::new();
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;

                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        if c.is_whitespace() {
                            if !cur_word.is_empty() {
                                let r = RectF::new(
                                    min_x * scale_x,
                                    min_y * scale_y,
                                    max_x * scale_x,
                                    max_y * scale_y,
                                );
                                self.page_words.push((cur_word.clone(), r));
                                if let Some(db) = &self.vocab_db {
                                    if let Some(entry) = db.lookup(&cur_word) {
                                        if self.vocab_prof.should_annotate(&entry) {
                                            candidate_entries.push((r, entry));
                                        }
                                    }
                                }
                                cur_word.clear();
                                min_x = f32::MAX;
                                min_y = f32::MAX;
                                max_x = f32::MIN;
                                max_y = f32::MIN;
                            }
                        } else {
                            cur_word.push(c);
                            let q = ch.quad();
                            min_x = min_x.min(q.ul.x).min(q.ll.x);
                            min_y = min_y.min(q.ul.y).min(q.ur.y);
                            max_x = max_x.max(q.ur.x).max(q.lr.x);
                            max_y = max_y.max(q.ll.y).max(q.lr.y);
                        }
                    }
                }
                if !cur_word.is_empty() {
                    let r = RectF::new(
                        min_x * scale_x,
                        min_y * scale_y,
                        max_x * scale_x,
                        max_y * scale_y,
                    );
                    self.page_words.push((cur_word.clone(), r));
                    if let Some(db) = &self.vocab_db {
                        if let Some(entry) = db.lookup(&cur_word) {
                            if self.vocab_prof.should_annotate(&entry) {
                                candidate_entries.push((r, entry));
                            }
                        }
                    }
                }
            }
        }

        // Budget annotations: take highest-difficulty words up to max_per_page
        candidate_entries.sort_by(|a, b| b.1.difficulty.cmp(&a.1.difficulty));
        self.page_annotations = candidate_entries
            .into_iter()
            .take(self.vocab_prof.max_per_page)
            .collect();
    }

    fn find_word_at_pos(&self, vx: f32, vy: f32) -> Option<(String, RectF)> {
        for (w, r) in &self.page_words {
            // Hit test with generous touch padding
            if vx >= r.x0 - 8.0 && vx <= r.x1 + 8.0 && vy >= r.y0 - 8.0 && vy <= r.y1 + 8.0 {
                return Some((w.clone(), *r));
            }
        }
        None
    }


    fn open_word_dialog(&mut self, entry: crate::vocab::WordEntry) -> Action {
        let mut prof = self.vocab_prof.clone();
        let word = entry.word.clone();
        let diff = entry.difficulty;

        Action::Push(Box::new(crate::word_dialog::WordDialog::new(
            entry,
            move |action| {
                match action {
                    crate::word_dialog::WordAction::StarLearning => {
                        prof.record_lookup(&word, diff);
                    }
                    crate::word_dialog::WordAction::MarkKnown => {
                        prof.mark_known(&word, diff);
                    }
                    crate::word_dialog::WordAction::Close => {}
                }
                Action::Pop
            },
        )))
    }
}


impl Screen for ReaderScreen {
    fn default_edges(&self) -> bool {
        // Handle edges internally to seamlessly support landscape + custom bottom-left settings
        false
    }

    fn on_enter(&mut self) -> Action {
        wifi::keep_awake(true);
        self.time_str = current_time_str();
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
                    plog(&format!("book warm: instant open (rss={})", rss_mib()));
                    self.doc = Some(doc);
                    self.total = total;
                    self.save_progress();
                    return Action::Redraw;
                }
            }
        }
        self.loading = Some(open_async(
            self.path.clone(),
            self.w,
            self.h,
            self.settings.font_size,
            self.settings.margin_pad,
        ));
        Action::Redraw
    }

    fn on_leave(&mut self) {
        wifi::keep_awake(false);
        if let Some(doc) = self.doc.take() {
            if let Ok(mut warm) = WARM.lock() {
                *warm = Some((
                    self.path.clone(),
                    SendDoc(doc),
                    self.total,
                    self.settings.font_size,
                ));
            }
        }
        plog(&format!(
            "book closed rss={} avail={}",
            rss_mib(),
            avail_mib()
        ));
    }

    fn on_resume(&mut self) -> Action {
        self.time_str = current_time_str();
        let pos = positions::resume_pos(&self.book_name());
        if let Some(s) = pos.settings {
            let font_changed = (s.font_size - self.settings.font_size).abs() > 0.01
                || s.margin_pad != self.settings.margin_pad;
            self.settings = s;
            self.sub_idx = 0;
            self.page_gray = None;

            if font_changed && !self.is_pdf() {
                // In-memory instant reflow without re-reading/re-parsing ZIP archive from disk
                if let Some(doc) = self.doc.take() {
                    self.loading = Some(reflow_async(
                        SendDoc(doc),
                        self.w,
                        self.h,
                        self.settings.font_size,
                        self.settings.margin_pad,
                    ));
                } else {
                    self.loading = Some(open_async(
                        self.path.clone(),
                        self.w,
                        self.h,
                        self.settings.font_size,
                        self.settings.margin_pad,
                    ));
                }
                return Action::Redraw;
            }
            return Action::Redraw;
        }
        Action::Redraw
    }


    fn tick_interval(&self) -> std::time::Duration {
        if self.loading.is_some() {
            std::time::Duration::from_millis(150)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    fn on_tick(&mut self) -> Action {
        self.time_str = current_time_str();
        let Some(rx) = &self.loading else {
            return Action::Keep;
        };
        match rx.try_recv() {
            Ok(Ok(ready)) => {
                self.loading = None;
                self.total = ready.total;
                let SendDoc(doc) = ready.doc;
                self.doc = Some(doc);

                // Drain any pending fast turns queued during background loading
                if self.pending_turns != 0 {
                    let total_steps = self.settings.split.total_steps(self.total);
                    let cur_step = self
                        .settings
                        .split
                        .page_sub_to_step(self.page_no, self.sub_idx);
                    let target_step = (cur_step as i32 + self.pending_turns)
                        .clamp(0, (total_steps as i32).saturating_sub(1)) as usize;
                    let (new_page, new_sub) = self.settings.split.step_to_page_sub(target_step);
                    self.page_no = new_page;
                    self.sub_idx = new_sub;
                    self.page_gray = None;
                    self.pending_turns = 0;
                }

                self.save_progress();
                Action::Redraw
            }
            Ok(Err(e)) => {
                plog(&format!("open {}: {}", self.path.display(), e));
                self.loading = None;
                self.err = Some("Could not open book".to_string());
                Action::Redraw
            }
            Err(TryRecvError::Empty) => Action::Keep,
            Err(TryRecvError::Disconnected) => {
                self.loading = None;
                self.err = Some("Could not open book".to_string());
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
                    rss_mib()
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


        // Render Word Wise Inline Annotations
        if self.vocab_prof.style == crate::vocab::AnnotationStyle::Interlinear {
            for (r, entry) in &self.page_annotations {
                let gloss = p.truncate(5.5, &entry.gloss_en, 180.0);
                let gx = r.x0.round() as i32;
                let gy = (r.y0 - 4.0).max(pt(20.0) as f32).round() as i32;
                p.text(gx, gy, 5.5, if is_night { 190 } else { 75 }, &gloss);
            }
        } else if self.vocab_prof.style == crate::vocab::AnnotationStyle::Margin {
            let mut my = h - pt(28.0);
            for (_, entry) in self.page_annotations.iter().take(2) {
                let line = format!("• {}: {}", entry.word, entry.gloss_en);
                let trunc = p.truncate(7.0, &line, p.width_pt() - 32.0);
                p.text(pt(16.0), my, 7.0, if is_night { 190 } else { 85 }, &trunc);
                my += pt(10.0);
            }
        }

        // Header Status Line (Clock + Battery + Title)
        if self.settings.show_header {
            let (bat_cap, _) = sysinfo::battery();
            let bat_str = format!("{}%", bat_cap);
            let title_trunc = p.truncate(7.0, &self.book_name(), p.width_pt() - 70.0);

            if self.settings.split.is_landscape() {
                // Header in landscape orientation
                let rot = self.settings.split.rotation;
                let header_text = format!("{} · {} · {}", self.time_str, title_trunc, bat_str);
                let cx = if rot == 270 { pt(10.0) } else { w - pt(10.0) };
                p.text_center_rotated(cx, h / 2, 6.5, fg_color, &header_text, rot);
            } else {
                // Header in portrait
                p.text(pt(16.0), pt(14.0), 7.0, fg_color, &self.time_str);
                p.text_center(pt(14.0), 7.0, fg_color, &title_trunc);
                p.text_right(w - pt(16.0), pt(14.0), 7.0, fg_color, &bat_str);
                p.hline_t(pt(20.0), pt(16.0), w - pt(16.0), 1, if is_night { 60 } else { 225 });
            }
        }

        // Bottom Footer Line (Reading Progress)
        let footer = if self.loading.is_some() {
            format!("page {} · Loading book…", self.page_no + 1)
        } else if self.settings.split.total_steps(self.total) > self.total {
            format!(
                "page {} ({}/{}) · {}/{}",
                self.page_no + 1,
                self.sub_idx + 1,
                self.settings.split.sub_box_count(),
                self.settings.split.page_sub_to_step(self.page_no, self.sub_idx) + 1,
                self.settings.split.total_steps(self.total)
            )
        } else {
            format!("page {} / {}", self.page_no + 1, self.total)
        };


        match self.settings.split.rotation {
            270 => {
                p.text_center_rotated(w - pt(10.0), h / 2, 7.0, fg_color, &footer, 270);
            }
            90 => {
                p.text_center_rotated(pt(10.0), h / 2, 7.0, fg_color, &footer, 90);
            }
            _ => {
                p.text_center(h - pt(10.0), 7.0, fg_color, &footer);
            }
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
            // Visual Swipes
            return match dir {
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
                    // Bottom-left swipe up -> Reader Settings!
                    if vx < vis_w * 40 / 100 && vy > vis_h * 75 / 100 {
                        self.open_settings_dialog()
                    } else if vx > vis_w * 60 / 100 && vy > vis_h * 75 / 100 {
                        // Bottom-right swipe up -> Back to Library
                        Action::Pop
                    } else {
                        Action::Keep
                    }
                }
            };
        }

        match g {
            Gesture::Tap { .. } => {
                // 1. Visual Top-Left corner -> Back to Library
                if vx < 240 && vy < 160 {
                    return Action::Pop;
                }

                // 2. Visual Bottom-Left corner -> Reader Settings Dialog
                if vx < 240 && vy > vis_h - 160 {
                    return self.open_settings_dialog();
                }

                // 3. Visual Bottom-Right corner -> Back to Library
                if vx > vis_w - 240 && vy > vis_h - 160 {
                    return Action::Pop;
                }

                // 4. Visual Top-Right corner -> Full Screen Refresh (clean flash)
                if vx > vis_w - 240 && vy < 160 {
                    return Action::RedrawFull;
                }

                // 5. Visual Top strip or Center -> Reader Settings Dialog
                let is_top_strip = vy < vis_h * 16 / 100 && vx > vis_w * 25 / 100 && vx < vis_w * 75 / 100;
                let is_center = vx > vis_w * 35 / 100
                    && vx < vis_w * 65 / 100
                    && vy > vis_h * 35 / 100
                    && vy < vis_h * 65 / 100;

                if is_top_strip || is_center {
                    return self.open_settings_dialog();
                }

                // 6. Word Tap: Check if user tapped a word on the page for definition / translation
                if let Some((word_text, _rect)) = self.find_word_at_pos(vx as f32, vy as f32) {
                    if let Some(db) = &self.vocab_db {
                        if let Some(entry) = db.lookup(&word_text) {
                            return self.open_word_dialog(entry);
                        }
                    }
                }

                // 7. Page turns
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
            _ => Action::Keep,
        }
    }
}

fn current_time_str() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

fn rss_mib() -> String {
    sysinfo::rss_kib().map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}

fn avail_mib() -> String {
    sysinfo::mem_available_kib()
        .map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}

fn render_page(
    doc: &Document,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let page = doc.load_page(page_no as i32).ok()?;
    let bounds = page.bounds().ok()?;
    let pw = bounds.x1 - bounds.x0;
    let ph = bounds.y1 - bounds.y0;
    if pw <= 0.0 || ph <= 0.0 {
        return None;
    }

    let config = &settings.split;
    let boxes = config.sub_boxes();
    let sub_box = boxes
        .get(sub_idx)
        .copied()
        .unwrap_or(RectF::new(0.0, 0.0, 1.0, 1.0));

    let bw = sub_box.width() * pw;
    let bh = sub_box.height() * ph;

    let margin_pad = settings.margin_pad;
    let mut out = vec![255u8; (w as usize) * (h as usize)];

    let is_landscape = config.is_landscape();
    let (vis_w, vis_h) = if is_landscape {
        (
            (h - 2 * margin_pad) as f32,
            (w - 2 * margin_pad - FOOTER_H - HEADER_H) as f32,
        )
    } else {
        (
            (w - 2 * margin_pad) as f32,
            (h - 2 * margin_pad - FOOTER_H - HEADER_H) as f32,
        )
    };

    let zoom = (vis_w / bw).min(vis_h / bh);

    let mut m = Matrix::IDENTITY;
    m.scale(zoom, zoom);
    let pm = page
        .to_pixmap(&m, &Colorspace::device_gray(), false, true)
        .ok()?;

    let pm_w = pm.width() as usize;
    let pm_h = pm.height() as usize;
    let stride = pm.stride() as usize;
    let samples = pm.samples();

    let src_x = (sub_box.x0 * pw * zoom).round() as usize;
    let src_y = (sub_box.y0 * ph * zoom).round() as usize;
    let rw = ((bw * zoom).round() as usize).min(pm_w.saturating_sub(src_x));
    let rh = ((bh * zoom).round() as usize).min(pm_h.saturating_sub(src_y));

    if rw == 0 || rh == 0 {
        settings.apply_lut(&mut out);
        return Some(out);
    }

    let vis_ox = ((vis_w as usize).saturating_sub(rw)) / 2 + margin_pad as usize;
    let vis_oy = ((vis_h as usize).saturating_sub(rh)) / 2 + (HEADER_H + margin_pad) as usize;

    // Calculate dashed reading boundary line position (where previous sub-page ended)
    let dash_y = if sub_idx > 0 && config.sub_box_count() > 1 {
        let n = config.sub_box_count() as f32;
        let ov = config.overlap.clamp(0.0, 0.35);
        let overlap_frac = (n * ov) / (1.0 + (n - 1.0) * ov);
        let dy = (overlap_frac * rh as f32).round() as usize;
        if dy > 2 && dy < rh.saturating_sub(2) {
            Some(dy)
        } else {
            None
        }
    } else {
        None
    };

    match config.rotation {
        270 => {
            // 270° CW (USB bezel on left):
            // Visual X (0..rw) maps to physical Y: (h - 1 - (vis_ox + vx))
            // Visual Y (0..rh) maps to physical X: (vis_oy + vy)
            let w_u = w as usize;
            let h_u = h as usize;

            for vy in 0..rh {
                let px = vis_oy + vy;
                if px >= w_u {
                    continue;
                }
                let src_row_start = (src_y + vy) * stride + src_x;
                let is_dash_row = dash_y == Some(vy);

                for vx in 0..rw {
                    let py = (h_u - 1).saturating_sub(vis_ox + vx);
                    if py < h_u && src_row_start + vx < samples.len() {
                        let mut pixel = samples[src_row_start + vx];
                        if is_dash_row && (vx / 8) % 2 == 0 && pixel > 140 {
                            pixel = 140; // Subtle dotted guide line
                        }
                        out[py * w_u + px] = pixel;
                    }
                }
            }
        }

        90 => {
            // 90° CCW (USB bezel on right):
            // Visual X (0..rw) maps to physical Y: (vis_ox + vx)
            // Visual Y (0..rh) maps to physical X: (w - 1 - (vis_oy + vy))
            let w_u = w as usize;
            let h_u = h as usize;

            for vy in 0..rh {
                let px = (w_u - 1).saturating_sub(vis_oy + vy);
                if px >= w_u {
                    continue;
                }
                let src_row_start = (src_y + vy) * stride + src_x;
                let is_dash_row = dash_y == Some(vy);

                for vx in 0..rw {
                    let py = vis_ox + vx;
                    if py < h_u && src_row_start + vx < samples.len() {
                        let mut pixel = samples[src_row_start + vx];
                        if is_dash_row && (vx / 8) % 2 == 0 && pixel > 140 {
                            pixel = 140;
                        }
                        out[py * w_u + px] = pixel;
                    }
                }
            }
        }

        _ => {
            // 0° Portrait:
            let w_u = w as usize;
            let h_u = h as usize;

            for vy in 0..rh {
                let dst_y = vis_oy + vy;
                if dst_y >= h_u {
                    break;
                }
                let src_start = (src_y + vy) * stride + src_x;
                let dst_start = dst_y * w_u + vis_ox;
                let len = rw.min(w_u.saturating_sub(vis_ox));
                if src_start + len <= samples.len() && dst_start + len <= out.len() {
                    out[dst_start..dst_start + len]
                        .copy_from_slice(&samples[src_start..src_start + len]);

                    if dash_y == Some(vy) {
                        for vx in 0..len {
                            if (vx / 8) % 2 == 0 {
                                let p = &mut out[dst_start + vx];
                                if *p > 140 {
                                    *p = 140;
                                }
                            }
                        }
                    }
                }
            }
        }
    }


    // Apply Contrast / Whitening / Invert LUT
    settings.apply_lut(&mut out);

    Some(out)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::{SplitConfig, SplitPreset};


    #[test]
    fn test_render_subbox_portrait() {
        let pdf_path = "/tmp/sample_book.pdf";
        if !std::path::Path::new(pdf_path).exists() {
            return;
        }
        let doc = Document::open(pdf_path).expect("open doc");
        let settings = ReaderSettings::default();
        let rendered = render_page(&doc, 20, 0, &settings, 1236, 1648);
        assert!(rendered.is_some());
        assert_eq!(rendered.unwrap().len(), 1236 * 1648);
    }

    #[test]
    fn test_mupdf_page_text_methods() {
        let pdf_path = "/tmp/sample_book.pdf";
        if !std::path::Path::new(pdf_path).exists() {
            return;
        }
        let doc = Document::open(pdf_path).expect("open doc");
        let page = doc.load_page(20).expect("load page 20");
        let tp = page.to_text_page(mupdf::TextPageFlags::empty()).expect("to_text_page");

        let mut words = Vec::new();
        for block in tp.blocks() {
            for line in block.lines() {
                let mut cur_word = String::new();
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;

                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        if c.is_whitespace() {
                            if !cur_word.is_empty() {
                                words.push((cur_word.clone(), min_x, min_y, max_x, max_y));
                                cur_word.clear();
                                min_x = f32::MAX;
                                min_y = f32::MAX;
                                max_x = f32::MIN;
                                max_y = f32::MIN;
                            }
                        } else {
                            cur_word.push(c);
                            let q = ch.quad();
                            min_x = min_x.min(q.ul.x).min(q.ll.x);
                            min_y = min_y.min(q.ul.y).min(q.ur.y);
                            max_x = max_x.max(q.ur.x).max(q.lr.x);
                            max_y = max_y.max(q.ll.y).max(q.lr.y);
                        }
                    }
                }
                if !cur_word.is_empty() {
                    words.push((cur_word, min_x, min_y, max_x, max_y));
                }
            }
        }

        println!("Extracted {} words from page 20. First 10:", words.len());
        for (w, x0, y0, x1, y1) in words.iter().take(10) {
            println!("  '{}' at [{:.1}, {:.1}, {:.1}, {:.1}]", w, x0, y0, x1, y1);
        }
        assert!(!words.is_empty());
    }





    #[test]
    fn test_render_subbox_horizontal2_landscape() {
        let pdf_path = "/tmp/sample_book.pdf";
        if !std::path::Path::new(pdf_path).exists() {
            return;
        }
        let doc = Document::open(pdf_path).expect("open doc");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        let r0 = render_page(&doc, 20, 0, &settings, 1236, 1648);
        assert!(r0.is_some());
        let r1 = render_page(&doc, 20, 1, &settings, 1236, 1648);
        assert!(r1.is_some());
    }


    #[test]
    fn test_render_subbox_horizontal3_landscape() {
        let pdf_path = "/tmp/sample_book.pdf";
        if !std::path::Path::new(pdf_path).exists() {
            return;
        }
        let doc = Document::open(pdf_path).expect("open doc");
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal3);
        for sub in 0..3 {
            let r = render_page(&doc, 20, sub, &settings, 1236, 1648);
            assert!(r.is_some());
        }
    }

    #[test]
    fn test_diagnostic_subboxes() {
        let pdf_path = "/tmp/sample_book.pdf";
        if !std::path::Path::new(pdf_path).exists() {
            return;
        }
        let doc = Document::open(pdf_path).expect("open doc");

        for page_no in [0, 1, 5, 20, 21, 50] {
            let page = doc.load_page(page_no).unwrap();
            let bounds = page.bounds().unwrap();
            println!("\n=== Page {} bounds: x0={} y0={} x1={} y1={} ===", page_no, bounds.x0, bounds.y0, bounds.x1, bounds.y1);
            let mut settings = ReaderSettings::default();
            settings.split = SplitConfig::for_preset(SplitPreset::Horizontal3);

            let boxes = settings.split.sub_boxes();
            for (sub_idx, b) in boxes.iter().enumerate() {
                let rendered = render_page(&doc, page_no as usize, sub_idx, &settings, 1236, 1648);
                assert!(rendered.is_some());
                println!("  Sub {}: box=[{:.4}, {:.4}, {:.4}, {:.4}] len={}", sub_idx, b.x0, b.y0, b.x1, b.y1, rendered.unwrap().len());
            }
        }
    }
}


