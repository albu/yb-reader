//! Text rasterization with fontdue, using the OFL-licensed Noto Sans
//! embedded in the binary (copied from the on-device KOReader bundle).
//!
//! Sizes here are raw PIXELS (fontdue's unit); Painter converts points.

use fontdue::{Font as DFont, FontSettings};

const NOTO: &[u8] = include_bytes!("../../resources/fonts/NotoSans-Regular.ttf");

pub struct Font {
    font: DFont,
}

impl Font {
    pub fn load() -> Result<Font, String> {
        let font = DFont::from_bytes(NOTO, FontSettings::default())
            .map_err(|e| format!("font parse: {}", e))?;
        Ok(Font { font })
    }

    pub fn text_width(&self, size: f32, text: &str) -> f32 {
        let mut w = 0f32;
        for ch in text.chars() {
            w += self.font.metrics(ch, size).advance_width;
        }
        w
    }

    /// Truncate `text` to fit within `max_width` px (uses "…").
    pub fn truncate(&self, size: f32, text: &str, max_width: f32) -> String {
        if self.text_width(size, text) <= max_width {
            return text.to_string();
        }
        let mut out = String::new();
        for ch in text.chars() {
            let candidate = format!("{}{}", out, ch);
            if self.text_width(size, &candidate) > max_width - 12.0 {
                break;
            }
            out = candidate;
        }
        out.push('…');
        out
    }

    /// Draw text into a grayscale buffer with the given row stride.
    /// (x, y) is the baseline of the first line. `color` is a gray value.
    pub fn draw(
        &self,
        buf: &mut [u8],
        stride: usize,
        x: i32,
        y: i32,
        size: f32,
        color: u8,
        text: &str,
    ) {
        let mut pen_x = x as f32;
        for ch in text.chars() {
            let (metrics, cov) = self.font.rasterize(ch, size);
            let gx = pen_x.round() as i32 + metrics.xmin;
            // fontdue's ymin is the bitmap's BOTTOM relative to the
            // baseline (negative = descender below it: 'A'=0, 'a'=-1,
            // 'g'/'y'=-7 at 26px). Top row = baseline - ymin - height;
            // the old `y - height + ymin` flipped the sign and floated
            // descenders ~14px above the line.
            let gy = y - metrics.ymin - metrics.height as i32;
            if gx >= 0 && gy >= 0 {
                for row in 0..metrics.height {
                    let by = gy + row as i32;
                    if by as usize >= buf.len() / stride {
                        continue;
                    }
                    let bx = gx as usize;
                    let row_start = by as usize * stride;
                    for col in 0..metrics.width {
                        let px = bx + col;
                        if px >= stride {
                            continue;
                        }
                        let a = cov[row * metrics.width + col] as u32;
                        if a == 0 {
                            continue;
                        }
                        let dst = &mut buf[row_start + px];
                        *dst = blend(*dst, color, a) as u8;
                    }
                }
            }
            pen_x += metrics.advance_width;
        }
    }

    /// Is `ch` a real glyph in the embedded font? The bundled Noto Sans
    /// is a trimmed subset with NO geometric/arrows coverage — ▾ ▸ ● ★ ↗
    /// ↓ ⛶ are absent and rasterize as fontdue's .notdef rectangle. UI
    /// markers use painter-drawn shapes instead (see home's kicker, the
    /// TOC's current-chapter square); this check keeps future strings
    /// honest before they ship tofu.
    pub fn has_glyph(&self, ch: char) -> bool {
        ch == ' ' || self.font.chars().contains_key(&ch)
    }
}

#[cfg(test)]
mod tests {
    use super::Font;

    /// Every char of the non-ASCII-bearing UI strings must be a real
    /// glyph; the marker chars must stay absent (they're painter-drawn —
    /// if a font swap ever covers them, revisit those call sites).
    #[test]
    fn ui_strings_have_full_glyph_coverage() {
        let f = Font::load().unwrap();
        for s in [
            " · ",
            "…",
            "• ",
            "—",
            "«xylophoneist» — no entry in the dictionary",
            "РУССКИЙ ПЕРЕВОД",
            "ALL BOOKS (12) · > rows expand",
            "tap above · swipe down to close",
        ] {
            for ch in s.chars() {
                assert!(
                    f.has_glyph(ch),
                    "{:?} (U+{:04X}) has no glyph — it draws as tofu in {:?}",
                    ch,
                    ch as u32,
                    s
                );
            }
        }
        for ch in ['▾', '▸', '●', '★', '☆', '↗', '↓', '⛶', '🗂', '🎉'] {
            assert!(
                !f.has_glyph(ch),
                "{} is covered now — the painter-drawn markers can come off",
                ch
            );
        }
    }
}

fn blend(dst: u8, fg: u8, a: u32) -> u32 {
    let d = dst as u32;
    let f = fg as u32;
    (d * (255 - a) + f * a) / 255
}
