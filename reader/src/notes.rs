//! Per-book reading highlights, persisted as plain lines on /mnt/us:
//! `page \t ts \t text`. The text is the span's words joined by single
//! spaces — the words were whitespace-split at capture, so splitting on
//! whitespace reproduces the exact word sequence for on-page matching.
//! No serde, no JSON: same philosophy as positions.txt.
//!
//! Text-keyed, not page-keyed: an EPUB re-layout changes page numbers but
//! preserves the word sequence, so [`matched_spans`] re-finds the span on
//! whatever page it lands on instead of stranding a page-numbered
//! highlight on the wrong page.

use crate::split::RectF;

pub struct Highlight {
    pub page: usize,
    pub ts: u64,
    pub text: String,
}

/// Overridable so host-side tests can aim the store at a temp dir.
fn notes_dir() -> String {
    std::env::var("YB_NOTES_DIR").unwrap_or_else(|_| "/mnt/us/notes".to_string())
}

fn path_for(book: &str) -> String {
    format!("{}/{}.hl", notes_dir(), book.replace('/', "_"))
}

pub fn load(book: &str) -> Vec<Highlight> {
    let Ok(text) = std::fs::read_to_string(path_for(book)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let page = it.next()?.parse().ok()?;
            let ts = it.next()?.parse().ok()?;
            let t = it.next()?.trim();
            if t.is_empty() {
                return None;
            }
            Some(Highlight {
                page,
                ts,
                text: t.to_string(),
            })
        })
        .collect()
}

pub fn save(book: &str, v: &[Highlight]) {
    let _ = std::fs::create_dir_all(notes_dir());
    let mut s = String::new();
    for h in v {
        s.push_str(&format!("{}\t{}\t{}\n", h.page, h.ts, h.text));
    }
    let p = path_for(book);
    let tmp = format!("{}.tmp", p);
    if std::fs::write(&tmp, &s).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// Add a highlight (deduped by exact text). Returns false for an empty or
/// already-known span.
pub fn add(book: &str, page: usize, text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let mut v = load(book);
    if v.iter().any(|h| h.text == text) {
        return false;
    }
    v.push(Highlight {
        page,
        ts: ybdev::log::now_ms() as u64 / 1000,
        text: text.to_string(),
    });
    v.sort_by_key(|h| h.ts);
    save(book, &v);
    true
}

/// Remove a highlight by its exact text (the same key `add` dedupes on).
/// Returns true if it existed.
pub fn remove(book: &str, text: &str) -> bool {
    let mut v = load(book);
    let before = v.len();
    v.retain(|h| h.text != text);
    if v.len() == before {
        return false;
    }
    save(book, &v);
    true
}

/// Match stored highlight texts against the given page's words (reading
/// order); returns inclusive (start, end) word-index ranges to underline.
/// Page-gated on purpose: matching every highlight's text against every
/// rendered page underlined common short spans wherever their words
/// appeared. The cost is that a reflow (font-size change) that moves the
/// span off its stored page loses the underline until it's re-captured.
pub fn matched_spans(
    hl: &[Highlight],
    page: usize,
    page_words: &[(String, RectF)],
) -> Vec<(usize, usize)> {
    let words: Vec<&str> = page_words.iter().map(|(w, _)| w.as_str()).collect();
    let mut out = Vec::new();
    for h in hl {
        if h.page != page {
            continue;
        }
        let needle: Vec<&str> = h.text.split_whitespace().collect();
        if needle.is_empty() || needle.len() > words.len() {
            continue;
        }
        for s in 0..=(words.len() - needle.len()) {
            if words[s..s + needle.len()] == needle[..] {
                out.push((s, s + needle.len() - 1));
                break;
            }
        }
    }
    out
}

/// Tests that repoint YB_NOTES_DIR must not run concurrently — env vars
/// are process-global, so parallel tests would clobber each other's store.
#[cfg(test)]
pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> RectF {
        RectF::new(0.0, 0.0, 1.0, 1.0)
    }

    #[test]
    fn roundtrip_and_dedupe() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("yb-notes-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_NOTES_DIR", dir.to_str().unwrap());

        assert!(add("book one.epub", 12, "the quick brown fox"));
        assert!(!add("book one.epub", 99, "the quick brown fox"), "dupe");
        assert!(add("book one.epub", 14, "jumps over"));
        let v = load("book one.epub");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].text, "the quick brown fox");
        assert_eq!(v[0].page, 12);
        assert_eq!(v[1].text, "jumps over");
        // Tabs in a highlight text are impossible (words are whitespace
        // split) — but a newline-bearing line must not corrupt the file.
        assert!(!add("book one.epub", 3, "   "));
        assert_eq!(load("book one.epub").len(), 2);
        assert!(load("other book.pdf").is_empty());

        assert!(remove("book one.epub", "jumps over"));
        assert!(!remove("book one.epub", "jumps over"), "already gone");
        assert!(!remove("book one.epub", "never was"));
        let v = load("book one.epub");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text, "the quick brown fox");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matched_spans_finds_exact_word_windows() {
        let hl = vec![
            Highlight { page: 0, ts: 1, text: "quick brown".into() },
            Highlight { page: 0, ts: 2, text: "fox".into() },
            Highlight { page: 7, ts: 3, text: "the quick".into() }, // other page: ignored
            Highlight { page: 0, ts: 4, text: "no such words here".into() },
            Highlight { page: 0, ts: 5, text: "lazy dog .".into() },
        ];
        let words: Vec<(String, RectF)> = ["the", "quick", "brown", "fox", "jumps"]
            .iter()
            .map(|w| (w.to_string(), r()))
            .collect();
        // "lazy dog ." only partially on-page: must NOT match a prefix.
        let spans = matched_spans(&hl, 0, &words);
        assert_eq!(spans, vec![(1, 2), (3, 3)]);
    }
}
