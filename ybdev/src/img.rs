//! Image decoding for the mirror frames.

/// Decode a PNG into a w*h 8-bit grayscale buffer (any input color type).
///
/// `Transformations::EXPAND` is required: without it the png crate hands back
/// sub-byte depths *packed* (a 4-bit frame is w*h/2 bytes) and the caller
/// indexes out of bounds at the nibble boundary. EXPAND normalizes palette
/// and <8-bit grayscale to 8-bit samples, and lets us rely on the color
/// branches below as written. 16-bit PNGs are rejected: EXPAND leaves them
/// at depth 16, where the byte-per-sample indexing and the `1u16 << depth`
/// scale math below would be wrong / panic.
pub fn decode_png_gray(data: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    use png::ColorType;
    let cursor = std::io::Cursor::new(data);
    let mut decoder = png::Decoder::new(cursor);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.bit_depth == png::BitDepth::Sixteen {
        return None;
    }
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

/// Decode a PNG file of any size and aspect-fit / center it into a (dst_w, dst_h)
/// 8-bit grayscale framebuffer with white (255) background.
pub fn load_png_fitted(data: &[u8], dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    use png::ColorType;
    let cursor = std::io::Cursor::new(data);
    let mut decoder = png::Decoder::new(cursor);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.bit_depth == png::BitDepth::Sixteen {
        return None;
    }
    let (src_w, src_h) = (info.width as usize, info.height as usize);
    if src_w == 0 || src_h == 0 {
        return None;
    }
    let mut raw = vec![0u8; reader.output_buffer_size()];
    let info_out = reader.next_frame(&mut raw).ok()?;
    let out = &raw[..info_out.buffer_size()];
    let ct = info_out.color_type;
    let bd = info_out.bit_depth as usize;

    let mut src_gray = vec![0u8; src_w * src_h];
    match ct {
        ColorType::Grayscale => {
            let max = (1u16 << bd) - 1;
            for (i, g) in src_gray.iter_mut().enumerate().take(out.len()) {
                *g = scale_pixel(out[i], max);
            }
        }
        ColorType::GrayscaleAlpha => {
            let max = (1u16 << bd) - 1;
            for i in 0..src_gray.len() {
                if i * 2 + 1 < out.len() {
                    let g = out[i * 2];
                    let a = out[i * 2 + 1];
                    let gv = scale_pixel(g, max) as u32;
                    let av = scale_pixel(a, max) as u32;
                    src_gray[i] = ((gv * av + 255 * (255 - av)) / 255) as u8;
                }
            }
        }
        ColorType::Rgb => {
            for i in 0..src_gray.len() {
                if i * 3 + 2 < out.len() {
                    let (r, g, b) = (out[i * 3], out[i * 3 + 1], out[i * 3 + 2]);
                    src_gray[i] = luma(r, g, b);
                }
            }
        }
        ColorType::Rgba => {
            for i in 0..src_gray.len() {
                if i * 4 + 3 < out.len() {
                    let (r, g, b, a) = (out[i * 4], out[i * 4 + 1], out[i * 4 + 2], out[i * 4 + 3]);
                    let l = luma(r, g, b) as u32;
                    src_gray[i] = ((l * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
                }
            }
        }
        ColorType::Indexed => {
            return None;
        }
    }

    if src_w == dst_w as usize && src_h == dst_h as usize {
        return Some(src_gray);
    }

    let mut dst = vec![255u8; (dst_w * dst_h) as usize];
    let scale_x = dst_w as f32 / src_w as f32;
    let scale_y = dst_h as f32 / src_h as f32;
    let scale = scale_x.min(scale_y);

    let fit_w = ((src_w as f32 * scale).round() as usize).min(dst_w as usize);
    let fit_h = ((src_h as f32 * scale).round() as usize).min(dst_h as usize);
    let off_x = (dst_w as usize).saturating_sub(fit_w) / 2;
    let off_y = (dst_h as usize).saturating_sub(fit_h) / 2;

    for dy in 0..fit_h {
        let sy = ((dy as f32 / scale).floor() as usize).min(src_h.saturating_sub(1));
        let dst_row = (off_y + dy) * dst_w as usize + off_x;
        let src_row = sy * src_w;
        for dx in 0..fit_w {
            let sx = ((dx as f32 / scale).floor() as usize).min(src_w.saturating_sub(1));
            dst[dst_row + dx] = src_gray[src_row + sx];
        }
    }

    Some(dst)
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

    /// Build a 16-bit grayscale PNG. EXPAND does not normalize these, so
    /// they must be rejected rather than decoded through the byte-per-sample
    /// path (shift overflow / divide-by-zero before the guard existed).
    fn make_gray16_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = Encoder::new(&mut out, w, h);
            enc.set_color(ColorType::Grayscale);
            enc.set_depth(BitDepth::Sixteen);
            let mut writer = enc.write_header().unwrap();
            let mut data = vec![0u8; (w * h) as usize * 2];
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn rejects_16bit_gray_without_panic() {
        let (w, h) = (16u32, 8u32);
        let png = make_gray16_png(w, h);
        assert!(decode_png_gray(&png, w, h).is_none());
        assert!(super::load_png_fitted(&png, w, h).is_none());
    }

    #[test]
    fn rejects_size_mismatch() {
        let png = make_gray4_png(16, 8);
        assert!(decode_png_gray(&png, 1236, 1648).is_none());
    }
}

