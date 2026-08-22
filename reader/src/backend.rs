use std::path::PathBuf;
use yui::screen::Action;
use crate::split::{RectF, ReaderSettings};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTurnResult {
    Changed { redraw_full: bool },
    AtBoundary,
    Queued,
}

pub struct RenderOutput {
    pub gray: Option<Vec<u8>>,
    pub words: Vec<(String, RectF)>,
    pub links: Vec<(RectF, String)>,
    pub is_loading: bool,
}

pub trait ReaderBackend {
    fn is_pdf(&self) -> bool;

    /// The mupdf document handle, for PDF-only dialogs the shared
    /// quick-settings sheet hosts (crop). None on every other engine.
    fn mupdf_doc(&self) -> Option<std::rc::Rc<mupdf::Document>> {
        None
    }
    fn book_name(&self) -> String;
    fn total_pages(&self) -> usize;
    fn current_page(&self) -> usize;
    fn current_sub_idx(&self) -> usize;
    fn is_ready(&self) -> bool;
    fn error(&self) -> Option<&str>;

    /// True while the backend still has background work (parse, layout,
    /// reflow, prefetch) — the App polls at a fast interval so results
    /// land promptly and no loading screen gets stuck.
    fn has_pending_work(&self) -> bool {
        false
    }

    fn poll(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> bool;
    fn turn_page(&mut self, delta: i32, vw: u32, vh: u32, settings: &ReaderSettings) -> PageTurnResult;

    fn jump_to_sub(&mut self, sub_idx: usize, vw: u32, vh: u32, settings: &ReaderSettings);
    #[allow(dead_code)]
    fn jump_to_page(&mut self, page: usize, vw: u32, vh: u32, settings: &ReaderSettings);

    fn render_page(&mut self, vw: u32, vh: u32, settings: &ReaderSettings) -> RenderOutput;
    fn footer_info(&self) -> (String, usize, usize);
    fn chapter_title(&self) -> Option<String>;

    /// 0-based pages where chapters start (outline destinations), for the
    /// footer's progress ticks. Empty when the format has no outline.
    fn toc_chapter_pages(&self) -> Vec<usize> {
        Vec::new()
    }

    fn resolve_link_or_footnote(
        &self,
        uri: &str,
        bg: Option<Vec<u8>>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action;

    fn open_toc_dialog(
        &self,
        cur_page: usize,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
    ) -> Action;

    fn open_scrubber_dialog(
        &self,
        cur_page: usize,
        bg: Option<Vec<u8>>,
        back: Option<(usize, usize)>,
        path_name: String,
        settings: ReaderSettings,
        w: u32,
        h: u32,
    ) -> Action;

    fn apply_settings_change(&mut self, old: &ReaderSettings, new: &ReaderSettings, vw: u32, vh: u32) -> bool;
    #[allow(dead_code)]
    fn interactive_preview(&mut self, settings: &ReaderSettings, vw: u32, vh: u32) -> Option<Vec<u8>>;
}

/// Human-readable message from a caught panic payload, for open workers
/// that convert parser/FFI panics into ordinary open errors.
pub fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

pub fn create_backend(path: PathBuf, resume_page: usize, resume_sub: usize, w: u32, h: u32, settings: &ReaderSettings) -> Box<dyn ReaderBackend> {
    let is_pdf = path
        .extension()
        .map(|e| e.to_string_lossy().eq_ignore_ascii_case("pdf") || e.to_string_lossy().eq_ignore_ascii_case("cbz"))
        .unwrap_or(false);

    if is_pdf {
        Box::new(crate::backend_mupdf::PdfBackend::new(path, resume_page, resume_sub))
    } else {
        Box::new(crate::backend_yread::YreadBackend::new(path, resume_sub, w, h, settings))
    }
}
