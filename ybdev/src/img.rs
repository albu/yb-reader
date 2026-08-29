//! Image decoding for the mirror frames.

use std::path::Path;

/// ~40 MP: far above any sane photo, far below memory trouble. Applied to
/// both decode paths — a "decode bomb" PNG header must fail the ceiling
/// check instead of OOMing the RAM-scarce device.
const MAX_PIXELS: u64 = 40_000_000;

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
    // The IHDR check above matches the client's screen size, but an APNG
    // first frame may be a sub-rectangle (fcTL) smaller than IHDR — the
    // decoded `out` then covers far fewer samples than `w*h` and the loops
    // below would index past its end. Validate the *decoded* frame, not
    // just the header.
    if info_out.width != w || info_out.height != h {
        return None;
    }
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
    // Ceiling checked on the *header* dims before any allocation — the
    // JPEG branch in load_image_fitted enforces the same limit.
    if info.width as u64 * info.height as u64 > MAX_PIXELS {
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

    Some(fit_center(
        &src_gray,
        src_w,
        src_h,
        dst_w as usize,
        dst_h as usize,
    ))
}

/// Aspect-fit `src` into a white dst_w×dst_h buffer, centered — the
/// shared tail of the PNG and generic image loaders.
fn fit_center(src_gray: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    let mut dst = vec![255u8; dst_w * dst_h];
    let scale_x = dst_w as f32 / src_w as f32;
    let scale_y = dst_h as f32 / src_h as f32;
    let scale = scale_x.min(scale_y);

    let fit_w = ((src_w as f32 * scale).round() as usize).min(dst_w);
    let fit_h = ((src_h as f32 * scale).round() as usize).min(dst_h);
    let off_x = dst_w.saturating_sub(fit_w) / 2;
    let off_y = dst_h.saturating_sub(fit_h) / 2;

    for dy in 0..fit_h {
        let sy = ((dy as f32 / scale).floor() as usize).min(src_h.saturating_sub(1));
        let dst_row = (off_y + dy) * dst_w + off_x;
        let src_row = sy * src_w;
        for dx in 0..fit_w {
            let sx = ((dx as f32 / scale).floor() as usize).min(src_w.saturating_sub(1));
            dst[dst_row + dx] = src_gray[src_row + sx];
        }
    }

    dst
}

