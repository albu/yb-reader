//! Image decoding for the mirror frames.

/// Decode a PNG into a w*h 8-bit grayscale buffer (any input color type).
///
/// `Transformations::EXPAND` is required: without it the png crate hands back
/// sub-byte depths *packed* (a 4-bit frame is w*h/2 bytes) and the caller
/// indexes out of bounds at the nibble boundary. EXPAND normalizes palette
/// and <8-bit grayscale to 8-bit samples, and lets us rely on the color
/// branches below as written.
pub fn decode_png_gray(data: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    use png::ColorType;
    let cursor = std::io::Cursor::new(data);
    let mut decoder = png::Decoder::new(cursor);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let (iw, ih) = (info.width, info.height);
    if iw != w || ih != h {
        return None;
    }
    let mut raw = vec![0u8; reader.output_buffer_size()];
    let info_out = reader.next_frame(&mut raw).ok()?;
    let out = &raw[..info_out.buffer_size()];
    let ct = info_out.color_type;
    let bd = info_out.bit_depth as usize;
    let spp = ct.samples();
    let mut gray = vec![0u8; (w * h) as usize];

    match ct {
        ColorType::Grayscale => {
            let max = (1u16 << bd) - 1;
            for (i, g) in gray.iter_mut().enumerate() {
                *g = scale_pixel(out[i], max);
            }
        }
        ColorType::GrayscaleAlpha => {
            let max = (1u16 << bd) - 1;
            for (i, px) in gray.iter_mut().enumerate() {
                let g = out[i * 2];
                let a = out[i * 2 + 1];
                let gv = scale_pixel(g, max) as u32;
                let av = scale_pixel(a, max) as u32;
                *px = ((gv * av + 255 * (255 - av)) / 255) as u8;
            }
        }
        ColorType::Rgb => {
            for (i, px) in gray.iter_mut().enumerate() {
                let (r, g, b) = (out[i * 3], out[i * 3 + 1], out[i * 3 + 2]);
                *px = luma(r, g, b);
            }
        }
        ColorType::Rgba => {
            for (i, px) in gray.iter_mut().enumerate() {
                let (r, g, b, a) = (out[i * 4], out[i * 4 + 1], out[i * 4 + 2], out[i * 4 + 3]);
                let l = luma(r, g, b) as u32;
                *px = ((l * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
            }
        }
        ColorType::Indexed => {
            // Unreachable after EXPAND (palettes become RGB), kept as a
            // defensive fallback.
            for (i, px) in gray.iter_mut().enumerate() {
                *px = out[i * spp];
            }
        }
    }
    Some(gray)
}

fn scale_pixel(v: u8, max: u16) -> u8 {
    if max == 255 {
        v
    } else {
        ((v as u32 * 255 + max as u32 / 2) / max as u32) as u8
    }
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

#[cfg(test)]
mod tests {
    use super::decode_png_gray;
    use png::{BitDepth, ColorType, Encoder};

    /// Build a genuine 4-bit grayscale PNG, the format server.py sends
    /// (bpp=4): samples are nibble-packed, high nibble first.
    fn make_gray4_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = Encoder::new(&mut out, w, h);
            enc.set_color(ColorType::Grayscale);
            enc.set_depth(BitDepth::Four);
            let mut writer = enc.write_header().unwrap();
            let row_bytes = ((w as usize + 1) / 2) * h as usize;
            let mut data = vec![0u8; row_bytes];
            for i in 0..(w * h) as usize {
                let v = (i % 16) as u8;
                if i % 2 == 0 {
                    data[i / 2] |= v << 4;
                } else {
                    data[i / 2] |= v;
                }
            }
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn decodes_4bit_gray_without_panic() {
        let (w, h) = (16u32, 8u32);
        let png = make_gray4_png(w, h);
        let gray = decode_png_gray(&png, w, h).expect("4-bit frame must decode");
        assert_eq!(gray.len(), (w * h) as usize);
        // EXPAND scales 4-bit samples to 0..255 in steps of 17.
        for (i, &v) in gray.iter().enumerate() {
            let expect = ((i % 16) as u32 * 255 / 15) as u8;
            assert_eq!(v, expect, "sample {} should be {}", i, expect);
        }
    }

    #[test]
    fn rejects_size_mismatch() {
        let png = make_gray4_png(16, 8);
        assert!(decode_png_gray(&png, 1236, 1648).is_none());
    }
}

