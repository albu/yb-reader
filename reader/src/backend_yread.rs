use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use ybdev::log::plog;
use yui::screen::Action;

use crate::backend::{PageTurnResult, ReaderBackend, RenderOutput};
use crate::dialogs;
use crate::split::{ReaderSettings, RectF};

pub struct YreadBackend {
    path: PathBuf,
    ybook: Option<Arc<yread::Book>>,
    yfonts: Option<Rc<yread::font::FontSystem>>,
    ycache: Arc<Mutex<yread::shape::ShapeCache>>,
    yworker_fonts: Option<Arc<yread::font::FontSystem>>,
    yraster: yread::raster::Rasterizer,
    ychap_idx: usize,
    ychap_page: usize,
    ychap_cache: HashMap<
        usize,
        (
            yread::model::ChapterPageTable,
            Vec<yread::paginate::PageLayout>,
        ),
    >,
    ychap_offsets: Vec<usize>,
    /// Per-chapter char counts + book total, counted ONCE at open —
    /// footer progress and page-offset estimation used to re-scan the
    /// whole book's text (O(book chars)) on every render/draw.
    ychap_chars: Vec<usize>,
    ychar_total: usize,
    total: usize,
    page_no: usize,
    sub_idx: usize,
    y_char_offset: usize,
    landing_char: Option<usize>,
    yreflow: bool,
    yopen_rx: Option<Receiver<Result<yread::Book, String>>>,
    ybg_rx: Option<
        Receiver<(
            usize,
            yread::model::ChapterPageTable,
            Vec<yread::paginate::PageLayout>,
        )>,
    >,
    yqueued_turns: i32,
    err: Option<String>,
}

/// Structured (chapter, character offset) pair packed into the single `sub_idx`
/// integer stored in the positions file for cross-session continuity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct YreadSub {
    pub chapter: usize,
    pub char_offset: usize,
}

impl YreadSub {
    pub const MAX_CHAR_OFFSET: usize = 999_999;
    pub const MULTIPLIER: usize = 1_000_000;

    pub const fn new(chapter: usize, char_offset: usize) -> Self {
        Self {
            chapter,
            char_offset: if char_offset > Self::MAX_CHAR_OFFSET {
                Self::MAX_CHAR_OFFSET
            } else {
                char_offset
            },
        }
    }

    /// Pack into a single usize persisted in positions.txt.
    pub const fn pack(self) -> usize {
        self.chapter * Self::MULTIPLIER + self.char_offset
    }

    /// Unpack from a single usize loaded from positions.txt.
    pub const fn unpack(raw: usize) -> Self {
        Self {
            chapter: raw / Self::MULTIPLIER,
            char_offset: raw % Self::MULTIPLIER,
        }
    }
}

pub fn pack_yread_sub(chapter: usize, char_offset: usize) -> usize {
    YreadSub::new(chapter, char_offset).pack()
}

pub fn unpack_yread_sub(raw: usize) -> (usize, usize) {
    let sub = YreadSub::unpack(raw);
    (sub.chapter, sub.char_offset)
}

