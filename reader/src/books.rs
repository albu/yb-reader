use std::path::PathBuf;

use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;

use crate::backend::{create_backend, PageTurnResult, ReaderBackend};
use crate::chrome;
use crate::curtain::CurtainScreen;
use crate::dialogs;
use crate::positions;
use crate::selection::{self, SelState};
use crate::split::{RectF, ReaderSettings};

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::Orientation;

pub struct ReaderScreen {
    pw: u32,
    ph: u32,
    backend: Box<dyn ReaderBackend>,
    settings: ReaderSettings,
    page_gray: Option<Vec<u8>>,
    dims: (i32, i32),
    time_str: String,
    vocab_db: Option<&'static crate::vocab::VocabDb>,
    vocab_prof: crate::vocab::VocabProfile,
    page_words: Vec<(String, RectF)>,
    page_links: Vec<(RectF, String)>,
    sel_mode: bool,
    sel: Option<SelState>,
    jump_history: Vec<(usize, usize)>,
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

        let cached_snap = if backend.is_pdf() {
            crate::cache::load_snapshot(&name, resume, sub_idx, &settings, vw, vh)
        } else {
            None
        };
        if cached_snap.is_some() {
            plog(&format!("loaded instant page snapshot for {}", name));
        }

        let vocab_db = crate::vocab::VocabDb::open();
        let vocab_prof = crate::vocab::VocabProfile::load();

