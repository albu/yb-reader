//! Document lifecycle for fixed-layout PDF / CBZ files via MuPDF.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use mupdf::Document;
use ybdev::log::plog;
use ybdev::sysinfo;

/// Wrapper around `mupdf::Document` to send across threads safely.
pub struct SendDoc(pub Document);
unsafe impl Send for SendDoc {}

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