impl YreadBackend {
    pub fn new(
        path: PathBuf,
        resume_sub: usize,
        vw: u32,
        vh: u32,
        settings: &ReaderSettings,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let path_cl = path.clone();
        std::thread::spawn(move || {
            let ext = path_cl
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let t0 = std::time::Instant::now();
            // A parser panic must reach the UI as an open error — an unwound
            // worker drops tx unsend and the reader would sit on "Opening…"
            // forever.
            let res =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match ext.as_str() {
                    "fb2" | "zip" => yread::fb2::parse_fb2_path(&path_cl),
                    "txt" => yread::txt::parse_txt_path(&path_cl),
                    _ => yread::epub::parse_epub_file(&path_cl),
                }))
                .unwrap_or_else(|p| {
                    Err(format!(
                        "parser panicked: {}",
                        crate::backend::panic_message(&p)
                    ))
                });
            plog(&format!("yread parse: {}ms", t0.elapsed().as_millis()));
            if let Ok(book) = &res {
                // Over-ceiling embedded images were dropped by the parser;
                // missing art must have a reason in the log.
                if !book.capped_binaries.is_empty() {
                    let ids: Vec<String> = book
                        .capped_binaries
                        .iter()
                        .take(8)
                        .cloned()
                        .collect();
                    plog(&format!(
                        "yread: dropped {} over-size image(s): {}{}",
                        book.capped_binaries.len(),
                        ids.join(", "),
                        if book.capped_binaries.len() > 8 { ", …" } else { "" }
                    ));
                }
            }
            let _ = tx.send(res);
        });

        let mut backend = Self {
            path,
            ybook: None,
            yfonts: None,
            ycache: Arc::new(Mutex::new(yread::shape::ShapeCache::new())),
            yworker_fonts: None,
            yraster: yread::raster::Rasterizer::new(),
            ychap_idx: 0,
            ychap_page: 0,
            ychap_cache: HashMap::new(),
            ychap_offsets: Vec::new(),
            ychap_chars: Vec::new(),
            ychar_total: 0,
            total: 1,
            page_no: 0,
            sub_idx: resume_sub,
            y_char_offset: 0,
            landing_char: None,
            yreflow: false,
            yopen_rx: Some(rx),
            ybg_rx: None,
            yqueued_turns: 0,
            err: None,
        };

        backend.yread_land_at_sub(resume_sub, vw, vh, settings);
        backend
    }

    fn hypher_lang_for(lang: &str) -> hypher::Lang {
        yread::hypher_lang(lang)
    }

    fn yread_layout_config(
        &self,
        settings: &ReaderSettings,
        vw: u32,
        vh: u32,
    ) -> yread::paginate::LayoutConfig {
        yread::paginate::LayoutConfig::reader(
            vw,
            vh,
            settings.margin_pad,
            settings.font_size,
            settings.line_spacing,
            settings.show_header,
        )
    }

    fn ensure_fonts(&mut self) {
        if self.yworker_fonts.is_none() {
            self.yworker_fonts = Some(Arc::new(yread::font::FontSystem::default()));
        }
        if self.yfonts.is_none() {
            self.yfonts = Some(Rc::new(yread::font::FontSystem::default()));
        }
    }

    fn yread_land_at(
        &mut self,
        chapter: usize,
        char_offset: usize,
        _vw: u32,
        _vh: u32,
        _settings: &ReaderSettings,
    ) {
        let target_ch = if let Some(b) = &self.ybook {
            let max_ch = b.chapters.len().saturating_sub(1);
            chapter.min(max_ch)
        } else {
            chapter
        };
        // A far landing makes the in-flight prefetch window (built around
        // the OLD chapter) useless. Drop it so the next render spawns a
        // current-first paginator for THIS chapter — otherwise the reader
        // would wait forever behind a worker that never paginates it.
        if target_ch != self.ychap_idx && !self.ychap_cache.contains_key(&target_ch) {
            self.ybg_rx = None;
        }
        self.ychap_idx = target_ch;
        self.ychap_page = 0;
        self.landing_char = Some(char_offset);
        self.y_char_offset = if char_offset == usize::MAX {
            self.ychap_cache
                .get(&self.ychap_idx)
                .and_then(|(_, layouts)| layouts.last())
                .map(|l| l.start_char)
                .unwrap_or(0)
        } else {
            char_offset
        };
        let global_p =
            self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
        self.page_no = global_p;
        self.sub_idx = pack_yread_sub(self.ychap_idx, self.y_char_offset);
    }

    fn yread_land_at_sub(&mut self, sub: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        let (ch, chr) = unpack_yread_sub(sub);
        self.yread_land_at(ch, chr, vw, vh, settings);
    }

    fn spawn_yread_background_paginator(&mut self, cfg: &yread::paginate::LayoutConfig) {
        self.ensure_fonts();
        let Some(ref yb) = self.ybook else { return };
        let Some(ref worker_fonts) = self.yworker_fonts else {
            return;
        };

        let book_arc = Arc::clone(yb);
        let fonts = Arc::clone(worker_fonts);
        let shape_cache = Arc::clone(&self.ycache);
        let cfg = *cfg;
        let cur = self.ychap_idx;
        let n = book_arc.chapters.len();

        let mut window: Vec<usize> = vec![cur];
        for d in 1..=2usize {
            if cur + d < n {
                window.push(cur + d);
            }
            if cur >= d {
                window.push(cur - d);
            }
        }
        window.retain(|i| !self.ychap_cache.contains_key(i));
        if window.is_empty() {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.ybg_rx = Some(rx);

        std::thread::spawn(move || {
            let lang = Self::hypher_lang_for(&book_arc.meta.language);
            for ch_idx in window {
                let mut cache = shape_cache.lock().unwrap_or_else(|p| p.into_inner());
                let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
                    &book_arc.chapters[ch_idx],
                    Some(&book_arc.image_sizes),
                    &cfg,
                    &fonts,
                    &mut cache,
                    Some(lang),
                );
                drop(cache);
                if tx.send((ch_idx, pt, layouts)).is_err() {
                    break;
                }
            }
        });
    }

    fn compute_chapter_page_offsets(&mut self, chars_per_page: f32) {
        if self.ychap_chars.is_empty() {
            return;
        }
        let cpp = chars_per_page.max(200.0);
        let mut offsets = Vec::with_capacity(self.ychap_chars.len());
        let mut cum = 0;
        for (i, &chars) in self.ychap_chars.iter().enumerate() {
            offsets.push(cum);
            if let Some((_, layouts)) = self.ychap_cache.get(&i) {
                cum += layouts.len().max(1);
            } else {
                let estimated = ((chars as f32 / cpp).round() as usize).max(1);
                cum += estimated;
            }
        }
        self.ychap_offsets = offsets;
        self.total = cum.max(1);
    }

    fn yread_receive_open(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> bool {
        let Some(rx) = self.yopen_rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(book)) => {
                // An empty-spine EPUB parses "successfully" with zero
                // chapters; render_page's ±4 eviction window would
                // underflow on `n - 1`. That is an open failure, not a
                // book.
                if book.chapters.is_empty() {
                    self.err = Some("no readable chapters".to_string());
                    return true;
                }
                let arc_book = Arc::new(book);
                self.ychap_chars = arc_book
                    .chapters
                    .iter()
                    .map(|c| c.text.chars().count())
                    .collect();
                self.ychar_total = self.ychap_chars.iter().sum();
                self.ybook = Some(arc_book);
                self.ensure_fonts();

                let (ch, chr) = (self.ychap_idx, self.y_char_offset);
                self.yread_land_at(ch, chr, vw, vh, settings);

                let cfg = self.yread_layout_config(settings, vw, vh);
                self.spawn_yread_background_paginator(&cfg);
                true
            }
            Ok(Err(e)) => {
                self.err = Some(e);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.yopen_rx = Some(rx);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The worker died without sending (panic past catch_unwind,
                // abort): surface it instead of "Opening…" forever.
                self.err = Some("open worker died".to_string());
                true
            }
        }
    }

    /// Drain background pagination results. Returns true when the CURRENT
    /// chapter's layout arrived — that is what clears the "Laying out…"
    /// screen, so the caller must repaint then. Neighbor prefetch results
    /// render when the reader turns to them; they must not force a
    /// re-render of the page being read.
    fn yread_receive_bg(&mut self) -> bool {
        let Some(rx) = self.ybg_rx.take() else {
            return false;
        };
        let mut current_arrived = false;
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok((ch_idx, pt, layouts)) => {
                    if self.yreflow && ch_idx == self.ychap_idx {
                        self.ychap_cache.clear();
                        self.ychap_offsets.clear();
                        self.yreflow = false;
                        plog("yread: reflow swap (async, UI never froze)");
                    }
                    self.ychap_cache.entry(ch_idx).or_insert((pt, layouts));
                    if ch_idx == self.ychap_idx {
                        current_arrived = true;
                        plog(&format!(
                            "yread: current chapter {} ready (async, no freeze)",
                            ch_idx
                        ));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !disconnected {
            self.ybg_rx = Some(rx);
        }
        current_arrived
    }
}

impl ReaderBackend for YreadBackend {
    fn is_pdf(&self) -> bool {
        false
    }

    fn book_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn total_pages(&self) -> usize {
        self.total
    }

    fn current_page(&self) -> usize {
        self.page_no
    }

    fn current_sub_idx(&self) -> usize {
        self.sub_idx
    }

    fn is_ready(&self) -> bool {
        self.ybook.is_some()
    }

    /// `total` holds the constructor's 1 until render_page computes real
    /// offsets — which needs the landing chapter in ychap_cache. See the
    /// trait method: saving before this flips true writes "0 of 1".
    fn is_paginated(&self) -> bool {
        self.ychap_cache.contains_key(&self.ychap_idx)
    }

    fn has_pending_work(&self) -> bool {
        self.ybook.is_none() || self.ybg_rx.is_some() || self.yreflow || self.yqueued_turns != 0
    }

    fn busy_phase(&self) -> Option<crate::backend::BusyPhase> {
        use crate::backend::BusyPhase;
        if self.ybook.is_none() {
            return Some(BusyPhase::Opening);
        }
        if self.yqueued_turns != 0 {
            return Some(BusyPhase::Turning);
        }
        // Neighbor prefetch (ybg_rx with the current chapter already
        // displayed) is deliberately NOT a busy phase — the shown page
        // is final and the footer must not claim otherwise.
        if self.yreflow || !self.ychap_cache.contains_key(&self.ychap_idx) {
            return Some(BusyPhase::LayingOut);
        }
        None
    }

    fn error(&self) -> Option<&str> {
        self.err.as_deref()
    }

    fn poll(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> bool {
        let mut redraw = self.yread_receive_open(vw, vh, settings);
        if self.yread_receive_bg() {
            // The current chapter's layout landed: repaint so the
            // "Laying out…" screen gives way to the book.
            redraw = true;
        }
        if self.yqueued_turns != 0 {
            // turn_page moves at most ONE page (or one chapter crossing)
            // per call; a cold chapter crossing re-queues the remainder
            // inside turn_page, which applies on a later poll when that
            // layout lands. The counter is taken out first: the closure
            // needs &mut self for turn_page.
            let mut q = std::mem::take(&mut self.yqueued_turns);
            let applied =
                crate::backend::drain_queued_turns(&mut q, |step| {
                    self.turn_page(step, vw, vh, settings)
                });
            self.yqueued_turns = q;
            if applied {
                redraw = true;
            }
        }
        redraw
    }

    fn turn_page(
        &mut self,
        delta: i32,
        vw: u32,
        vh: u32,
        settings: &ReaderSettings,
    ) -> PageTurnResult {
        if self.ybook.is_none() {
            self.yqueued_turns += delta;
            return PageTurnResult::Queued;
        }

        let Some((_pt, layouts)) = self.ychap_cache.get(&self.ychap_idx) else {
            self.yqueued_turns += delta;
            return PageTurnResult::Queued;
        };

        if delta > 0 {
            if self.ychap_page + 1 < layouts.len() {
                self.ychap_page += 1;
                // An explicit page move supersedes a pending landing:
                // render_page would otherwise snap ychap_page back to
                // page_for_char(landing) after poll already applied the
                // queued turn — silently eating it.
                self.landing_char = None;
                if let Some(l) = layouts.get(self.ychap_page) {
                    self.y_char_offset = l.start_char;
                }
                let global_p =
                    self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
                self.page_no = global_p;
                self.sub_idx = pack_yread_sub(self.ychap_idx, self.y_char_offset);
                PageTurnResult::Changed { redraw_full: false }
            } else {
                let total_ch = self.ybook.as_ref().map(|b| b.chapters.len()).unwrap_or(0);
                if self.ychap_idx + 1 < total_ch {
                    self.yread_land_at(self.ychap_idx + 1, 0, vw, vh, settings);
                    PageTurnResult::Changed { redraw_full: false }
                } else {
                    PageTurnResult::AtBoundary
                }
            }
        } else if delta < 0 {
            if self.ychap_page > 0 {
                self.ychap_page -= 1;
                // Same supersede as the forward branch.
                self.landing_char = None;
                if let Some(l) = layouts.get(self.ychap_page) {
                    self.y_char_offset = l.start_char;
                }
                let global_p =
                    self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
                self.page_no = global_p;
                self.sub_idx = pack_yread_sub(self.ychap_idx, self.y_char_offset);
                PageTurnResult::Changed { redraw_full: false }
            } else if self.ychap_idx > 0 {
                self.yread_land_at(self.ychap_idx - 1, usize::MAX, vw, vh, settings);
                PageTurnResult::Changed { redraw_full: false }
            } else {
                PageTurnResult::AtBoundary
            }
        } else {
            PageTurnResult::AtBoundary
        }
    }

    fn jump_to_sub(&mut self, sub_idx: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        self.yread_land_at_sub(sub_idx, vw, vh, settings);
    }

    fn jump_to_page(&mut self, page: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        if self.ychap_offsets.is_empty() {
            return;
        }
        let ch_idx = match self.ychap_offsets.binary_search(&page) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let page_in_ch = page.saturating_sub(self.ychap_offsets[ch_idx]);
        let char_off = self
            .ychap_cache
            .get(&ch_idx)
            .and_then(|(_, layouts)| layouts.get(page_in_ch))
            .map(|l| l.start_char)
            .unwrap_or(0);
        self.yread_land_at(ch_idx, char_off, vw, vh, settings);
    }

    fn render_page(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> RenderOutput {
        if self.ybook.is_none() {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        }

        self.ensure_fonts();
        let Some(book) = self.ybook.as_ref().cloned() else {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        };
        let Some(fonts) = self.yfonts.as_ref().cloned() else {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        };

        let cfg = self.yread_layout_config(settings, vw, vh);

        // Hysteresis eviction
        let n = book.chapters.len();
        if n == 0 {
            // Guarded at open (yread_receive_open); kept so a zero-chapter
            // book can never underflow the window math below.
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        }
        let lo = self.ychap_idx.saturating_sub(4);
        let hi = (self.ychap_idx + 4).min(n.saturating_sub(1));
        self.ychap_cache.retain(|&i, _| i >= lo && i <= hi);

        let window_uncached = (self.ychap_idx.saturating_sub(2)..=(self.ychap_idx + 2).min(n - 1))
            .filter(|i| !self.ychap_cache.contains_key(i))
            .count();
        if self.ybg_rx.is_none() && window_uncached > 0 {
            self.spawn_yread_background_paginator(&cfg);
        }

        // Cold chapter: NEVER paginate here on the UI thread. The spawn
        // above already queued the current chapter FIRST in the background
        // window; paginating synchronously can take seconds on a long
        // chapter — and lock-contend with the worker — which is the
        // "kindle froze" the async path exists to avoid. Return the
        // loading state; poll() drains the result and the next draw
        // renders it.
        if !self.ychap_cache.contains_key(&self.ychap_idx) {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        }

        let cur_chars = book.chapters[self.ychap_idx].char_count().max(100);
        let cur_layout_len = self
            .ychap_cache
            .get(&self.ychap_idx)
            .map(|(_, l)| l.len())
            .unwrap_or(1);
        let chars_per_page = (cur_chars as f32 / cur_layout_len.max(1) as f32).max(200.0);
        self.compute_chapter_page_offsets(chars_per_page);

        let Some((pt, layouts)) = self.ychap_cache.get(&self.ychap_idx) else {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        };

        if let Some(target_char) = self.landing_char.take() {
            self.ychap_page = if target_char == usize::MAX {
                layouts.len().saturating_sub(1)
            } else {
                pt.page_for_char(target_char)
                    .min(layouts.len().saturating_sub(1))
            };
        }

        let Some(cur_layout) = layouts.get(self.ychap_page) else {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        };

        self.y_char_offset = cur_layout.start_char;

        let global_p =
            self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
        self.page_no = global_p;
        self.sub_idx = pack_yread_sub(self.ychap_idx, self.y_char_offset);

        let t0 = std::time::Instant::now();
        let mut gray = vec![255u8; (vw * vh) as usize];
        self.yraster
            .render_page(&book, cur_layout, &cfg, &fonts, &mut gray, vw as usize);
        // (white_cutoff deliberately not gated here: its only editor was
        // the deleted settings dialog, and a legacy persisted value must
        // not wash out rendering with no UI to reset it.)
        if settings.invert || settings.contrast != crate::split::ContrastMode::Normal {
            settings.apply_lut(&mut gray);
        }
        let elapsed = t0.elapsed().as_millis();
        let rss = ybdev::sysinfo::rss_kib().unwrap_or(0) / 1024;
        plog(&format!(
            "yread render chap {} p {}: {}ms rss={}m",
            self.ychap_idx, self.ychap_page, elapsed, rss
        ));

        // Extract words & links matching painted pen math
        let mut words = Vec::new();
        let mut links = Vec::new();
        let chapter = &book.chapters[self.ychap_idx];
        let origin_x = cfg.margin_left as f32;
        let origin_y = cfg.margin_top as f32;

        for elem in &cur_layout.elements {
            if let yread::paginate::PageElement::Line { line, x, y } = elem {
                let (align_off, extra_space) = yread::raster::alignment_adjust(line);
                let mut cur_x = origin_x + x + align_off;
                for item in &line.items {
                    match item {
                        yread::line::LineItem::Word {
                            byte_start,
                            byte_end,
                            shaped,
                            style,
                            ..
                        } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y
                                        - word_top_offset(line, style, &fonts, cfg.font_size),
                                    cur_x + shaped.advance,
                                    origin_y
                                        + y
                                        + word_bottom_offset(line, style, &fonts, cfg.font_size),
                                );
                                words.push((w_str.to_string(), rect));
                                if let Some(target) = &style.footnote_ref {
                                    links.push((rect, target.clone()));
                                }
                            }
                            cur_x += shaped.advance;
                        }
                        yread::line::LineItem::HyphenatedPrefix {
                            byte_start,
                            byte_end,
                            prefix_shaped,
                            hyphen_adv,
                            style,
                            ..
                        } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y
                                        - word_top_offset(line, style, &fonts, cfg.font_size),
                                    cur_x + prefix_shaped.advance + hyphen_adv,
                                    origin_y
                                        + y
                                        + word_bottom_offset(line, style, &fonts, cfg.font_size),
                                );
                                words.push((w_str.to_string(), rect));
                                if let Some(target) = &style.footnote_ref {
                                    links.push((rect, target.clone()));
                                }
                            }
                            cur_x += prefix_shaped.advance + hyphen_adv;
                        }
                        yread::line::LineItem::Space { adv, .. } => {
                            cur_x += *adv + extra_space;
                        }
                        yread::line::LineItem::HardBreak => {}
                    }
                }
            }
        }

        RenderOutput {
            gray: Some(gray),
            words,
            links,
            is_loading: false,
        }
    }

    fn footer_info(&self) -> (String, usize, usize) {
        let (pct, chap_name) = if let Some(book) = &self.ybook {
            let total_chars = self.ychar_total.max(1);
            let prev_chars: usize = self.ychap_chars.iter().take(self.ychap_idx).sum();
            let cur_chars = prev_chars + self.y_char_offset;
            let p = ((cur_chars as f64 / total_chars as f64) * 100.0).clamp(0.0, 100.0) as usize;
            let cname = book
                .chapters
                .get(self.ychap_idx)
                .map(|c| c.title.clone())
                .unwrap_or_default();
            (p, cname)
        } else {
            (0, String::new())
        };

        let footer_str = if chap_name.is_empty() {
            format!("{}%", pct)
        } else {
            format!("{}% · {}", pct, chap_name)
        };
        (footer_str, self.current_page() + 1, self.total_pages())
    }

    fn chapter_title(&self) -> Option<String> {
        self.ybook
            .as_ref()
            .and_then(|b| b.chapters.get(self.ychap_idx))
            .map(|c| c.title.clone())
            .filter(|t| !t.is_empty())
    }

    fn resolve_link_or_footnote(
        &self,
        uri: &str,
        bg: Option<Vec<u8>>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action {
        if let Some(book) = &self.ybook {
            let cur_chars = book
                .chapters
                .get(self.ychap_idx)
                .map(|c| c.char_count())
                .unwrap_or(1500);
            let cur_pages = self
                .ychap_cache
                .get(&self.ychap_idx)
                .map(|(_, l)| l.len())
                .unwrap_or(1)
                .max(1);
            let chars_per_page = (cur_chars as f32 / cur_pages as f32).max(200.0);
            return dialogs::footnote_dialog_yread(
                book,
                self.ychap_idx,
                uri,
                bg,
                path_name,
                self.total,
                settings,
                &self.ychap_offsets,
                chars_per_page,
            );
        }
        Action::Keep
    }

    fn open_toc_dialog(
        &self,
        _cur_page: usize,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action {
        if let Some(book) = &self.ybook {
            let cur_char = self
                .ychap_cache
                .get(&self.ychap_idx)
                .and_then(|(_, layouts)| layouts.get(self.ychap_page))
                .map(|l| l.end_char)
                .unwrap_or(self.y_char_offset);

            return dialogs::yread_toc_dialog(
                &book.toc,
                self.ychap_idx,
                cur_char,
                &self.ychap_offsets,
                back,
                path_name,
                self.total,
                settings,
            );
        }
        Action::Keep
    }

    fn open_scrubber_dialog(
        &self,
        cur_page: usize,
        _bg: Option<Vec<u8>>,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
        _w: u32,
        _h: u32,
    ) -> Action {
        self.open_toc_dialog(cur_page, back, path_name, settings)
    }

    fn apply_settings_change(
        &mut self,
        old: &ReaderSettings,
        new: &ReaderSettings,
        vw: u32,
        vh: u32,
    ) -> bool {
        let layout_changed = old.font_size != new.font_size
            || old.line_spacing != new.line_spacing
            || old.margin_pad != new.margin_pad
            || old.split.rotation != new.split.rotation;

        if layout_changed {
            self.yread_land_at(self.ychap_idx, self.y_char_offset, vw, vh, new);
            self.ensure_fonts();
            self.ybg_rx = None;
            self.ychap_cache.clear();
            self.ychap_offsets.clear();
            if self.ybook.is_some() {
                self.yreflow = true;
                let cfg = self.yread_layout_config(new, vw, vh);
                self.spawn_yread_background_paginator(&cfg);
            }
            return true;
        }
        false
    }

    fn interactive_preview(
        &mut self,
        settings: &ReaderSettings,
        vw: u32,
        vh: u32,
    ) -> Option<Vec<u8>> {
        self.ensure_fonts();
        let book = self.ybook.as_ref()?.clone();
        let chap = book.chapters.get(self.ychap_idx)?;
        let fonts = self.yfonts.as_ref()?.clone();

        let cfg = self.yread_layout_config(settings, vw, vh);
        // Never block the UI on the shape-cache mutex: if the background
        // paginator holds it, skip this preview frame instead of freezing
        // the settings sheet for the chapter's pagination duration.
        let Ok(mut cache) = self.ycache.try_lock() else {
            return None;
        };
        let lang = Self::hypher_lang_for(&book.meta.language);
        let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
            chap,
            Some(&book.image_sizes),
            &cfg,
            &fonts,
            &mut cache,
            Some(lang),
        );
        let landing_page = pt
            .page_for_char(self.y_char_offset)
            .min(layouts.len().saturating_sub(1));
        let layout = layouts.get(landing_page)?;
        let mut raster = yread::raster::Rasterizer::new();
        let mut gray = vec![255u8; (vw * vh) as usize];
        raster.render_page(&book, layout, &cfg, &fonts, &mut gray, vw as usize);
        if settings.invert || settings.contrast != crate::split::ContrastMode::Normal {
            settings.apply_lut(&mut gray);
        }
        Some(gray)
    }
}

