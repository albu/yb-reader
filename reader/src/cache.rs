//! Page snapshot caching and garbage collection for instant book opening and turns.

use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::split::ReaderSettings;

const CACHE_DIR: &str = "/mnt/us/extensions/reader/cache";
const MAGIC: &[u8; 8] = b"YBSNAP13"; // 13: layout fingerprint over the whole settings (see layout_fingerprint)
const SNAP12: &[u8; 8] = b"YBSNAP12"; // 12: four typography fields in key
const SNAP11: &[u8; 8] = b"YBSNAP11"; // 11: line_spacing in key
const SNAP10: &[u8; 8] = b"YBSNAP10"; // 10: pre-line_spacing
pub const MAX_CACHED_FILES: usize = 24; // orientation/preset variants coexist

/// Deterministic FNV-1a hasher. std's DefaultHasher is explicitly not
/// guaranteed stable across releases, and the fingerprint is persisted in
/// the snapshot header, so it must be.
struct FnvHasher(u64);

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        self.0 = h;
    }
}

/// The snapshot's layout identity: an FNV hash over every layout-affecting
/// field of ReaderSettings plus page/sub/dims/engine, in one fixed place.
/// The recurring "field missing from the cache key" bug family
/// (line_spacing -> typography four -> spacing two) now has exactly one
/// spot to extend — add a new layout-affecting field here and the key
/// follows automatically; forget it and the fingerprint is still stable
/// for the old fields, so nothing silently serves stale pixels for the
/// fields that ARE covered.
fn layout_fingerprint(
    settings: &ReaderSettings,
    page: u32,
    sub: u32,
    width: u32,
    height: u32,
    engine: u8,
) -> u64 {
    let mut h = FnvHasher(0xcbf29ce484222325);
    let s = settings;
    h.write(&page.to_le_bytes());
    h.write(&sub.to_le_bytes());
    h.write(&width.to_le_bytes());
    h.write(&height.to_le_bytes());
    h.write(&[engine]);
    h.write(&s.font_size.to_le_bytes());
    h.write(&s.margin_pad.to_le_bytes());
    h.write(&s.line_spacing.to_le_bytes());
    h.write(&s.paragraph_spacing.to_le_bytes());
    h.write(&s.indent_em.to_le_bytes());
    h.write(&[s.hyphenate as u8]);
    h.write(&[align_code(s.body_align)]);
    h.write(&s.word_spacing_mult.to_le_bytes());
    h.write(&s.letter_spacing_px.to_le_bytes());
    h.write(&[s.font_family.id()]);
    h.write(&[s.contrast as u8]);
    h.write(&[s.white_cutoff]);
    h.write(&[s.invert as u8]);
    h.write(&[s.show_header as u8]);
    h.write(&[preset_code(&s.split.preset)]);
    h.write(&s.split.rotation.to_le_bytes());
    h.write(&s.split.overlap.to_le_bytes());
    h.write(&s.split.margin_left.to_le_bytes());
    h.write(&s.split.margin_top.to_le_bytes());
    h.write(&s.split.margin_right.to_le_bytes());
    h.write(&s.split.margin_bottom.to_le_bytes());
    h.write(&[s.split.mirror_even_odd as u8]);
    h.finish()
}

/// Byte code for the split preset — part of the snapshot's identity.
fn preset_code(p: &crate::split::SplitPreset) -> u8 {
    use crate::split::SplitPreset::*;
    match p {
        FitPage => 0,
        Horizontal2 => 1,
        Horizontal3 => 2,
        Vertical2 => 3,
        Grid4 => 4,
    }
}

/// Byte code for the body alignment — part of the snapshot's identity.
fn align_code(a: yread::model::TextAlign) -> u8 {
    match a {
        yread::model::TextAlign::Left => 0,
        yread::model::TextAlign::Center => 1,
        yread::model::TextAlign::Right => 2,
        yread::model::TextAlign::Justify => 3,
    }
}

/// Floats came through the positions store as decimals — compare with a
/// tolerance finer than any UI step (0.02) but coarser than f32 noise.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.002
}

/// Compute a safe filesystem cache key from book filename.
///
/// The sanitized characters alone lose identity: "a b.epub" and "a_b.epub"
/// (or two distinct CJK titles) map to the same string and would serve
/// each other's cached page. An FNV identity hash of the original name
/// breaks the collision — the filename keeps readability, the hash
/// guarantees uniqueness.
pub fn book_cache_key(book_name: &str) -> String {
    let mut s = String::new();
    for c in book_name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    if s.is_empty() {
        s = "default".to_string();
    }
    format!("{}_{:016x}", s, ybdev::img::fnv1a(book_name.as_bytes()))
}

