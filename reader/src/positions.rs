//! Per-book reading positions and reader settings, persisted on /mnt/us.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::split::{ContrastMode, ReaderSettings, SplitConfig, SplitPreset};

fn store_path() -> &'static str {
    if std::path::Path::new("/mnt/us/extensions/reader").exists() {
        "/mnt/us/extensions/reader/positions.txt"
    } else {
        "/tmp/positions.txt"
    }
}

fn global_store_path() -> &'static str {
    if std::path::Path::new("/mnt/us/extensions/reader").exists() {
        "/mnt/us/extensions/reader/global.txt"
    } else {
        "/tmp/global.txt"
    }
}

pub fn global_refresh_interval() -> usize {
    if let Ok(s) = std::fs::read_to_string(global_store_path()) {
        if let Ok(v) = s.trim().parse::<usize>() {
            return v;
        }
    }
    10 // Default: refresh every 10 pages
}

pub fn set_global_refresh_interval(val: usize) {
    // Atomic + fsync'd: the other stores route through ybdev::atomic; a
    // plain in-place write could leave a torn value after a crash.
    if !ybdev::atomic::write(global_store_path(), format!("{}\n", val).as_bytes()) {
        ybdev::log::plog("positions: failed to save refresh interval");
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]

pub struct Pos {
    pub page: usize,
    pub total: usize,
    pub ts: u64,
    pub sub_idx: usize,
    pub settings: Option<ReaderSettings>,
}

impl Pos {
    pub fn simple(page: usize, total: usize, ts: u64) -> Self {
        Self {
            page,
            total,
            ts,
            sub_idx: 0,
            settings: None,
        }
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn preset_to_str(p: SplitPreset) -> &'static str {
    match p {
        SplitPreset::FitPage => "fit",
        SplitPreset::Horizontal2 => "h2",
        SplitPreset::Horizontal3 => "h3",
        SplitPreset::Vertical2 => "v2",
        SplitPreset::Grid4 => "g4",
    }
}

fn str_to_preset(s: &str) -> SplitPreset {
    match s {
        "h2" => SplitPreset::Horizontal2,
        "h3" => SplitPreset::Horizontal3,
        "v2" => SplitPreset::Vertical2,
        "g4" => SplitPreset::Grid4,
        _ => SplitPreset::FitPage,
    }
}

fn contrast_to_str(c: ContrastMode) -> &'static str {
    match c {
        ContrastMode::Normal => "norm",
        ContrastMode::BoldText => "bold",
        ContrastMode::HighContrast => "high",
        ContrastMode::ScanClean => "scan",
    }
}

fn str_to_contrast(s: &str) -> ContrastMode {
    match s {
        "bold" => ContrastMode::BoldText,
        "high" => ContrastMode::HighContrast,
        "scan" => ContrastMode::ScanClean,
        _ => ContrastMode::Normal,
    }
}

/// Parse the store text. Malformed lines are skipped, not fatal.
fn parse(text: &str) -> HashMap<String, Pos> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(name), Some(page_str), Some(total_str), Some(ts_str)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let (Ok(page), Ok(total), Ok(ts)) = (page_str.parse(), total_str.parse(), ts_str.parse())
        else {
            continue;
        };

        let mut sub_idx = 0;
        let mut settings = None;

        if let Some(sub_str) = it.next() {
            if let Ok(sub) = sub_str.parse() {
                sub_idx = sub;
            }
            if let (
                Some(preset_str),
                Some(rot_str),
                Some(ov_str),
                Some(ml_str),
                Some(mt_str),
                Some(mr_str),
                Some(mb_str),
            ) = (
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
            ) {
                if let (Ok(rot), Ok(ov), Ok(ml), Ok(mt), Ok(mr), Ok(mb)) = (
                    rot_str.parse(),
                    ov_str.parse(),
                    ml_str.parse(),
                    mt_str.parse(),
                    mr_str.parse(),
                    mb_str.parse(),
                ) {
                    let split = SplitConfig {
                        preset: str_to_preset(preset_str),
                        rotation: rot,
                        overlap: if ov > 0.035 { 0.018 } else { ov },
                        margin_left: ml,
                        margin_top: mt,
                        margin_right: mr,
                        margin_bottom: mb,
                    };

                    let font_size = it.next().and_then(|s| s.parse().ok()).unwrap_or(11.0);
                    let contrast = it.next().map(str_to_contrast).unwrap_or_default();
                    let white_cut = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let invert = it.next().map(|s| s == "1").unwrap_or(false);
                    // Appended last so pre-margin-field lines (and out-of-range
                    // garbage) fall back to the old hardcoded default.
                    let margin_pad = it
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|m| (16..=216).contains(m))
                        .unwrap_or(72);
                    // Appended after margin_pad (same backward-compatible
                    // pattern): old lines without it keep the book's own
                    // leading.
                    let line_spacing = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .filter(|v| (0.8..=1.8).contains(v))
                        .unwrap_or(1.0);

                    settings = Some(ReaderSettings {
                        split,
                        font_size,
                        margin_pad,
                        line_spacing,
                        contrast,
                        white_cutoff: white_cut,
                        invert,
                        show_header: true,
                    });
                }
            }
        }

        map.insert(
            name.to_string(),
            Pos {
                page,
                total,
                ts,
                sub_idx,
                settings,
            },
        );
    }
    map
}

