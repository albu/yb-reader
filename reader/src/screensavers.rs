use crate::chrome::{
    DIM, DIVIDER, FOOTER_BASE_PT, INK, MUTED, PAD_PT, ROW_H_PT, TRIVIA_BASE_PT, TRIVIA_RULE_PT,
};
use std::collections::{HashMap, HashSet};

use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

// Section
const SEC_LABEL_PT: f32 = 52.0;
const ROWS_TOP_PT: f32 = 60.0;

const THUMB_W_PT: f32 = 21.0;
const THUMB_H_PT: f32 = 28.0;

/// The thumbnail size both the boot prewarm and the screen's rows request.
/// The disk cache encodes the exact size in the filename and header, so a
/// mismatch between the two call sites silently wastes the entire prewarm
/// pass — keep them on one source of truth (pinned by a test).
fn thumb_size() -> (i32, i32) {
    (pt(THUMB_W_PT), pt(THUMB_H_PT))
}

/// Same dirs, same order, as yui's picker — this screen manages what
/// that code draws. Defaults to device directories, plus optional YB_SCREENSAVER_DIR for dev/tests.
pub fn screensaver_dirs() -> Vec<String> {
    let mut dirs = vec![
        "/mnt/us/screensavers".to_string(),
        "/mnt/us/extensions/reader/screensavers".to_string(),
    ];
    if let Ok(dev) = std::env::var("YB_SCREENSAVER_DIR") {
        dirs.push(dev);
    }
    dirs
}

pub struct ScreensaversScreen {
    w: i32,
    h: i32,
    /// (file name, bytes) — display order.
    files: Vec<(String, u64)>,
    disabled: HashSet<String>,
    thumbnails: HashMap<String, ThumbState>,
    thumb_rx: Option<std::sync::mpsc::Receiver<(String, Option<Vec<u8>>)>>,
    thumb_tx: std::sync::mpsc::Sender<(String, Option<Vec<u8>>)>,
    offset: usize,
    per_page: usize,
}

/// Thumbnail state for one screensaver row: `Pending` while the
/// background worker decodes, `Ready` with the bytes, or `Failed` — a
/// terminal state, so a rejected image stops being polled and the screen
/// settles back to its slow tick instead of spinning at 150 ms forever.
enum ThumbState {
    Pending,
    Ready(Vec<u8>),
    Failed,
}

fn is_image(p: &std::path::Path) -> bool {
    // Dotfiles rejected: macOS drops `._name.jpg` AppleDouble sidecars
    // (4 kB Finder metadata) on every FAT-volume copy — same
    // discipline as the library scan and the yui picker.
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with('.') {
        return false;
    }
    matches!(
        p.extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref(),
        Some("png") | Some("jpg") | Some("jpeg")
    )
}

/// Find the absolute path of a screensaver file by scanning known directories.
fn find_image_path(name: &str) -> Option<std::path::PathBuf> {
    for d in screensaver_dirs() {
        let p = std::path::Path::new(&d).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// The union of all rotation dirs, sorted, deduped by file name.
pub fn scan() -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for d in screensaver_dirs() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if !is_image(&p) {
                continue;
            }
            let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if seen.insert(name.clone()) {
                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((name, sz));
            }
        }
    }
    out.sort();
    out
}

/// The disabled-list path, overridable for host tests (same pattern as
/// receive.rs's YB_SAVE_DIR). The device path matches yui's picker.
fn disabled_path() -> String {
    std::env::var("YB_SS_DISABLED_LIST")
        .unwrap_or_else(|_| yui::widgets::SS_DISABLED_LIST.to_string())
}

fn load_disabled() -> HashSet<String> {
    std::fs::read_to_string(disabled_path())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn save_disabled(disabled: &HashSet<String>) {
    let path = disabled_path();
    let mut names: Vec<&str> = disabled.iter().map(String::as_str).collect();
    names.sort();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, names.join("\n") + "\n");
}

