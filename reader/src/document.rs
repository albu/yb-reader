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
        let r = (|| -> Result<BookReady, String> {
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
        })();
        let _ = tx.send(r);
    });
    rx
}

pub fn rss_mib() -> String {
    sysinfo::rss_kib().map_or_else(|| "?".into(), |k| format!("{}m", k / 1024))
}
