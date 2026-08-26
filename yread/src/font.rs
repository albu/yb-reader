//! Font management, fallback chain, and font metrics.

use crate::model::FontStyle;
use std::sync::OnceLock;
use swash::FontRef;

pub static LITERATA_REGULAR_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/Literata-Regular.ttf");
pub static LITERATA_BOLD_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata-Bold.ttf");
pub static LITERATA_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/Literata-Italic.ttf");
pub static LITERATA_BOLD_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/Literata-BoldItalic.ttf");
pub static NOTO_SANS_BYTES: &[u8] = include_bytes!("../../resources/fonts/NotoSans-Regular.ttf");

pub static PTSERIF_REGULAR_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/PTSerif-Regular.ttf");
pub static PTSERIF_BOLD_BYTES: &[u8] = include_bytes!("../../resources/fonts/PTSerif-Bold.ttf");
pub static PTSERIF_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/PTSerif-Italic.ttf");
pub static PTSERIF_BOLD_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/PTSerif-BoldItalic.ttf");
pub static BITTER_REGULAR_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/Bitter-Regular.ttf");
pub static BITTER_BOLD_BYTES: &[u8] = include_bytes!("../../resources/fonts/Bitter-Bold.ttf");
pub static BITTER_ITALIC_BYTES: &[u8] = include_bytes!("../../resources/fonts/Bitter-Italic.ttf");
pub static BITTER_BOLD_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/Bitter-BoldItalic.ttf");
pub static PTSANS_REGULAR_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/PTSans-Regular.ttf");
pub static PTSANS_BOLD_BYTES: &[u8] = include_bytes!("../../resources/fonts/PTSans-Bold.ttf");
pub static PTSANS_ITALIC_BYTES: &[u8] = include_bytes!("../../resources/fonts/PTSans-Italic.ttf");
pub static PTSANS_BOLD_ITALIC_BYTES: &[u8] =
    include_bytes!("../../resources/fonts/PTSans-BoldItalic.ttf");

/// The body typeface families shipped in the binary. All OFL-1.1, all with
/// Latin + Cyrillic coverage (the Russian Word Wise / book corpus demands
/// it). Literata is the signature default; the others are subsetted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamily {
    Literata = 0,
    PtSerif = 1,
    Bitter = 2,
    PtSans = 3,
}

impl FontFamily {
    pub const ALL: [FontFamily; 4] = [
        FontFamily::Literata,
        FontFamily::PtSerif,
        FontFamily::Bitter,
        FontFamily::PtSans,
    ];

    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn label(self) -> &'static str {
        match self {
            FontFamily::Literata => "Literata",
            FontFamily::PtSerif => "PT Serif",
            FontFamily::Bitter => "Bitter",
            FontFamily::PtSans => "PT Sans",
        }
    }

    /// The embedded bytes for one style of this family.
    pub fn face_bytes(self, style: FontStyle) -> &'static [u8] {
        use FontFamily::*;
        use FontStyle::*;
        match (self, style) {
            (Literata, Regular) => LITERATA_REGULAR_BYTES,
            (Literata, Bold) => LITERATA_BOLD_BYTES,
            (Literata, Italic) => LITERATA_ITALIC_BYTES,
            (Literata, BoldItalic) => LITERATA_BOLD_ITALIC_BYTES,
            (PtSerif, Regular) => PTSERIF_REGULAR_BYTES,
            (PtSerif, Bold) => PTSERIF_BOLD_BYTES,
            (PtSerif, Italic) => PTSERIF_ITALIC_BYTES,
            (PtSerif, BoldItalic) => PTSERIF_BOLD_ITALIC_BYTES,
            (Bitter, Regular) => BITTER_REGULAR_BYTES,
            (Bitter, Bold) => BITTER_BOLD_BYTES,
            (Bitter, Italic) => BITTER_ITALIC_BYTES,
            (Bitter, BoldItalic) => BITTER_BOLD_ITALIC_BYTES,
            (PtSans, Regular) => PTSANS_REGULAR_BYTES,
            (PtSans, Bold) => PTSANS_BOLD_BYTES,
            (PtSans, Italic) => PTSANS_ITALIC_BYTES,
            (PtSans, BoldItalic) => PTSANS_BOLD_ITALIC_BYTES,
        }
    }
}