fn load_at(path: &str) -> HashMap<String, Pos> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

fn save_at(path: &str, map: &HashMap<String, Pos>) {
    let mut lines: Vec<String> = map
        .iter()
        .map(|(k, p)| {
            if let Some(s) = p.settings {
                let sc = s.split;
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.1}\t{}\t{}\t{}\t{}\t{:.1}",
                    k,
                    p.page,
                    p.total,
                    p.ts,
                    p.sub_idx,
                    preset_to_str(sc.preset),
                    sc.rotation,
                    sc.overlap,
                    sc.margin_left,
                    sc.margin_top,
                    sc.margin_right,
                    sc.margin_bottom,
                    s.font_size,
                    contrast_to_str(s.contrast),
                    s.white_cutoff,
                    if s.invert { "1" } else { "0" },
                    s.margin_pad,
                    s.line_spacing,
                )
            } else if p.sub_idx > 0 {
                format!("{}\t{}\t{}\t{}\t{}", k, p.page, p.total, p.ts, p.sub_idx)
            } else {
                format!("{}\t{}\t{}\t{}", k, p.page, p.total, p.ts)
            }
        })
        .collect();
    lines.sort();
    // Atomic + fsync'd swap — contract and rationale in ybdev::atomic.
    if !ybdev::atomic::write(path, (lines.join("\n") + "\n").as_bytes()) {
        ybdev::log::plog(&format!("positions: failed to save progress store {}", path));
    }
}

/// Saved pos for a book (page 0, sub 0 when never opened).
pub fn resume_pos(name: &str) -> Pos {
    load_at(store_path())
        .get(name)
        .copied()
        .unwrap_or(Pos::simple(0, 0, 0))
}

/// The whole store in one read — for library-level ordering (last-read
/// first) without an O(books) pile of single-entry loads.
pub fn all() -> HashMap<String, Pos> {
    load_at(store_path())
}

/// Saved page for a book (0 when never opened).
pub fn resume_page(name: &str) -> usize {
    resume_pos(name).page
}

/// Record into a store, skipping the write when the entry is already
/// identical — save_progress fires in bursts (open, turn, leave, busy
/// ticks) and a same-second duplicate must not become a flash write.
fn record_at(path: &str, name: &str, pos: Pos) {
    let mut map = load_at(path);
    if map.get(name) == Some(&pos) {
        return;
    }
    map.insert(name.to_string(), pos);
    save_at(path, &map);
}

/// Record progress with settings; stamps the entry now.
pub fn record_pos(
    name: &str,
    page: usize,
    total: usize,
    sub_idx: usize,
    settings: Option<ReaderSettings>,
) {
    record_at(
        store_path(),
        name,
        Pos {
            page,
            total,
            ts: now_ts(),
            sub_idx,
            settings,
        },
    );
}

/// Simple record (for books without custom settings).
#[allow(dead_code)]
pub fn record(name: &str, page: usize, total: usize) {
    let mut map = load_at(store_path());
    let prev_settings = map.get(name).and_then(|p| p.settings);
    let prev_sub = map.get(name).map(|p| p.sub_idx).unwrap_or(0);
    map.insert(
        name.to_string(),
        Pos {
            page,
            total,
            ts: now_ts(),
            sub_idx: prev_sub,
            settings: prev_settings,
        },
    );
    save_at(store_path(), &map);
}

/// The most recently opened book, if any.
pub fn last_read() -> Option<(String, Pos)> {
    load_at(store_path()).into_iter().max_by_key(|(_, p)| p.ts)
}

/// Drop entries whose files are gone — the library scan owns this;
/// positions never self-clean otherwise. Never prunes against an empty
/// live set: a documents/ directory that failed to read is a failed
/// scan, not an empty library, and must not wipe the store.
fn prune_at(path: &str, live: &[String]) {
    if live.is_empty() {
        return;
    }
    // Set, not slice scan: this runs per home-screen scan against every
    // stored entry — O(n·m) was measurable with a few hundred books.
    let live_set: std::collections::HashSet<&String> = live.iter().collect();
    let mut map = load_at(path);
    let before = map.len();
    map.retain(|name, _| live_set.contains(name));
    if map.len() != before {
        save_at(path, &map);
    }
}

