//! The top curtain — a full-screen control-center sheet: big clock +
//! date, status rows (battery / wifi / ssh / storage), brightness +
//! tone sliders. Opened by the top-edge swipe / two-finger tap from
//! anywhere (the App edge overlay).

use std::process::Command;

use ybdev::frontlight::Frontlight;
use ybdev::input::{Gesture, SwipeDir};
use ybdev::sysinfo;

use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

// --- layout (pt) ---
const CLOCK_BASE_PT: f32 = 112.0;
const CLOCK_SIZE_PT: f32 = 40.0;
const DATE_BASE_PT: f32 = 144.0;
const DATE_SIZE_PT: f32 = 11.0;
const RULE1_PT: f32 = 168.0;
const STATUS_TOP_PT: f32 = 186.0;
const STATUS_ROW_PT: f32 = 17.0;
const LABEL_PT: f32 = 7.5;
const VALUE_PT: f32 = 9.0;
const RULE2_PT: f32 = 268.0;
const BAR_SIDE_PT: f32 = 24.0;
const BAR_H_PT: f32 = 18.0;
const HIT_MARGIN_PT: f32 = 6.0;
const BRIGHT_LABEL_PT: f32 = 292.0;
const BRIGHT_TOP_PT: f32 = 306.0;
const TONE_LABEL_PT: f32 = 352.0;
const TONE_TOP_PT: f32 = 366.0;
const PAD_PT: f32 = 24.0;

// --- grays on white ---
const DIM: u8 = 125; // secondary text
const INK: u8 = 0; // primary text
const TRACK: u8 = 220; // slider background
const RULE: u8 = 195;

pub struct CurtainScreen {
    fl: Option<Frontlight>,
    time: String,
    date: String,
    /// Slider geometry (px), cached in draw (draw runs before gestures).
    bar_left: i32,
    bar_right: i32,
    bright_top: i32,
    tone_top: i32,
    bar_h: i32,
    margin: i32,
}

impl CurtainScreen {
    pub fn new() -> CurtainScreen {
        // The device `date` prints framework-adjusted LOCAL time (the raw
        // epoch is UTC and /etc/TZ lies) — one fork per curtain open.
        let (time, date) = Command::new("date")
            .arg("+%H:%M|%A, %d %B")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .map(|s| match s.split_once('|') {
                Some((t, d)) => (t.to_string(), d.to_string()),
                None => (s, String::new()),
            })
            .unwrap_or_else(|| ("--:--".to_string(), String::new()));

        CurtainScreen {
            fl: None,
            time,
            date,
            bar_left: 0,
            bar_right: 0,
            bright_top: 0,
            tone_top: 0,
            bar_h: 0,
            margin: 0,
        }
    }

    fn slider_row(
        &self,
        p: &mut Painter,
        label_y: i32,
        label: &str,
        bar_top: i32,
        frac: f32,
        tone_on: bool,
    ) {
        let w = p.size().0;
        p.text(pt(PAD_PT), label_y, LABEL_PT, DIM, label);
        let val = if tone_on {
            format!("{}%", (frac * 100.0).round() as i32)
        } else {
            format!("{}%", (frac * 100.0).round() as i32)
        };
        p.text_right(w - pt(PAD_PT), label_y, LABEL_PT, DIM, &val);
        // Slider: light-gray track, black fill.
        p.rect(Rect::new(self.bar_left, bar_top, self.bar_right - self.bar_left, self.bar_h), TRACK);
        let fw = ((self.bar_right - self.bar_left) as f32 * frac.clamp(0.0, 1.0)) as i32;
        if fw > 0 {
            p.rect(Rect::new(self.bar_left, bar_top, fw, self.bar_h), 0);
        }
    }

    fn status_row(p: &mut Painter, y: i32, label: &str, value: &str) {
        let w = p.size().0;
        p.text(pt(PAD_PT), y, LABEL_PT, DIM, label);
        p.text_right(w - pt(PAD_PT), y, VALUE_PT, INK, value);
    }
}

impl Default for CurtainScreen {
    fn default() -> Self {
        CurtainScreen::new()
    }
}

