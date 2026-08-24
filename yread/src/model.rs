//! Content model for reflowable books (FB2, EPUB, TXT).
//!
//! Immutable content tree:
//! Book -> Chapter -> Block -> Run
//!
//! All positions within a chapter are represented as character offsets
//! into the chapter's flat `text` String, making layout and pagination
//! a pure function of (content, style_params).

use std::collections::HashMap;
use std::io::{Read, Seek};
use zip::ZipArchive;

/// Boxed reader backing a file-backed archive.
pub trait ImageSource: Read + Seek + Send {}
impl<T: Read + Seek + Send> ImageSource for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(Default)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    #[default]
    Justify,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub font_style: FontStyle,
    /// Multiplier relative to base font size (1.0 = normal, 1.3 = h2, 1.6 = h1, 0.85 = small)
    pub size_mult: f32,
    pub align: TextAlign,
    /// First line indentation (e.g. true for standard body paragraphs)
    pub indent: bool,
    /// Superscript (e.g. exponents, footnote markers)
    pub is_sup: bool,
    /// Subscript (e.g. chemical indices)
    pub is_sub: bool,
    /// Inline code / monospace styling
    pub is_code: bool,
    /// Is this run a footnote reference link?
    pub footnote_ref: Option<String>,
    /// Foreground gray color (0 = black, 255 = white). Defaults to 0 (pure black).
    pub color: Option<u8>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_style: FontStyle::Regular,
            size_mult: 1.0,
            align: TextAlign::Justify,
            indent: false,
            is_sup: false,
            is_sub: false,
            is_code: false,
            footnote_ref: None,
            color: None,
        }
    }
}

/// A contiguous slice of text sharing the exact same styling.
/// Range indexes into `Chapter.text` (in UTF-8 byte range).
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph {
        runs: Vec<Run>,
        indent: bool,
        align: TextAlign,
        left_margin_em: f32,
        bullet_prefix: Option<String>,
        is_quote: bool,
    },
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    CodeBlock {
        code: String,
    },
    Image {
        id: String,
        caption: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    },
    Rule,
    Spacer(u32),
}

/// Push onto a parser nesting stack, refusing growth past `cap`.
/// Crafted documents can nest `<b>`/`<ul>` thousands of levels deep without
/// closing; the stack would otherwise hold one entry per tag (~16 MB per
/// MB of input). Deeper nesting is also semantically meaningless — styles
/// that deep are invisible.
pub fn push_capped<T>(stack: &mut Vec<T>, item: T, cap: usize) {
    if stack.len() < cap {
        stack.push(item);
    }
}

/// Depth limit shared by the EPUB and FB2 parsers' style/list stacks.
pub const MAX_NEST_DEPTH: usize = 64;

/// Hard ceiling for one decompressed archive entry (EPUB chapter, FB2
/// payload, embedded image). Legit entries sit far below this; a crafted
/// entry — a "zip bomb" — would otherwise OOM the device.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Read `src` to EOF but never past `cap + 1` bytes, so the caller can
/// detect an oversized stream by length alone. This is the actual
/// enforcement point for [`MAX_ENTRY_BYTES`]: the zip header's declared
/// size is a claim the reader does not check, and a lying entry (or a
/// streamed zip whose size lives in a data descriptor) must not be able
/// to decompress unbounded.
pub fn read_capped<R: std::io::Read>(src: R, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut out = Vec::new();
    src.take(cap + 1).read_to_end(&mut out)?;
    Ok(out)
}

/// Length half of the [`read_capped`] contract: true when the stream ran
/// past the cap (the `cap + 1` byte made it into the buffer).
pub fn over_cap(len: usize, cap: u64) -> bool {
    len as u64 > cap
}

#[derive(Debug, Clone, Default)]
pub struct Chapter {
    pub id: String,
    pub href: String,
    pub title: String,
    /// Flat normalized text representation of the chapter
    pub text: String,
    pub blocks: Vec<Block>,
    /// Element ID -> char offset in `text`
    pub anchors: HashMap<String, usize>,
}

impl Chapter {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            href: String::new(),
            title: title.into(),
            text: String::new(),
            blocks: Vec::new(),
            anchors: HashMap::new(),
        }
    }

    /// Character count in the chapter's flat text
    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Convert byte offset in `text` to char index
    pub fn byte_to_char(&self, byte_offset: usize) -> usize {
        let clamped = byte_offset.min(self.text.len());
        self.text[..clamped].chars().count()
    }

    /// Convert char index to byte offset in `text`
    pub fn char_to_byte(&self, char_offset: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_offset)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }
}

