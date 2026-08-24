//! Document lifecycle for fixed-layout PDF / CBZ files via MuPDF.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use mupdf::Document;
use ybdev::log::plog;
use ybdev::sysinfo;

/// Wrapper around `mupdf::Document` to send across threads safely.
///
/// SAFETY: the mupdf crate (v0.7) deliberately keeps `Document` !Send —
/// it holds a raw `*mut fz_document`, and its thread model is a
/// per-thread `fz_context` cloned from a shared base (`fz_clone_context`),
/// with the document refcounted (`fz_keep`) so it outlives the creating
/// context. Soundness depends on a single invariant: a `SendDoc` is
/// opened on the worker thread, moved to the main thread, and thereafter
/// touched on the main thread only — never accessed from two threads
/// simultaneously (MuPDF builds without FZ_ENABLE_MUTEX, so concurrent
/// rendering would be a data race). Do NOT add Sync; keep every access
/// single-threaded.
pub struct SendDoc(Document);
unsafe impl Send for SendDoc {}

impl SendDoc {
    /// Unwrap the document on the destination thread.
    pub fn into_inner(self) -> Document {
        self.0
    }
}

/// Result returned from background PDF loading.
pub struct BookReady {
    pub doc: SendDoc,
    pub total: usize,
}

pub fn open_async(path: PathBuf) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        // A panic inside the mupdf FFI must still send a result — an
        // unwound worker drops tx unsend and the reader would sit on
        // "Opening…" forever.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<BookReady, String> {
                let doc = Document::open(path.as_os_str()).map_err(|e| e.to_string())?;
                let total = doc.page_count().unwrap_or(1).max(1) as usize;
                let open_ms = t0.elapsed().as_millis();
                plog(&format!(
                    "pdf open in {}ms ({} pages, rss={})",
                    open_ms,
                    total,
                    rss_mib()
                ));
                Ok(BookReady {
                    doc: SendDoc(doc),
                    total,
                })
            },
        ))
        .unwrap_or_else(|p| {
            Err(format!(
                "pdf open panicked: {}",
                crate::backend::panic_message(&p)
            ))
        });
        let _ = tx.send(r);
    });
    rx
}

pub fn rss_mib() -> String {
    sysinfo::rss_kib().map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}
