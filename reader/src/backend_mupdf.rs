use mupdf::Document;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use yui::screen::Action;

use crate::backend::{PageTurnResult, ReaderBackend, RenderOutput};
use crate::dialogs;
use crate::document::{self as doc_store, BookReady};
use crate::positions;
use crate::render::{self, render_page};
use crate::split::ReaderSettings;

struct CachedPage {
    page_no: usize,
    zoom_bits: u32,
    bounds: mupdf::Rect,
    pixmap: mupdf::Pixmap,
    text_page: Option<mupdf::TextPage>,
    links: Vec<(crate::split::RectF, String)>,
}

pub struct PdfBackend {
    path: PathBuf,
    loading: Option<Receiver<Result<BookReady, String>>>,
    doc: Option<Rc<Document>>,
    err: Option<String>,
    page_no: usize,
    sub_idx: usize,
    total: usize,
    sub_box_count: usize,
    /// Turn deltas that arrived while the document was still opening,
    /// replayed on ready. Returning Queued without storing them made taps
    /// during open vanish silently — the yread engine queues, this one
    /// must too.
    queued_turns: i32,
    cached_page: Option<CachedPage>,
}

impl PdfBackend {
    pub fn new(path: PathBuf, resume_page: usize, resume_sub: usize) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pos = positions::resume_pos(&name);
        let sub_idx = if pos.page == resume_page {
            resume_sub
        } else {
            0
        };

        let rx = doc_store::open_async(path.clone());

        Self {
            path,
            loading: Some(rx),
            doc: None,
            err: None,
            page_no: resume_page,
            sub_idx,
            total: pos.total.max(1),
            sub_box_count: 1,
            queued_turns: 0,
            cached_page: None,
        }
    }
}

impl ReaderBackend for PdfBackend {
    fn is_pdf(&self) -> bool {
        true
    }

    fn mupdf_doc(&self) -> Option<Rc<Document>> {
        self.doc.clone()
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
        self.doc.is_some()
    }

    fn busy_phase(&self) -> Option<crate::backend::BusyPhase> {
        if self.doc.is_none() {
            Some(crate::backend::BusyPhase::Opening)
        } else {
            None
        }
    }

    fn error(&self) -> Option<&str> {
        self.err.as_deref()
    }

    fn poll(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> bool {
        let Some(rx) = &self.loading else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(ready)) => {
                self.doc = Some(Rc::new(ready.doc.into_inner()));
                self.total = ready.total;
                self.page_no = self.page_no.min(self.total.saturating_sub(1));
                self.loading = None;
                // Drain taps buffered during open. The counter is taken out
                // first: the closure needs &mut self for turn_page. The pdf
                // backend never re-queues on its own, so the queue drains
                // fully here (mem::take already left it at 0).
                let mut q = std::mem::take(&mut self.queued_turns);
                let _applied =
                    crate::backend::drain_queued_turns(&mut q, |step| {
                        self.turn_page(step, vw, vh, settings)
                    });
                // The document just became ready: repaint regardless of
                // whether any queued taps applied.
                true
            }
            Ok(Err(e)) => {
                self.err = Some(e);
                self.loading = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The worker died without sending (panic past catch_unwind,
                // abort): surface it instead of "Opening…" forever.
                self.err = Some("open worker died".to_string());
                self.loading = None;
                true
            }
        }
    }

    fn turn_page(
        &mut self,
        delta: i32,
        _vw: u32,
        _vh: u32,
        settings: &ReaderSettings,
    ) -> PageTurnResult {
        if self.doc.is_none() {
            self.queued_turns += delta;
            return PageTurnResult::Queued;
        }

        self.sub_box_count = settings.split.sub_boxes().len().max(1);

        if delta > 0 {
            if self.sub_idx + 1 < self.sub_box_count {
                self.sub_idx += 1;
                PageTurnResult::Changed { redraw_full: false }
            } else if self.page_no + 1 < self.total {
                self.page_no += 1;
                self.sub_idx = 0;
                PageTurnResult::Changed { redraw_full: false }
            } else {
                PageTurnResult::AtBoundary
            }
        } else if delta < 0 {
            if self.sub_idx > 0 {
                self.sub_idx -= 1;
                PageTurnResult::Changed { redraw_full: false }
            } else if self.page_no > 0 {
                self.page_no -= 1;
                self.sub_idx = self.sub_box_count.saturating_sub(1);
                PageTurnResult::Changed { redraw_full: false }
            } else {
                PageTurnResult::AtBoundary
            }
        } else {
            PageTurnResult::AtBoundary
        }
    }

    fn jump_to_sub(&mut self, sub_idx: usize, _vw: u32, _vh: u32, settings: &ReaderSettings) {
        if self.sub_idx_is_mupdf() {
            // Clamp against the settings-derived box count: the render-state
            // `sub_box_count` is still the constructor default (1) until the
            // first turn_page/render_page, and clamping against it zeroed
            // every restored split position on reopen.
            let count = settings.split.sub_boxes().len().max(1);
            self.sub_idx = sub_idx.min(count.saturating_sub(1));
        }
    }

    fn jump_to_page(&mut self, page: usize, _vw: u32, _vh: u32, _settings: &ReaderSettings) {
        self.page_no = page.min(self.total.saturating_sub(1));
        self.sub_idx = 0;
    }