        ReaderScreen {
            pw: w,
            ph: h,
            backend,
            settings,
            page_gray: cached_snap,
            dims: (w as i32, h as i32),
            time_str: chrome::current_time_str(),
            vocab_db,
            vocab_prof,
            page_words: Vec::new(),
            page_links: Vec::new(),
            sel_mode: false,
            sel: None,
            jump_history: Vec::new(),
        }
    }

    fn book_name(&self) -> String {
        self.backend.book_name()
    }

    fn visual_dims(&self) -> (u32, u32) {
        Orientation::from_rotation(self.settings.split.rotation).visual_dims(self.pw, self.ph)
    }

    fn is_pdf(&self) -> bool {
        self.backend.is_pdf()
    }

    fn save_progress(&self) {
        let name = self.book_name();
        positions::record_pos(
            &name,
            self.backend.current_page(),
            self.backend.total_pages(),
            self.backend.current_sub_idx(),
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
                if best.as_ref().map_or(true, |(_, _, d)| dist < *d) {
                    best = Some((word.clone(), *r, dist));
                }
            }
        }
        best.map(|(w, r, _)| (w, r))
    }

    fn find_link_at_pos(&self, vx: f32, vy: f32) -> Option<(RectF, String)> {
        let pad = 12.0f32;
        let mut best: Option<(RectF, String, f32)> = None;

        for (r, uri) in &self.page_links {
            if vx >= r.x0 - pad && vx <= r.x1 + pad && vy >= r.y0 - pad && vy <= r.y1 + pad {
                let cx = (r.x0 + r.x1) / 2.0;
                let cy = (r.y0 + r.y1) / 2.0;
                let dist = (vx - cx).powi(2) + (vy - cy).powi(2);
                if best.as_ref().map_or(true, |(_, _, d)| dist < *d) {
                    best = Some((*r, uri.clone(), dist));
                }
            }
        }
        best.map(|(r, u, _)| (r, u))
    }

    fn open_footnote_or_link(&self, uri: &str) -> Action {
        self.backend.resolve_link_or_footnote(
            uri,
            self.page_gray.clone(),
            self.book_name(),
            self.settings,
        )
    }

    fn open_toc_dialog(&mut self) -> Action {
        let cur_page = self.backend.current_page();
        let back = self.jump_history.last().copied();
        self.backend.open_toc_dialog(cur_page, back, self.book_name(), self.settings)
    }

    fn open_scrubber_dialog(&mut self) -> Action {
        let cur_page = self.backend.current_page();
        let (vw, vh) = self.visual_dims();
        let back = self.jump_history.last().copied();
        self.backend.open_scrubber_dialog(
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
        let total = self.backend.total_pages();
        let page_no = self.backend.current_page();
        let sub_idx = self.backend.current_sub_idx();
        let settings = self.settings;
        let is_pdf = self.is_pdf();
        let base_gray = self.page_gray.clone();

        dialogs::quick_settings_sheet(
            path_name,
            page_no,
            sub_idx,
            total,
            settings,
            is_pdf,
            None,
            base_gray,
            move |_new_settings| {
                None
            },
        )
    }

    fn open_word_dialog(&mut self, entry: crate::vocab::WordEntry) -> Action {
        dialogs::word_dialog(entry, self.vocab_prof.clone(), self.page_gray.clone())
    }

    fn touch_to_visual(&self, x: u32, y: u32) -> (i32, i32) {
        let orient = Orientation::from_rotation(self.settings.split.rotation);
        orient.point_to_visual(self.pw, self.ph, x as i32, y as i32)
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
        let (vw, vh) = self.visual_dims();

        if let Some(s) = pos.settings {
            if s != self.settings {
                let old = self.settings;
                self.settings = s;
                self.backend.apply_settings_change(&old, &s, vw, vh);
                self.page_gray = None;
            }
        }

        self.backend.jump_to_sub(pos.sub_idx, vw, vh, &self.settings);
        self.save_progress();

        if self.page_gray.is_none() {
            Action::RedrawFull
        } else {
            Action::Redraw
        }
    }

    fn tick_interval(&self) -> std::time::Duration {
        if !self.backend.is_ready() {
            std::time::Duration::from_millis(150)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    fn on_tick(&mut self) -> Action {
        self.time_str = chrome::current_time_str();
        let (vw, vh) = self.visual_dims();

        if self.backend.poll(vw, vh, &self.settings) {
            Action::Redraw
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        let (vw, vh) = self.visual_dims();

        if let Some(err) = self.backend.error() {
            p.clear(255);
            let box_w = (w - pt(48.0)).min(pt(320.0));
            let box_h = pt(140.0);
            let box_x = (w - box_w) / 2;
            let box_y = (h - box_h) / 2;
            let r = Rect::new(box_x, box_y, box_w, box_h);
            p.rect(r, 255);
            p.rect_outline_t(r, 2, 0);
            p.text_center_in(box_x, box_x + box_w, box_y + pt(28.0), 12.0, 0, "Failed to Open Book");
            let err_trunc = p.truncate(8.0, err, (box_w - pt(24.0)) as f32);
            p.text_center_in(box_x, box_x + box_w, box_y + pt(55.0), 8.0, 100, &err_trunc);
            return;
        }

        let render_output = self.backend.render_page(vw, vh, &self.settings);
        if render_output.is_loading {
            p.clear(255);
            p.text_center(h / 2, 12.0, 100, "Opening…");
            return;
        }

        if let Some(gray) = render_output.gray {
            p.blit_gray(0, 0, vw as i32, vh as i32, &gray, vw as usize);
            self.page_gray = Some(gray);
        } else if let Some(cached) = &self.page_gray {
            p.blit_gray(0, 0, vw as i32, vh as i32, cached, vw as usize);
        }

        self.page_words = render_output.words;
        self.page_links = render_output.links;

        // Header & Footer Chrome
        let (footer_text, footer_page, footer_total) = self.backend.footer_info();
        let chap_title = self.backend.chapter_title();
        let book_name = self.book_name();

        let top_title = if self.is_pdf() {
            book_name.as_str()
        } else {
            chap_title.as_deref().unwrap_or(book_name.as_str())
        };

        chrome::draw_header(
            p,
            &self.time_str,
            top_title,
            self.settings.invert,
        );

        chrome::draw_footer(
            p,
            &footer_text,
            footer_page,
            footer_total,
            &[],
            self.settings.invert,
        );

        // Selection overlay
        if let Some(sel) = &self.sel {
            selection::draw_selection(p, sel, &self.page_words);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (vis_w, vis_h) = self.dims;
        let (vw, vh) = self.visual_dims();

        // 1. Edge Gestures: Curtain, Back, Brightness
        if g.top_edge_swipe() {
            return Action::Push(Box::new(CurtainScreen::new()));
        }
        if g.corner_back() {
            return Action::Pop;
        }

        match g {
            Gesture::Swipe { dir, .. } => {
                match dir {
                    SwipeDir::East => {
                        let res = self.backend.turn_page(-1, vw, vh, &self.settings);
                        if let PageTurnResult::Changed { redraw_full } = res {
                            self.save_progress();
                            return if redraw_full { Action::RedrawFull } else { Action::Redraw };
                        }
                    }
                    SwipeDir::West => {
                        let res = self.backend.turn_page(1, vw, vh, &self.settings);
                        if let PageTurnResult::Changed { redraw_full } = res {
                            self.save_progress();
                            return if redraw_full { Action::RedrawFull } else { Action::Redraw };
                        }
                    }
                    _ => {}
                }
                Action::Keep
            }
            Gesture::Tap { x, y } => {
                let (vx, vy) = self.touch_to_visual(x, y);

                // Bookmark toggle in top right
                if vx > vis_w - pt(64.0) && vy < pt(34.0) {
                    self.sel_mode = !self.sel_mode;
                    self.sel = None;
                    return Action::Redraw;
                }

                // Header TOC icon in top left
                if vx < pt(60.0) && vy < pt(34.0) {
                    return self.open_toc_dialog();
                }

                // Bottom Left -> Quick Settings Sheet
                if vx < pt(80.0) && vy > vis_h - pt(45.0) {
                    return self.open_quick_settings_sheet();
                }

                // Bottom Center -> Scrubber / Seek
                if vx >= pt(80.0) && vx <= vis_w - pt(80.0) && vy > vis_h - pt(45.0) {
                    return self.open_scrubber_dialog();
                }

                // Check footnote / hyperlink tap
                if let Some((_rect, uri)) = self.find_link_at_pos(vx as f32, vy as f32) {
                    return self.open_footnote_or_link(&uri);
                }

                // Page Turn tap zones
                if vx < vis_w / 3 {
                    let res = self.backend.turn_page(-1, vw, vh, &self.settings);
                    if let PageTurnResult::Changed { redraw_full } = res {
                        self.save_progress();
                        return if redraw_full { Action::RedrawFull } else { Action::Redraw };
                    }
                } else if vx > vis_w * 2 / 3 {
                    let res = self.backend.turn_page(1, vw, vh, &self.settings);
                    if let PageTurnResult::Changed { redraw_full } = res {
                        self.save_progress();
                        return if redraw_full { Action::RedrawFull } else { Action::Redraw };
                    }
                }
                Action::Keep
            }
            Gesture::LongPress { x, y } => {
                let (vx, vy) = self.touch_to_visual(x, y);
                if let Some((word_text, _rect)) = self.find_word_at_pos(vx as f32, vy as f32) {
                    if word_text.starts_with('*') || word_text.starts_with('[') {
                        if let Some((_, uri)) = self.find_link_at_pos(vx as f32, vy as f32) {
                            return self.open_footnote_or_link(&uri);
                        }
                    }

                    let found = self
                        .vocab_db
                        .as_ref()
                        .and_then(|db| db.lookup(&word_text));
                    if let Some(entry) = found {
                        return self.open_word_dialog(entry);
                    }
                    return dialogs::dict_miss(&word_text, self.page_gray.clone());
                }
                Action::Keep
            }
            _ => Action::Keep,
        }
    }
}