/// Vertical extents of a word's hit-rect relative to the line baseline —
/// the same math raster.rs `render_line` uses to PAINT each item. Regular
/// words take the line-level extents; sup/sub words paint at a shifted
/// baseline and reduced run size (footnote refs are 0.75× superscripts),
/// and the flat line extents miss them high/low — the vertical twin of
/// the alignment_adjust drift that broke dictionary taps.
fn word_top_offset(
    line: &yread::line::LayoutLine,
    style: &yread::model::Style,
    fonts: &yread::font::FontSystem,
    base_font_size: f32,
) -> f32 {
    if style.is_sup || style.is_sub {
        let run_size = base_font_size * style.size_mult;
        let m = fonts.metrics(style.font_style, run_size);
        let shift = if style.is_sup {
            -(run_size * 0.40 * (300.0 / 72.0))
        } else {
            run_size * 0.25 * (300.0 / 72.0)
        };
        m.ascender - shift
    } else {
        line.ascender
    }
}

fn word_bottom_offset(
    line: &yread::line::LayoutLine,
    style: &yread::model::Style,
    fonts: &yread::font::FontSystem,
    base_font_size: f32,
) -> f32 {
    if style.is_sup || style.is_sub {
        let run_size = base_font_size * style.size_mult;
        let m = fonts.metrics(style.font_style, run_size);
        let shift = if style.is_sup {
            -(run_size * 0.40 * (300.0 / 72.0))
        } else {
            run_size * 0.25 * (300.0 / 72.0)
        };
        m.descender + shift
    } else {
        line.height - line.ascender
    }
}

