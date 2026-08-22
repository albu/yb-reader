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
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Default for FontStyle {
    fn default() -> Self {
        FontStyle::Regular
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

impl Default for TextAlign {
    fn default() -> Self {
        TextAlign::Justify
    }
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
        let mut file = archive.by_name(path).ok()?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
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
    /// Footnotes/notes keyed by target ID -> blocks
    pub footnotes: HashMap<String, Vec<Block>>,
    /// On-demand image source (file-backed epubs).
    pub lazy_images: std::sync::Arc<LazyImages>,
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
            .field("lazy_images", &self.lazy_images)
            .finish()
    }
}

impl Book {
    pub fn total_chars(&self) -> usize {
        self.chapters.iter().map(|c| c.char_count()).sum()
    }

    /// Register an image and record its true pixel dimensions.
    pub fn add_image(&mut self, id: String, bytes: Vec<u8>) {
        if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format() {
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

    /// Find which page contains the given character offset (binary search).
    pub fn page_for_char(&self, char_offset: usize) -> usize {
        if self.pages.is_empty() {
            return 0;
        }
        match self.pages.binary_search_by_key(&char_offset, |p| p.char_offset) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
    }

    /// Get the starting char offset for a given page index.
    pub fn char_for_page(&self, page_idx: usize) -> usize {
        self.pages
            .get(page_idx)
            .map(|p| p.char_offset)
            .unwrap_or(0)
    }
}
