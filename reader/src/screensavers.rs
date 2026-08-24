//! Screensaver rotation manager (System → Screensavers). Lists every
//! image the sleep screen's picker can draw (yui::widgets owns the
//! rotation) with a checkbox per row: deselected images leave the
//! rotation immediately — the picker rereads the list on every sleep.
//! Images get onto the device the boring ways: USB transfer mode (the
//! mounted drive's `screensavers/` folder — the picker's first-choice
//! dir), the web manager, or scp.

use std::collections::HashSet;

use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const PAD_PT: f32 = 18.0;
const TITLE_BASE_PT: f32 = 30.0;
const TITLE_SIZE_PT: f32 = 13.0;
const ROWS_TOP_PT: f32 = 66.0;
const ROW_H_PT: f32 = 26.0;
const NAME_PT: f32 = 9.0;
const FOOT_PT: f32 = 7.0;

const DIM: u8 = 110;
const INK: u8 = 0;

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
    offset: usize,
    per_page: usize,
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
        ScreensaversScreen {
            w: 1236,
            h: 1648,
            files: scan(),
            disabled: load_disabled(),
            offset: 0,
            per_page: 1,
        }
    }

    /// Content hashes of every current image (enabled or not) — the
    /// disk cache GC keep-set.
    fn current_hashes() -> Vec<u64> {
        let mut out = Vec::new();
        for d in screensaver_dirs() {
            let Ok(entries) = std::fs::read_dir(d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if is_image(&p) && p.is_file() {
                    if let Ok(bytes) = std::fs::read(&p) {
                        out.push(ybdev::img::fnv1a(&bytes));
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

    fn draw_checkbox(p: &mut Painter, x: i32, cy: i32, on: bool) {
        let s = pt(6.5);
        let r = Rect::new(x, cy - s / 2, s, s);
        p.rect_outline_t(r, 1, INK);
        if on {
            let i = pt(1.6);
            p.rect(Rect::new(r.x + i, r.y + i, r.w - 2 * i, r.h - 2 * i), INK);
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
/// pass every sleep is a plain 2 MB read. The trailing GC drops
/// renders of images that were replaced or deleted since the last
/// boot — the cache is self-cleaning, nothing accumulates forever.
pub fn prewarm() {
    std::thread::Builder::new()
        .name("ss-prewarm".to_string())
        .spawn(|| {
            let mut hashes = Vec::new();
            for d in screensaver_dirs() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if !is_image(&p) || !p.is_file() {
                        continue;
                    }
                    if let Ok(bytes) = std::fs::read(&p) {
                        hashes.push(ybdev::img::fnv1a(&bytes));
                        let _ = ybdev::img::load_image_fitted_disk_cached(&bytes, 1236, 1648);
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                }
            }
            ybdev::img::ss_cache_gc(&hashes);
        })
        .ok();
}

impl Screen for ScreensaversScreen {
    fn on_enter(&mut self) -> Action {
        // Opening the manager is also the natural GC point: replaced or
        // deleted images must not leave orphaned render files behind.
        ybdev::img::ss_cache_gc(&Self::current_hashes());
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.w = w;
        self.h = h;
        p.clear(255);

        let pad = pt(PAD_PT);

        p.text(pad, pt(TITLE_BASE_PT), TITLE_SIZE_PT, INK, "Screensavers");
        let rotating = self
            .files
            .iter()
            .filter(|(n, _)| self.in_rotation(n))
            .count();
        p.text_right(
            w - pad,
            pt(TITLE_BASE_PT),
            7.0,
            DIM,
            &format!("{} of {} in rotation", rotating, self.files.len()),
        );
        p.hline_t(pt(TITLE_BASE_PT) + pt(9.0), pad, w - pad, 2, 180);

        if self.files.is_empty() {
            p.text_center(h / 2 - pt(4.0), 10.0, INK, "No screensaver images yet");
            p.text_center(
                h / 2 + pt(14.0),
                8.0,
                DIM,
                "copy .png / .jpg into the Kindle's screensavers/ folder",
            );
            p.text_center(
                h / 2 + pt(28.0),
                8.0,
                DIM,
                "(USB transfer mode, the web manager, or scp)",
            );
            return;
        }

        let rows_top = pt(ROWS_TOP_PT);
        self.per_page = (((h - rows_top - pt(24.0)) / pt(ROW_H_PT)) as usize).max(1);
        let visible = self.files.len().min(self.offset + self.per_page);
        for (i, idx) in (self.offset..visible).enumerate() {
            let top = rows_top + i as i32 * pt(ROW_H_PT);
            let (name, sz) = &self.files[idx];
            let cy = top + pt(ROW_H_PT) / 2;

            Self::draw_checkbox(p, pad, cy, self.in_rotation(name));

            let budget = p.width_pt() - 2.0 * PAD_PT - 14.0 - 12.0;
            let label = p.truncate(NAME_PT, name, budget);
            p.text(pad + pt(14.0), top + pt(17.0), NAME_PT, INK, &label);

            let size_str = if *sz >= 1024 * 1024 {
                format!("{:.1} MB", *sz as f64 / 1048576.0)
            } else {
                format!("{} KB", sz / 1024)
            };
            p.text_right(w - pad, top + pt(17.0), 7.0, DIM, &size_str);

            p.hline_t(top + pt(ROW_H_PT), pad, w - pad, 1, 225);
        }

        p.text_center(
            h - pt(10.0),
            FOOT_PT,
            DIM,
            &format!(
                "{}-{} of {} · tap: toggle rotation{}",
                self.offset + 1,
                visible,
                self.files.len(),
                if self.files.len() > self.per_page {
                    " · swipe: page"
                } else {
                    ""
                },
            ),
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
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

    #[test]
    fn row_mapping_covers_only_visible_rows() {
        let mut s = ScreensaversScreen::new();
        s.files = (0..30)
            .map(|i| (format!("img{:02}.png", i), 1024))
            .collect();
        s.disabled = HashSet::new();
        s.per_page = 10;
        s.offset = 10;
        // First visible row maps to absolute index 10.
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + 2), Some(10));
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + pt(ROW_H_PT) + 2), Some(11));
        // Past the page → None (no phantom rows).
        assert_eq!(s.row_at(pt(ROWS_TOP_PT) + 10 * pt(ROW_H_PT) + 2), None);
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
}
