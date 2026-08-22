//! Content model for reflowable books (FB2, EPUB, TXT).
//!
//! Immutable content tree:
//! Book -> Chapter -> Block -> Run
//!
//! All positions within a chapter are represented as character offsets
//! into the chapter's flat `text` String, making layout and pagination
//! a pure function of (content, style_params).

use std::collections::HashMap;

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
    },
    Heading {
        level: u8,
        runs: Vec<Run>,
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
    pub title: String,
    /// Flat normalized text representation of the chapter
    pub text: String,
    pub blocks: Vec<Block>,
}

impl Chapter {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            text: String::new(),
            blocks: Vec::new(),
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

#[derive(Debug, Clone, Default)]
pub struct Book {
    pub meta: BookMetadata,
    pub chapters: Vec<Chapter>,
    /// Embedded images keyed by ID/filename -> raw bytes (PNG/JPEG)
    pub images: HashMap<String, Vec<u8>>,
    /// Footnotes/notes keyed by target ID -> blocks
    pub footnotes: HashMap<String, Vec<Block>>,
}

impl Book {
    pub fn total_chars(&self) -> usize {
        self.chapters.iter().map(|c| c.char_count()).sum()
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