#[cfg(test)]
mod save_guard_tests {
    use super::*;

    fn bare_backend() -> YreadBackend {
        YreadBackend {
            path: PathBuf::from("/x/t.epub"),
            ybook: None,
            yfonts: None,
            ycache: Arc::new(Mutex::new(yread::shape::ShapeCache::new())),
            yworker_fonts: None,
            yraster: yread::raster::Rasterizer::new(),
            ychap_idx: 0,
            ychap_page: 0,
            ychap_cache: HashMap::new(),
            ychap_offsets: Vec::new(),
            ychap_chars: vec![100],
            ychar_total: 100,
            // The constructor default at the center of the bug: total
            // stays 1 until render_page computes real offsets.
            total: 1,
            page_no: 0,
            sub_idx: 0,
            y_char_offset: 0,
            landing_char: None,
            yreflow: false,
            yopen_rx: None,
            ybg_rx: None,
            yqueued_turns: 0,
            err: None,
        }
    }

    #[test]
    fn ready_but_unpaginated_must_not_be_savable() {
        let mut b = bare_backend();
        // Parse landed: ready flips true, but the landing chapter is
        // still in the background paginator and total_pages() is the
        // placeholder 1. save_progress must refuse in this window —
        // entering a book and exiting during "Laying out…" used to record
        // "page 0 of 1" over a real reading position.
        let book = yread::txt::parse_txt_bytes(b"one\n\ntwo\n", "T").unwrap();
        b.ybook = Some(Arc::new(book));
        assert!(b.is_ready());
        assert_eq!(b.total_pages(), 1);
        assert!(!b.is_paginated());

        // The chapter lands: savable.
        b.ychap_cache.insert(0, (Default::default(), Vec::new()));
        assert!(b.is_paginated());
    }

    #[test]
    fn unopened_backend_is_neither_ready_nor_paginated() {
        let b = bare_backend();
        assert!(!b.is_ready());
        assert!(!b.is_paginated());
    }

    #[test]
    fn yread_sub_packing_and_unpacking_roundtrip() {
        let sub = YreadSub::new(42, 12345);
        let packed = sub.pack();
        assert_eq!(packed, 42_012_345);
        assert_eq!(YreadSub::unpack(packed), sub);

        // Clamping overflow beyond 999_999:
        let clamped = YreadSub::new(7, 2_000_000);
        assert_eq!(clamped.char_offset, YreadSub::MAX_CHAR_OFFSET);
        assert_eq!(clamped.pack(), 7_999_999);
    }
}
