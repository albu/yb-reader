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
                    let mut split = SplitConfig {
                        preset: str_to_preset(preset_str),
                        rotation: rot,
                        overlap: if ov > 0.035 { 0.018 } else { ov },
                        margin_left: ml,
                        margin_top: mt,
                        margin_right: mr,
                        margin_bottom: mb,
                        mirror_even_odd: false,
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
                    let mirror_even_odd = it.next().map(|s| s == "1").unwrap_or(false);
                    split.mirror_even_odd = mirror_even_odd;
                    // Typography fields, appended after mirror_even_odd
                    // (same backward-compatible pattern): old lines without
                    // them keep the book's current defaults.
                    let paragraph_spacing = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .filter(|v| (0.0..=1.0).contains(v))
                        .unwrap_or(0.25);
                    let indent_em = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .filter(|v| (0.0..=3.0).contains(v))
                        .unwrap_or(1.2);
                    let hyphenate = it.next().map(|s| s != "0").unwrap_or(true);
                    let body_align = match it.next() {
                        Some("l") => yread::model::TextAlign::Left,
                        _ => yread::model::TextAlign::Justify,
                    };
                    let word_spacing_mult = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .filter(|v| (0.5..=2.0).contains(v))
                        .unwrap_or(1.0);
                    let letter_spacing_px = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .filter(|v| (0.0..=5.0).contains(v))
                        .unwrap_or(0.0);
                    let font_family = match it.next() {
                        Some("ptserif") => yread::font::FontFamily::PtSerif,
                        Some("bitter") => yread::font::FontFamily::Bitter,
                        Some("ptsans") => yread::font::FontFamily::PtSans,
                        _ => yread::font::FontFamily::Literata,
                    };

                    settings = Some(ReaderSettings {
                        split,
                        font_size,
                        margin_pad,
                        line_spacing,
                        paragraph_spacing,
                        indent_em,
                        hyphenate,
                        body_align,
                        word_spacing_mult,
                        letter_spacing_px,
                        font_family,
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
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{:.1}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{:.2}\t{:.2}\t{}\t{}\t{:.3}\t{:.2}\t{}",
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
                    if sc.mirror_even_odd { "1" } else { "0" },
                    s.paragraph_spacing,
                    s.indent_em,
                    if s.hyphenate { "1" } else { "0" },
                    match s.body_align {
                        yread::model::TextAlign::Left => "l",
                        _ => "j",
                    },
                    s.word_spacing_mult,
                    s.letter_spacing_px,
                    match s.font_family {
                        yread::font::FontFamily::Literata => "literata",
                        yread::font::FontFamily::PtSerif => "ptserif",
                        yread::font::FontFamily::Bitter => "bitter",
                        yread::font::FontFamily::PtSans => "ptsans",
                    },
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

    // Guard 1: Never overwrite a valid reading position (page > 0 or sub_idx > 0)
    // with an uninitialized dummy placeholder (page 0, sub 0, total <= 1) during
    // async book startup or quick exit.
    if pos.page == 0 && pos.sub_idx == 0 && pos.total <= 1 {
        if let Some(stored) = map.get(name) {
            if stored.page > 0 || stored.sub_idx > 0 {
                return;
            }
        }
    }

    // Guard 2: The yread backend reports total=1 until the landing chapter is
    // paginated, and dialogs (settings sheet, TOC, scrubber, curtain,
    // footnotes) capture that placeholder at open time — so a settings
    // change or jump inside that window would clobber the real total with
    // "page X of 1" (the hero card then shows the wrong count after a
    // restart). A real book never shrinks to 1 page, so a stored total
    // larger than 1 always wins over an incoming 1.
    let pos = if pos.total <= 1 {
        let stored = map.get(name).map(|p| p.total).unwrap_or(0);
        if stored > 1 {
            Pos { total: stored, ..pos }
        } else {
            pos
        }
    } else {
        pos
    };
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

        let path = std::env::temp_dir().join("yb-positions-roundtrip-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        save_at(p, &map);
        assert_eq!(load_at(p), map);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn settings_roundtrip() {
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(SplitPreset::Horizontal2);
        settings.split.mirror_even_odd = true;
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
        let path = std::env::temp_dir().join("yb-positions-settings-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        save_at(p, &map);
        let loaded = load_at(p);
        assert_eq!(loaded["paper.pdf"].page, 5);
        assert_eq!(loaded["paper.pdf"].sub_idx, 1);
        let s = loaded["paper.pdf"].settings.unwrap();
        assert_eq!(s.split.preset, SplitPreset::Horizontal2);
        assert!(s.split.mirror_even_odd);
        assert_eq!(s.font_size, 13.5);
        assert_eq!(s.contrast, ContrastMode::BoldText);
        assert!(s.invert);
        // margin_pad survives the store (it was silently reset to 72).
        assert_eq!(s.margin_pad, 108);
        let _ = std::fs::remove_file(p);
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
        let path = std::env::temp_dir().join("yb-positions-yread-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        save_at(p, &map);
        let loaded = load_at(p);
        assert_eq!(loaded["book.epub"].page, 7);
        assert_eq!(loaded["book.epub"].sub_idx, 2);
        let s = loaded["book.epub"].settings.unwrap();
        assert!((s.line_spacing - 1.3).abs() < 0.01);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn record_skips_identical_entries_but_writes_changes() {
        let path = std::env::temp_dir().join("yb-positions-dedupe-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        let pos = Pos::simple(3, 30, 1000);
        record_at(p, "a.epub", pos);
        // Tamper externally with an UNPARSEABLE line: if the second,
        // identical record is a no-op the content survives; if it wrote,
        // the reload would drop the bad line and the rewrite would
        // remove it.
        std::fs::write(p, "a.epub\t3\t30\t1000\nsentinel\tx\t1\t1\n").unwrap();
        record_at(p, "a.epub", pos);
        assert!(
            std::fs::read_to_string(p).unwrap().contains("sentinel"),
            "identical record must not rewrite the store"
        );
        // A real change still writes (and the tampered line is gone).
        record_at(p, "a.epub", Pos::simple(4, 30, 1001));
        let after = std::fs::read_to_string(p).unwrap();
        assert!(!after.contains("sentinel"));
        assert!(after.contains("a.epub\t4\t30\t1001"));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn prune_drops_dead_books_but_refuses_empty_live_sets() {
        let mut map = HashMap::new();
        map.insert("keep.epub".to_string(), Pos::simple(1, 10, 1));
        map.insert("gone.epub".to_string(), Pos::simple(2, 10, 2));
        let path = std::env::temp_dir().join("yb-positions-prune-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        save_at(p, &map);

        prune_at(p, &["keep.epub".to_string()]);
        let after = load_at(p);
        assert_eq!(after.len(), 1);
        assert!(after.contains_key("keep.epub"));

        // An empty live set is a failed scan (unreadable documents/),
        // not an empty library: prune must leave the store alone.
        prune_at(p, &[]);
        assert_eq!(load_at(p).len(), 1);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn typography_fields_roundtrip_and_defaults() {
        // A line from before the typography fields existed: all four fall
        // back to the reader defaults.
        let old = "a.epub\t3\t9\t1700000000\t1\th2\t270\t0.1000\t0.1000\t0.1000\t0.1000\t0.1000\t12.0\tnorm\t0\t0\t72\t1.0\t0\n";
        let s = parse(old)["a.epub"].settings.unwrap();
        assert_eq!(s.paragraph_spacing, 0.25);
        assert_eq!(s.indent_em, 1.2);
        assert!(s.hyphenate);
        assert_eq!(s.body_align, yread::model::TextAlign::Justify);
        assert_eq!(s.font_family, yread::font::FontFamily::Literata);

        // And a full line round-trips through the store.
        let mut settings = ReaderSettings::default();
        settings.paragraph_spacing = 0.6;
        settings.indent_em = 0.0;
        settings.hyphenate = false;
        settings.body_align = yread::model::TextAlign::Left;
        settings.font_family = yread::font::FontFamily::Bitter;
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
        let path = std::env::temp_dir().join("yb-positions-typography-test.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        save_at(p, &map);
        let loaded = load_at(p);
        let s = loaded["paper.pdf"].settings.unwrap();
        assert_eq!(s.paragraph_spacing, 0.6);
        assert_eq!(s.indent_em, 0.0);
        assert!(!s.hyphenate);
        assert_eq!(s.body_align, yread::model::TextAlign::Left);
        assert_eq!(s.font_family, yread::font::FontFamily::Bitter);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn placeholder_total_never_clobbers_a_real_total() {
        // The yread backend reports total=1 until pagination; a dialog
        // opened in that window records "page X of 1" over the real
        // total. The write path must keep the larger stored total.
        let path = std::env::temp_dir().join("yb-positions-total-guard.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);

        record_at(p, "book.epub", Pos::simple(10, 200, 1000));
        // A settings change captured before pagination: total=1.
        record_at(p, "book.epub", Pos::simple(12, 1, 1001));
        assert_eq!(
            load_at(p)["book.epub"].total,
            200,
            "placeholder 1 must not downgrade the real 200"
        );
        // But the page/settings still updated (only the total was kept).
        assert_eq!(load_at(p)["book.epub"].page, 12);

        // A genuinely one-page book stays 1.
        record_at(p, "short.txt", Pos::simple(0, 1, 2000));
        assert_eq!(load_at(p)["short.txt"].total, 1);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn dummy_startup_pos_never_clobbers_real_reading_position() {
        let path = std::env::temp_dir().join("yb-positions-startup-guard.txt");
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);

        // Book was on page 440 (with chapter/char sub_idx)
        record_at(
            p,
            "dune.epub",
            Pos {
                page: 440,
                total: 800,
                ts: 1000,
                sub_idx: 15_000_250,
                settings: None,
            },
        );

        // An async startup poll or early save with dummy (0, 0, 1) fires
        record_at(
            p,
            "dune.epub",
            Pos {
                page: 0,
                total: 1,
                ts: 1001,
                sub_idx: 0,
                settings: None,
            },
        );

        let saved = load_at(p)["dune.epub"];
        assert_eq!(saved.page, 440, "dummy startup position must not clobber real page");
        assert_eq!(saved.sub_idx, 15_000_250, "dummy sub_idx must not clobber real sub_idx");
        assert_eq!(saved.total, 800, "dummy total must not clobber real total");

        let _ = std::fs::remove_file(p);
    }
}
