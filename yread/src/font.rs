//! Font management, fallback chain, and font metrics.

use std::sync::{Arc, OnceLock};
use swash::FontRef;
use crate::model::FontStyle;

pub static LITERATA_REGULAR_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata-Regular.ttf");
pub static LITERATA_BOLD_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata-Bold.ttf");
pub static LITERATA_ITALIC_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata-Italic.ttf");
pub static LITERATA_BOLD_ITALIC_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata-BoldItalic.ttf");
pub static NOTO_SANS_BYTES: &[u8] = include_bytes!("../../resources/fonts/NotoSans-Regular.ttf");

#[derive(Clone)]
pub struct FontFace {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

impl FontFace {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: Arc::new(bytes.to_vec()),
            index: 0,
        }
    }

    pub fn as_swash(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }

    pub fn as_rustybuzz(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(&self.data, self.index)
    }
}

pub struct FontSystem {
    pub regular: FontFace,
    pub bold: FontFace,
    pub italic: FontFace,
    pub bold_italic: FontFace,
    pub fallback: FontFace,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self {
            regular: FontFace::from_bytes(LITERATA_REGULAR_BYTES),
            bold: FontFace::from_bytes(LITERATA_BOLD_BYTES),
            italic: FontFace::from_bytes(LITERATA_ITALIC_BYTES),
            bold_italic: FontFace::from_bytes(LITERATA_BOLD_ITALIC_BYTES),
            fallback: FontFace::from_bytes(NOTO_SANS_BYTES),
        }
    }
}

/// Font-file constants (units per em, ascender, descender, line gap) —
/// parsed once per process per style. `metrics()` used to call
/// `Face::from_slice` (a full 320 kB sfnt parse) on EVERY call, and
/// build_line calls it per item per line: ~34k parses per chapter —
/// measured as ~75% of all pagination time.
struct RawFaceMetrics {
    upem: f32,
    ascender: f32,
    descender: f32,
    line_gap: f32,
}

fn raw_metrics(bytes: &'static [u8]) -> RawFaceMetrics {
    let face = rustybuzz::Face::from_slice(bytes, 0).expect("embedded font parses");
    RawFaceMetrics {
        upem: face.units_per_em() as f32,
        ascender: face.ascender() as f32,
        descender: face.descender().abs() as f32,
        line_gap: face.line_gap() as f32,
    }
}

static RAW_REGULAR: OnceLock<RawFaceMetrics> = OnceLock::new();
static RAW_BOLD: OnceLock<RawFaceMetrics> = OnceLock::new();
static RAW_ITALIC: OnceLock<RawFaceMetrics> = OnceLock::new();
static RAW_BOLD_ITALIC: OnceLock<RawFaceMetrics> = OnceLock::new();

fn raw_for(style: FontStyle) -> &'static RawFaceMetrics {
    match style {
        FontStyle::Regular => RAW_REGULAR.get_or_init(|| raw_metrics(LITERATA_REGULAR_BYTES)),
        FontStyle::Bold => RAW_BOLD.get_or_init(|| raw_metrics(LITERATA_BOLD_BYTES)),
        FontStyle::Italic => RAW_ITALIC.get_or_init(|| raw_metrics(LITERATA_ITALIC_BYTES)),
        FontStyle::BoldItalic => RAW_BOLD_ITALIC.get_or_init(|| raw_metrics(LITERATA_BOLD_ITALIC_BYTES)),
    }
}

static RB_REGULAR: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();
static RB_BOLD: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();
static RB_ITALIC: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();
static RB_BOLD_ITALIC: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();

impl FontSystem {
    pub fn face_for_style(&self, style: FontStyle) -> &FontFace {
        match style {
            FontStyle::Regular => &self.regular,
            FontStyle::Bold => &self.bold,
            FontStyle::Italic => &self.italic,
            FontStyle::BoldItalic => &self.bold_italic,
        }
    }

    /// A parsed rustybuzz face for shaping — built once from the embedded
    /// 'static bytes (every FontSystem is constructed from the same
    /// statics, so this is identical to re-parsing face_for_style().
    /// data on every shape call, just ~µs cheaper).
    pub fn rustybuzz_face(&self, style: FontStyle) -> &'static rustybuzz::Face<'static> {
        let build = |bytes: &'static [u8]| rustybuzz::Face::from_slice(bytes, 0).expect("embedded font parses");
        match style {
            FontStyle::Regular => RB_REGULAR.get_or_init(|| build(LITERATA_REGULAR_BYTES)),
            FontStyle::Bold => RB_BOLD.get_or_init(|| build(LITERATA_BOLD_BYTES)),
            FontStyle::Italic => RB_ITALIC.get_or_init(|| build(LITERATA_ITALIC_BYTES)),
            FontStyle::BoldItalic => RB_BOLD_ITALIC.get_or_init(|| build(LITERATA_BOLD_ITALIC_BYTES)),
        }
    }

    pub fn fallback_face(&self) -> &FontFace {
        &self.fallback
    }

    /// Calculate baseline ascender, descender, and natural line height in pixels at a given point size (at 300 PPI).
    pub fn metrics(&self, style: FontStyle, size_pt: f32) -> FontMetrics {
        // Same numbers the per-call Face parse produced; now O(1).
        let raw = raw_for(style);
        let scale = (size_pt * (300.0 / 72.0)) / raw.upem;
        let ascender = raw.ascender * scale;
        let descender = raw.descender * scale;
        let line_gap = raw.line_gap * scale;
        let line_height = (ascender + descender + line_gap).max(size_pt * (300.0 / 72.0) * 1.2);
        FontMetrics {
            ascender,
            descender,
            line_height,
            scale,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub ascender: f32,
    pub descender: f32,
    pub line_height: f32,
    pub scale: f32,
}