/// Decode a PNG or JPEG (screensavers are whatever the user dragged in)
/// into a fitted grayscale framebuffer. PNG keeps the mirror's
/// battle-tested path by magic bytes; everything else rides the image
/// crate. Dimensions are read from the header BEFORE the decode so a
/// crafted "decode bomb" fails the ceiling check instead of OOMing the
/// device (same discipline as yread's embedded-image ceiling).
pub fn load_image_fitted(data: &[u8], dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() >= 8 && data[..8] == PNG_MAGIC {
        return load_png_fitted(data, dst_w, dst_h);
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let (w0, h0) = reader.into_dimensions().ok()?;
    if w0 as u64 * h0 as u64 > MAX_PIXELS {
        return None;
    }
    let img = image::load_from_memory(data).ok()?.to_luma8();
    let (src_w, src_h) = (img.width() as usize, img.height() as usize);
    if src_w == 0 || src_h == 0 {
        return None;
    }
    let mut out = if src_w == dst_w as usize && src_h == dst_h as usize {
        img.into_raw()
    } else {
        fit_center(
            &img.into_raw(),
            src_w,
            src_h,
            dst_w as usize,
            dst_h as usize,
        )
    };
    bayer16(&mut out, dst_w as usize, dst_h as usize);
    Some(out)
}

/// Disk-backed decode cache: a 2 MP jpeg decode+dither costs real time
/// on the device CPU, and RAM is too scarce to cache buffers. The
/// fitted, dithered, framebuffer-ready buffer is written ONCE per
/// image (content-addressed by FNV of the source bytes, so a replaced
/// file never hits a stale entry) and every later sleep is a plain
/// read — no decode, no steady-state memory cost. Format: 16-byte
/// header (magic, w, h) + w*h bytes, written .part → rename.
pub fn load_image_fitted_disk_cached(data: &[u8], dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    let dir = ss_cache_dir();
    let key = fnv1a(data);
    let name = format!("{key:016x}.gray");
    if let Some(buf) = read_gray_cache(&format!("{dir}/{name}"), dst_w, dst_h) {
        return Some(buf);
    }

    // Miss: render, persist, return.
    let t0 = std::time::Instant::now();
    let out = load_image_fitted(data, dst_w, dst_h)?;
    write_gray_cache(&dir, &name, dst_w, dst_h, &out);
    crate::log::plog(&format!(
        "screensaver: rendered {key:016x} {}x{} in {}ms",
        dst_w,
        dst_h,
        t0.elapsed().as_millis()
    ));
    Some(out)
}

/// Stable identity for a source file: name + length + mtime, hashed.
/// Thumbnails used to be content-addressed, which forced the draw path
/// to read whole multi-MB sources just to look up a ~10 KB thumbnail;
/// identity keying costs one stat. A replaced file gets a new
/// (len, mtime) and never hits a stale entry.
pub fn source_key(src: &Path) -> Option<u64> {
    let md = std::fs::metadata(src).ok()?;
    let name = src.file_name()?.to_string_lossy();
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mut id = Vec::with_capacity(name.len() + 16);
    id.extend_from_slice(name.as_bytes());
    id.extend_from_slice(&md.len().to_le_bytes());
    id.extend_from_slice(&mtime.to_le_bytes());
    Some(fnv1a(&id))
}

/// Fast stat-only read of a cached thumbnail: no source bytes needed.
pub fn read_thumb_disk_cached_fast(src: &Path, dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    let key = source_key(src)?;
    let dir = ss_cache_dir();
    read_gray_cache(
        &format!("{dir}/{key:016x}_thumb_{dst_w}x{dst_h}.gray"),
        dst_w,
        dst_h,
    )
}

/// Disk-backed decode cache for small screensaver thumbnails. `src` is
/// stat'ed for the cache key; `data` (the same file's bytes) is decoded
/// on a miss.
pub fn load_image_thumb_disk_cached(
    src: &Path,
    data: &[u8],
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    let key = source_key(src)?;
    let dir = ss_cache_dir();
    let name = format!("{key:016x}_thumb_{dst_w}x{dst_h}.gray");
    if let Some(buf) = read_gray_cache(&format!("{dir}/{name}"), dst_w, dst_h) {
        return Some(buf);
    }
    let out = load_image_fitted(data, dst_w, dst_h)?;
    write_gray_cache(&dir, &name, dst_w, dst_h, &out);
    Some(out)
}

/// Read one YBGRAY01 cache file, validating magic, size header, and
/// exact payload length — a torn or foreign file degrades to a miss.
fn read_gray_cache(path: &str, dst_w: u32, dst_h: u32) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    use std::io::Read as _;
    let mut head = [0u8; 16];
    f.read_exact(&mut head).ok()?;
    if head[..8] != *b"YBGRAY01" {
        return None;
    }
    let (w, h) = (
        u32::from_le_bytes(head[8..12].try_into().unwrap()),
        u32::from_le_bytes(head[12..16].try_into().unwrap()),
    );
    if w != dst_w || h != dst_h {
        return None;
    }
    let mut buf = Vec::with_capacity((dst_w as usize) * (dst_h as usize));
    f.read_to_end(&mut buf).ok()?;
    (buf.len() == (dst_w as usize) * (dst_h as usize)).then_some(buf)
}

/// Atomically persist one YBGRAY01 cache file (write .part, rename).
fn write_gray_cache(dir: &str, name: &str, dst_w: u32, dst_h: u32, out: &[u8]) {
    let _ = std::fs::create_dir_all(dir);
    let mut buf = Vec::with_capacity(16 + out.len());
    buf.extend_from_slice(b"YBGRAY01");
    buf.extend_from_slice(&dst_w.to_le_bytes());
    buf.extend_from_slice(&dst_h.to_le_bytes());
    buf.extend_from_slice(out);
    let part = format!("{dir}/{name}.part");
    let _ = std::fs::write(&part, &buf);
    let _ = std::fs::rename(&part, &format!("{dir}/{name}"));
}

/// The cache dir, overridable for host tests.
pub fn ss_cache_dir() -> String {
    std::env::var("YB_SS_CACHE_DIR")
        .unwrap_or_else(|_| "/mnt/us/extensions/reader/cache/screensavers".to_string())
}

/// Drop full-size renders whose content hashes are not in `keep`.
/// Content keying needs the source bytes, so this runs from prewarm,
/// which has every file's bytes in hand anyway. Full-size entries are a
/// bare 16-hex stem; thumbnails carry the `_thumb_` tag and belong to
/// [`ss_thumb_cache_gc`].
pub fn ss_cache_gc(keep: &[u64]) {
    let dir = ss_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut freed = 0u64;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".gray") else {
            continue;
        };
        if stem.len() != 16 || stem.contains('_') {
            continue;
        }
        let Ok(h) = u64::from_str_radix(stem, 16) else {
            continue;
        };
        if !keep.contains(&h) {
            if let Ok(md) = e.metadata() {
                freed += md.len();
            }
            let _ = std::fs::remove_file(e.path());
        }
    }
    if freed > 0 {
        crate::log::plog(&format!("screensaver: gc freed {} KB", freed / 1024));
    }
}

