//! The generic screens every yb-* app needs: a launcher menu and a
//! tap-to-dismiss message overlay. Geometry is ported verbatim from the
//! ad-hoc ui.rs screens (pt-authored).

use std::sync::atomic::{AtomicI32, Ordering};

use ybdev::input::{Gesture, SwipeDir};

use crate::painter::{pt, Painter};
use crate::screen::{Action, Screen};

/// --- MenuScreen layout (pt): ported from ui.rs::menu_loop ---
const TITLE_BASE_PT: f32 = 32.0;
const TITLE_SIZE_PT: f32 = 15.0;
const ITEM_SIZE_PT: f32 = 11.5;
const ITEM_BASE_OFF_PT: f32 = 8.0;
const TOP_PT: f32 = 54.0;
const ITEM_H_PT: f32 = 26.0;
const FOOTER_BASE_OFF_PT: f32 = 30.0;
const FOOTER_SIZE_PT: f32 = 7.0;

pub struct MenuScreen {
    title: String,
    items: Vec<String>,
    on_select: Box<dyn FnMut(usize) -> Action>,
}

impl MenuScreen {
    /// `on_select` maps the tapped item index to the next action.
    pub fn new(
        title: &str,
        items: &[&str],
        on_select: impl FnMut(usize) -> Action + 'static,
    ) -> MenuScreen {
        MenuScreen {
            title: title.to_string(),
            items: items.iter().map(|s| s.to_string()).collect(),
            on_select: Box::new(on_select),
        }
    }

    /// Pure hit-test (px in, item index out) — shared by draw and tests.
    pub fn hit(y_px: i32, n_items: usize) -> Option<usize> {
        let top = pt(TOP_PT);
        let item_h = pt(ITEM_H_PT);
        if y_px < top || y_px >= top + n_items as i32 * item_h {
            return None;
        }
        let idx = ((y_px - top) / item_h) as usize;
        (idx < n_items).then_some(idx)
    }
}

impl Screen for MenuScreen {
    fn draw(&mut self, p: &mut Painter) {
        let h = p.size().1;
        p.clear(255);
        p.text_center(pt(TITLE_BASE_PT), TITLE_SIZE_PT, 0, &self.title);
        for (i, item) in self.items.iter().enumerate() {
            let y = pt(TOP_PT) + i as i32 * pt(ITEM_H_PT) + pt(ITEM_BASE_OFF_PT);
            p.text_center(y, ITEM_SIZE_PT, 0, item);
        }
        p.text_center(
            h - pt(FOOTER_BASE_OFF_PT),
            FOOTER_SIZE_PT,
            120,
            "swipe down: exit · swipe from top: brightness",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::Tap { y, .. } => match MenuScreen::hit(y as i32, self.items.len()) {
                Some(idx) => (self.on_select)(idx),
                None => Action::Keep,
            },
            // Vertical swipe on the root = leave the app (the old
            // menu_loop returned None on the same gestures).
            Gesture::Swipe { dir: SwipeDir::North, .. }
            | Gesture::Swipe { dir: SwipeDir::South, .. } => Action::Quit,
            Gesture::Swipe { .. } => Action::Keep,
            Gesture::TwoFingerTap => Action::Keep,
            _ => Action::Keep,
        }
    }

}

/// --- MessageScreen layout (pt): ported from ui.rs::message ---
const MSG_SIZE_PT: f32 = 10.0;
const MSG_LINE_PT: f32 = 14.0;
const MSG_FIRST_OFF_PT: f32 = 10.0;

/// Fullscreen message overlay; any gesture dismisses it.
pub struct MessageScreen {
    lines: Vec<String>,
}