#[derive(Debug, Clone, Default)]
pub struct BookMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub language: String,
    pub cover_image_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TocEntry {
    pub title: String,
    pub chapter_idx: usize,
    pub byte_offset: usize,
    pub char_offset: usize,
    pub level: usize,
}

/// Lazily-loaded image source for file-backed books: the zip archive stays
/// open; image bytes are pulled (and memoized, small) only when a page that
/// shows them is rendered. Keeps 300MB illustrated epubs out of RAM.
pub struct LazyImages {
    archive: std::sync::Mutex<Option<ZipArchive<Box<dyn ImageSource>>>>,
    /// image id -> path inside the archive
    entries: HashMap<String, String>,
    loaded: std::sync::Mutex<HashMap<String, std::sync::Arc<Vec<u8>>>>,
}

impl Default for LazyImages {
    fn default() -> Self {
        Self {
            archive: std::sync::Mutex::new(None),
            entries: HashMap::new(),
            loaded: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl std::fmt::Debug for LazyImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyImages")
            .field("entries", &self.entries.len())
            .field("loaded", &self.loaded.lock().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
}

impl LazyImages {
    pub fn from_archive(
        archive: ZipArchive<Box<dyn ImageSource>>,
        entries: HashMap<String, String>,
    ) -> Self {
        Self {
            archive: std::sync::Mutex::new(Some(archive)),
            entries,
            loaded: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn load(&self, id: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        if let Some(hit) = self.loaded.lock().ok()?.get(id) {
            return Some(std::sync::Arc::clone(hit));
        }
        let path = self.entries.get(id)?;
        let mut guard = self.archive.lock().ok()?;
        let archive = guard.as_mut()?;
        let file = archive.by_name(path).ok()?;
        // Bounded on the actual decompressed stream (see read_capped):
        // a crafted image entry must fail here, not OOM at render time.
        let bytes = read_capped(file, MAX_ENTRY_BYTES).ok()?;
        if over_cap(bytes.len(), MAX_ENTRY_BYTES) {
            return None;
        }
        let arc = std::sync::Arc::new(bytes);
        if let Ok(mut loaded) = self.loaded.lock() {
            if loaded.len() >= 24 {
                loaded.clear();
            }
            loaded.insert(id.to_string(), std::sync::Arc::clone(&arc));
        }
        Some(arc)
    }
}

#[derive(Clone, Default)]
pub struct Book {
    pub meta: BookMetadata,
    pub chapters: Vec<Chapter>,
    /// Table of Contents (hierarchical entries with section titles & levels)
    pub toc: Vec<TocEntry>,
    /// Embedded images keyed by ID/filename -> raw bytes (PNG/JPEG).
    /// Eager store (fb2, in-memory parses); file-backed epubs use
    /// `lazy_images` instead.
    pub images: HashMap<String, Vec<u8>>,
    /// Precalculated pixel dimensions (width, height) for each image ID
    pub image_sizes: HashMap<String, (u32, u32)>,
    /// Footnotes/notes keyed by target ID -> note text
    pub footnotes: HashMap<String, String>,
    /// fb2 `<binary>` ids dropped for exceeding the per-image ceiling
    /// (fb2::MAX_BINARY_BYTES). Surfaced so the caller's log can say why
    /// embedded art went missing instead of silently not rendering.
    pub capped_binaries: Vec<String>,
    /// On-demand image source (file-backed epubs).
    pub lazy_images: std::sync::Arc<LazyImages>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootnoteResolution {
    pub title: String,
    pub text: String,
    pub target: Option<(usize, usize)>, // (chapter_idx, char_offset)
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book")
            .field("meta", &self.meta)
            .field("chapters", &self.chapters.len())
            .field("toc", &self.toc.len())
            .field("images", &self.images.len())
            .field("image_sizes", &self.image_sizes.len())
            .field("footnotes", &self.footnotes.len())
            .field("capped_binaries", &self.capped_binaries.len())
            .field("lazy_images", &self.lazy_images)
            .finish()
    }
}

impl Book {
    pub fn total_chars(&self) -> usize {
        self.chapters.iter().map(|c| c.char_count()).sum()
    }

    /// Resolve a footnote or link target URI to a snippet text and destination.
    pub fn resolve_footnote_or_link(&self, cur_chap_idx: usize, uri: &str) -> FootnoteResolution {
        let uri_clean = uri.trim();
        let stripped_anchor = uri_clean.trim_start_matches('#');

        // 1. Direct lookup in book.footnotes (FB2 notes body / embedded notes)
        if let Some(text) = self
            .footnotes
            .get(stripped_anchor)
            .or_else(|| self.footnotes.get(uri_clean))
        {
            let title = if stripped_anchor.is_empty() {
                "Footnote".to_string()
            } else {
                format!("Footnote [{}]", stripped_anchor)
            };
            return FootnoteResolution {
                title,
                text: text.clone(),
                target: None,
            };
        }

        // 2. Parse URI into (file_part, anchor_part)
        let (file_part, anchor_part) = if let Some(pos) = uri_clean.find('#') {
            (&uri_clean[..pos], Some(&uri_clean[pos + 1..]))
        } else {
            (uri_clean, None)
        };

        if file_part.is_empty() {
            if let Some(anchor) = anchor_part {
                // Check current chapter anchors first
                if let Some(ch) = self.chapters.get(cur_chap_idx) {
                    if let Some(&char_off) = ch.anchors.get(anchor) {
                        let text = extract_snippet_at(&ch.text, char_off);
                        return FootnoteResolution {
                            title: format!("Note [{}]", anchor),
                            text,
                            target: Some((cur_chap_idx, char_off)),
                        };
                    }
                }
                // Check all other chapters
                for (idx, ch) in self.chapters.iter().enumerate() {
                    if let Some(&char_off) = ch.anchors.get(anchor) {
                        let text = extract_snippet_at(&ch.text, char_off);
                        return FootnoteResolution {
                            title: format!("Note [{}]", anchor),
                            text,
                            target: Some((idx, char_off)),
                        };
                    }
                }
            }
        } else {
            let target_fname = std::path::Path::new(file_part)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(file_part);

            let matched_idx = self.chapters.iter().position(|c| {
                let ch_fname = std::path::Path::new(&c.href)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(&c.href);
                ch_fname.eq_ignore_ascii_case(target_fname)
                    || c.href.eq_ignore_ascii_case(file_part)
            });

            if let Some(target_idx) = matched_idx {
                let ch = &self.chapters[target_idx];
                let char_off = if let Some(anchor) = anchor_part {
                    ch.anchors.get(anchor).copied().unwrap_or(0)
                } else {
                    0
                };
                let text = extract_snippet_at(&ch.text, char_off);
                let title = if let Some(anchor) = anchor_part {
                    format!("Note [{}]", anchor)
                } else if !ch.title.is_empty() {
                    ch.title.clone()
                } else {
                    format!("Chapter {}", target_idx + 1)
                };
                return FootnoteResolution {
                    title,
                    text,
                    target: Some((target_idx, char_off)),
                };
            }
        }

        // 3. Fallback: if stripped_anchor matches any chapter anchor
        if let Some(anchor) = anchor_part.or(if !stripped_anchor.is_empty() {
            Some(stripped_anchor)
        } else {
            None
        }) {
            for (idx, ch) in self.chapters.iter().enumerate() {
                if let Some(&char_off) = ch.anchors.get(anchor) {
                    let text = extract_snippet_at(&ch.text, char_off);
                    return FootnoteResolution {
                        title: format!("Note [{}]", anchor),
                        text,
                        target: Some((idx, char_off)),
                    };
                }
            }
        }

        FootnoteResolution {
            title: "Link Target".to_string(),
            text: format!("Link: {}", uri_clean),
            target: None,
        }
    }

    /// Register an image and record its true pixel dimensions.
    pub fn add_image(&mut self, id: String, bytes: Vec<u8>) {
        if let Ok(reader) =
            image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()
        {
            if let Ok((w, h)) = reader.into_dimensions() {
                self.image_sizes.insert(id.clone(), (w, h));
            }
        }
        self.images.insert(id, bytes);
    }

    /// Image bytes by id, eager store first, then lazy archive load
    /// (with a bare file-name fallback for books whose references use
    /// short names). Results are memoized.
    pub fn get_image(&self, id: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        if let Some(bytes) = self.images.get(id) {
            return Some(std::sync::Arc::new(bytes.clone()));
        }
        if let Some(hit) = self.lazy_images.load(id) {
            return Some(hit);
        }
        let fname = std::path::Path::new(id)
            .file_name()
            .and_then(|f| f.to_str())?;
        if let Some(bytes) = self.images.get(fname) {
            return Some(std::sync::Arc::new(bytes.clone()));
        }
        self.lazy_images.load(fname)
    }
}

fn extract_snippet_at(text: &str, char_offset: usize) -> String {
    let byte_start = text
        .char_indices()
        .nth(char_offset)
        .map(|(b, _)| b)
        .unwrap_or(0);
    let slice = &text[byte_start..];
    let max_chars = 1500;
    let snippet: String = slice.chars().take(max_chars).collect();
    let trimmed = snippet.trim();
    if trimmed.is_empty() {
        "Note content".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Exact location of a page break within a chapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBreak {
    pub block_idx: usize,
    /// Byte offset in Chapter.text where this page begins
    pub byte_offset: usize,
    /// Char offset in Chapter.text where this page begins
    pub char_offset: usize,
}

/// Page table for a single chapter at a specific layout configuration.
#[derive(Debug, Clone, Default)]
pub struct ChapterPageTable {
    /// Starts of each page. Page 0 always starts at (0, 0, 0).
    pub pages: Vec<PageBreak>,
}

impl ChapterPageTable {
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    /// Find which page contains the given character offset.
    ///
    /// Uses partition_point rather than binary_search: consecutive
    /// image-only pages resolve their sentinel start to the SAME char
    /// offset (no text advanced), so the table can hold duplicates and
    /// binary_search would pick an arbitrary match among them. Stepping
    /// back from the first strictly-greater entry deterministically
    /// returns the LAST page starting at or before the offset — i.e. the
    /// page reading actually reached.
    pub fn page_for_char(&self, char_offset: usize) -> usize {
        if self.pages.is_empty() {
            return 0;
        }
        match self.pages.partition_point(|p| p.char_offset <= char_offset) {
            0 => 0,
            n => n - 1,
        }
    }

    /// Get the starting char offset for a given page index.
    pub fn char_for_page(&self, page_idx: usize) -> usize {
        self.pages.get(page_idx).map(|p| p.char_offset).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap is enforced on what the stream actually yields, never on
    /// what any header claims about it: a source bigger than the cap
    /// stops at cap+1 (so `over_cap` flags it), one smaller reads whole.
    #[test]
    fn read_capped_bounds_the_stream_not_the_claim() {
        let big = vec![0u8; 100];
        let out = read_capped(std::io::Cursor::new(&big), 50).unwrap();
        assert_eq!(out.len(), 51);
        assert!(over_cap(out.len(), 50));

        let small = vec![0u8; 40];
        let out = read_capped(std::io::Cursor::new(&small), 50).unwrap();
        assert_eq!(out.len(), 40);
        assert!(!over_cap(out.len(), 50));

        // Boundary: exactly the cap is fine, one byte more is not.
        let exact = vec![0u8; 50];
        let out = read_capped(std::io::Cursor::new(&exact), 50).unwrap();
        assert_eq!(out.len(), 50);
        assert!(!over_cap(out.len(), 50));
    }

    /// Two oversized images back-to-back paginate into pages whose
    /// sentinel-resolved start_char is identical (no text advanced).
    /// page_for_char must resolve that run deterministically to its LAST
    /// member — where reading continued — not an arbitrary match.
    #[test]
    fn page_for_char_handles_duplicate_start_offsets() {
        let table = ChapterPageTable {
            pages: vec![
                PageBreak {
                    block_idx: 0,
                    byte_offset: 0,
                    char_offset: 0,
                },
                PageBreak {
                    block_idx: 0,
                    byte_offset: 10,
                    char_offset: 10,
                },
                // Image pages: starts all sentinel-resolve to 10.
                PageBreak {
                    block_idx: 1,
                    byte_offset: 10,
                    char_offset: 10,
                },
                PageBreak {
                    block_idx: 1,
                    byte_offset: 10,
                    char_offset: 10,
                },
                // Text resumes on the page after the image run.
                PageBreak {
                    block_idx: 2,
                    byte_offset: 20,
                    char_offset: 20,
                },
            ],
        };

        assert_eq!(table.page_for_char(0), 0);
        // Any offset inside [10, 20) lands on the final image page —
        // deterministic, and the natural resume point after the run.
        assert_eq!(table.page_for_char(10), 3);
        assert_eq!(table.page_for_char(15), 3);
        assert_eq!(table.page_for_char(19), 3);
        assert_eq!(table.page_for_char(20), 4);
        // Below the first start clamps to page 0.
        assert_eq!(table.page_for_char(999), 4);
    }
}
