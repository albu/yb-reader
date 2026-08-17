//! Local library + EPUB/PDF reader screens via MuPDF (the same engine
//! family KOReader uses). EPUB is reflowed to the panel width; pages
//! render in grayscale and are CACHED as pixels — re-presenting a page
//! after an overlay (frontlight) is a blit, never a MuPDF re-render.
//!
//! Opening is ASYNC: mupdf reflows the WHOLE book up front (measured:
//! 7s for a 1691-page EPUB, vs 75ms open and 79ms per-page render), so
//! open+layout run on a worker thread behind an "Opening…" screen — the
//! UI stays responsive and corner-back works mid-load. The last closed
//! book is kept warm (laid-out) in a process-global cache, so resuming
//! via the Continue row is instant.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Mutex;
use std::time::Instant;

use mupdf::{Colorspace, Document, Matrix};
use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::plog;
use ybdev::sysinfo;

use crate::positions;
use crate::wifi;
use yui::painter::{pt, Painter};
use yui::screen::{Action, Screen};

const LIB_DIR: &str = "/mnt/us/documents";
const MARGIN: u32 = 72; // px
const FOOTER_H: u32 = 88; // px

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

/// The most recently closed book, already laid out. One entry — a laid
/// out 1700-page EPUB costs tens of MB, and the Continue flow only ever
/// needs the last one.
///
/// RAM audit (2026-08-17, device has 474 MB total / ~96 MB available):
/// at most ONE entry by construction — taken on open (a *different*
/// book is dropped before the new layout spawns, so two laid-out docs
/// never coexist), replaced on close, and nothing pushes a reader on
/// top of a reader. Measured: the warm 1691-page epub costs ~55 MB,
/// 41 page turns moved RSS by +1 MB total (mupdf's 256 MB store cap
/// is never approached on text content), warm reopen is free, and a
/// book swap drops the old doc first (73→66 MB observed). The rss=
/// log lines below keep watching it.
static WARM: Mutex<Option<(PathBuf, SendDoc, usize)>> = Mutex::new(None);

fn open_async(
    path: PathBuf,
    w: u32,
    h: u32,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let r = (|| -> Result<BookReady, String> {
            let mut doc = Document::open(path.as_os_str()).map_err(|e| e.to_string())?;
            let open_ms = t0.elapsed().as_millis();
            // Reflow to the reading area (no-op for fixed-layout docs).
            // mupdf.layout() works in POINTS: raw pixels laid books out
            // ~4x too wide and body text rendered ~4x too small.
            let avail_w = (w - 2 * MARGIN) as f32 * 72.0 / 300.0;
            let avail_h = (h - 2 * MARGIN - FOOTER_H) as f32 * 72.0 / 300.0;
            let _ = doc.layout(avail_w, avail_h, 11.0);
            let total = doc.page_count().unwrap_or(1).max(1) as usize;
            plog(&format!(
                "book open {}ms + layout {}ms ({} pages) rss={} avail={}",
                open_ms,
                t0.elapsed().as_millis() - open_ms,
                total,
                rss_mib(),
                avail_mib()
            ));
            Ok(BookReady { doc: SendDoc(doc), total })
        })();
        let _ = tx.send(r); // receiver gone = user backed out; doc drops here
    });
    rx
}

// ---- ReaderScreen -------------------------------------------------------

pub struct ReaderScreen {
    path: PathBuf,
    w: u32,
    h: u32,
    /// Pending worker result while the "Opening…" screen shows.
    loading: Option<Receiver<Result<BookReady, String>>>,
    doc: Option<Document>,
    err: Option<String>,
    total: usize,
    page_no: usize,
    /// Rendered page pixels (stride == width). Presenting the same page
    /// again — overlay pop, screen-clean tap — is a blit from this cache.
    page_gray: Option<Vec<u8>>,
    /// Tap-zone math (px); draw runs before gestures, cache dims there.
    dims: (i32, i32),
}

impl ReaderScreen {
    /// `resume` is the page to open on (from the positions store).
    pub fn new(path: PathBuf, resume: usize, w: u32, h: u32) -> ReaderScreen {
        ReaderScreen {
            path,
            w,
            h,
            loading: None,
            doc: None,
            err: None,
            total: 1,
            page_no: resume,
            page_gray: None,
            dims: (1236, 1648),
        }
    }

    fn book_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn turn(&mut self, forward: bool) -> Action {
        let target = if forward {
            (self.page_no + 1).min(self.total.saturating_sub(1))
        } else {
            self.page_no.saturating_sub(1)
        };
        if target == self.page_no {
            // No-op at either end: no re-render, no refresh.
            return Action::Keep;
        }
        self.page_no = target;
        self.page_gray = None;
        // Persist progress on every turn — cheap (one small file) and
        // crash-safe (a kill never loses the position).
        positions::record(&self.book_name(), self.page_no, self.total);
        Action::Redraw
    }
}

impl Screen for ReaderScreen {
    fn on_enter(&mut self) -> Action {
        wifi::keep_awake(true);
        // Opening marks the book as last-read immediately; the real total
        // follows when the layout completes.
        positions::record(&self.book_name(), self.page_no, 0);

        // Warm cache: same book reopened → skip open+layout entirely.
        if let Ok(mut warm) = WARM.lock() {
            if let Some((p, SendDoc(doc), total)) = warm.take() {
                if p == self.path {
                    plog(&format!("book warm: instant open (rss={})", rss_mib()));
                    self.doc = Some(doc);
                    self.total = total;
                    positions::record(&self.book_name(), self.page_no, self.total);
                    return Action::RedrawFull;
                }
                // Different book: the cache holds one; drop the old doc.
            }
        }
        self.loading = Some(open_async(self.path.clone(), self.w, self.h));
        Action::RedrawFull
    }