/// Drop thumbnails whose source identity is not in `keep` — the
/// `*_thumb_WxH.gray` siblings of [`ss_cache_gc`]'s full-size renders.
/// Stat-only (keys come from [`source_key`]), so the Screensavers
/// screen can call it on open without reading any source file.
pub fn ss_thumb_cache_gc(keep: &[u64]) {
    let dir = ss_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut freed = 0u64;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".gray") else {
            continue;
        };
        let Some(hex) = stem.split("_thumb_").next() else {
            continue;
        };
        if stem == hex {
            continue;
        }
        let Ok(k) = u64::from_str_radix(hex, 16) else {
            continue;
        };
        if !keep.contains(&k) {
            if let Ok(md) = e.metadata() {
                freed += md.len();
            }
            let _ = std::fs::remove_file(e.path());
        }
    }
    if freed > 0 {
        crate::log::plog(&format!("screensaver: thumb gc freed {} KB", freed / 1024));
    }
}

/// Content hash used as the disk-cache key (also the GC set element).
pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 8×8 Bayer threshold matrix (values 0..63).
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Ordered-dither down to the panel's ~16 gray levels. The EPDC
/// quantizes on its own; pre-quantizing with the matrix converts
/// gradient contour bands (what a photo looks like snapped to 16
/// levels) into a fine texture the eye integrates back into the
/// gradient. Screensavers only — text/UI pixels never pass through.
fn bayer16(buf: &mut [u8], w: usize, _h: usize) {
    const STEP: f32 = 255.0 / 15.0;
    for (i, px) in buf.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let t = (BAYER8[y % 8][x % 8] as f32 + 0.5) / 64.0 - 0.5;
        let q = ((*px as f32 / STEP) + t).round().clamp(0.0, 15.0);
        *px = (q * STEP).round() as u8;
    }
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

    /// APNG whose first frame (fcTL) is a sub-rectangle far smaller than
    /// IHDR. The IHDR dims match the caller's screen, but the decoded
    /// frame is tiny — the grayscale loops must not index past it. This
    /// used to panic (remote DoS: ~1 KB PNG crashes the reader).
    #[test]
    fn rejects_apng_subrect_first_frame_without_panic() {
        use png::{BitDepth, ColorType, Encoder};
        let (w, h) = (16u32, 8u32);
        let mut out = Vec::new();
        {
            let mut enc = Encoder::new(&mut out, w, h);
            enc.set_color(ColorType::Grayscale);
            enc.set_depth(BitDepth::Eight);
            enc.set_animated(2, 0).unwrap();
            let mut writer = enc.write_header().unwrap();
            writer.set_frame_dimension(1, 1).unwrap();
            writer.set_frame_position(0, 0).unwrap();
            writer.write_image_data(&[7u8]).unwrap();
        }
        assert!(decode_png_gray(&out, w, h).is_none());
    }
}

#[cfg(test)]
mod image_fitted_tests {
    use super::load_image_fitted;

    /// A real JPEG, end to end: encode with the image crate, decode +
    /// fit through the generic loader. Screensavers are user-dragged
    /// .jpgs — this is the path that was silently PNG-only before.
    #[test]
    fn jpeg_decodes_and_fits() {
        let img = image::GrayImage::from_fn(64, 32, |x, _| image::Luma([(x * 4) as u8]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&img)
            .unwrap();
        // JPEG magic, not PNG — exercises the image-crate branch.
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);

        // Exact-fit target: dimensions preserved, gradient-ish content.
        let out = load_image_fitted(&jpeg, 64, 32).expect("jpeg decodes");
        assert_eq!(out.len(), 64 * 32);
        // Left edge dark, right edge bright (jpeg is lossy: loose bounds).
        assert!(out[2] < 40, "left edge not dark: {}", out[2]);
        assert!(out[63] > 200, "right edge not bright: {}", out[63]);

        // Fitted target: white letterbox rows top/bottom for 2:1 into 1:1.
        let fitted = load_image_fitted(&jpeg, 32, 32).expect("jpeg fits");
        assert_eq!(fitted.len(), 32 * 32);
        assert!(fitted[0] == 255, "letterbox row not white");
    }

    #[test]
    fn garbage_and_oversized_headers_are_rejected() {
        // Not an image at all (e.g. an ._ AppleDouble sidecar).
        assert!(load_image_fitted(b"not an image", 8, 8).is_none());
        // Truncated jpeg magic — header parse must fail, not hang.
        assert!(load_image_fitted(&[0xFF, 0xD8, 0xFF], 8, 8).is_none());
    }
}

