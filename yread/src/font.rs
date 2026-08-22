//! Font management, fallback chain, and font metrics.

use std::sync::Arc;
use swash::FontRef;
use crate::model::FontStyle;

pub static LITERATA_BYTES: &[u8] = include_bytes!("../../resources/fonts/Literata.ttf");
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
    pub fallback: FontFace,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self {
            regular: FontFace::from_bytes(LITERATA_BYTES),
            fallback: FontFace::from_bytes(NOTO_SANS_BYTES),
        }
    }
}

impl FontSystem {
    pub fn face_for_style(&self, _style: FontStyle) -> &FontFace {
        // When bold/italic TTFs are loaded, route accordingly.
        &self.regular
    }

    pub fn fallback_face(&self) -> &FontFace {
        &self.fallback
    }

    /// Calculate baseline ascender, descender, and natural line height in pixels at a given point size (at 300 PPI).
    pub fn metrics(&self, style: FontStyle, size_pt: f32) -> FontMetrics {
        let face = self.face_for_style(style);
        if let Some(rb) = face.as_rustybuzz() {
            let upem = rb.units_per_em() as f32;
            let scale = (size_pt * (300.0 / 72.0)) / upem;
            let ascender = rb.ascender() as f32 * scale;
            let descender = rb.descender().abs() as f32 * scale;
            let line_gap = rb.line_gap() as f32 * scale;
            let line_height = (ascender + descender + line_gap).max(size_pt * (300.0 / 72.0) * 1.2);
            FontMetrics {
                ascender,
                descender,
                line_height,
                scale,
            }
        } else {
            let px = size_pt * (300.0 / 72.0);
            FontMetrics {
                ascender: px * 0.8,
                descender: px * 0.2,
                line_height: px * 1.2,
                scale: 1.0,
            }
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