    fn on_leave(&mut self) {
        wifi::keep_awake(false);
        if let Some(doc) = self.doc.take() {
            if let Ok(mut warm) = WARM.lock() {
                *warm = Some((self.path.clone(), SendDoc(doc), self.total));
            }
        }
        plog(&format!(
            "book closed rss={} avail={}",
            rss_mib(),
            avail_mib()
        ));
    }

    fn tick_interval(&self) -> std::time::Duration {
        if self.loading.is_some() {
            std::time::Duration::from_millis(150)
        } else {
            std::time::Duration::from_secs(1)
        }
    }

    fn on_tick(&mut self) -> Action {
        let Some(rx) = &self.loading else {
            return Action::Keep;
        };
        match rx.try_recv() {
            Ok(Ok(ready)) => {
                self.loading = None;
                self.total = ready.total;
                let SendDoc(doc) = ready.doc;
                self.doc = Some(doc);
                positions::record(&self.book_name(), self.page_no, self.total);
                Action::RedrawFull
            }
            Ok(Err(e)) => {
                plog(&format!("open {}: {}", self.path.display(), e));
                self.loading = None;
                self.err = Some("Could not open book".to_string());
                Action::RedrawFull
            }
            Err(TryRecvError::Empty) => Action::Keep,
            Err(TryRecvError::Disconnected) => {
                self.loading = None;
                self.err = Some("Could not open book".to_string());
                Action::RedrawFull
            }
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        if let Some(err) = &self.err {
            p.text_center(h / 2, 10.0, 0, err);
            return;
        }
        if self.doc.is_none() {
            p.text_center(h / 2, 10.0, 0, "Opening…");
            let name = p.truncate(8.0, &self.book_name(), p.width_pt() - 24.0);
            p.text_center(h / 2 + pt(16.0), 8.0, 130, &name);
            return;
        }
        let Some(doc) = &mut self.doc else {
            return;
        };

        if self.page_gray.is_none() {
            let t0 = Instant::now();
            let page = render_page(doc, self.page_no, w as u32, h as u32);
            plog(&format!(
                "render page {}: {}ms rss={}",
                self.page_no,
                t0.elapsed().as_millis(),
                rss_mib()
            ));
            match page {
                Some(gray) => self.page_gray = Some(gray),
                None => {
                    self.err = Some("Render failed".to_string());
                    p.text_center(h / 2, 10.0, 0, "Render failed");
                    return;
                }
            }
        }
        let gray = self.page_gray.as_ref().unwrap();
        p.blit_gray(0, 0, w, h, gray, w as usize);
        let footer = format!("{} / {}", self.page_no + 1, self.total);
        p.text_center(h - pt(10.0), 7.0, 110, &footer);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        if self.err.is_some() {
            return Action::Pop;
        }
        if self.doc.is_none() {
            // Still laying out: corner-back (edge gesture) still pops; the
            // rest waits.
            return Action::Keep;
        }
        let (w, h) = self.dims;
        match g {
            Gesture::Tap { x, y } => {
                if (x as i32) > w * 85 / 100 && (y as i32) < h * 12 / 100 {
                    // Screen-clean corner: re-present the cached page with
                    // a full flash (the old code re-rendered it — same
                    // pixels, just slower).
                    return Action::RedrawFull;
                }
                if (x as i32) < w / 3 {
                    self.turn(false)
                } else {
                    self.turn(true)
                }
            }
            Gesture::Swipe { dir: SwipeDir::East, .. } => self.turn(false),
            Gesture::Swipe { dir: SwipeDir::West, .. } => self.turn(true),
            Gesture::Swipe { .. } => Action::Keep,
            Gesture::TwoFingerTap => Action::Keep,
        }
    }
}

// /proc snapshots for the memory log lines (RAM audit, 2026-08-17).
// "?" rather than a wrong number if /proc misbehaves.
fn rss_mib() -> String {
    sysinfo::rss_kib().map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}

fn avail_mib() -> String {
    sysinfo::mem_available_kib()
        .map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}

fn render_page(doc: &Document, page_no: usize, w: u32, h: u32) -> Option<Vec<u8>> {
    let page = doc.load_page(page_no as i32).ok()?;
    let bounds = page.bounds().ok()?;
    let pw = bounds.x1 - bounds.x0;
    let ph = bounds.y1 - bounds.y0;
    if pw <= 0.0 || ph <= 0.0 {
        return None;
    }
    let target_w = (w - 2 * MARGIN) as f32;
    // No upscale cap: since layout() sizes the page in points, rendering
    // to pixels needs the full ~4.17x scale-up (mupdf is vectorial, so it
    // stays crisp). The old min(1.0) cap is what shrank book text.
    let zoom = target_w / pw;
    let m = Matrix::new_scale(zoom, zoom);
    let pm = page
        .to_pixmap(&m, &Colorspace::device_gray(), false, true)
        .ok()?;
    let pw = pm.width() as usize;
    let ph = pm.height() as usize;
    let stride = pm.stride() as usize;
    let samples = pm.samples();
    let mut out = vec![255u8; (w as usize) * (h as usize)];
    let ox = (w as usize - pw) / 2;
    let oy = MARGIN as usize;
    let copy_w = pw.min(w as usize - ox);
    let max_rows = (h as usize - oy - FOOTER_H as usize).min(ph);
    for row in 0..max_rows {
        let src = row * stride;
        let dst = (oy + row) * w as usize + ox;
        out[dst..dst + copy_w].copy_from_slice(&samples[src..src + copy_w]);
    }
    Some(out)
}