#[cfg(test)]
mod bayer_tests {
    /// The dither must snap every pixel onto the panel's 16-level grid
    /// while keeping local averages faithful — noise moves into space,
    /// not bias. That is exactly the property that turns contour bands
    /// into invisible texture.
    #[test]
    fn bayer_snaps_to_grid_and_preserves_means() {
        let (w, h) = (64usize, 8usize);
        let ramp: Vec<u8> = (0..w * h).map(|i| ((i % w) * 4) as u8).collect();
        let mut d = ramp.clone();
        super::bayer16(&mut d, w, h);
        assert!(
            d.iter().all(|&v| v % 17 == 0),
            "every value must sit on the 17-grid"
        );
        for x in (0..w).step_by(4) {
            let src: f32 = (x * 4) as f32;
            let mean: f32 = (0..h).map(|y| d[y * w + x] as f32).sum::<f32>() / h as f32;
            assert!(
                (mean - src).abs() <= 9.0,
                "column {x}: dithered mean {mean} drifted from source {src}"
            );
        }
    }
}

#[cfg(test)]
mod disk_cache_tests {
    use super::*;

    fn tiny_jpeg() -> Vec<u8> {
        let img = image::GrayImage::from_fn(16, 16, |x, _| image::Luma([(x * 15) as u8]));
        let mut v = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut v)
            .encode_image(&img)
            .unwrap();
        v
    }

    fn fresh_dir(tag: &str) -> String {
        let d = std::env::temp_dir().join(format!("yb_ss_cache_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.to_str().unwrap().to_string()
    }

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn disk_cache_round_trip_and_gc() {
        let _guard = TEST_LOCK.lock().unwrap();
        std::env::set_var("YB_SS_CACHE_DIR", fresh_dir("rt"));
        let jpeg = tiny_jpeg();

        // Miss → renders, writes the entry, returns content.
        let a = load_image_fitted_disk_cached(&jpeg, 16, 16).expect("first render");
        let h = fnv1a(&jpeg);
        let entry = format!("{}/{h:016x}.gray", ss_cache_dir());
        assert!(std::path::Path::new(&entry).exists(), "cache entry written");

        // Hit → same bytes from disk.
        let b = load_image_fitted_disk_cached(&jpeg, 16, 16).expect("cache hit");
        assert_eq!(a, b);

        // Different dims → different request; renders fresh (cache keyed
        // by content only, header dims guard serves no wrong buffer).
        let c = load_image_fitted_disk_cached(&jpeg, 8, 8).expect("other dims");
        assert_eq!(c.len(), 8 * 8);

        // Corrupted entry: wrong header must not serve garbage.
        let _ = std::fs::write(&entry, b"garbage-not-a-render");
        let d = load_image_fitted_disk_cached(&jpeg, 16, 16).expect("re-render after corrupt");
        assert_eq!(a, d);

        // GC: keep only this hash — the 8x8 render (same hash, kept) and
        // a foreign entry must go.
        let foreign = format!("{}/deadbeef00000000.gray", ss_cache_dir());
        let _ = std::fs::write(&foreign, b"x");
        ss_cache_gc(&[h]);
        assert!(!std::path::Path::new(&foreign).exists(), "orphan removed");
        assert!(std::path::Path::new(&entry).exists(), "live entry kept");

        std::env::remove_var("YB_SS_CACHE_DIR");
    }

    #[test]
    fn thumbnail_disk_cache_roundtrip_and_fast_read() {
        let _guard = TEST_LOCK.lock().unwrap();
        std::env::set_var("YB_SS_CACHE_DIR", fresh_dir("thumb_rt"));
        let dir = fresh_dir("thumb_src");
        let jpeg = tiny_jpeg();
        let src = std::path::Path::new(&dir).join("a.jpg");
        std::fs::write(&src, &jpeg).unwrap();

        // Stat-only fast read before render returns None (no bytes read).
        assert!(read_thumb_disk_cached_fast(&src, 8, 8).is_none());

        // Decode & cache thumbnail
        let rendered = load_image_thumb_disk_cached(&src, &jpeg, 8, 8).expect("thumb render");
        assert_eq!(rendered.len(), 64);

        // Fast read after render succeeds and matches rendered bytes
        let fast = read_thumb_disk_cached_fast(&src, 8, 8).expect("fast read hit");
        assert_eq!(fast, rendered);

        // The full-size GC must not touch thumbnails (separate GCs).
        ss_cache_gc(&[]);
        assert!(read_thumb_disk_cached_fast(&src, 8, 8).is_some());

        // Thumb GC keeps the entry while the source identity is live…
        let key = source_key(&src).expect("key");
        ss_thumb_cache_gc(&[key]);
        assert!(read_thumb_disk_cached_fast(&src, 8, 8).is_some());

        // …and removes it once the source is gone (or replaced: a new
        // len/mtime means a new key).
        ss_thumb_cache_gc(&[]);
        assert!(read_thumb_disk_cached_fast(&src, 8, 8).is_none());

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("YB_SS_CACHE_DIR");
    }
}
