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
use yui::Orientation;

pub struct ReaderScreen {
    path: PathBuf,
    /// Panel dims (orientation-independent). Visual dims derive on demand
    /// from the split rotation — every render/cache interaction speaks
    /// visual space.
    pw: u32,
    ph: u32,
    loading: Option<std::sync::mpsc::Receiver<Result<BookReady, String>>>,
    /// Captured right before a reflow, consumed when it lands: the text
    /// at the top of the current page, so the new pagination can be
    /// searched for the same words (page numbers don't survive).
    reflow_anchor: Option<doc_store::Anchor>,
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
    // --- yRead Pure-Rust Layout Engine ---
    ybook: Option<Rc<yread::Book>>,
    yfonts: Option<Rc<yread::font::FontSystem>>,
    ycache: yread::shape::ShapeCache,
    yraster: yread::raster::Rasterizer,
    ychap_idx: usize,
    ychap_page: usize,
    ychap_cache: std::collections::HashMap<usize, (yread::model::ChapterPageTable, Vec<yread::paginate::PageLayout>)>,
    ychap_offsets: Vec<usize>,
    y_char_offset: usize,
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

        let is_pdf = path
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);

        // Visual dims under the book's persisted orientation
        let (vw, vh) = Orientation::from_rotation(settings.split.rotation).visual_dims(w, h);
        let cached_snap = if settings.engine == crate::split::ReaderEngine::YRead && !is_pdf {
            None
        } else {
            crate::cache::load_snapshot(&name, resume, sub_idx, &settings, vw, vh)
        };
        if cached_snap.is_some() {
            plog(&format!("loaded instant page snapshot for {}", name));
        }

        let vocab_db = crate::vocab::VocabDb::open();
        let vocab_prof = crate::vocab::VocabProfile::load();

        ReaderScreen {
            path,
            pw: w,
            ph: h,
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
            reflow_anchor: None,
            sel_mode: false,
            sel: None,
            highlights: Vec::new(),
            ybook: None,
            yfonts: None,
            ycache: yread::shape::ShapeCache::new(),
            yraster: yread::raster::Rasterizer::new(),
            ychap_idx: 0,
            ychap_page: 0,
            ychap_cache: std::collections::HashMap::new(),
            ychap_offsets: Vec::new(),
            y_char_offset: 0,
        }
    }

    fn book_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Visual dims under the current split rotation.
    fn visual_dims(&self) -> (u32, u32) {
        Orientation::from_rotation(self.settings.split.rotation).visual_dims(self.pw, self.ph)
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

    fn ensure_yread_loaded(&mut self) {
        if self.ybook.is_some() {
            return;
        }
        if let Ok(data) = std::fs::read(&self.path) {
            let ext = self
                .path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let book_res = if ext == "fb2" || ext == "zip" {
                yread::fb2::parse_fb2(&data)
            } else {
                yread::epub::parse_epub(&data)
            };
            if let Ok(b) = book_res {
                self.ybook = Some(Rc::new(b));
                if self.yfonts.is_none() {
                    self.yfonts = Some(Rc::new(yread::font::FontSystem::default()));
                }
            }
        }
    }

    fn compute_chapter_page_offsets(&mut self, cfg: &yread::paginate::LayoutConfig) {
        if !self.ychap_offsets.is_empty() {
            return;
        }
        let Some(ref ybook) = self.ybook else { return };
        let Some(ref yfonts) = self.yfonts else { return };
        let mut offsets = Vec::with_capacity(ybook.chapters.len());
        let mut cum = 0;
        for (i, ch) in ybook.chapters.iter().enumerate() {
            offsets.push(cum);
            if let Some((_, layouts)) = self.ychap_cache.get(&i) {
                cum += layouts.len();
            } else {
                let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
                    ch,
                    Some(&ybook.image_sizes),
                    cfg,
                    yfonts,
                    &mut self.ycache,
                    Some(hypher::Lang::English),
                );
                cum += layouts.len();
                self.ychap_cache.insert(i, (pt, layouts));
            }
        }
        self.ychap_offsets = offsets;
        self.total = cum.max(1);
    }

    fn set_yread_global_page(&mut self, g_page: usize) {
        if self.ychap_offsets.is_empty() {
            self.ychap_idx = 0;
            self.ychap_page = 0;
            self.y_char_offset = 0;
            return;
        }
        let g = g_page.min(self.total.saturating_sub(1));
        let mut best_chap = 0;
        for (idx, &offset) in self.ychap_offsets.iter().enumerate() {
            if offset <= g {
                best_chap = idx;
            } else {
                break;
            }
        }
        self.ychap_idx = best_chap;
        let offset = self.ychap_offsets[best_chap];
        self.ychap_page = g.saturating_sub(offset);
        if let Some((_, layouts)) = self.ychap_cache.get(&self.ychap_idx) {
            if let Some(l) = layouts.get(self.ychap_page) {
                self.y_char_offset = l.start_char;
            }
        }
        self.page_no = g;
    }

    fn paginate_yread_chapter(&mut self, chap_idx: usize, cfg: &yread::paginate::LayoutConfig) {
        if self.ychap_cache.contains_key(&chap_idx) {
            return;
        }
        let Some(ref ybook) = self.ybook else { return };
        let Some(ref yfonts) = self.yfonts else { return };
        if chap_idx >= ybook.chapters.len() {
            return;
        }
        let chapter = &ybook.chapters[chap_idx];
        let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
            chapter,
            Some(&ybook.image_sizes),
            cfg,
            yfonts,
            &mut self.ycache,
            Some(hypher::Lang::English),
        );
        self.ychap_cache.insert(chap_idx, (pt, layouts));
    }

    fn render_yread_page(&mut self) -> Option<Vec<u8>> {
        self.ensure_yread_loaded();
        let ybook = self.ybook.as_ref()?.clone();
        let yfonts = self.yfonts.as_ref()?.clone();
        if ybook.chapters.is_empty() {
            return None;
        }

        let (vw, vh) = self.visual_dims();
        let cfg = yread::paginate::LayoutConfig {
            page_width: vw,
            page_height: vh,
            margin_left: self.settings.margin_pad,
            margin_right: self.settings.margin_pad,
            margin_top: self.settings.margin_pad + if self.settings.show_header { 92 } else { 0 },
            margin_bottom: self.settings.margin_pad + 50,
            font_size: self.settings.font_size,
            line_spacing: self.settings.line_spacing,
            paragraph_spacing: 0.15,
            indent_em: 1.2,
            hyphenate: true,
        };

        self.compute_chapter_page_offsets(&cfg);

        // Map global page_no to chapter
        if !self.ychap_offsets.is_empty() {
            let g = self.page_no.min(self.total.saturating_sub(1));
            let mut best_chap = 0;
            for (idx, &offset) in self.ychap_offsets.iter().enumerate() {
                if offset <= g {
                    best_chap = idx;
                } else {
                    break;
                }
            }
            self.ychap_idx = best_chap;
            self.ychap_page = g.saturating_sub(self.ychap_offsets[best_chap]);
        }

        if self.ychap_idx >= ybook.chapters.len() {
            self.ychap_idx = 0;
        }

        // 1. Paginate current chapter
        self.paginate_yread_chapter(self.ychap_idx, &cfg);

        // 2. Opportunistic Pre-pagination of adjacent chapters (ahead / behind)
        let cur_layout_len = self.ychap_cache.get(&self.ychap_idx).map(|(_, l)| l.len()).unwrap_or(0);
        if (self.ychap_page + 2 >= cur_layout_len) && (self.ychap_idx + 1 < ybook.chapters.len()) {
            self.paginate_yread_chapter(self.ychap_idx + 1, &cfg);
        }
        if self.ychap_page <= 1 && self.ychap_idx > 0 {
            self.paginate_yread_chapter(self.ychap_idx - 1, &cfg);
        }

        let Some((pt, layouts)) = self.ychap_cache.get(&self.ychap_idx) else {
            return None;
        };
        if layouts.is_empty() {
            return None;
        }

        if self.y_char_offset == usize::MAX {
            self.ychap_page = layouts.len().saturating_sub(1);
        } else if self.y_char_offset > 0 && self.ychap_page == 0 {
            self.ychap_page = pt.page_for_char(self.y_char_offset).min(layouts.len().saturating_sub(1));
        }
        self.ychap_page = self.ychap_page.min(layouts.len().saturating_sub(1));
        let cur_layout = &layouts[self.ychap_page];
        self.y_char_offset = cur_layout.start_char;

        let global_p = self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
        self.page_no = global_p;
        self.sub_idx = 0;

        let mut fb = vec![255u8; (vw * vh) as usize];
        self.yraster.render_page(
            &ybook,
            cur_layout,
            &cfg,
            &yfonts,
            &mut fb,
            vw as usize,
        );

        // Word extraction for dictionary lookups
        self.page_words.clear();
        let chapter = &ybook.chapters[self.ychap_idx];
        let origin_x = cfg.margin_left as f32;
        let origin_y = cfg.margin_top as f32;
        for elem in &cur_layout.elements {
            if let yread::paginate::PageElement::Line { line, x, y } = elem {
                let mut cur_x = origin_x + x;
                for item in &line.items {
                    match item {
                        yread::line::LineItem::Word { byte_start, byte_end, shaped, .. } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y - line.ascender,
                                    cur_x + shaped.advance,
                                    origin_y + y - line.ascender + line.height,
                                );
                                self.page_words.push((w_str.to_string(), rect));
                            }
                            cur_x += shaped.advance;
                        }
                        yread::line::LineItem::HyphenatedPrefix { byte_start, byte_end, prefix_shaped, hyphen_adv, .. } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y - line.ascender,
                                    cur_x + prefix_shaped.advance + hyphen_adv,
                                    origin_y + y - line.ascender + line.height,
                                );
                                self.page_words.push((w_str.to_string(), rect));
                            }
                            cur_x += prefix_shaped.advance + hyphen_adv;
                        }
                        yread::line::LineItem::Space { adv, .. } => {
                            cur_x += *adv;
                        }
                    }
                }
            }
        }

        Some(fb)
    }

    fn turn(&mut self, forward: bool) -> Action {
        if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
            let cur_g = self.page_no;
            let next_g = if forward {
                (cur_g + 1).min(self.total.saturating_sub(1))
            } else {
                cur_g.saturating_sub(1)
            };
            if next_g == cur_g {
                return Action::Keep;
            }
            self.set_yread_global_page(next_g);
            self.page_gray = None;
            self.save_progress();
            self.turns_since_full += 1;
            let global_interval = positions::global_refresh_interval();
            if global_interval > 0 && self.turns_since_full >= global_interval {
                self.turns_since_full = 0;
                return Action::RedrawFull;
            } else {
                return Action::Redraw;
            }
        }

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
            let (vw, vh) = self.visual_dims();
            if let Some(snap) =
                crate::cache::load_snapshot(&self.book_name(), new_page, new_sub, &self.settings, vw, vh)
            {
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
        if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
            self.ensure_yread_loaded();
            let (vw, vh) = self.visual_dims();
            let cfg = yread::paginate::LayoutConfig {
                page_width: vw,
                page_height: vh,
                margin_left: self.settings.margin_pad,
                margin_right: self.settings.margin_pad,
                margin_top: self.settings.margin_pad + if self.settings.show_header { 92 } else { 0 },
                margin_bottom: self.settings.margin_pad + 50,
                font_size: self.settings.font_size,
                line_spacing: self.settings.line_spacing,
                paragraph_spacing: 0.15,
                indent_em: 1.2,
                hyphenate: true,
            };
            self.compute_chapter_page_offsets(&cfg);
            if let Some(ref yb) = self.ybook {
                let chapters = yb.chapters.clone();
                let offsets = self.ychap_offsets.clone();
                let cur_page = self.page_no;
                let path_name = self.book_name();
                let total = self.total;
                let settings = self.settings;
                return dialogs::yread_toc_dialog(
                    &chapters,
                    &offsets,
                    cur_page,
                    path_name,
                    total,
                    settings,
                );
            }
        }
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
        // The scrubber is a proportional bottom card — it inherits the
        // reader's orientation, so backdrop and previews render at the
        // current visual dims.
        let (vw, vh) = self.visual_dims();
        dialogs::scrubber_dialog(
            doc,
            self.page_no,
            self.total,
            self.page_gray.clone(),
            self.book_name(),
            self.settings,
            vw,
            vh,
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
            self.sub_idx,
            self.settings,
            self.is_pdf(),
            self.book_name(),
            self.total,
        )
    }


    fn open_curtain(&mut self) -> Action {
        Action::Push(Box::new(CurtainScreen::new_with_rotation(
            crate::curtain::RotateCtx {
                book: self.book_name(),
                page: self.page_no,
                sub: self.sub_idx,
                total: self.total,
                settings: self.settings,
            },
        )))
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

    fn extract_words_and_links(&mut self) {
        self.page_words.clear();
        self.page_annotations.clear();
        self.page_links.clear();

        let Some(doc) = &self.doc else { return };
        crate::document::apply_layout_css(self.settings.line_spacing);
        let Ok(page) = doc.load_page(self.page_no as i32) else { return };
        let (vw, vh) = self.visual_dims();
        let Some(geom) = crate::render::LayoutGeom::new(
            &self.settings,
            page.bounds().unwrap_or_default(),
            self.sub_idx,
            vw,
            vh,
        ) else {
            return;
        };
        let Ok(tp) = page.to_text_page(mupdf::TextPageFlags::empty()) else { return };

        self.page_links = crate::render::links_from_page(&page, &geom);
        self.page_words = crate::render::words_from_text_page(&tp, &geom);
    }

    fn open_quick_settings(&mut self) -> Action {
        let book = self.book_name();
        let page = self.page_no;
        let sub = self.sub_idx;
        let tot = self.total;
        let s = self.settings;
        let is_pdf = self.is_pdf();
        let gray = self.page_gray.clone();
        let (vw, vh) = self.visual_dims();
        let doc_rc = if is_pdf { self.doc.clone() } else { None };

        Action::Push(Box::new(crate::quick_settings::QuickSettingsSheet::new(
            book,
            page,
            sub,
            tot,
            s,
            is_pdf,
            None,
            gray,
            move |new_settings| {
                if let Some(doc) = &doc_rc {
                    crate::render::render_page(doc.as_ref(), page, sub, &new_settings, vw, vh)
                } else {
                    None
                }
            },
        )))
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

    /// The grip this book renders in, straight from its split settings —
    /// App flips the panel accordingly (with a full refresh).
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::from_rotation(self.settings.split.rotation))
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

        if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
            self.ensure_yread_loaded();
            self.page_no = pos.page;
            self.sub_idx = 0;
            self.page_gray = None;
            return Action::RedrawFull;
        }

        // Warm cache check: the document was laid out for exactly these
        // visual dims and settings — anything else (e.g. a rotation change
        // while away) needs a cold open.
        let (vw, vh) = self.visual_dims();
        if let Ok(mut warm) = WARM.lock() {
            if let Some((p, SendDoc(doc), total, warm_settings, warm_w, warm_h)) = warm.take() {
                if p == self.path
                    && warm_settings == self.settings
                    && warm_w == vw
                    && warm_h == vh
                {
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
            vw,
            vh,
            self.settings.font_size,
            self.settings.margin_pad,
            self.settings.line_spacing,
            None, // fresh open: position comes from positions.txt
        ));
        Action::Redraw
    }

    fn on_leave(&mut self) {
        if let Some(doc_rc) = self.doc.take() {
            if let Ok(doc) = Rc::try_unwrap(doc_rc) {
                if let Ok(mut warm) = WARM.lock() {
                    let (vw, vh) = self.visual_dims();
                    *warm = Some((
                        self.path.clone(),
                        SendDoc(doc),
                        self.total,
                        self.settings,
                        vw,
                        vh,
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
            if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
                self.set_yread_global_page(pos.page);
            }
            self.page_gray = None;
            self.page_words.clear();
            self.page_annotations.clear();
            self.page_start_time = Instant::now();
        }

        if let Some(s) = pos.settings {
            // Any layout-affecting change — font, margin, or the split
            // config (rotation, preset, overlap, crop margins) —
            // invalidates the current render; reflowables must re-layout.
            let layout_changed = (s.font_size - self.settings.font_size).abs() > 0.01
                || (s.line_spacing - self.settings.line_spacing).abs() > 0.01
                || s.margin_pad != self.settings.margin_pad
                || s.engine != self.settings.engine
                || s.split != self.settings.split;
            self.settings = s;
            // A preset with fewer sub-boxes: keep the position valid.
            self.sub_idx = self
                .sub_idx
                .min(self.settings.split.sub_box_count().saturating_sub(1));

            if layout_changed {
                self.page_words.clear();
                self.page_annotations.clear();

                if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
                    self.ychap_cache.clear();
                    self.page_gray = None;
                    return Action::Redraw;
                }

                if !self.is_pdf() {
                    if self.page_words.is_empty() {
                        self.extract_words_and_links();
                    }
                    let words: String = self
                        .page_words
                        .iter()
                        .map(|(w, _)| w.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let anchor = if words.trim().is_empty() {
                        None
                    } else if let Some(doc) = &self.doc {
                        Some(doc_store::Anchor::capture(doc.as_ref(), self.page_no, self.total, words))
                    } else {
                        None
                    };
                    self.reflow_anchor = anchor.clone();
                    self.sub_idx = 0;
                    self.page_gray = None;
                    let (vw, vh) = self.visual_dims();
                    if let Some(doc_rc) = self.doc.take() {
                        if let Ok(doc) = Rc::try_unwrap(doc_rc) {
                            self.loading = Some(doc_store::reflow_async(
                                SendDoc(doc),
                                vw,
                                vh,
                                self.settings.font_size,
                                self.settings.margin_pad,
                                self.settings.line_spacing,
                                anchor,
                            ));
                        } else {
                            self.loading = Some(doc_store::open_async(
                                self.path.clone(),
                                vw,
                                vh,
                                self.settings.font_size,
                                self.settings.margin_pad,
                                self.settings.line_spacing,
                                anchor,
                            ));
                        }
                    } else {
                        self.loading = Some(doc_store::open_async(
                            self.path.clone(),
                            vw,
                            vh,
                            self.settings.font_size,
                            self.settings.margin_pad,
                            self.settings.line_spacing,
                            anchor,
                        ));
                    }
                    return Action::Redraw;
                }
                // PDF: the render derives from settings each draw —
                // dropping the stale snapshot re-renders in the new
                // orientation/crop immediately.
                self.page_gray = None;
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
            if self.doc.is_some() && self.page_gray.is_some() {
                let (w, h) = self.dims;
                self.pre_cache_neighbors(w as u32, h as u32);
            }
            return Action::Keep;
        };
        match rx.try_recv() {
            Ok(Ok(BookReady { doc, total, landing })) => {
                self.doc = Some(Rc::new(doc.0));
                self.total = total;
                self.scan_toc_chapters();
                self.highlights = crate::notes::load(&self.book_name());
                self.loading = None;

                // Reflow survival: land on the page carrying the same
                // top-of-page words; if the text match missed, the
                // fraction estimate still beats a stale page number.
                if let Some(target) = landing {
                    self.page_no = target.min(total.saturating_sub(1));
                    self.sub_idx = 0;
                    self.page_gray = None;
                } else if let Some(a) = self.reflow_anchor.take() {
                    let target = (a.fraction * total as f64).round() as usize;
                    let target = target.min(total.saturating_sub(1));
                    if target != self.page_no {
                        self.page_no = target;
                        self.sub_idx = 0;
                        self.page_gray = None;
                    }
                }

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

        // If doc is loading / reflowing:
        if self.loading.is_some() && self.page_gray.is_none() {
            let msg = if self.total > 0 { "Reflowing…" } else { "Opening…" };
            p.text_center(h / 2, 10.0, fg_color, msg);
            let name = p.truncate(8.0, &self.book_name(), p.width_pt() - 24.0);
            p.text_center(h / 2 + pt(16.0), 8.0, fg_color, &name);
            return;
        }

        if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
            if self.page_gray.is_none() {
                let t0 = Instant::now();
                if let Some(gray) = self.render_yread_page() {
                    plog(&format!(
                        "yread render chap {} p {}: {}ms",
                        self.ychap_idx,
                        self.ychap_page,
                        t0.elapsed().as_millis(),
                    ));
                    self.page_gray = Some(gray);
                } else {
                    self.err = Some("yRead render failed".to_string());
                    p.text_center(h / 2, 10.0, fg_color, "yRead render failed");
                    return;
                }
            }
        } else if let Some(doc) = &mut self.doc {
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

        // Word/link extraction for selection & lookups
        if self.doc.is_some() && self.page_words.is_empty() && self.settings.engine != crate::split::ReaderEngine::YRead {
            self.extract_words_and_links();
        }

        // Header status line + progress footer
        if self.settings.show_header {
            chrome::draw_header(p, &self.time_str, &self.book_name(), is_night);
        }

        let (footer, footer_page, footer_total) = if self.settings.engine == crate::split::ReaderEngine::YRead && !self.is_pdf() {
            let chap_title = self.ybook.as_ref().and_then(|b| b.chapters.get(self.ychap_idx)).map(|c| c.title.trim()).unwrap_or("");
            let text = if chap_title.is_empty() {
                format!("page {} / {}", self.page_no + 1, self.total)
            } else {
                let trunc_title = p.truncate(7.5, chap_title, 140.0);
                format!("page {} / {} · {}", self.page_no + 1, self.total, trunc_title)
            };
            (text, self.page_no, self.total)
        } else {
            let time_left = chrome::time_left_str(
                self.total,
                self.page_no,
                &self.toc_chapters,
                self.avg_secs_per_page,
            );
            let text = chrome::footer_str(
                self.loading.is_some(),
                self.page_no,
                self.sub_idx,
                self.total,
                &self.settings,
                &time_left,
            );
            (text, self.page_no, self.total)
        };

        chrome::draw_footer(
            p,
            &footer,
            footer_page,
            footer_total,
            &self.toc_chapters,
            is_night,
        );

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

        // Gestures arrive already in visual space (App un-rotates input);
        // self.dims is the visual canvas cached at draw.
        let (vis_w, vis_h) = self.dims;

        let (vx, vy, v_dir) = match g {
            Gesture::Tap { x, y } | Gesture::LongPress { x, y } | Gesture::Drag { x, y } => {
                (x as i32, y as i32, None)
            }
            Gesture::Swipe { dir, x, y, .. } => (x as i32, y as i32, Some(dir)),
            _ => (0, 0, None),
        };

        if let Some(dir) = v_dir {
            // Any swipe cancels a pending selection (mode stays on).
            let had_sel = self.sel.take().is_some();
            // Visual Swipes
            let act = match dir {
                SwipeDir::West => self.turn(true),  // swipe left -> forward
                SwipeDir::East => self.turn(false), // swipe right -> back
                SwipeDir::South => {
                    // Top swipe down -> Curtain (brightness/control center)
                    if vy < vis_h * 25 / 100 {
                        self.open_curtain()
                    } else {
                        Action::Keep
                    }
                }
                SwipeDir::North => {
                    // Bottom swipes:
                    // 1. Bottom-left swipe up -> In-Book Quick Settings Sheet
                    if vx < vis_w * 35 / 100 && vy > vis_h * 70 / 100 {
                        self.open_quick_settings()
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

                // 3. Visual Bottom-Left corner -> TOC Dialog
                if vx < 240 && vy > vis_h - 160 {
                    return self.open_toc_dialog();
                }

                // 4. Visual Bottom-Right corner -> Quick Settings Sheet
                if vx > vis_w - 240 && vy > vis_h - 160 {
                    return self.open_quick_settings();
                }

                // 5. Visual Bottom Footer Strip -> Open Interactive Page Scrubber & "Go to Page"
                if vy > vis_h - 140 && vx > 240 && vx < vis_w - 240 {
                    return self.open_scrubber_dialog();
                }

                // 6. Visual Top strip (middle) -> Curtain (Brightness / Network / Controls)
                if vy < 140 && vx > 240 && vx < vis_w - 240 {
                    return self.open_curtain();
                }

                // 7. Page turns (Left third = Back, Right two-thirds = Forward)
                if vx < vis_w / 3 {
                    self.turn(false)
                } else {
                    self.turn(true)
                }
            }
            Gesture::Swipe { dir, .. } => {
                // Bottom-left up-swipe -> In-Book Quick Settings Sheet
                if dir == ybdev::input::SwipeDir::North && vx < 350 && vy > vis_h - 260 {
                    return self.open_quick_settings();
                }
                Action::Keep
            }
            Gesture::TwoFingerTap => {
                // Two-finger tap anywhere -> instant full waveform E-Ink clean refresh
                Action::RedrawFull
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