#[derive(Clone)]
pub struct FontFace {
    pub data: &'static [u8],
    pub index: u32,
}

impl FontFace {
    pub fn from_bytes(bytes: &'static [u8]) -> Self {
        Self {
            data: bytes,
            index: 0,
        }
    }

    pub fn as_swash(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(self.data, self.index as usize)
    }

    pub fn as_rustybuzz(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(self.data, self.index)
    }
}

pub struct FontSystem {
    pub regular: FontFace,
    pub bold: FontFace,
    pub italic: FontFace,
    pub bold_italic: FontFace,
    pub code: FontFace,
    pub fallback: FontFace,
    /// The body family this system is built for. Code / fallback faces
    /// stay Noto Sans regardless of family.
    pub family: FontFamily,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self::for_family(FontFamily::Literata)
    }
}

impl FontSystem {
    pub fn for_family(family: FontFamily) -> Self {
        Self {
            regular: FontFace::from_bytes(family.face_bytes(FontStyle::Regular)),
            bold: FontFace::from_bytes(family.face_bytes(FontStyle::Bold)),
            italic: FontFace::from_bytes(family.face_bytes(FontStyle::Italic)),
            bold_italic: FontFace::from_bytes(family.face_bytes(FontStyle::BoldItalic)),
            code: FontFace::from_bytes(NOTO_SANS_BYTES),
            fallback: FontFace::from_bytes(NOTO_SANS_BYTES),
            family,
        }
    }
}

/// Font-file constants (units per em, ascender, descender, line gap) —
/// parsed once per process per (family, style). `metrics()` used to call
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

static RAW_METRICS: [[OnceLock<RawFaceMetrics>; 4]; 4] = [
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
    [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
];

fn raw_for(family: FontFamily, style: FontStyle) -> &'static RawFaceMetrics {
    RAW_METRICS[family.id() as usize][style as usize]
        .get_or_init(|| raw_metrics(family.face_bytes(style)))
}

static RB_FACES: [[OnceLock<rustybuzz::Face<'static>>; 4]; 4] =
    [
        [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
        [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
        [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
        [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()],
    ];
static RB_CODE: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();

impl FontSystem {
    pub fn face_for_style(&self, style: FontStyle) -> &FontFace {
        match style {
            FontStyle::Regular => &self.regular,
            FontStyle::Bold => &self.bold,
            FontStyle::Italic => &self.italic,
            FontStyle::BoldItalic => &self.bold_italic,
        }
    }

    pub fn code_face(&self) -> &FontFace {
        &self.code
    }

    /// A parsed rustybuzz face for shaping — built once from the embedded
    /// 'static bytes (every FontSystem is constructed from the same
    /// statics, so this is identical to re-parsing face_for_style().
    /// data on every shape call, just ~µs cheaper).
    pub fn rustybuzz_face(&self, style: FontStyle) -> &'static rustybuzz::Face<'static> {
        RB_FACES[self.family.id() as usize][style as usize]
            .get_or_init(|| rustybuzz::Face::from_slice(self.family.face_bytes(style), 0).expect("embedded font parses"))
    }

    pub fn rustybuzz_code_face(&self) -> &'static rustybuzz::Face<'static> {
        let build = |bytes: &'static [u8]| {
            rustybuzz::Face::from_slice(bytes, 0).expect("embedded font parses")
        };
        RB_CODE.get_or_init(|| build(NOTO_SANS_BYTES))
    }

    pub fn fallback_face(&self) -> &FontFace {
        &self.fallback
    }

    /// Calculate baseline ascender, descender, and natural line height in pixels at a given point size (at 300 PPI).
    pub fn metrics(&self, style: FontStyle, size_pt: f32) -> FontMetrics {
        // Same numbers the per-call Face parse produced; now O(1).
        let raw = raw_for(self.family, style);
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
