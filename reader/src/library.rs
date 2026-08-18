//! The local library: which files in documents/ count as books.

use std::path::PathBuf;

const LIB_DIR: &str = "/mnt/us/documents";

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