pub fn cache_file_path_at(dir: &str, book_name: &str, page_no: usize, sub_idx: usize) -> PathBuf {
    let key = book_cache_key(book_name);
    Path::new(dir).join(format!("{}_p{}_s{}.snap", key, page_no, sub_idx))
}

/// Load cached grayscale page if it exists and matches settings and dimensions.
pub fn load_snapshot_from(
    dir: &str,
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    engine: u8,
) -> Option<Vec<u8>> {
    let path = cache_file_path_at(dir, book_name, page_no, sub_idx);
    let bytes = fs::read(&path).ok()?;
    // YBSNAP13: one fingerprint compare covers every layout-affecting
    // field (and anything added to ReaderSettings later).
    let header_len = if bytes.len() >= 33 && &bytes[0..8] == MAGIC {
        let stored = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
        if stored
            != layout_fingerprint(settings, page_no as u32, sub_idx as u32, w, h, engine)
        {
            return None;
        }
        33
    } else {
        // Legacy headers: per-field compare, with defaults for the fields
        // they predate, so a pre-upgrade cache entry still serves.
        let (
            hlen,
            snap_spacing,
            snap_para,
            snap_indent,
            snap_hyphenate,
            snap_align,
            snap_word,
            snap_tracking,
        ) = if bytes.len() >= 72 && &bytes[0..8] == SNAP12 {
            let sp = f32::from_le_bytes(bytes[58..62].try_into().ok()?);
            let para = f32::from_le_bytes(bytes[62..66].try_into().ok()?);
            let indent = f32::from_le_bytes(bytes[66..70].try_into().ok()?);
            let hyphen = bytes[70] != 0;
            let align = bytes[71];
            (72, sp, para, indent, hyphen, align, 1.0, 0.0)
        } else if bytes.len() >= 62 && &bytes[0..8] == SNAP11 {
            let sp = f32::from_le_bytes(bytes[58..62].try_into().ok()?);
            (
                62,
                sp,
                0.25,
                1.2,
                true,
                align_code(yread::model::TextAlign::Justify),
                1.0,
                0.0,
            )
        } else if bytes.len() >= 58 && &bytes[0..8] == SNAP10 {
            (
                58,
                1.0f32,
                0.25,
                1.2,
                true,
                align_code(yread::model::TextAlign::Justify),
                1.0,
                0.0,
            )
        } else {
            return None;
        };

        let snap_page = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let snap_sub = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        let snap_w = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
        let snap_h = u32::from_le_bytes(bytes[20..24].try_into().ok()?);
        let snap_font = f32::from_le_bytes(bytes[24..28].try_into().ok()?);
        let snap_margin = u32::from_le_bytes(bytes[28..32].try_into().ok()?);
        let snap_contrast = bytes[32];
        let snap_invert = bytes[33] != 0;
        let snap_preset = preset_code(&settings.split.preset) ^ bytes[34];
        let snap_engine = engine ^ bytes[35];
        let snap_rotation =
            u16::from_le_bytes(bytes[36..38].try_into().ok()?) ^ settings.split.rotation;
        let snap_overlap = f32::from_le_bytes(bytes[38..42].try_into().ok()?);
        let snap_ml = f32::from_le_bytes(bytes[42..46].try_into().ok()?);
        let snap_mt = f32::from_le_bytes(bytes[46..50].try_into().ok()?);
        let snap_mr = f32::from_le_bytes(bytes[50..54].try_into().ok()?);
        let snap_mb = f32::from_le_bytes(bytes[54..58].try_into().ok()?);

        // Must match the requested page, dimensions, font size, visual
        // settings, spacing and the whole split identity (rotation,
        // preset, overlap, crop margins) — a tuned crop's snapshot must
        // never serve a different crop, and a portrait render must never
        // pose as landscape.
        if snap_page != page_no
            || snap_sub != sub_idx
            || snap_w != w
            || snap_h != h
            || (snap_font - settings.font_size).abs() > 0.01
            || (snap_spacing - settings.line_spacing).abs() > 0.01
            || (snap_para - settings.paragraph_spacing).abs() > 0.01
            || (snap_indent - settings.indent_em).abs() > 0.01
            || snap_hyphenate != settings.hyphenate
            || snap_align != align_code(settings.body_align)
            || (snap_word - settings.word_spacing_mult).abs() > 0.01
            || (snap_tracking - settings.letter_spacing_px).abs() > 0.01
            || snap_margin != settings.margin_pad
            || snap_contrast != (settings.contrast as u8)
            || snap_invert != settings.invert
            || snap_preset != 0
            || snap_engine != 0
            || snap_rotation != 0
            || !near(snap_overlap, settings.split.overlap)
            || !near(snap_ml, settings.split.margin_left)
            || !near(snap_mt, settings.split.margin_top)
            || !near(snap_mr, settings.split.margin_right)
            || !near(snap_mb, settings.split.margin_bottom)
        {
            return None;
        }
        hlen
    };

    let expected_len = (w * h) as usize;
    let pixel_data = &bytes[header_len..];
    if pixel_data.len() != expected_len {
        return None;
    }

    Some(pixel_data.to_vec())
}