impl ScreensaversScreen {
    pub fn new() -> ScreensaversScreen {
        let (thumb_tx, thumb_rx) = std::sync::mpsc::channel();
        ScreensaversScreen {
            w: 1236,
            h: 1648,
            files: scan(),
            disabled: load_disabled(),
            thumbnails: HashMap::new(),
            thumb_rx: Some(thumb_rx),
            thumb_tx,
            offset: 0,
            per_page: 7,
        }
    }

    /// Identity keys of every current image (enabled or not) — the
    /// thumbnail GC keep-set. Stat-only: opening the screen no longer
    /// reads every multi-MB source just to hash it.
    fn current_keys() -> Vec<u64> {
        let mut out = Vec::new();
        for d in screensaver_dirs() {
            let Ok(entries) = std::fs::read_dir(d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if is_image(&p) && p.is_file() {
                    if let Some(k) = ybdev::img::source_key(&p) {
                        out.push(k);
                    }
                }
            }
        }
        out
    }

    fn in_rotation(&self, name: &str) -> bool {
        !self.disabled.contains(name)
    }

    /// Pure row-index math, host-testable.
    fn row_at(&self, y: i32) -> Option<usize> {
        if y < pt(ROWS_TOP_PT) {
            return None;
        }
        let rel = (y - pt(ROWS_TOP_PT)) / pt(ROW_H_PT);
        if rel < 0 || rel as usize >= self.per_page {
            return None;
        }
        let idx = self.offset + rel as usize;
        if idx < self.files.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn draw_persistent_header(p: &mut Painter, w: i32, pad: i32) {
        crate::chrome::draw_settings_header(p, w, pad, "Screensavers");
    }

    fn get_thumbnail(&mut self, name: &str, tw: u32, th: u32) -> Option<&[u8]> {
        if !self.thumbnails.contains_key(name) {
            // 1. Fast non-blocking disk cache check: one stat + a ~10 KB
            // read — the source image itself is never read here.
            let mut found_cached = None;
            let mut src = None;
            if let Some(path) = find_image_path(name) {
                match ybdev::img::read_thumb_disk_cached_fast(&path, tw, th) {
                    Some(cached) => found_cached = Some(cached),
                    None => src = Some(path),
                }
            }

            if let Some(cached) = found_cached {
                self.thumbnails.insert(name.to_string(), ThumbState::Ready(cached));
            } else {
                // 2. Mark as in-flight and dispatch background decode
                // worker: it reads the source once, decodes, and writes
                // the identity-keyed cache entry.
                self.thumbnails.insert(name.to_string(), ThumbState::Pending);
                let tx = self.thumb_tx.clone();
                let name_cl = name.to_string();
                std::thread::Builder::new()
                    .name("ss-thumb".into())
                    .spawn(move || {
                        let thumb = src.and_then(|p| {
                            let bytes = std::fs::read(&p).ok()?;
                            ybdev::img::load_image_thumb_disk_cached(&p, &bytes, tw, th)
                        });
                        let _ = tx.send((name_cl, thumb));
                    })
                    .ok();
            }
        }

        match self.thumbnails.get(name) {
            Some(ThumbState::Ready(b)) => Some(b.as_slice()),
            _ => None,
        }
    }
}

impl Default for ScreensaversScreen {
    fn default() -> Self {
        ScreensaversScreen::new()
    }
}

/// Prewarm the disk cache at boot: render every current image once in
/// a background thread (serial, with a breath between files so a cold
/// boot's first minute never competes with the UI for CPU). After this
/// pass every sleep is a plain 2 MB read and every screensavers screen
/// open is an instant 0ms thumbnail read. The trailing GC drops
/// renders of images that were replaced or deleted since the last
/// boot — the cache is self-cleaning, nothing accumulates forever.
pub fn prewarm() {
    std::thread::Builder::new()
        .name("ss-prewarm".to_string())
        .spawn(|| {
            let mut hashes = Vec::new();
            let mut keys = Vec::new();
            for d in screensaver_dirs() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if !is_image(&p) || !p.is_file() {
                        continue;
                    }
                    if let Some(k) = ybdev::img::source_key(&p) {
                        keys.push(k);
                    }
                    if let Ok(bytes) = std::fs::read(&p) {
                        hashes.push(ybdev::img::fnv1a(&bytes));
                        let _ = ybdev::img::load_image_fitted_disk_cached(&bytes, 1236, 1648);
                        let (tw, th) = thumb_size();
                        let _ = ybdev::img::load_image_thumb_disk_cached(
                            &p,
                            &bytes,
                            tw as u32,
                            th as u32,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                }
            }
            ybdev::img::ss_cache_gc(&hashes);
            ybdev::img::ss_thumb_cache_gc(&keys);
        })
        .ok();
}

impl Screen for ScreensaversScreen {
    fn on_enter(&mut self) -> Action {
        // Opening the manager is the natural GC point for thumbnails:
        // replaced or deleted images must not leave orphaned render
        // files on the flash. Stat-only — full-size renders are
        // content-keyed and GC'd by prewarm, which has the bytes anyway.
        ybdev::img::ss_thumb_cache_gc(&Self::current_keys());
        self.files = scan();
        self.disabled = load_disabled();
        Action::Redraw
    }