/// Prune the real store against the currently listed books.
pub fn prune(live: &[String]) {
    prune_at(store_path(), live);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        let text = "a.epub\t12\t340\t1700000000\nb.epub\t0\t0\t1700000005\n";
        let map = parse(text);
        assert_eq!(map["a.epub"], Pos::simple(12, 340, 1700000000));
        assert_eq!(map.len(), 2);

        let path = "/tmp/yb-positions-test.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);
        assert_eq!(load_at(path), map);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_roundtrip() {
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        settings.font_size = 13.5;
        settings.contrast = ContrastMode::BoldText;
        settings.invert = true;
        settings.margin_pad = 108;

        let mut map = HashMap::new();
        map.insert(
            "paper.pdf".to_string(),
            Pos {
                page: 5,
                total: 20,
                ts: 1700000000,
                sub_idx: 1,
                settings: Some(settings),
            },
        );
        let path = "/tmp/yb-positions-settings-test.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);
        let loaded = load_at(path);
        assert_eq!(loaded["paper.pdf"].page, 5);
        assert_eq!(loaded["paper.pdf"].sub_idx, 1);
        let s = loaded["paper.pdf"].settings.unwrap();
        assert_eq!(s.split.preset, SplitPreset::Horizontal2);
        assert_eq!(s.font_size, 13.5);
        assert_eq!(s.contrast, ContrastMode::BoldText);
        assert!(s.invert);
        // margin_pad survives the store (it was silently reset to 72).
        assert_eq!(s.margin_pad, 108);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn margin_field_defaults_on_old_lines_and_garbage() {
        // 16-field line from before margin_pad existed: falls back to 72.
        let old = "b.pdf\t3\t9\t1700000000\t1\th2\t270\t0.1000\t0.1000\t0.1000\t0.1000\t0.1000\t12.0\tnorm\t0\t0\n";
        let map = parse(old);
        assert_eq!(map["b.pdf"].settings.unwrap().margin_pad, 72);
        // Same line with a garbage 17th field: still 72, not a parse failure.
        let bad = "c.pdf\t3\t9\t1700000000\t1\th2\t270\t0.1000\t0.1000\t0.1000\t0.1000\t0.1000\t12.0\tnorm\t0\t0\txx\n";
        let map = parse(bad);
        assert_eq!(map["c.pdf"].settings.unwrap().margin_pad, 72);
        // And a real value round-trips through parse.
        let good = "d.pdf\t3\t9\t1700000000\t1\th2\t270\t0.1000\t0.1000\t0.1000\t0.1000\t0.1000\t12.0\tnorm\t0\t0\t36\n";
        let map = parse(good);
        assert_eq!(map["d.pdf"].settings.unwrap().margin_pad, 36);
    }

    #[test]
    fn test_yread_engine_settings_roundtrip() {
        let mut settings = ReaderSettings::default();
        settings.line_spacing = 1.3;

        let mut map = HashMap::new();
        map.insert(
            "book.epub".to_string(),
            Pos {
                page: 7,
                total: 24,
                ts: 1700000000,
                sub_idx: 2,
                settings: Some(settings),
            },
        );
        let path = "/tmp/yb-positions-yread-test.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);
        let loaded = load_at(path);
        assert_eq!(loaded["book.epub"].page, 7);
        assert_eq!(loaded["book.epub"].sub_idx, 2);
        let s = loaded["book.epub"].settings.unwrap();
        assert!((s.line_spacing - 1.3).abs() < 0.01);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn record_skips_identical_entries_but_writes_changes() {
        let path = "/tmp/yb-positions-dedupe-test.txt";
        let _ = std::fs::remove_file(path);
        let pos = Pos::simple(3, 30, 1000);
        record_at(path, "a.epub", pos);
        // Tamper externally with an UNPARSEABLE line: if the second,
        // identical record is a no-op the content survives; if it wrote,
        // the reload would drop the bad line and the rewrite would
        // remove it.
        std::fs::write(path, "a.epub\t3\t30\t1000\nsentinel\tx\t1\t1\n").unwrap();
        record_at(path, "a.epub", pos);
        assert!(
            std::fs::read_to_string(path).unwrap().contains("sentinel"),
            "identical record must not rewrite the store"
        );
        // A real change still writes (and the tampered line is gone).
        record_at(path, "a.epub", Pos::simple(4, 30, 1001));
        let after = std::fs::read_to_string(path).unwrap();
        assert!(!after.contains("sentinel"));
        assert!(after.contains("a.epub\t4\t30\t1001"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prune_drops_dead_books_but_refuses_empty_live_sets() {
        let mut map = HashMap::new();
        map.insert("keep.epub".to_string(), Pos::simple(1, 10, 1));
        map.insert("gone.epub".to_string(), Pos::simple(2, 10, 2));
        let path = "/tmp/yb-positions-prune-test.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);

        prune_at(path, &["keep.epub".to_string()]);
        let after = load_at(path);
        assert_eq!(after.len(), 1);
        assert!(after.contains_key("keep.epub"));

        // An empty live set is a failed scan (unreadable documents/),
        // not an empty library: prune must leave the store alone.
        prune_at(path, &[]);
        assert_eq!(load_at(path).len(), 1);
        let _ = std::fs::remove_file(path);
    }
}
