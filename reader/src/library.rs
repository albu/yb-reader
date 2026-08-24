//! The local library: which files in documents/ count as books, and what
//! they're really called — title/author pulled from the files themselves
//! via one mupdf open (PDF docinfo and EPUB OPF through the same door),
//! cached as plain TSV. Filenames stay the identity everywhere
//! (positions/highlights are filename-keyed).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use mupdf::{Document, MetadataName};

const LIB_DIR: &str = "/mnt/us/documents";
const META_PATH: &str = "/mnt/us/extensions/reader/data/meta.tsv";

/// Files in documents that carry a book-ish extension but belong to the
/// framework (clippings ledger) or the jailbreak — not library entries.
const SYSTEM_FILES: [&str; 2] = ["My Clippings.txt", "JAILBROKEN.txt"];

fn scan_dir_recursive(dir: &Path, v: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with('.')
                || name.ends_with(".sdr")
                || SYSTEM_FILES.contains(&name.as_str())
            {
                continue;
            }
            if p.is_dir() {
                scan_dir_recursive(&p, v);
            } else if p.is_file() {
                let ext = p
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if matches!(
                    ext.as_str(),
                    // mobi/azw3 stay out: no parser exists, so listing
                    // them only sets up an open error.
                    "epub" | "pdf" | "fb2" | "txt" | "cbz"
                ) {
                    v.push(p);
                }
            }
        }
    }
}

pub fn list_books() -> Vec<PathBuf> {
    let mut v = Vec::new();
    scan_dir_recursive(Path::new(LIB_DIR), &mut v);
    v.sort();
    v
}

/// Return the collection (subfolder path relative to documents/) for a book.
pub fn collection_of(p: &Path) -> Option<String> {
    let rel = p.strip_prefix(LIB_DIR).ok()?;
    let parent = rel.parent()?;
    let s = parent.to_string_lossy();
    if s.is_empty() {
        None
    } else {
        Some(s.into_owned())
    }
}

/// Extract title and author metadata from book files.
/// Uses yread's native pure-Rust parsers for EPUB/FB2 and MuPDF for PDF.
pub fn book_meta(path: &Path) -> Option<(String, String)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "epub" => {
            // Metadata-only: container.xml + OPF, never a spine document.
            // Same contract as the FB2 branch below: a malformed archive
            // can panic inside the parser, so the scan must catch it and
            // fall back to filename-only metadata instead of unwinding
            // out of HomeScreen.
            let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                yread::epub::EpubParser::open_metadata(path)
            }))
            .unwrap_or_else(|p| {
                ybdev::log::plog(&format!(
                    "epub meta parse panicked: {}",
                    crate::backend::panic_message(&p)
                ));
                Err(String::new())
            });
            if let Ok(meta) = parsed {
                let title = sanitize(&meta.title);
                if usable_title(&title) {
                    let author = sanitize(&meta.authors.join(", "));
                    return Some((title, author));
                }
            }
        }
        "fb2" => {
            // Same contract as the reader's open worker (backend_yread.rs):
            // a malformed FB2 can panic inside the parser, so the scan must
            // catch it and fall back to filename-only metadata instead of
            // unwinding out of HomeScreen.
            let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                yread::fb2::parse_fb2_path(path)
            }))
            .unwrap_or_else(|p| {
                ybdev::log::plog(&format!(
                    "fb2 meta parse panicked: {}",
                    crate::backend::panic_message(&p)
                ));
                Err(String::new())
            });
            if let Ok(book) = parsed {
                let title = sanitize(&book.meta.title);
                if usable_title(&title) {
                    let author = sanitize(&book.meta.authors.join(", "));
                    return Some((title, author));
                }
            }
        }
        // PDF/CBZ and anything else mupdf can open share one path.
        _ => {
            if let Ok(doc) = Document::open(path) {
                let title = sanitize(&doc.metadata(MetadataName::Title).unwrap_or_default());
                if usable_title(&title) {
                    let author = sanitize(&doc.metadata(MetadataName::Author).unwrap_or_default());
                    return Some((title, author));
                }
            }
        }
    }
    None
}

/// Empty or loader-placeholder titles (mupdf answers "Unknown" for azw3
/// files with no metadata) must not beat the filename as a display.
fn usable_title(t: &str) -> bool {
    let lower = t.to_lowercase();
    !t.is_empty() && lower != "unknown" && !lower.starts_with("untitled")
}

/// Tabs/newlines would break the TSV; spaces are the honest replacement.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => ' ',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// mtime + size — the staleness key for a cache line.
fn stat(p: &Path) -> (u64, u64) {
    match std::fs::metadata(p) {
        Ok(m) => (
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            m.len(),
        ),
        Err(_) => (0, 0),
    }
}

type BookMeta = Option<(String, String)>;
type CacheEntry = (String, u64, u64, BookMeta);
type Cache = HashMap<String, (u64, u64, BookMeta)>;