    fn render_page(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> RenderOutput {
        let Some(doc) = self.doc.as_ref().cloned() else {
            return RenderOutput {
                gray: None,
                words: Vec::new(),
                links: Vec::new(),
                is_loading: true,
            };
        };
        self.sub_box_count = settings.split.sub_boxes().len().max(1);

        let t0 = std::time::Instant::now();
        let mut words = Vec::new();
        let mut links = Vec::new();

        // 1. Intra-page cache hit (e.g. sub 0 -> sub 1 in 2-split mode):
        let mut gray = None;
        if let Some(cached) = &self.cached_page {
            if cached.page_no == self.page_no {
                if let Some(geom) = render::LayoutGeom::new_for_page(settings, cached.bounds, self.page_no, self.sub_idx, vw, vh) {
                    if geom.zoom.to_bits() == cached.zoom_bits {
                        if let Some(tp) = &cached.text_page {
                            words = render::words_from_text_page(tp, &geom);
                        }
                        links = cached.links.clone();
                        let sliced = render::slice_pixmap(&cached.pixmap, &geom, self.sub_idx, settings, vw, vh);
                        let elapsed = t0.elapsed().as_millis();
                        ybdev::log::plog(&format!(
                            "pdf intra-page slice p{} s{} in {}ms",
                            self.page_no,
                            self.sub_idx,
                            elapsed
                        ));
                        gray = Some(sliced);
                    }
                }
            }
        }

        // 2. Cold full render if not cached:
        if gray.is_none() {
            match doc.load_page(self.page_no as i32) {
                Err(e) => {
                    ybdev::log::plog(&format!("render: load_page {}: {}", self.page_no, e));
                }
                Ok(p) => match p.bounds() {
                    Err(e) => {
                        ybdev::log::plog(&format!("render: page {} bounds: {}", self.page_no, e));
                    }
                    Ok(bounds) => match render::LayoutGeom::new_for_page(settings, bounds, self.page_no, self.sub_idx, vw, vh) {
                        None => {
                            ybdev::log::plog(&format!(
                                "render: page {} has a degenerate box {:?}",
                                self.page_no, bounds
                            ));
                        }
                        Some(geom) => {
                            let tp = p.to_text_page(mupdf::TextPageFlags::empty()).ok();
                            if let Some(ref text_page) = tp {
                                words = render::words_from_text_page(text_page, &geom);
                            }
                            links = render::links_from_page(&p, &geom);

                            mupdf::Context::get().set_text_aa_level(render::TEXT_AA_LEVEL);
                            let mut m = mupdf::Matrix::IDENTITY;
                            m.scale(geom.zoom, geom.zoom);
                            match p.to_pixmap(&m, &mupdf::Colorspace::device_gray(), false, true) {
                                Err(e) => {
                                    ybdev::log::plog(&format!("render: to_pixmap failed: {}", e));
                                }
                                Ok(pm) => {
                                    let sliced = render::slice_pixmap(&pm, &geom, self.sub_idx, settings, vw, vh);
                                    let elapsed = t0.elapsed().as_millis();
                                    ybdev::log::plog(&format!(
                                        "pdf full render p{} s{} in {}ms (rss={})",
                                        self.page_no,
                                        self.sub_idx,
                                        elapsed,
                                        crate::document::rss_mib()
                                    ));
                                    self.cached_page = Some(CachedPage {
                                        page_no: self.page_no,
                                        zoom_bits: geom.zoom.to_bits(),
                                        bounds,
                                        pixmap: pm,
                                        text_page: tp,
                                        links: links.clone(),
                                    });
                                    gray = Some(sliced);
                                }
                            }
                        }
                    },
                },
            }
        }

        RenderOutput {
            gray,
            words,
            links,
            is_loading: false,
        }
    }

    fn footer_info(&self) -> (String, usize, usize) {
        let cur = self.page_no + 1;
        (format!("p. {} / {}", cur, self.total), cur, self.total)
    }

    fn chapter_title(&self) -> Option<String> {
        None
    }

    fn toc_chapter_pages(&self) -> Vec<usize> {
        let mut pages = Vec::new();
        if let Some(doc) = &self.doc {
            if let Ok(outlines) = doc.outlines() {
                Self::collect_outline_pages(&outlines, &mut pages);
            }
        }
        pages.sort_unstable();
        pages.dedup();
        pages
    }

    fn resolve_link_or_footnote(
        &self,
        uri: &str,
        bg: Option<Vec<u8>>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action {
        if let Some(doc) = &self.doc {
            dialogs::footnote_dialog(doc.as_ref(), uri, bg, path_name, self.total, settings)
        } else {
            Action::Keep
        }
    }

    fn open_toc_dialog(
        &self,
        cur_page: usize,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action {
        if let Some(doc) = &self.doc {
            if let Ok(outlines) = doc.outlines() {
                return dialogs::toc_dialog(
                    &outlines, cur_page, back, path_name, self.total, settings,
                );
            }
        }
        Action::Keep
    }

    fn open_scrubber_dialog(
        &self,
        cur_page: usize,
        bg: Option<Vec<u8>>,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
        w: u32,
        h: u32,
    ) -> Action {
        if let Some(doc) = &self.doc {
            dialogs::scrubber_dialog(
                doc, cur_page, self.total, bg, back, path_name, settings, w, h,
            )
        } else {
            Action::Keep
        }
    }

    fn apply_settings_change(
        &mut self,
        _old: &ReaderSettings,
        _new: &ReaderSettings,
        _vw: u32,
        _vh: u32,
    ) -> bool {
        self.cached_page = None;
        true
    }

    fn interactive_preview(
        &mut self,
        settings: &ReaderSettings,
        vw: u32,
        vh: u32,
    ) -> Option<Vec<u8>> {
        self.cached_page = None;
        self.doc
            .as_ref()
            .and_then(|doc| render_page(doc.as_ref(), self.page_no, self.sub_idx, settings, vw, vh))
    }
}

impl PdfBackend {
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

    fn sub_idx_is_mupdf(&self) -> bool {
        self.sub_idx < 100
    }
}
