//! Document lifecycle: async open/reflow and the warm-document cache.
//!
//! Opening and laying out a book happens on a background thread so the
//! panel keeps painting; the receiver hands the finished document back
//! to the screen's tick loop. The warm cache keeps one closed document
//! alive for instant re-entry from the library.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::Instant;

use mupdf::Document;
use ybdev::log::plog;
use ybdev::sysinfo;

pub struct BookReady {
    pub doc: SendDoc,
    pub total: usize,
}

/// mupdf-rs guards every call with the global BASE_CONTEXT mutex, so a
/// Document handle is safe to move across threads: all uses serialize.
pub struct SendDoc(pub Document);
unsafe impl Send for SendDoc {}

/// (path, doc, total, font, visual w, visual h) — the layout dims are part
/// of the key: a document laid out portrait must not be reused in a
/// landscape grip.
pub static WARM: Mutex<Option<(PathBuf, SendDoc, usize, f32, u32, u32)>> = Mutex::new(None);

pub fn reflow_async(
    mut send_doc: SendDoc,
    w: u32,
    h: u32,
    font_size: f32,
    margin_pad: u32,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let (avail_w, avail_h) = crate::render::avail_pt(w, h, margin_pad);
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

pub fn open_async(
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
            let (avail_w, avail_h) = crate::render::avail_pt(w, h, margin_pad);
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

pub fn rss_mib() -> String {
    sysinfo::rss_kib().map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}

pub fn avail_mib() -> String {
    sysinfo::mem_available_kib()
        .map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}