impl MessageScreen {
    pub fn new(lines: &[&str]) -> MessageScreen {
        MessageScreen {
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn from_strings(lines: Vec<String>) -> MessageScreen {
        MessageScreen { lines }
    }
}

impl Screen for MessageScreen {
    fn draw(&mut self, p: &mut Painter) {
        let h = p.size().1;
        p.clear(255);
        let line_h = pt(MSG_LINE_PT);
        let total = self.lines.len() as i32 * line_h;
        let mut y = (h - total) / 2 + pt(MSG_FIRST_OFF_PT);
        for l in &self.lines {
            p.text_center(y, MSG_SIZE_PT, 0, l);
            y += line_h;
        }
    }

    fn on_gesture(&mut self, _g: Gesture) -> Action {
        Action::Pop
    }

    fn default_edges(&self) -> bool {
        false
    }
}

/// Low-power sleep overlay: turns off frontlight, locks touch input,
/// renders a clean "Sleeping" badge, and wakes up on power button press.
pub struct SleepScreen {
    prev_bright: i32,
    prev_tone: i32,
    /// Raw image bytes; decoded+fitted lazily at first draw, when the
    /// visual canvas dims (orientation included) are actually known.
    image_raw: Option<Vec<u8>>,
    image: Option<Vec<u8>>,
    /// Sleep-entry wall-clock timestamp + battery, for drain accounting.
    /// Wall clock, NOT Instant: CLOCK_MONOTONIC stops during
    /// suspend-to-RAM (a 55-minute sleep logged "after 0m"), and
    /// suspended time is precisely what we're measuring. A post-wake NTP
    /// jump can distort one line — an acceptable trade.
    entered_wall: std::time::SystemTime,
    batt_enter: u8,
    /// Gauge charge at entry (mAh) — sub-percent drain accounting for the
    /// wake line, where capacity% granularity (1% ≈ 17mAh) floors out.
    q_enter: i64,
    /// Kernel suspends this session. Every wake re-suspends on the next
    /// tick unless a real gesture arrived, so suspends-1 is the count of
    /// wakes that were NOT the power key — the spurious-wake churn number
    /// the battery investigation wants.
    suspends: u32,
}

/// Pre-sleep frontlight state, stashed when SleepScreen blanks it so the
/// emergency exit path (panic / terminating signal — paths that skip
/// on_leave and Drop) can still restore it. -1 = not currently sleeping.
static SAVED_FL_BRIGHT: AtomicI32 = AtomicI32::new(-1);
static SAVED_FL_TONE: AtomicI32 = AtomicI32::new(-1);

/// Last-ditch hardware restore for the panic/signal guard: undo the
/// sleep-screen state so a dying process doesn't leave the device dark
/// and offline. Best effort — runs on the way out, once.
pub fn emergency_wake_restore() {
    let bright = SAVED_FL_BRIGHT.swap(-1, Ordering::SeqCst);
    let tone = SAVED_FL_TONE.swap(-1, Ordering::SeqCst);
    if bright >= 0 {
        if let Ok(fl) = ybdev::frontlight::Frontlight::open() {
            fl.set(bright);
            fl.tone_set(tone.max(0));
        }
    }
    // Wi-Fi back to the framework default (harmless if it never went
    // down). No restore verification here: this is a teardown path, it
    // must not spawn threads.
    ybdev::wifi::turn_on();
}

/// Enter kernel suspend-to-RAM. Returns when the SoC wakes (power key or
/// a spurious source); the caller re-suspends on its next tick unless a
/// real gesture arrived — the KOReader standby loop pattern.
fn suspend_to_mem() {
    let _ = std::process::Command::new("sync").status();
    if let Ok(mut f) = std::fs::File::create("/sys/power/state") {
        use std::io::Write;
        let _ = f.write_all(b"mem");
    }
}

/// Gauge-integrated charge right now, mAh (0 if unreadable).
fn charge_now_mah() -> i64 {
    std::fs::read_to_string("/sys/class/power_supply/bd71827_bat/charge_now")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|ua| ua / 1000)
        .unwrap_or(0)
}

fn batt_stats() -> String {
    let read = |f: &str| {
        std::fs::read_to_string(format!("/sys/class/power_supply/bd71827_bat/{}", f))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    // q = the gauge's integrated charge in mAh (charge_now is µAh, and
    // capacity% is just q/charge_full rounded — verified 1225/1703 → 72).
    // It gives the sleep drain a real number where % granularity would
    // floor at 1% per 8h.
    format!(
        "batt={}% v={}mV i={}mA q={}mAh",
        read("capacity"),
        read("voltage_now") / 1000,
        read("current_now") / 1000,
        read("charge_now") / 1000
    )
}

impl SleepScreen {
    pub fn new() -> SleepScreen {
        let fl = ybdev::frontlight::Frontlight::open().ok();
        let prev_bright = fl.as_ref().map(|f| f.get()).unwrap_or(0);
        let prev_tone = fl.as_ref().map(|f| f.tone_get()).unwrap_or(0);
        // Publish for the emergency path before the light goes out.
        SAVED_FL_BRIGHT.store(prev_bright, Ordering::SeqCst);
        SAVED_FL_TONE.store(prev_tone, Ordering::SeqCst);
        if let Some(f) = &fl {
            f.set(0);
            f.tone_set(0);
        }

        // Shut off Wi-Fi radio power amplifier to eliminate standby drain
        ybdev::wifi::turn_off();

        let image_raw = pick_random_screensaver();

        let batt_enter = ybdev::sysinfo::battery().0;
        let q_enter = charge_now_mah();
        ybdev::log::plog(&format!("sleep: enter {}", batt_stats()));

        SleepScreen {
            prev_bright,
            prev_tone,
            image_raw,
            image: None,
            entered_wall: std::time::SystemTime::now(),
            batt_enter,
            q_enter,
            suspends: 0,
        }
    }
}

fn pick_random_screensaver() -> Option<Vec<u8>> {
    let dirs = [
        "/mnt/us/screensavers",
        "/mnt/us/extensions/reader/screensavers",
        "/tmp/dev_screensaver",
    ];
    let mut files = Vec::new();
    for d in dirs {
        if let Ok(entries) = std::fs::read_dir(d) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(ext) = p.extension() {
                    let ext_str = ext.to_string_lossy().to_ascii_lowercase();
                    if ext_str == "png" || ext_str == "jpg" || ext_str == "jpeg" {
                        files.push(p);
                    }
                }
            }
        }
    }
    if files.is_empty() {
        return None;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let choice = &files[(seed as usize) % files.len()];
    std::fs::read(choice).ok()
}

impl Screen for SleepScreen {
    fn on_enter(&mut self) -> Action {
        Action::Redraw
    }

