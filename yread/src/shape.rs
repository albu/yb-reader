//! Text shaping using rustybuzz with high-performance word-advance caching.

use rustybuzz::UnicodeBuffer;
use std::collections::HashMap;
use std::sync::Arc;

use crate::font::FontSystem;
use crate::model::FontStyle;

#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Debug, Clone)]
pub struct ShapedWord {
    pub advance: f32,
    pub glyphs: Vec<ShapedGlyph>,
}

fn hash_word(word: &str, style: FontStyle, size_scaled: u16) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in word.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^= style as u64;
    h = h.wrapping_mul(0x100000001b3);
    h ^= size_scaled as u64;
    h = h.wrapping_mul(0x100000001b3);
    h
}

pub struct ShapeCache {
    cache: HashMap<u64, (String, FontStyle, u16, Arc<ShapedWord>)>,
    hyphen_advance: HashMap<(FontStyle, u16), f32>,
    space_advance: HashMap<(FontStyle, u16), f32>,
}

impl Default for ShapeCache {
    fn default() -> Self {
        Self {
            cache: HashMap::with_capacity(8192),
            hyphen_advance: HashMap::new(),
            space_advance: HashMap::new(),
        }
    }
}

impl ShapeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shape a word (or get from cache). Advances are in pixels (300 PPI).
    pub fn shape_word(
        &mut self,
        word: &str,
        style: FontStyle,
        size_pt: f32,
        fonts: &FontSystem,
    ) -> Arc<ShapedWord> {
        let size_scaled = (size_pt * 10.0).round() as u16;
        let h = hash_word(word, style, size_scaled);

        if let Some((w, s, sz, shaped)) = self.cache.get(&h) {
            if *sz == size_scaled && *s == style && w == word {
                return Arc::clone(shaped);
            }
        }

        // Parsed once per process (see FontSystem::rustybuzz_face) — a
        // per-call Face::from_slice parsed the 320 kB sfnt ~2.4k times
        // per chapter on top of the metrics() hot path.
        let rb_face = fonts.rustybuzz_face(style);
        let shaped = Arc::new(shape_string_with_face(word, rb_face, size_pt));
        if self.cache.len() >= 65536 {
            self.cache.clear();
        }
        self.cache.insert(
            h,
            (word.to_string(), style, size_scaled, Arc::clone(&shaped)),
        );
        shaped
    }

    /// Shape a code string using the dedicated code font (Noto Sans).
    pub fn shape_code_word(
        &mut self,
        word: &str,
        size_pt: f32,
        fonts: &FontSystem,
    ) -> Arc<ShapedWord> {
        let size_scaled = (size_pt * 10.0).round() as u16;
        let h = hash_word(word, FontStyle::Regular, size_scaled ^ 0x79b9);

        if let Some((w, _s, sz, shaped)) = self.cache.get(&h) {
            if *sz == size_scaled && w == word {
                return Arc::clone(shaped);
            }
        }

        let rb_face = fonts.rustybuzz_code_face();
        let shaped = Arc::new(shape_string_with_face(word, rb_face, size_pt));
        if self.cache.len() >= 65536 {
            self.cache.clear();
        }
        self.cache.insert(
            h,
            (
                word.to_string(),
                FontStyle::Regular,
                size_scaled,
                Arc::clone(&shaped),
            ),
        );
        shaped
    }

    /// Fast lookup for single space advance width in pixels.
    pub fn space_advance(&mut self, style: FontStyle, size_pt: f32, fonts: &FontSystem) -> f32 {
        let size_scaled = (size_pt * 10.0).round() as u16;
        if let Some(&adv) = self.space_advance.get(&(style, size_scaled)) {
            return adv;
        }
        let shaped = self.shape_word(" ", style, size_pt, fonts);
        let adv = shaped.advance;
        self.space_advance.insert((style, size_scaled), adv);
        adv
    }

    /// Fast lookup for hyphen advance width in pixels.
    pub fn hyphen_advance(&mut self, style: FontStyle, size_pt: f32, fonts: &FontSystem) -> f32 {
        let size_scaled = (size_pt * 10.0).round() as u16;
        if let Some(&adv) = self.hyphen_advance.get(&(style, size_scaled)) {
            return adv;
        }
        let shaped = self.shape_word("-", style, size_pt, fonts);
        let adv = shaped.advance;
        self.hyphen_advance.insert((style, size_scaled), adv);
        adv
    }
}

fn shape_string_with_face(text: &str, rb_face: &rustybuzz::Face, size_pt: f32) -> ShapedWord {
    let upem = rb_face.units_per_em() as f32;
    let scale = (size_pt * (300.0 / 72.0)) / upem;

    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();

    let output = rustybuzz::shape(&rb_face, &[], buffer);

    let infos = output.glyph_infos();
    let positions = output.glyph_positions();

    let mut glyphs = Vec::with_capacity(infos.len());
    let mut total_advance = 0.0f32;

    for (info, pos) in infos.iter().zip(positions.iter()) {
        let x_adv = pos.x_advance as f32 * scale;
        let y_adv = pos.y_advance as f32 * scale;
        let x_off = pos.x_offset as f32 * scale;
        let y_off = pos.y_offset as f32 * scale;

        glyphs.push(ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            cluster: info.cluster,
            x_advance: x_adv,
            y_advance: y_adv,
            x_offset: x_off,
            y_offset: y_off,
        });

        total_advance += x_adv;
    }

    ShapedWord {
        advance: total_advance,
        glyphs,
    }
}