fn parse_line(line: &str) -> Option<CacheEntry> {
    let mut it = line.splitn(5, '\t');
    let name = it.next()?.to_string();
    let mtime: u64 = it.next()?.trim().parse().ok()?;
    let size: u64 = it.next()?.trim().parse().ok()?;
    let title = it.next().unwrap_or("").to_string();
    let author = it.next().unwrap_or("").to_string();
    if name.is_empty() {
        return None;
    }
    let meta = if title.is_empty() {
        None
    } else {
        Some((title, author))
    };
    Some((name, mtime, size, meta))
}

fn fmt_line(name: &str, mtime: u64, size: u64, meta: &BookMeta) -> String {
    let (t, a) = meta.clone().unwrap_or_default();
    format!("{}\t{}\t{}\t{}\t{}", name, mtime, size, t, a)
}

fn load_cache() -> Cache {
    let mut m = HashMap::new();
    if let Ok(s) = std::fs::read_to_string(META_PATH) {
        for line in s.lines() {
            if let Some((name, mt, sz, meta)) = parse_line(line) {
                m.insert(name, (mt, sz, meta));
            }
        }
    }
    m
}

fn save_cache(c: &Cache) {
    if let Some(dir) = Path::new(META_PATH).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut lines: Vec<String> = c
        .iter()
        .map(|(n, (mt, sz, meta))| fmt_line(n, *mt, *sz, meta))
        .collect();
    lines.sort();
    // Atomic + fsync'd swap (ybdev::atomic) — a torn cache would forget
    // every book's extracted title on the next scan.
    let _ = ybdev::atomic::write(META_PATH, (lines.join("\n") + "\n").as_bytes());
}

/// Cache lookup with lazy extraction: same mtime+size = hit (the
/// extractor must not run), anything else = re-extract and mark dirty.
/// Negative results cache too — a file with no metadata shouldn't be
/// re-opened on every scan. No fs, no mupdf: testable with a fake.
fn cache_lookup(
    cache: &mut Cache,
    name: &str,
    mtime: u64,
    size: u64,
    extract: impl FnOnce() -> Option<(String, String)>,
) -> (Option<(String, String)>, bool) {
    if let Some((mt, sz, meta)) = cache.get(name) {
        if *mt == mtime && *sz == size {
            return (meta.clone(), false);
        }
    }
    let meta = extract();
    cache.insert(name.to_string(), (mtime, size, meta.clone()));
    (meta, true)
}

/// Metadata for a whole scan, cache-first: unknown or stale files pay one
/// mupdf open each (first-ever scan only), deleted books get pruned.
pub fn meta_for(books: &[PathBuf]) -> Vec<Option<(String, String)>> {
    let mut cache = load_cache();
    let mut dirty = false;
    let mut out = Vec::with_capacity(books.len());
    for p in books {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (mt, sz) = stat(p);
        let (meta, d) = cache_lookup(&mut cache, &name, mt, sz, || book_meta(p));
        dirty |= d;
        out.push(meta);
    }
    let live: HashSet<String> = books
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    let before = cache.len();
    cache.retain(|n, _| live.contains(n));
    if cache.len() != before {
        dirty = true;
    }
    if dirty {
        save_cache(&cache);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_round_trips_and_skips_corrupt() {
        let line = fmt_line("a.epub", 12, 3400, &Some(("T".into(), "Au".into())));
        assert_eq!(
            parse_line(&line).unwrap(),
            ("a.epub".into(), 12, 3400, Some(("T".into(), "Au".into())))
        );
        // A negative result (file has no metadata) survives the round
        // trip — empty title field means "known absent", not "unknown".
        let l2 = fmt_line("b.epub", 1, 2, &None);
        assert_eq!(parse_line(&l2).unwrap(), ("b.epub".into(), 1, 2, None));
        assert!(parse_line("nope").is_none());
        assert!(parse_line("x\tNaN\t2\tt\ta").is_none());
        assert!(parse_line("\t1\t2\tt").is_none());
    }

    #[test]
    fn sanitize_strips_tsv_breakers() {
        assert_eq!(sanitize("a\tb\nc "), "a b c");
        assert_eq!(sanitize("  "), "");
    }

    #[test]
    fn placeholder_titles_reject_the_filename_fallback() {
        assert!(usable_title("A Sample Book"));
        assert!(!usable_title(""));
        assert!(!usable_title("Unknown"));
        assert!(!usable_title("untitled document"));
        assert!(usable_title("The Unknown Soldier"));
    }

    #[test]
    fn cache_lookup_hits_and_invalidates() {
        let mut c = HashMap::new();
        let (m, d) = cache_lookup(&mut c, "a", 1, 1, || Some(("T".into(), "A".into())));
        assert!(d && m.is_some());
        // Same stat = hit, extractor must not even run.
        let (_, d2) = cache_lookup(&mut c, "a", 1, 1, || panic!("must not extract"));
        assert!(!d2);
        // Stale size re-extracts; a failure caches as a negative.
        let (m3, d3) = cache_lookup(&mut c, "a", 1, 2, || None);
        assert!(d3 && m3.is_none());
        let (m4, d4) = cache_lookup(&mut c, "a", 1, 2, || panic!("must not extract"));
        assert!(!d4 && m4.is_none());
    }
}
