//! Document lifecycle: async open/reflow and the warm-document cache.
//!
//! Opening and laying out a book happens on a background thread so the
//! panel keeps painting; the receiver hands the finished document back
//! to the screen's tick loop. The warm cache keeps one closed document
//! alive for instant re-entry from the library.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::Instant;

use mupdf::Document;
use ybdev::log::plog;
use ybdev::sysinfo;

/// The bundled reading font (OFL). mupdf's user CSS can't @font-face
/// (verified 2026-08-21), but the DOCUMENT css can when the font lives
/// inside the book's archive — so epubs are opened through a patched
/// copy: unzipped to the cache, font dropped in, one CSS rule appended.
/// mupdf opens the directory exactly like the zip (verified: identical
/// pixel output), which is why no rezip is ever needed.
const EPUB_FONT: &[u8] = include_bytes!("../../resources/fonts/Literata-Regular.ttf");
const EPUB_FONT_FAMILY: &str = "Literata";

/// The patched copy of `book` — same book, one injected font. Falls
/// back to the original path on any failure (missing unzip, no css):
/// the reader stays readable, just in mupdf's light serif.
fn patched_book(book: &Path) -> PathBuf {
    let fallback = book.to_path_buf();
    if book.extension().and_then(|e| e.to_str()) != Some("epub") {
        return fallback;
    }
    let root = std::env::var("YB_FONT_CACHE")
        .unwrap_or_else(|_| "/mnt/us/extensions/reader/cache/fnt".to_string());
    let meta = match std::fs::metadata(book) {
        Ok(m) => m,
        Err(_) => return fallback,
    };
    // Identity: name + size + mtime — a changed book re-patches.
    let key = format!(
        "{}-{}-{}",
        book.file_stem().and_then(|s| s.to_str()).unwrap_or("book"),
        meta.len(),
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let dir = Path::new(&root).join(&key);
    let marker = dir.join("readerfont.ttf");
    if marker.exists() {
        // Touch marker to keep fresh in LRU ordering
        let _ = std::fs::write(&marker, EPUB_FONT);
        return dir;
    }
    prune_font_cache(&root);
    let t0 = Instant::now();
    if std::fs::remove_dir_all(&dir).is_err() {
        let _ = std::fs::create_dir_all(&dir);
    }
    if std::fs::create_dir_all(&dir).is_err() {
        plog("font-patch: cannot create cache dir — using original book");
        return fallback;
    }
    let ok = Command::new("unzip")
        .arg("-qo")
        .arg(book)
        .arg("-d")
        .arg(&dir)
        .output();
    match ok {
        Ok(o) if o.status.success() => {}
        _ => {
            plog("font-patch: unzip failed — using original book");
            return fallback;
        }
    }
    let font_path = dir.join("readerfont.ttf");
    if std::fs::write(&font_path, EPUB_FONT).is_err() {
        plog("font-patch: cannot write font — using original book");
        return fallback;
    }
    let mut css_files = Vec::new();
    collect_css(&dir, &mut css_files);
    if css_files.is_empty() {
        plog("font-patch: no css in book — using original book");
        return fallback;
    }
    let mut patched = 0usize;
    for css in &css_files {
        // The font sits at the archive root; each stylesheet reaches it
        // by ../ per directory level it is nested at.
        let depth = css
            .strip_prefix(&dir)
            .map(|r| r.components().count().saturating_sub(1))
            .unwrap_or(0);
        let url = format!("{}readerfont.ttf", "../".repeat(depth));
        let rule = format!(
            "\n@font-face {{ font-family: \"{EPUB_FONT_FAMILY}\"; src: url(\"{url}\"); }}\n* {{ font-family: \"{EPUB_FONT_FAMILY}\" !important; }}\n"
        );
        if std::fs::OpenOptions::new().append(true).open(css)
            .and_then(|mut f| std::io::Write::write_all(&mut f, rule.as_bytes()))
            .is_ok()
        {
            patched += 1;
        }
    }
    plog(&format!(
        "font-patch: {key} in {}ms ({patched}/{} css) rss={}",
        t0.elapsed().as_millis(),
        css_files.len(),
        rss_mib()
    ));
    dir
}

fn collect_css(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_css(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("css") {
            out.push(p);
        }
    }
}

pub const MAX_CACHED_BOOKS: usize = 8;

pub fn prune_font_cache(root: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            dirs.push((path, mtime));
        }
    }
    if dirs.len() > MAX_CACHED_BOOKS {
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        for (path, _) in &dirs[MAX_CACHED_BOOKS..] {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub struct BookReady {
    pub doc: SendDoc,
    pub total: usize,
    /// Page to land on after a reflow (anchor-resolved); None = no
    /// anchor was captured, keep the caller's page.
    pub landing: Option<usize>,
}

/// Reading-position anchor for reflow survival. A page NUMBER is
/// Reading-position anchor for reflow survival.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub text: String,
    pub fraction: f64,
    pub old_page: usize,
    pub old_total: usize,
    pub chapter: usize,
    pub page_in_chapter: usize,
    pub old_chapter_pages: usize,
    pub bookmark: Option<FzBookmark>,
}

impl Anchor {
    pub fn capture(doc: &Document, page_no: usize, total: usize, text: String) -> Self {
        let ctx_ptr = raw_ctx();
        let doc_ptr = raw_doc(doc);
        let mut chapter = 0;
        let mut page_in_chapter = 0;
        let mut old_chapter_pages = 1;
        let mut bookmark = None;

        if !ctx_ptr.is_null() && !doc_ptr.is_null() {
            unsafe {
                let loc = fz_location_from_page_number(ctx_ptr, doc_ptr, page_no as libc::c_int);
                if loc.chapter >= 0 && loc.page >= 0 {
                    chapter = loc.chapter as usize;
                    page_in_chapter = loc.page as usize;
                    let ch_pages = fz_count_chapter_pages(ctx_ptr, doc_ptr, loc.chapter);
                    old_chapter_pages = (ch_pages as usize).max(1);
                    let mark = fz_make_bookmark(ctx_ptr, doc_ptr, loc);
                    if mark != 0 {
                        bookmark = Some(mark);
                    }
                }
            }
        }

        Self {
            text,
            fraction: if total > 0 {
                page_no as f64 / total as f64
            } else {
                0.0
            },
            old_page: page_no,
            old_total: total,
            chapter,
            page_in_chapter,
            old_chapter_pages,
            bookmark,
        }
    }
}

/// Anchor normalization: lowercase, alphanumerics only.
pub fn anchor_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Match test: the anchor is the first words of the old viewport, so a
/// landing page must carry them near its own start (top fifth).
fn find_anchor_in(normalized_page: &str, key: &str) -> bool {
    normalized_page
        .find(key)
        .is_some_and(|i| i <= normalized_page.len() / 5)
}

/// Resolve the landing page after layout.
pub fn resolve_landing(doc: &Document, total: usize, anchor: &Anchor) -> Option<usize> {
    let ctx_ptr = raw_ctx();
    let doc_ptr = raw_doc(doc);

    // 1. Native MuPDF bookmark (instant, 100% exact when layout didn't purge DOM)
    if let Some(mark) = anchor.bookmark {
        if let Some(p) = lookup_bookmark(doc, mark) {
            if p < total {
                if !ctx_ptr.is_null() && !doc_ptr.is_null() {
                    unsafe {
                        let loc = fz_location_from_page_number(ctx_ptr, doc_ptr, p as libc::c_int);
                        if loc.chapter as usize == anchor.chapter {
                            return Some(p);
                        }
                    }
                } else {
                    return Some(p);
                }
            }
        }
    }

    // 2. Chapter-scoped resolution (fast and exact across DOM purges / line spacing changes)
    if !ctx_ptr.is_null() && !doc_ptr.is_null() {
        unsafe {
            let num_chapters = fz_count_chapters(ctx_ptr, doc_ptr);
            if num_chapters > 0 && anchor.chapter < num_chapters as usize {
                let new_ch_pages = fz_count_chapter_pages(ctx_ptr, doc_ptr, anchor.chapter as libc::c_int).max(1) as usize;
                let ch_start_loc = FzLocation { chapter: anchor.chapter as libc::c_int, page: 0 };
                let ch_start_page = fz_page_number_from_location(ctx_ptr, doc_ptr, ch_start_loc);

                if ch_start_page >= 0 {
                    let ch_start = ch_start_page as usize;
                    let full = anchor_key(&anchor.text);
                    let key = &full[..full.len().min(60)];

                    // Search inside THIS chapter ONLY (typically 1-15 pages, fast!)
                    if key.len() >= 12 {
                        for pass in 0..2 {
                            for p_off in 0..new_ch_pages {
                                let p = ch_start + p_off;
                                if p >= total { break; }
                                if let Ok(tp) = doc.load_page(p as i32).and_then(|page| page.to_text_page(mupdf::TextPageFlags::empty())) {
                                    if let Ok(txt) = tp.to_text() {
                                        let k = anchor_key(&txt);
                                        let hit = if pass == 0 {
                                            find_anchor_in(&k, key)
                                        } else {
                                            k.contains(key)
                                        };
                                        if hit {
                                            return Some(p);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Fallback within chapter: scale position by old chapter fraction
                    let frac = anchor.page_in_chapter as f64 / anchor.old_chapter_pages as f64;
                    let scaled_in_ch = (frac * new_ch_pages as f64).round() as usize;
                    let target_page = (ch_start + scaled_in_ch.min(new_ch_pages.saturating_sub(1))).min(total.saturating_sub(1));
                    return Some(target_page);
                }
            }
        }
    }

    // 3. Fallback: global fraction
    Some(((anchor.fraction * total as f64).round() as usize).min(total.saturating_sub(1)))
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FzLocation {
    pub chapter: libc::c_int,
    pub page: libc::c_int,
}

pub type FzBookmark = libc::intptr_t;

extern "C" {
    fn fz_location_from_page_number(ctx: *mut libc::c_void, doc: *mut libc::c_void, number: libc::c_int) -> FzLocation;
    fn fz_make_bookmark(ctx: *mut libc::c_void, doc: *mut libc::c_void, loc: FzLocation) -> FzBookmark;
    fn fz_lookup_bookmark(ctx: *mut libc::c_void, doc: *mut libc::c_void, mark: FzBookmark) -> FzLocation;
    fn fz_page_number_from_location(ctx: *mut libc::c_void, doc: *mut libc::c_void, loc: FzLocation) -> libc::c_int;
    fn fz_count_chapters(ctx: *mut libc::c_void, doc: *mut libc::c_void) -> libc::c_int;
    fn fz_count_chapter_pages(ctx: *mut libc::c_void, doc: *mut libc::c_void, chapter: libc::c_int) -> libc::c_int;
    fn fz_purge_stored_html(ctx: *mut libc::c_void, doc: *mut libc::c_void);
    fn mupdf_layout_document(ctx: *mut libc::c_void, doc: *mut libc::c_void, w: f32, h: f32, em: f32, err: *mut *mut libc::c_void);
}

pub fn raw_doc(doc: &Document) -> *mut libc::c_void {
    unsafe { *(doc as *const Document as *const *mut libc::c_void) }
}

pub fn raw_ctx() -> *mut libc::c_void {
    let ctx = mupdf::Context::get();
    unsafe { *(&ctx as *const mupdf::Context as *const *mut libc::c_void) }
}

pub fn bookmark_page(doc: &Document, page_no: usize) -> Option<FzBookmark> {
    let ctx_ptr = raw_ctx();
    let doc_ptr = raw_doc(doc);
    if ctx_ptr.is_null() || doc_ptr.is_null() {
        return None;
    }
    unsafe {
        let loc = fz_location_from_page_number(ctx_ptr, doc_ptr, page_no as libc::c_int);
        if loc.chapter < 0 || loc.page < 0 {
            return None;
        }
        let mark = fz_make_bookmark(ctx_ptr, doc_ptr, loc);
        if mark == 0 {
            None
        } else {
            Some(mark)
        }
    }
}

pub fn lookup_bookmark(doc: &Document, mark: FzBookmark) -> Option<usize> {
    let ctx_ptr = raw_ctx();
    let doc_ptr = raw_doc(doc);
    if ctx_ptr.is_null() || doc_ptr.is_null() || mark == 0 {
        return None;
    }
    unsafe {
        let loc = fz_lookup_bookmark(ctx_ptr, doc_ptr, mark);
        if loc.chapter < 0 || loc.page < 0 {
            return None;
        }
        let page = fz_page_number_from_location(ctx_ptr, doc_ptr, loc);
        if page < 0 {
            None
        } else {
            Some(page as usize)
        }
    }
}

pub struct SendDoc(pub Document);
unsafe impl Send for SendDoc {}

use crate::split::ReaderSettings;

/// (path, doc, total, settings, visual w, visual h) — the layout dims and settings are part
/// of the key: a document laid out portrait must not be reused in a
/// landscape grip.
pub static WARM: Mutex<Option<(PathBuf, SendDoc, usize, ReaderSettings, u32, u32)>> = Mutex::new(None);

/// Apply the pre-layout render knobs on the calling (layout) thread's
/// context.
pub fn apply_layout_css(line_spacing: f32) {
    let mut css = String::new();
    if crate::render::SANS_EPUB_BODY {
        css.push_str("* { font-family: sans-serif !important; } ");
    }
    if crate::render::BOLD_EPUB_BODY {
        css.push_str("* { font-weight: bold !important; } ");
    }
    if (line_spacing - 1.0).abs() > 0.01 {
        css.push_str(&format!(
            "html, body, p, div, span, a, em, strong, b, i, li, ul, ol, blockquote, section, article, h1, h2, h3, h4, h5, h6, * {{ line-height: {line_spacing:.2} !important; }} "
        ));
    }
    let _ = mupdf::Context::get().set_user_css(&css);
}

pub fn purge_stored_html(doc: &Document) {
    let ctx_ptr = raw_ctx();
    let doc_ptr = raw_doc(doc);
    if !ctx_ptr.is_null() && !doc_ptr.is_null() {
        unsafe {
            fz_purge_stored_html(ctx_ptr, doc_ptr);
        }
    }
}

/// Instant in-memory reflow of an open document.
pub fn reflow_in_place(
    doc: &Document,
    w: u32,
    h: u32,
    font_size: f32,
    margin_pad: u32,
    line_spacing: f32,
    current_page: usize,
    current_text: &str,
) -> (usize, usize) {
    let total_before = doc.page_count().unwrap_or(1).max(1) as usize;
    let anchor = Anchor::capture(doc, current_page, total_before, current_text.to_string());
    let (avail_w, avail_h) = crate::render::avail_pt(w, h, margin_pad);
    apply_layout_css(line_spacing);
    purge_stored_html(doc);
    let ctx_ptr = raw_ctx();
    let doc_ptr = raw_doc(doc);
    if !ctx_ptr.is_null() && !doc_ptr.is_null() {
        unsafe {
            let mut err = std::ptr::null_mut();
            mupdf_layout_document(
                ctx_ptr,
                doc_ptr,
                avail_w,
                avail_h,
                font_size,
                &mut err,
            );
        }
    }
    let total = doc.page_count().unwrap_or(1).max(1) as usize;
    let new_page = resolve_landing(doc, total, &anchor).unwrap_or(current_page).min(total.saturating_sub(1));
    (new_page, total)
}

pub fn reflow_async(
    mut send_doc: SendDoc,
    w: u32,
    h: u32,
    font_size: f32,
    margin_pad: u32,
    line_spacing: f32,
    anchor: Option<Anchor>,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let (avail_w, avail_h) = crate::render::avail_pt(w, h, margin_pad);
        apply_layout_css(line_spacing);
        purge_stored_html(&send_doc.0);
        let _ = send_doc.0.layout(avail_w, avail_h, font_size);
        let total = send_doc.0.page_count().unwrap_or(1).max(1) as usize;
        let landing = anchor.as_ref().and_then(|a| resolve_landing(&send_doc.0, total, a));
        plog(&format!(
            "book in-memory reflow in {}ms ({} pages, font={:.1}pt, sp={:.1}) rss={} avail={} landing={:?}",
            t0.elapsed().as_millis(),
            total,
            font_size,
            line_spacing,
            rss_mib(),
            avail_mib(),
            landing
        ));
        let _ = tx.send(Ok(BookReady {
            doc: send_doc,
            total,
            landing,
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
    line_spacing: f32,
    anchor: Option<Anchor>,
) -> Receiver<Result<BookReady, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let r = (|| -> Result<BookReady, String> {
            let book = patched_book(&path);
            let mut doc = Document::open(book.as_os_str()).map_err(|e| e.to_string())?;
            let open_ms = t0.elapsed().as_millis();
            let (avail_w, avail_h) = crate::render::avail_pt(w, h, margin_pad);
            apply_layout_css(line_spacing);
            purge_stored_html(&doc);
            let _ = doc.layout(avail_w, avail_h, font_size);
            let total = doc.page_count().unwrap_or(1).max(1) as usize;
            let landing = anchor.as_ref().and_then(|a| resolve_landing(&doc, total, a));
            plog(&format!(
                "book open {}ms + layout {}ms ({} pages, font={:.1}pt, sp={:.1}) rss={} avail={} landing={:?}",
                open_ms,
                t0.elapsed().as_millis() - open_ms,
                total,
                font_size,
                line_spacing,
                rss_mib(),
                avail_mib(),
                landing
            ));
            Ok(BookReady {
                doc: SendDoc(doc),
                total,
                landing,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_key_strips_everything_but_letters() {
        assert_eq!(anchor_key("well-known, dif- ferent!"), "wellknowndifferent");
        assert_eq!(anchor_key("Über Straße 42"), "überstraße42");
        assert_eq!(anchor_key(""), "");
    }

    #[test]
    fn chapter_anchor_roundtrip_on_real_book() {
        // The full mechanism on the lab book: build chapter anchors at
        // one font size, re-layout at another, land on the same text.
        let book = Path::new("/tmp/lab.epub");
        if !book.exists() {
            return;
        }
        let mut doc = Document::open(book.as_os_str()).expect("open");
        let _ = doc.layout(280.0, 340.0, 11.0);
        let total_a = doc.page_count().unwrap_or(0) as usize;

        // Anchor = top-of-page words of some mid-book page.
        let old_page = total_a * 2 / 3;
        let text = doc
            .load_page(old_page as i32)
            .unwrap()
            .to_text_page(mupdf::TextPageFlags::empty())
            .unwrap()
            .to_text()
            .unwrap();
        let anchor = Anchor::capture(&doc, old_page, total_a, text);
        let _ = doc.layout(280.0, 340.0, 9.0);
        let total_b = doc.page_count().unwrap_or(0) as usize;
        let landing = resolve_landing(&doc, total_b, &anchor).expect("landing resolved");
        let want = anchor_key(&anchor.text)[..60].to_string();
        let mut true_page = None;
        for p in 0..total_b {
            if let Ok(t) = (|| -> Result<String, mupdf::Error> {
                doc.load_page(p as i32)?
                    .to_text_page(mupdf::TextPageFlags::empty())?
                    .to_text()
            })() {
                if anchor_key(&t).find(&want).is_some() {
                    true_page = Some(p);
                    break;
                }
            }
        }
        let tp = true_page.expect("anchor text must exist somewhere");
        println!("Landing: resolved={landing}, true_page={tp}");
        assert_eq!(landing, tp, "Reflow landing should exactly match true page");
    }

    #[test]
    fn font_patch_produces_openable_book() {
        // Exercises the whole shipping path on the host: patch the lab
        // epub (YB_FONT_CACHE redirects the cache), open the patched
        // directory, lay out, rasterize — ink must move vs the stock
        // serif (Literata is ~3x Charis' coverage at the same size).
        let book = Path::new("/tmp/lab.epub");
        if !book.exists() {
            return;
        }
        std::env::set_var("YB_FONT_CACHE", "/tmp/fntlab");
        std::fs::remove_dir_all("/tmp/fntlab").ok();
        let dir = patched_book(book);
        assert!(dir.is_dir(), "patched dir exists");
        assert!(dir.join("readerfont.ttf").exists(), "font dropped in");
        let mut doc = Document::open(dir.as_os_str()).expect("open patched");
        let _ = doc.layout(280.0, 340.0, 11.0);
        let page = doc.load_page(50).expect("page");
        let pm = page
            .to_pixmap(&mupdf::Matrix::IDENTITY, &mupdf::Colorspace::device_gray(), false, true)
            .expect("pixmap");
        let dark = pm.samples().iter().filter(|&&v| v < 100).count();
        assert!(dark > 4000, "Literata ink present (dark={dark})");
    }

    #[test]
    fn anchor_matches_near_page_top_only() {
        let page = "thequickbrownfoxjumpsoverthelazydogandthengoesonandon";
        assert!(find_anchor_in(page, "thequickbrownfox"));
        // Same phrase appearing only deep in the page must not match —
        // a repeated running head later in the book must not steal it.
        let late = "fillerfillerfillerfillerfillerfiller".to_string() + page;
        assert!(!find_anchor_in(&late, page));
    }

    #[test]
    fn test_mupdf_native_bookmark() {
        let book = Path::new("/tmp/lab.epub");
        if !book.exists() {
            return;
        }
        let mut doc = Document::open(book.as_os_str()).expect("open");
        let _ = doc.layout(280.0, 340.0, 11.0);
        let total_a = doc.page_count().unwrap_or(0) as usize;
        let old_page = total_a * 2 / 3;

        let mark = bookmark_page(&doc, old_page).expect("make bookmark");
        assert!(mark != 0);

        let _ = doc.layout(280.0, 340.0, 9.0);
        let total_b = doc.page_count().unwrap_or(0) as usize;
        let new_page = lookup_bookmark(&doc, mark).expect("lookup bookmark");
        println!("Native bookmark: old_page={old_page}/{total_a} -> new_page={new_page}/{total_b}");
        assert!(new_page < total_b);
    }

    #[test]
    fn test_spacing_reflow() {
        let book = Path::new("/tmp/lab.epub");
        if !book.exists() {
            return;
        }
        let mut doc = Document::open(book.as_os_str()).expect("open");
        apply_layout_css(1.0);
        let _ = doc.layout(280.0, 340.0, 11.0);
        let total_1 = doc.page_count().unwrap_or(0);

        // Load a page so chapters are cached in fz_store
        let _ = doc.load_page(50);

        // Now change spacing to 1.6
        apply_layout_css(1.6);
        let ctx_ptr = raw_ctx();
        let doc_ptr = raw_doc(&doc);
        unsafe { fz_purge_stored_html(ctx_ptr, doc_ptr); }
        let _ = doc.layout(280.0, 340.0, 11.0);
        let total_2 = doc.page_count().unwrap_or(0);

        println!("Spacing test: total at 1.0={total_1}, total at 1.6={total_2}");
        assert!(total_2 > total_1, "Increasing line spacing must increase page count (1.0: {total_1}, 1.6: {total_2})");
    }

    #[test]
    fn test_reflow_in_place_sequential() {
        let book = Path::new("/tmp/lab.epub");
        if !book.exists() {
            return;
        }
        let mut doc = Document::open(book.as_os_str()).expect("open");
        let (aw, ah) = crate::render::avail_pt(1236, 1648, 72);
        apply_layout_css(1.0);
        let _ = doc.layout(aw, ah, 11.0);
        let total_init = doc.page_count().unwrap_or(0) as usize;
        println!("Initial layout total={total_init}");

        let (p1, tot1) = reflow_in_place(&doc, 1236, 1648, 11.0, 72, 1.0, 50, "");
        println!("Seq test 1 (font=11, sp=1.0): page={p1}, total={tot1}");

        // Change font size
        let (p2, tot2) = reflow_in_place(&doc, 1236, 1648, 14.0, 72, 1.0, p1, "");
        println!("Seq test 2 (font=14, sp=1.0): page={p2}, total={tot2}");

        // Change spacing
        let (p3, tot3) = reflow_in_place(&doc, 1236, 1648, 14.0, 72, 1.4, p2, "");
        println!("Seq test 3 (font=14, sp=1.4): page={p3}, total={tot3}");

        // Change back to original font & spacing
        let (p4, tot4) = reflow_in_place(&doc, 1236, 1648, 11.0, 72, 1.0, p3, "");
        println!("Seq test 4 (font=11, sp=1.0): page={p4}, total={tot4}");

        assert!(tot2 > tot1, "Font 14 should have more pages than font 11 (tot1={tot1}, tot2={tot2})");
        assert!(tot3 > tot2, "Spacing 1.4 should have more pages than spacing 1.0 (tot2={tot2}, tot3={tot3})");
        assert!((tot4 as i32 - tot1 as i32).abs() <= 25, "Returning to original layout must yield approximately same page count (tot1={tot1}, tot4={tot4})");
    }

    #[test]
    fn test_prune_font_cache() {
        let test_root = "/tmp/yb_fnt_prune_test";
        let _ = std::fs::remove_dir_all(test_root);
        let _ = std::fs::create_dir_all(test_root);

        for i in 0..(MAX_CACHED_BOOKS + 5) {
            let dir = Path::new(test_root).join(format!("book_{}", i));
            let _ = std::fs::create_dir_all(&dir);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }

        prune_font_cache(test_root);

        let remaining = std::fs::read_dir(test_root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .count();
        assert_eq!(remaining, MAX_CACHED_BOOKS);

        let _ = std::fs::remove_dir_all(test_root);
    }
}