    fn tick_interval(&self) -> std::time::Duration {
        let visible = self.files.len().min(self.offset + self.per_page);
        let pending = (self.offset..visible).any(|idx| {
            let (name, _) = &self.files[idx];
            matches!(self.thumbnails.get(name), None | Some(ThumbState::Pending))
        });
        if pending {
            std::time::Duration::from_millis(150)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    fn on_tick(&mut self) -> Action {
        let mut arrived = false;
        if let Some(ref rx) = self.thumb_rx {
            while let Ok((name, thumb)) = rx.try_recv() {
                self.thumbnails.insert(
                    name,
                    match thumb {
                        Some(b) => ThumbState::Ready(b),
                        None => ThumbState::Failed,
                    },
                );
                arrived = true;
            }
        }
        if arrived {
            Action::Redraw
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);

        // 1. Persistent Ambient Header
        Self::draw_persistent_header(p, w, pad);

        // 2. Summary Sub-header
        let rotating = self
            .files
            .iter()
            .filter(|(n, _)| self.in_rotation(n))
            .count();
        let total = self.files.len();
        let summary_str = if total == 0 {
            "No screensaver images installed".to_string()
        } else {
            format!("{rotating} of {total} in rotation")
        };
        p.text(pad, pt(TRIVIA_BASE_PT), 6.8, DIM, &summary_str);
        if total > 0 {
            p.text_right(w - pad, pt(TRIVIA_BASE_PT), 6.8, MUTED, "Tap to toggle rotation");
        }
        p.hline_t(pt(TRIVIA_RULE_PT), pad, w - pad, 1, 235);

        // 3. Section Label
        p.text(pad, pt(SEC_LABEL_PT), 6.8, MUTED, "LOCK SCREEN IMAGES");

        if self.files.is_empty() {
            let empty_top = pt(ROWS_TOP_PT) + pt(8.0);
            p.text(pad, empty_top + pt(14.0), 9.5, INK, "No screensaver images yet");
            p.text(
                pad,
                empty_top + pt(30.0),
                7.5,
                DIM,
                "Copy .png or .jpg files into the Kindle's screensavers/ folder",
            );
            p.text(
                pad,
                empty_top + pt(44.0),
                7.5,
                DIM,
                "via Wi-Fi (Receive page), USB storage, or scp.",
            );
            return;
        }

        let rows_top = pt(ROWS_TOP_PT);
        let row_h = pt(ROW_H_PT);
        self.per_page = 7;
        let visible = self.files.len().min(self.offset + self.per_page);

        let (tw, th) = thumb_size();
        let tx = pad + tw + pt(10.0);
        let budget = (p.width_pt() - 2.0 * PAD_PT - THUMB_W_PT - 70.0).max(10.0);

        for (i, idx) in (self.offset..visible).enumerate() {
            let top = rows_top + i as i32 * row_h;
            let (name, sz) = self.files[idx].clone();
            let in_rot = self.in_rotation(&name);

            // Thumbnail Preview
            let thumb_y = top + (row_h - th) / 2;
            let thumb_opt = self.get_thumbnail(&name, tw as u32, th as u32);
            if let Some(thumb) = thumb_opt {
                p.blit_gray(pad, thumb_y, tw, th, thumb, tw as usize);
                p.rect_outline_t(Rect::new(pad, thumb_y, tw, th), 1, DIVIDER);
            } else {
                // Placeholder image card
                p.rect(Rect::new(pad, thumb_y, tw, th), 248);
                p.rect_outline_t(Rect::new(pad, thumb_y, tw, th), 1, DIVIDER);
                p.circle_fill(pad + tw * 2 / 3, thumb_y + th / 3, pt(1.5), 180);
                p.line_w(pad + pt(3.0), thumb_y + th - pt(4.0), pad + tw / 2, thumb_y + th / 2, 1, 180);
                p.line_w(pad + tw / 2, thumb_y + th / 2, pad + tw - pt(3.0), thumb_y + th - pt(4.0), 1, 180);
            }

            // Title
            let label = p.truncate(10.5, &name, budget);
            p.text(tx, top + pt(15.5), 10.5, INK, &label);

            // Subtitle
            let size_str = if sz >= 1024 * 1024 {
                format!("{:.1} MB", sz as f64 / 1048576.0)
            } else {
                format!("{} KB", sz / 1024)
            };
            let sub = if in_rot {
                format!("{size_str} · In lock screen rotation")
            } else {
                format!("{size_str} · Excluded from rotation")
            };
            let sub_trunc = p.truncate(7.5, &sub, budget);
            p.text(tx, top + pt(28.0), 7.5, DIM, &sub_trunc);

            // Right Badge
            let cy = top + row_h / 2;
            if in_rot {
                crate::chrome::draw_badge(p, w - pad, cy, "ACTIVE", true);
            } else {
                crate::chrome::draw_badge(p, w - pad, cy, "OFF", false);
            }

            p.hline_t(top + row_h, pad, w - pad, 1, DIVIDER);
        }

        // Footer pagination
        let page_info = format!(
            "{}-{} of {}{}",
            self.offset + 1,
            visible,
            self.files.len(),
            if self.files.len() > self.per_page {
                " · Swipe horizontally to turn pages"
            } else {
                " · Swipe up bottom-right to exit"
            }
        );
        p.text_center(pt(FOOTER_BASE_PT), 7.5, MUTED, &page_info);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w, self.h);
        if g.corner_back() || g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }

        match g {
            Gesture::Tap { x: _, y } => {
                let y = y as i32;
                if let Some(idx) = self.row_at(y) {
                    let (name, _) = self.files[idx].clone();
                    if !self.disabled.remove(&name) {
                        self.disabled.insert(name);
                    }
                    save_disabled(&self.disabled);
                    return Action::Redraw;
                }
                Action::Keep
            }
            Gesture::Swipe { dir, .. } => {
                match dir {
                    SwipeDir::North | SwipeDir::South => Action::Pop,
                    SwipeDir::East | SwipeDir::West => {
                        // Page through long lists, library-style.
                        let max_off = self.files.len().saturating_sub(self.per_page);
                        self.offset = if dir == SwipeDir::East {
                            self.offset.saturating_sub(self.per_page)
                        } else {
                            (self.offset + self.per_page).min(max_off)
                        };
                        Action::Redraw
                    }
                }
            }
            Gesture::TwoFingerTap => Action::Pop,
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::save_preview_artifact;

    #[test]
    fn failed_thumbnail_settles_the_tick() {
        let mut s = ScreensaversScreen::new();
        s.files = vec![
            ("a.png".to_string(), 1),
            ("b.png".to_string(), 2),
            ("c.png".to_string(), 3),
        ];
        // Ready and Failed are terminal: no pending row, slow tick.
        s.thumbnails.insert("a.png".to_string(), ThumbState::Ready(vec![0u8; 4]));
        s.thumbnails.insert("b.png".to_string(), ThumbState::Failed);
        s.thumbnails.insert("c.png".to_string(), ThumbState::Ready(vec![0u8; 4]));
        assert_eq!(
            s.tick_interval(),
            std::time::Duration::from_secs(10),
            "a failed decode must not keep the 150 ms tick alive"
        );

        // A genuinely pending row keeps the fast tick.
        s.thumbnails.insert("c.png".to_string(), ThumbState::Pending);
        assert_eq!(s.tick_interval(), std::time::Duration::from_millis(150));
    }

    #[test]
    fn thumbnail_size_is_shared_and_pinned() {
        // prewarm() and the draw path both call thumb_size(), so they can
        // never drift; pin the concrete value so a layout change to
        // THUMB_*_PT that would silently orphan every prewarmed thumbnail
        // (the disk cache encodes the exact size in filename + header)
        // fails here instead.
        assert_eq!(thumb_size(), (87, 116));
    }

    #[test]
    fn row_mapping_covers_only_visible_rows() {
        let mut s = ScreensaversScreen::new();
        s.files = (0..30)
            .map(|i| (format!("img{:02}.png", i), 1024))
            .collect();
        s.disabled = HashSet::new();
        s.per_page = 7;
        s.offset = 7;
        // First visible row maps to absolute index 7.
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + 2), Some(7));
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + pt(ROW_H_PT) + 2), Some(8));
        // Past the page → None (no phantom rows).
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + 7 * pt(ROW_H_PT) + 2), None);
        // Above the list → None.
        assert_eq!(s.row_at(pt(30.0)), None);
    }

    #[test]
    fn toggle_persist_round_trip() {
        // Host test: the path override keeps writes off the device path.
        let tmp = std::env::temp_dir().join(format!("yb_ss_{}.txt", std::process::id()));
        let tmp = tmp.to_str().unwrap();
        let _ = std::fs::remove_file(tmp);
        std::env::set_var("YB_SS_DISABLED_LIST", tmp);

        let mut s = ScreensaversScreen::new();
        s.disabled = HashSet::from(["gone.png".to_string()]);
        save_disabled(&s.disabled);
        assert!(load_disabled().contains("gone.png"));
        // Toggle off-and-on leaves an empty (but valid) list.
        s.disabled.clear();
        save_disabled(&s.disabled);
        assert!(load_disabled().is_empty());

        std::env::remove_var("YB_SS_DISABLED_LIST");
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn screensavers_screen_renders_preview() {
        let font = yui::font::Font::load().unwrap();
        let dir = std::env::temp_dir().join(format!("yb_ss_preview_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("YB_SCREENSAVER_DIR", dir.to_str().unwrap());

        // Create a real small PNG to test live thumbnail decoding
        let img_path = dir.join("mountain_lake_sunset_highres.png");
        if let Ok(file) = std::fs::File::create(&img_path) {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 120, 160);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut w) = enc.write_header() {
                let mut data = vec![240u8; 120 * 160];
                for y in 40..120 {
                    for x in 30..90 {
                        data[y * 120 + x] = 60;
                    }
                }
                let _ = w.write_image_data(&data);
            }
        }

        let (mut canvas, mut panel) = (vec![255u8; 1236 * 1648], vec![255u8; 1248 * 1648]);
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );

        let mut s = ScreensaversScreen::new();
        s.files = vec![
            ("mountain_lake_sunset_highres.png".to_string(), 1450230),
            ("forest_path_morning_fog.jpg".to_string(), 850120),
            ("minimal_geometric_abstract.png".to_string(), 320400),
            ("classic_kindle_woodcut_engraving.jpg".to_string(), 1920100),
        ];
        s.disabled.insert("minimal_geometric_abstract.png".to_string());

        s.draw(&mut p);
        crate::testutil::save_preview_artifact("screensavers_preview.png", &canvas);

        let lit = canvas.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1236 * 1648 * 88 / 100);

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("YB_SCREENSAVER_DIR");
    }
}
