//! Page snapshot caching and garbage collection for instant book opening and turns.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::split::ReaderSettings;

const CACHE_DIR: &str = "/mnt/us/extensions/reader/cache";
const MAGIC: &[u8; 8] = b"YBSNAP11"; // 11: Includes line_spacing in snapshot key
pub const MAX_CACHED_FILES: usize = 24; // orientation/preset variants coexist

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

/// Floats came through the positions store as decimals — compare with a
/// tolerance finer than any UI step (0.02) but coarser than f32 noise.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.002
}

/// Compute a safe filesystem cache key from book filename.
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
    s
}

pub fn cache_file_path_at(dir: &str, book_name: &str, page_no: usize, sub_idx: usize) -> PathBuf {
    let key = book_cache_key(book_name);
    Path::new(dir).join(format!("{}_p{}_s{}.snap", key, page_no, sub_idx))
}

#[allow(dead_code)]
pub fn cache_file_path(book_name: &str, page_no: usize, sub_idx: usize) -> PathBuf {
    cache_file_path_at(CACHE_DIR, book_name, page_no, sub_idx)
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
) -> Option<Vec<u8>> {
    let path = cache_file_path_at(dir, book_name, page_no, sub_idx);
    let bytes = fs::read(&path).ok()?;
    let (header_len, snap_spacing) = if bytes.len() >= 62 && &bytes[0..8] == MAGIC {
        let sp = f32::from_le_bytes(bytes[58..62].try_into().ok()?);
        (62, sp)
    } else if bytes.len() >= 58 && &bytes[0..8] == b"YBSNAP10" {
        (58, 1.0f32)
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
    // XOR so a mismatch shows up as nonzero in one integer compare path.
    let snap_preset = preset_code(&settings.split.preset) ^ bytes[34];
    let snap_rotation = u16::from_le_bytes(bytes[36..38].try_into().ok()?) ^ settings.split.rotation;
    let snap_overlap = f32::from_le_bytes(bytes[38..42].try_into().ok()?);
    let snap_ml = f32::from_le_bytes(bytes[42..46].try_into().ok()?);
    let snap_mt = f32::from_le_bytes(bytes[46..50].try_into().ok()?);
    let snap_mr = f32::from_le_bytes(bytes[50..54].try_into().ok()?);
    let snap_mb = f32::from_le_bytes(bytes[54..58].try_into().ok()?);

    // Must match the requested page, dimensions, font size, visual
    // settings AND the whole split identity (rotation, preset, overlap,
    // crop margins) — a tuned crop's snapshot must never serve a
    // different crop, and a portrait render must never pose as landscape.
    if snap_page != page_no
        || snap_sub != sub_idx
        || snap_w != w
        || snap_h != h
        || (snap_font - settings.font_size).abs() > 0.01
        || (snap_spacing - settings.line_spacing).abs() > 0.01
        || snap_margin != settings.margin_pad
        || snap_contrast != (settings.contrast as u8)
        || snap_invert != settings.invert
        || snap_preset != 0
        || snap_rotation != 0
        || !near(snap_overlap, settings.split.overlap)
        || !near(snap_ml, settings.split.margin_left)
        || !near(snap_mt, settings.split.margin_top)
        || !near(snap_mr, settings.split.margin_right)
        || !near(snap_mb, settings.split.margin_bottom)
    {
        return None;
    }

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
) -> Option<Vec<u8>> {
    load_snapshot_from(CACHE_DIR, book_name, page_no, sub_idx, settings, w, h)
}

/// Save page snapshot to cache and run garbage collection.
#[allow(dead_code)]
pub fn save_snapshot_to(
    dir: &str,
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    pixels: &[u8],
) {
    if pixels.len() != (w * h) as usize {
        return;
    }
    let _ = fs::create_dir_all(dir);
    let path = cache_file_path_at(dir, book_name, page_no, sub_idx);

    let mut buf = Vec::with_capacity(62 + pixels.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(page_no as u32).to_le_bytes());
    buf.extend_from_slice(&(sub_idx as u32).to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
    buf.extend_from_slice(&settings.font_size.to_le_bytes());
    buf.extend_from_slice(&settings.margin_pad.to_le_bytes());
    buf.push(settings.contrast as u8);
    buf.push(if settings.invert { 1 } else { 0 });
    buf.push(preset_code(&settings.split.preset));
    buf.push(0u8); // padding: rotation below is u16-aligned
    buf.extend_from_slice(&settings.split.rotation.to_le_bytes());
    let sc = settings.split;
    buf.extend_from_slice(&sc.overlap.to_le_bytes());
    buf.extend_from_slice(&sc.margin_left.to_le_bytes());
    buf.extend_from_slice(&sc.margin_top.to_le_bytes());
    buf.extend_from_slice(&sc.margin_right.to_le_bytes());
    buf.extend_from_slice(&sc.margin_bottom.to_le_bytes());
    buf.extend_from_slice(&settings.line_spacing.to_le_bytes());
    buf.extend_from_slice(pixels);

    let tmp = format!("{}.tmp", path.display());
    if fs::write(&tmp, &buf).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }

    prune_cache_dir(dir);
}

#[allow(dead_code)]
pub fn save_snapshot(
    book_name: &str,
    page_no: usize,
    sub_idx: usize,
    settings: &ReaderSettings,
    w: u32,
    h: u32,
    pixels: &[u8],
) {
    if pixels.len() != (w * h) as usize {
        return;
    }
    let name = book_name.to_string();
    let s = *settings;
    let data = pixels.to_vec();
    std::thread::spawn(move || {
        save_snapshot_to(CACHE_DIR, &name, page_no, sub_idx, &s, w, h, &data);
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
        files.sort_by(|a, b| b.1.cmp(&a.1));
        // Remove excess older files
        for (path, _) in &files[MAX_CACHED_FILES..] {
            let _ = fs::remove_file(path);
        }
    }
}

#[allow(dead_code)]
pub fn prune_cache() {
    prune_cache_dir(CACHE_DIR);
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

        save_snapshot_to(dir, "my_book.epub", 5, 0, &settings, w, h, &pixels);

        // Exact match -> Loads successfully
        let loaded = load_snapshot_from(dir, "my_book.epub", 5, 0, &settings, w, h);
        assert_eq!(loaded, Some(pixels.clone()));

        // Different page -> Rejected
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 6, 0, &settings, w, h), None);

        // Different font size -> Rejected
        let mut diff_font = settings;
        diff_font.font_size = 14.0;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_font, w, h), None);

        // Different invert -> Rejected
        let mut diff_inv = settings;
        diff_inv.invert = true;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_inv, w, h), None);

        // Different rotation -> Rejected (the bug this header field exists
        // for: portrait renders used to pose as landscape and vice versa).
        let mut diff_rot = settings;
        diff_rot.split.rotation = 270;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_rot, w, h), None);

        // Different preset -> Rejected (H2 and H3 share visual dims and
        // sub numbering but crop different regions).
        let mut diff_preset = settings;
        diff_preset.split.preset = crate::split::SplitPreset::Horizontal2;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_preset, w, h), None);

        // Different crop margin -> Rejected (the big-PDF tuning workflow).
        let mut diff_crop = settings;
        diff_crop.split.margin_top = 0.10;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_crop, w, h), None);

        // Different line spacing -> Rejected
        let mut diff_spacing = settings;
        diff_spacing.line_spacing = 1.4;
        assert_eq!(load_snapshot_from(dir, "my_book.epub", 5, 0, &diff_spacing, w, h), None);

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
            save_snapshot_to(dir, &book_name, i, 0, &settings, w, h, &pixels);
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
