use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use yui::screen::Action;
use ybdev::log::plog;

use crate::backend::{PageTurnResult, ReaderBackend, RenderOutput};
use crate::dialogs;
use crate::split::{RectF, ReaderSettings};

pub struct YreadBackend {
    path: PathBuf,
    ybook: Option<Arc<yread::Book>>,
    yfonts: Option<Rc<yread::font::FontSystem>>,
    ycache: Arc<Mutex<yread::shape::ShapeCache>>,
    yworker_fonts: Option<Arc<yread::font::FontSystem>>,
    yraster: yread::raster::Rasterizer,
    ychap_idx: usize,
    ychap_page: usize,
    ychap_cache: HashMap<usize, (yread::model::ChapterPageTable, Vec<yread::paginate::PageLayout>)>,
    ychap_offsets: Vec<usize>,
    total: usize,
    page_no: usize,
    sub_idx: usize,
    y_char_offset: usize,
    landing_char: Option<usize>,
    yreflow: bool,
    ylayout_wait: bool,
    yopen_rx: Option<Receiver<Result<yread::Book, String>>>,
    ybg_rx: Option<Receiver<(usize, yread::model::ChapterPageTable, Vec<yread::paginate::PageLayout>)>>,
    yqueued_turns: i32,
    err: Option<String>,
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
            let res = match ext.as_str() {
                "fb2" | "zip" => yread::fb2::parse_fb2_path(&path_cl),
                _ => yread::epub::parse_epub_file(&path_cl),
            };
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
            total: 1,
            page_no: 0,
            sub_idx: resume_sub,
            y_char_offset: 0,
            landing_char: None,
            yreflow: false,
            ylayout_wait: false,
            yopen_rx: Some(rx),
            ybg_rx: None,
            yqueued_turns: 0,
            err: None,
        };

        backend.yread_land_at_sub(resume_sub, vw, vh, settings);
        backend
    }

    fn hypher_lang_for(lang: &str) -> hypher::Lang {
        let code = lang.to_lowercase();
        if code.starts_with("ru") {
            hypher::Lang::Russian
        } else if code.starts_with("de") {
            hypher::Lang::German
        } else if code.starts_with("fr") {
            hypher::Lang::French
        } else if code.starts_with("es") {
            hypher::Lang::Spanish
        } else if code.starts_with("it") {
            hypher::Lang::Italian
        } else {
            hypher::Lang::English
        }
    }

    fn yread_layout_config(&self, settings: &ReaderSettings, vw: u32, vh: u32) -> yread::paginate::LayoutConfig {
        let pad = settings.margin_pad;
        yread::paginate::LayoutConfig {
            page_width: vw,
            page_height: vh,
            margin_left: pad,
            margin_right: pad,
            margin_top: pad + if settings.show_header { 92 } else { 0 },
            margin_bottom: pad + 50,
            font_size: settings.font_size,
            line_spacing: settings.line_spacing,
            paragraph_spacing: 0.25,
            indent_em: 1.2,
            hyphenate: true,
        }
    }

    fn ensure_fonts(&mut self) {
        if self.yworker_fonts.is_none() {
            self.yworker_fonts = Some(Arc::new(yread::font::FontSystem::default()));
        }
        if self.yfonts.is_none() {
            self.yfonts = Some(Rc::new(yread::font::FontSystem::default()));
        }
    }

    fn decode_yread_sub(sub: usize) -> (usize, usize) {
        (sub / 1_000_000, sub % 1_000_000)
    }

    fn yread_land_at(&mut self, chapter: usize, char_offset: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        let max_ch = self
            .ybook
            .as_ref()
            .map(|b| b.chapters.len())
            .unwrap_or(1)
            .saturating_sub(1);
        let target_ch = chapter.min(max_ch);
        if target_ch != self.ychap_idx && !self.ychap_cache.contains_key(&target_ch) {
            self.ybg_rx = None;
            self.ylayout_wait = true;
            if self.ybook.is_some() {
                self.ychap_idx = target_ch;
                let cfg = self.yread_layout_config(settings, vw, vh);
                self.spawn_yread_background_paginator(&cfg);
            }
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
    }

    fn yread_land_at_sub(&mut self, sub: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        let (ch, chr) = Self::decode_yread_sub(sub);
        self.yread_land_at(ch, chr, vw, vh, settings);
    }

    fn spawn_yread_background_paginator(&mut self, cfg: &yread::paginate::LayoutConfig) {
        self.ensure_fonts();
        let Some(ref yb) = self.ybook else { return };
        let Some(ref worker_fonts) = self.yworker_fonts else { return };

        let book_arc = Arc::clone(yb);
        let fonts = Arc::clone(worker_fonts);
        let shape_cache = Arc::clone(&self.ycache);
        let cfg = cfg.clone();
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
        let Some(ref ybook) = self.ybook else { return };
        let cpp = chars_per_page.max(200.0);
        let mut offsets = Vec::with_capacity(ybook.chapters.len());
        let mut cum = 0;
        for (i, ch) in ybook.chapters.iter().enumerate() {
            offsets.push(cum);
            if let Some((_, layouts)) = self.ychap_cache.get(&i) {
                cum += layouts.len().max(1);
            } else {
                let estimated = ((ch.char_count() as f32 / cpp).round() as usize).max(1);
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
                let arc_book = Arc::new(book);
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
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        }
    }

    fn yread_receive_bg(&mut self) -> bool {
        let Some(rx) = self.ybg_rx.take() else {
            return false;
        };
        let mut received_any = false;
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
                    if !self.ychap_cache.contains_key(&ch_idx) {
                        self.ychap_cache.insert(ch_idx, (pt, layouts));
                    }
                    if ch_idx == self.ychap_idx && self.ylayout_wait {
                        self.ylayout_wait = false;
                    }
                    received_any = true;
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
        received_any
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
        self.ybook.is_some() && !self.ylayout_wait
    }

    fn error(&self) -> Option<&str> {
        self.err.as_deref()
    }

    fn poll(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> bool {
        let mut redraw = self.yread_receive_open(vw, vh, settings);
        if self.yread_receive_bg() {
            if self.yqueued_turns != 0 && !self.ylayout_wait {
                let q = self.yqueued_turns;
                self.yqueued_turns = 0;
                self.turn_page(q, vw, vh, settings);
            }
            redraw = true;
        }
        redraw
    }

    fn turn_page(&mut self, delta: i32, vw: u32, vh: u32, settings: &ReaderSettings) -> PageTurnResult {
        if self.ylayout_wait || self.ybook.is_none() {
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
                if let Some(l) = layouts.get(self.ychap_page) {
                    self.y_char_offset = l.start_char;
                }
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
                if let Some(l) = layouts.get(self.ychap_page) {
                    self.y_char_offset = l.start_char;
                }
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

    fn jump_to_yread(&mut self, chapter_idx: usize, char_offset: usize, vw: u32, vh: u32, settings: &ReaderSettings) {
        self.yread_land_at(chapter_idx, char_offset, vw, vh, settings);
    }

    fn render_page(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> RenderOutput {
        if self.ybook.is_none() || self.ylayout_wait {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        }

        self.ensure_fonts();
        let Some(book) = self.ybook.as_ref().cloned() else {
            return RenderOutput { gray: None, words: Vec::new(), links: Vec::new(), is_loading: true };
        };
        let Some(fonts) = self.yfonts.as_ref().cloned() else {
            return RenderOutput { gray: None, words: Vec::new(), links: Vec::new(), is_loading: true };
        };

        let cfg = self.yread_layout_config(settings, vw, vh);

        // Hysteresis eviction
        let n = book.chapters.len();
        let lo = self.ychap_idx.saturating_sub(4);
        let hi = (self.ychap_idx + 4).min(n.saturating_sub(1));
        self.ychap_cache.retain(|&i, _| i >= lo && i <= hi);

        let window_uncached = (self.ychap_idx.saturating_sub(2)..=(self.ychap_idx + 2).min(n - 1))
            .filter(|i| !self.ychap_cache.contains_key(i))
            .count();
        if self.ybg_rx.is_none() && window_uncached > 0 {
            self.spawn_yread_background_paginator(&cfg);
        }

        if !self.ychap_cache.contains_key(&self.ychap_idx) {
            let mut cache = self.ycache.lock().unwrap_or_else(|p| p.into_inner());
            let lang = Self::hypher_lang_for(&book.meta.language);
            let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
                &book.chapters[self.ychap_idx],
                Some(&book.image_sizes),
                &cfg,
                &fonts,
                &mut cache,
                Some(lang),
            );
            self.ychap_cache.insert(self.ychap_idx, (pt, layouts));
        }

        let cur_chars = book.chapters[self.ychap_idx].char_count().max(100);
        let cur_layout_len = self.ychap_cache.get(&self.ychap_idx).map(|(_, l)| l.len()).unwrap_or(1);
        let chars_per_page = (cur_chars as f32 / cur_layout_len.max(1) as f32).max(200.0);
        self.compute_chapter_page_offsets(chars_per_page);

        let Some((pt, layouts)) = self.ychap_cache.get(&self.ychap_idx) else {
            return RenderOutput { gray: None, words: Vec::new(), links: Vec::new(), is_loading: true };
        };

        if let Some(target_char) = self.landing_char.take() {
            self.ychap_page = if target_char == usize::MAX {
                layouts.len().saturating_sub(1)
            } else {
                pt.page_for_char(target_char).min(layouts.len().saturating_sub(1))
            };
        }

        let Some(cur_layout) = layouts.get(self.ychap_page) else {
            return RenderOutput { gray: None, words: Vec::new(), links: Vec::new(), is_loading: true };
        };

        self.y_char_offset = cur_layout.start_char;

        let global_p = self.ychap_offsets.get(self.ychap_idx).copied().unwrap_or(0) + self.ychap_page;
        self.page_no = global_p;
        self.sub_idx = self.ychap_idx * 1_000_000 + (self.y_char_offset % 1_000_000);

        let t0 = std::time::Instant::now();
        let mut gray = vec![255u8; (vw * vh) as usize];
        self.yraster.render_page(&book, cur_layout, &cfg, &fonts, &mut gray, vw as usize);
        if settings.invert || settings.contrast != crate::split::ContrastMode::Normal || settings.white_cutoff != 0 {
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
                        yread::line::LineItem::Word { byte_start, byte_end, shaped, style, .. } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y - line.ascender,
                                    cur_x + shaped.advance,
                                    origin_y + y - line.ascender + line.height,
                                );
                                words.push((w_str.to_string(), rect));
                                if let Some(target) = &style.footnote_ref {
                                    links.push((rect, target.clone()));
                                }
                            }
                            cur_x += shaped.advance;
                        }
                        yread::line::LineItem::HyphenatedPrefix { byte_start, byte_end, prefix_shaped, hyphen_adv, style, .. } => {
                            if let Some(w_str) = chapter.text.get(*byte_start..*byte_end) {
                                let rect = RectF::new(
                                    cur_x,
                                    origin_y + y - line.ascender,
                                    cur_x + prefix_shaped.advance + hyphen_adv,
                                    origin_y + y - line.ascender + line.height,
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
            let total_chars = book.chapters.iter().map(|c| c.text.chars().count()).sum::<usize>().max(1);
            let prev_chars: usize = book.chapters.iter().take(self.ychap_idx).map(|c| c.text.chars().count()).sum();
            let cur_chars = prev_chars + self.y_char_offset;
            let p = ((cur_chars as f64 / total_chars as f64) * 100.0).clamp(0.0, 100.0) as usize;
            let cname = book.chapters.get(self.ychap_idx).map(|c| c.title.clone()).unwrap_or_default();
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
            let cur_chars = book.chapters.get(self.ychap_idx).map(|c| c.char_count()).unwrap_or(1500);
            let cur_pages = self.ychap_cache.get(&self.ychap_idx).map(|(_, l)| l.len()).unwrap_or(1).max(1);
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
        cur_page: usize,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action {
        if let Some(book) = &self.ybook {
            let chars_per_page = 1500.0;
            return dialogs::yread_toc_dialog(
                &book.toc,
                &self.ychap_offsets,
                chars_per_page,
                cur_page,
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

    fn apply_settings_change(&mut self, old: &ReaderSettings, new: &ReaderSettings, vw: u32, vh: u32) -> bool {
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

    fn interactive_preview(&mut self, settings: &ReaderSettings, vw: u32, vh: u32) -> Option<Vec<u8>> {
        self.ensure_fonts();
        let book = self.ybook.as_ref()?.clone();
        let chap = book.chapters.get(self.ychap_idx)?;
        let fonts = self.yfonts.as_ref()?.clone();

        let cfg = self.yread_layout_config(settings, vw, vh);
        let mut cache = self.ycache.lock().unwrap_or_else(|p| p.into_inner());
        let lang = Self::hypher_lang_for(&book.meta.language);
        let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
            chap,
            Some(&book.image_sizes),
            &cfg,
            &fonts,
            &mut cache,
            Some(lang),
        );
        let landing_page = pt.page_for_char(self.y_char_offset).min(layouts.len().saturating_sub(1));
        let layout = layouts.get(landing_page)?;
        let mut raster = yread::raster::Rasterizer::new();
        let mut gray = vec![255u8; (vw * vh) as usize];
        raster.render_page(&book, layout, &cfg, &fonts, &mut gray, vw as usize);
        if settings.invert || settings.contrast != crate::split::ContrastMode::Normal || settings.white_cutoff != 0 {
            settings.apply_lut(&mut gray);
        }
        Some(gray)
    }
}