pub fn load_snapshot(
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    engine: u8,
) -> Option<Vec<u8>> {
    load_snapshot_from(
        CACHE_DIR, book_name, page_no, sub_idx, settings, w, h, engine,
    )
}

/// Save page snapshot to cache and run garbage collection.
pub fn save_snapshot_to(
    dir: &str,
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    pixels: &[u8],
    engine: u8,
) {
    if pixels.len() != (w * h) as usize {
        return;
    }
    let _ = fs::create_dir_all(dir);
    let path = cache_file_path_at(dir, book_name, page_no, sub_idx);

    let mut buf = Vec::with_capacity(33 + pixels.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(
        &layout_fingerprint(settings, page_no as u32, sub_idx as u32, w, h, engine).to_le_bytes(),
    );
    buf.extend_from_slice(&(page_no as u32).to_le_bytes());
    buf.extend_from_slice(&(sub_idx as u32).to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
    buf.push(engine);
    buf.extend_from_slice(pixels);

    let tmp = format!("{}.tmp", path.display());
    if fs::write(&tmp, &buf).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }

    prune_cache_dir(dir);
}

pub fn save_snapshot(
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    pixels: &[u8],
    engine: u8,
) {
    if pixels.len() != (w * h) as usize {
        return;
    }
    let name = book_name.to_string();
    let s = *settings;
    let data = pixels.to_vec();
    std::thread::spawn(move || {
        save_snapshot_to(CACHE_DIR, &name, page_no, sub_idx, &s, w, h, &data, engine);
    });
}

/// Prune cache directory: keeps only the newest MAX_CACHED_FILES snapshot files.
pub fn prune_cache_dir(dir: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("snap") {
            if let Ok(meta) = entry.metadata() {
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((path, modified));
            }
        }
    }

    if files.len() > MAX_CACHED_FILES {
        // Sort descending by modification time (newest first)
        files.sort_by_key(|b| std::cmp::Reverse(b.1));
        // Remove excess older files
        for (path, _) in &files[MAX_CACHED_FILES..] {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::ContrastMode;

    #[test]
    fn test_snapshot_roundtrip_and_settings_match() {
        let dir = "/tmp/yb_cache_test_1";
        let _ = fs::remove_dir_all(dir);

        let settings = ReaderSettings {
            font_size: 12.0,
            margin_pad: 72,
            contrast: ContrastMode::HighContrast,
            invert: false,
            ..Default::default()
        };

        let w = 10;
        let h = 10;
        let pixels = vec![128u8; (w * h) as usize];

        save_snapshot_to(dir, "my_book.epub", 5, 0, &settings, w, h, &pixels, 0);

        // Exact match -> Loads successfully
        let loaded = load_snapshot_from(dir, "my_book.epub", 5, 0, &settings, w, h, 0);
        assert_eq!(loaded, Some(pixels.clone()));

        // Different page -> Rejected
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 6, 0, &settings, w, h, 0),
            None
        );

        // Different engine -> Rejected (a yread bitmap must never pose as
        // a mupdf one and vice versa — the engines paginate differently).
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &settings, w, h, 1),
            None
        );

        // Different font size -> Rejected
        let mut diff_font = settings;
        diff_font.font_size = 14.0;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_font, w, h, 0),
            None
        );

        // Different invert -> Rejected
        let mut diff_inv = settings;
        diff_inv.invert = true;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_inv, w, h, 0),
            None
        );

        // Different rotation -> Rejected (the bug this header field exists
        // for: portrait renders used to pose as landscape and vice versa).
        let mut diff_rot = settings;
        diff_rot.split.rotation = 270;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_rot, w, h, 0),
            None
        );

        // Different preset -> Rejected (H2 and H3 share visual dims and
        // sub numbering but crop different regions).
        let mut diff_preset = settings;
        diff_preset.split.preset = crate::split::SplitPreset::Horizontal2;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_preset, w, h, 0),
            None
        );

        // Different crop margin -> Rejected (the big-PDF tuning workflow).
        let mut diff_crop = settings;
        diff_crop.split.margin_top = 0.10;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_crop, w, h, 0),
            None
        );

        // Different line spacing -> Rejected
        let mut diff_spacing = settings;
        diff_spacing.line_spacing = 1.4;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_spacing, w, h, 0),
            None
        );

        // Different typography fields -> Rejected (a snapshot rendered with
        // one indent/hyphenation/alignment must never serve another).
        let mut diff_para = settings;
        diff_para.paragraph_spacing = 0.6;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_para, w, h, 0),
            None
        );
        let mut diff_indent = settings;
        diff_indent.indent_em = 0.0;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_indent, w, h, 0),
            None
        );
        let mut diff_hyphen = settings;
        diff_hyphen.hyphenate = false;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_hyphen, w, h, 0),
            None
        );
        let mut diff_align = settings;
        diff_align.body_align = yread::model::TextAlign::Left;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_align, w, h, 0),
            None
        );

        // Different word/letter spacing -> Rejected (the fields that
        // triggered the YBSNAP13 fingerprint bump).
        let mut diff_word = settings;
        diff_word.word_spacing_mult = 1.25;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_word, w, h, 0),
            None
        );
        let mut diff_track = settings;
        diff_track.letter_spacing_px = 1.0;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_track, w, h, 0),
            None
        );
        let mut diff_family = settings;
        diff_family.font_family = yread::font::FontFamily::PtSerif;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_family, w, h, 0),
            None
        );

        // A field that predates the key entirely (show_header changes
        // margins -> layout) is now covered by the fingerprint too.
        let mut diff_header = settings;
        diff_header.show_header = false;
        assert_eq!(
            load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_header, w, h, 0),
            None
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_snapshot_typography_roundtrip_and_legacy_fallback() {
        let dir = "/tmp/yb_cache_test_typo";
        let _ = fs::remove_dir_all(dir);

        // Non-default typography + spacing round-trips through the
        // YBSNAP13 fingerprint header.
        let settings = ReaderSettings {
            paragraph_spacing: 0.6,
            indent_em: 0.0,
            hyphenate: false,
            body_align: yread::model::TextAlign::Left,
            word_spacing_mult: 1.25,
            letter_spacing_px: 1.0,
            ..Default::default()
        };
        let w = 10;
        let h = 10;
        let pixels = vec![128u8; (w * h) as usize];
        let legacy = ReaderSettings {
            font_size: 11.0,
            ..Default::default()
        };
        save_snapshot_to(dir, "typo.epub", 1, 0, &settings, w, h, &pixels, 0);
        assert_eq!(
            load_snapshot_from(dir, "typo.epub", 1, 0, &settings, w, h, 0),
            Some(pixels.clone())
        );

        // A hand-built YBSNAP12 snapshot (typography fields, no spacing)
        // loads with spacing defaults — a pre-YBSNAP13 entry still serves.
        let path12 = cache_file_path_at(dir, "legacy12.epub", 1, 0);
        let mut buf12 = Vec::with_capacity(72 + pixels.len());
        buf12.extend_from_slice(SNAP12);
        buf12.extend_from_slice(&1u32.to_le_bytes()); // page
        buf12.extend_from_slice(&0u32.to_le_bytes()); // sub
        buf12.extend_from_slice(&w.to_le_bytes());
        buf12.extend_from_slice(&h.to_le_bytes());
        buf12.extend_from_slice(&11.0f32.to_le_bytes()); // font
        buf12.extend_from_slice(&72u32.to_le_bytes()); // margin
        buf12.push(0); // contrast
        buf12.push(0); // invert
        buf12.push(0); // preset
        buf12.push(0); // engine
        buf12.extend_from_slice(&0u16.to_le_bytes()); // rotation
        buf12.extend_from_slice(&0.018f32.to_le_bytes()); // overlap
        buf12.extend_from_slice(&0.0f32.to_le_bytes()); // ml
        buf12.extend_from_slice(&0.0f32.to_le_bytes()); // mt
        buf12.extend_from_slice(&0.0f32.to_le_bytes()); // mr
        buf12.extend_from_slice(&0.0f32.to_le_bytes()); // mb
        buf12.extend_from_slice(&1.0f32.to_le_bytes()); // line_spacing
        buf12.extend_from_slice(&0.25f32.to_le_bytes()); // paragraph_spacing
        buf12.extend_from_slice(&1.2f32.to_le_bytes()); // indent_em
        buf12.push(1); // hyphenate
        buf12.push(3); // align = Justify
        buf12.extend_from_slice(&pixels);
        fs::write(&path12, &buf12).unwrap();
        assert_eq!(
            load_snapshot_from(dir, "legacy12.epub", 1, 0, &legacy, w, h, 0),
            Some(pixels.clone()),
            "YBSNAP12 fallback must serve with spacing defaults"
        );
        let changed_spacing = ReaderSettings {
            font_size: 11.0,
            word_spacing_mult: 1.25,
            ..Default::default()
        };
        assert_eq!(
            load_snapshot_from(dir, "legacy12.epub", 1, 0, &changed_spacing, w, h, 0),
            None,
            "spacing change must miss a YBSNAP12 cache entry"
        );

        // A hand-built YBSNAP11 snapshot (no typography fields) loads with
        // the reader defaults — a pre-upgrade cache entry still serves.
        let path = cache_file_path_at(dir, "legacy.epub", 1, 0);
        let mut buf = Vec::with_capacity(62 + pixels.len());
        buf.extend_from_slice(SNAP11);
        buf.extend_from_slice(&1u32.to_le_bytes()); // page
        buf.extend_from_slice(&0u32.to_le_bytes()); // sub
        buf.extend_from_slice(&w.to_le_bytes());
        buf.extend_from_slice(&h.to_le_bytes());
        buf.extend_from_slice(&11.0f32.to_le_bytes()); // font
        buf.extend_from_slice(&72u32.to_le_bytes()); // margin
        buf.push(0); // contrast
        buf.push(0); // invert
        buf.push(0); // preset
        buf.push(0); // engine
        buf.extend_from_slice(&0u16.to_le_bytes()); // rotation
        buf.extend_from_slice(&0.018f32.to_le_bytes()); // overlap (SplitConfig::default)
        buf.extend_from_slice(&0.0f32.to_le_bytes()); // ml
        buf.extend_from_slice(&0.0f32.to_le_bytes()); // mt
        buf.extend_from_slice(&0.0f32.to_le_bytes()); // mr
        buf.extend_from_slice(&0.0f32.to_le_bytes()); // mb
        buf.extend_from_slice(&1.0f32.to_le_bytes()); // line_spacing
        buf.extend_from_slice(&pixels);
        fs::write(&path, &buf).unwrap();

        assert_eq!(
            load_snapshot_from(dir, "legacy.epub", 1, 0, &legacy, w, h, 0),
            Some(pixels.clone()),
            "YBSNAP11 fallback must serve with typography defaults"
        );
        // But a typography change after the legacy snapshot was written
        // misses the cache (the reason for the bump in the first place).
        let changed = ReaderSettings {
            font_size: 11.0,
            indent_em: 0.0,
            ..Default::default()
        };
        assert_eq!(
            load_snapshot_from(dir, "legacy.epub", 1, 0, &changed, w, h, 0),
            None
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_garbage_collection_pruning() {
        let dir = "/tmp/yb_cache_test_prune";
        let _ = fs::remove_dir_all(dir);

        let settings = ReaderSettings::default();
        let w = 4;
        let h = 4;
        let pixels = vec![255u8; 16];

        // Create more snapshots than the cap
        for i in 0..(MAX_CACHED_FILES + 8) {
            let book_name = format!("book_{}.epub", i);
            save_snapshot_to(dir, &book_name, i, 0, &settings, w, h, &pixels, 0);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }

        // Must be pruned down to MAX_CACHED_FILES
        let count = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("snap"))
            .count();
        assert_eq!(count, MAX_CACHED_FILES);

        let _ = fs::remove_dir_all(dir);
    }
}