impl Screen for CurtainScreen {
    fn on_enter(&mut self) -> Action {
        self.fl = Frontlight::open().ok();
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        p.clear(255);

        // Cache slider geometry (px) for gesture hit-testing.
        self.bar_left = pt(BAR_SIDE_PT);
        self.bar_right = w - pt(BAR_SIDE_PT);
        self.bar_h = pt(BAR_H_PT);
        self.margin = pt(HIT_MARGIN_PT);
        self.bright_top = pt(BRIGHT_TOP_PT);
        self.tone_top = pt(TONE_TOP_PT);

        // --- clock ---
        p.text_center(pt(CLOCK_BASE_PT), CLOCK_SIZE_PT, INK, &self.time);
        if !self.date.is_empty() {
            p.text_center(pt(DATE_BASE_PT), DATE_SIZE_PT, DIM, &self.date);
        }
        p.hline_t(pt(RULE1_PT), pt(PAD_PT), w - pt(PAD_PT), 2, RULE);

        // --- status rows ---
        let (cap, plugged) = sysinfo::battery();
        let bat = if plugged {
            format!("{}% · charging", cap)
        } else {
            format!("{}%", cap)
        };
        let ip = sysinfo::wifi_ip();
        let wifi = ip.clone().unwrap_or_else(|| "off".to_string());
        let ssh = if ybdev::ssh::running() {
            "on · :2222".to_string()
        } else {
            "off".to_string()
        };
        let storage = sysinfo::storage_free_gb();
        let mut y = pt(STATUS_TOP_PT);
        CurtainScreen::status_row(p, y, "battery", &bat);
        y += pt(STATUS_ROW_PT);
        CurtainScreen::status_row(p, y, "wifi", &wifi);
        y += pt(STATUS_ROW_PT);
        CurtainScreen::status_row(p, y, "ssh", &ssh);
        y += pt(STATUS_ROW_PT);
        let storage = storage
            .map(|gb| format!("{:.1} GB free", gb))
            .unwrap_or_else(|| "—".to_string());
        CurtainScreen::status_row(p, y, "storage", &storage);
        p.hline_t(pt(RULE2_PT), pt(PAD_PT), w - pt(PAD_PT), 2, RULE);

        // --- sliders ---
        let Some(fl) = &self.fl else {
            p.text_center(h / 2, 10.0, INK, "No frontlight device");
            return;
        };
        let max = fl.max().max(1);
        let v = fl.get();
        let tmax = fl.tone_max();
        let t = fl.tone_get();
        self.slider_row(p, pt(BRIGHT_LABEL_PT), "brightness", pt(BRIGHT_TOP_PT), v as f32 / max as f32, true);
        if tmax > 0 {
            self.slider_row(p, pt(TONE_LABEL_PT), "tone", pt(TONE_TOP_PT), t as f32 / tmax as f32, true);
        } else {
            p.text(pt(PAD_PT), pt(TONE_LABEL_PT), LABEL_PT, DIM, "tone (unsupported)");
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        // Geometry as locals: the setter closures below must not borrow
        // self while `fl` holds &mut self.fl.
        let (bar_left, bar_right) = (self.bar_left, self.bar_right);
        let frac = move |x: i32| {
            ((x - bar_left) as f32 / (bar_right - bar_left) as f32).clamp(0.0, 1.0)
        };
        let (bright_top, tone_top, bar_h, margin) =
            (self.bright_top, self.tone_top, self.bar_h, self.margin);
        let in_bright = move |y: i32| y >= bright_top - margin && y < bright_top + bar_h + margin;
        let in_tone = move |y: i32| y >= tone_top - margin && y < tone_top + bar_h + margin;
        let Some(fl) = &mut self.fl else {
            return Action::Pop;
        };
        let max = fl.max().max(1);
        let tmax = fl.tone_max();
        let set_bright = |fl: &mut Frontlight, x: i32| -> bool {
            let v = (frac(x) * max as f32).round() as i32;
            if v == fl.get() {
                return false;
            }
            fl.set(v);
            true
        };
        let set_tone = |fl: &mut Frontlight, x: i32| -> bool {
            if tmax == 0 {
                return false;
            }
            let t = (frac(x) * tmax as f32).round() as i32;
            if t == fl.tone_get() {
                return false;
            }
            fl.tone_set(t);
            true
        };

        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                if in_bright(y) {
                    if set_bright(fl, x) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                } else if in_tone(y) {
                    if set_tone(fl, x) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                } else {
                    Action::Pop
                }
            }
            // Drag-on-bar adjusts (end position sets the value), even for
            // vertical drags; everything else vertical closes.
            Gesture::Swipe { y, ex, .. } => {
                let (y, ex) = (y as i32, ex as i32);
                if in_bright(y) {
                    if set_bright(fl, ex) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                } else if in_tone(y) {
                    if set_tone(fl, ex) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                } else {
                    match g {
                        Gesture::Swipe { dir: SwipeDir::North, .. }
                        | Gesture::Swipe { dir: SwipeDir::South, .. } => Action::Pop,
                        _ => Action::Keep,
                    }
                }
            }
            Gesture::TwoFingerTap => Action::Pop,
        }
    }

    // The App edge gesture would push a NEW curtain on top of this one —
    // opt out so our own close gestures win.
    fn default_edges(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    /// Headless render: the curtain must put dark ink in every band on
    /// the white sheet — written while chasing a (false-alarm) "full
    /// black screen" report, kept as a regression guard.
    #[test]
    fn curtain_draws_visible_content_on_white() {
        let font = Font::load().unwrap();
        let mut buf = vec![255u8; 1248 * 1648];
        let mut c = CurtainScreen::new();
        {
            let mut p = yui::Painter::new(&mut buf, 1236, 1648, 1248, &font);
            c.draw(&mut p);
        }
        let ink = |name: &str, y0: usize, y1: usize, min: usize| {
            let n = buf[y0 * 1248..y1 * 1248].iter().filter(|&&b| b < 140).count();
            assert!(n >= min, "{name}: only {n} ink pixels in rows {y0}-{y1}");
        };
        // White sheet: everything else must stay light.
        let lit = buf.iter().filter(|&&b| b > 200).count();
        assert!(lit > 1248 * 1648 * 95 / 100, "sheet is not white: {lit}");
        ink("clock", 340, 480, 500);
        ink("date", 560, 615, 50);
        // Mac has no /dev/frontlight: the no-frontlight message is the
        // only content mid-screen.
        ink("no-fl message", 760, 900, 100);
    }
}