    fn on_resume(&mut self) -> Action {
        Action::Redraw
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        // First draw is where the visual dims (orientation included) are
        // known — decode and fit exactly once, to this canvas.
        if self.image.is_none() {
            if let Some(raw) = &self.image_raw {
                self.image = ybdev::img::load_png_fitted(raw, w as u32, h as u32);
            }
        }
        if let Some(img) = &self.image {
            p.blit_gray(0, 0, w, h, img, w as usize);
        } else {
            let bw = pt(220.0);
            let bh = pt(60.0);
            let bx = (w - bw) / 2;
            let by = (h - bh) / 2;

            p.rect(crate::painter::Rect::new(bx, by, bw, bh), 255);
            p.rect_outline_t(crate::painter::Rect::new(bx, by, bw, bh), 2, 0);
            p.text_center(by + pt(22.0), 12.0, 0, "Sleeping");
            p.text_center(by + pt(44.0), 8.0, 100, "Press Power Button to Wake");
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        match g {
            Gesture::PowerButton => Action::Pop,
            _ => Action::Keep, // Ignore all touch events while sleeping
        }
    }

    fn default_edges(&self) -> bool {
        false
    }

    fn is_sleep(&self) -> bool {
        true
    }

    fn tick_interval(&self) -> std::time::Duration {
        // Short on purpose: the first tick after the screensaver paints is
        // what carries us into kernel suspend, and each spurious wake
        // re-suspends on the next one.
        std::time::Duration::from_millis(300)
    }

    fn on_tick(&mut self) -> Action {
        // The panel has painted by now (draw ran before the first tick),
        // so the screensaver is on glass — safe to actually sleep. The
        // power key that wakes the SoC queues a gesture that pops this
        // screen; anything else (spurious wake source) falls through to
        // this tick again and re-suspends.
        self.suspends = self.suspends.wrapping_add(1);
        suspend_to_mem();
        Action::Keep
    }

    fn on_leave(&mut self) {
        // Restore frontlight
        if let Ok(fl) = ybdev::frontlight::Frontlight::open() {
            fl.set(self.prev_bright);
            fl.tone_set(self.prev_tone);
        }
        // Handled cleanly: retract what the emergency path would restore.
        SAVED_FL_BRIGHT.store(-1, Ordering::SeqCst);
        SAVED_FL_TONE.store(-1, Ordering::SeqCst);
        // Restore Wi-Fi, then verify the restore actually associated.
        // This is the only code that runs on the power-button wake path
        // (the App resume hook is skipped while the sleep screen is on
        // top), and a restored radio that never associates scans at
        // ~3× the idle drain — ybdev::wifi::verify_or_power_down has
        // the measured numbers and the drain guard.
        ybdev::wifi::turn_on();
        ybdev::wifi::verify_or_power_down();

        // Drain accounting: %/h over this sleep session, plus the suspend
        // count — one entry per wake, so suspends-1 is how many wakes were
        // NOT the power key. (A tick can race the pop after the real wake
        // and briefly re-suspend, so a single-digit count is noise.)
        let mins = self
            .entered_wall
            .elapsed()
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0);
        let now = ybdev::sysinfo::battery().0;
        let rate = if mins >= 30 {
            format!(
                " (-{:.2}%/h -{:.1}mAh)",
                (self.batt_enter.saturating_sub(now)) as f64 * 60.0 / mins as f64,
                (self.q_enter - charge_now_mah()).max(0)
            )
        } else {
            String::new()
        };
        ybdev::log::plog(&format!(
            "sleep: wake {} after {}m suspends={} spurious={}{}",
            batt_stats(),
            mins,
            self.suspends,
            self.suspends.saturating_sub(1),
            rate
        ));
    }
}





impl Drop for SleepScreen {
    fn drop(&mut self) {
        if let Ok(fl) = ybdev::frontlight::Frontlight::open() {
            fl.set(self.prev_bright);
            fl.tone_set(self.prev_tone);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_hit_maps_rows_and_rejects_outside() {
        let top = pt(TOP_PT);
        let item_h = pt(ITEM_H_PT);
        // First row: anywhere from top to top+item_h.
        assert_eq!(MenuScreen::hit(top + 1, 5), Some(0));
        assert_eq!(MenuScreen::hit(top + item_h - 1, 5), Some(0));
        // Boundary lands in the second row.
        assert_eq!(MenuScreen::hit(top + item_h, 5), Some(1));
        assert_eq!(MenuScreen::hit(top + 4 * item_h + 5, 5), Some(4));
        // Above the list (title area) and below it: nothing.
        assert_eq!(MenuScreen::hit(top - 1, 5), None);
        assert_eq!(MenuScreen::hit(top + 5 * item_h, 5), None);
        // Trailing rows of a short menu don't exist.
        assert_eq!(MenuScreen::hit(top + 2 * item_h, 2), None);
    }

    #[test]
    fn menu_layout_uses_real_rows() {
        // 26pt at 300dpi = 108.33px -> 108. 54pt = 225.0 mathematically,
        // but PX (300/72) isn't exact in binary, so the product truncates
        // to 224 — same value the old ad-hoc screens computed. Pin both.
        assert_eq!(pt(26.0), 108);
        assert_eq!(pt(54.0), 224);
    }
}
