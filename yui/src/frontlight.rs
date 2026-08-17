//! Frontlight overlay: two slider bars — white brightness and warm tone —
//! tap or drag a bar to set, tap outside / vertical swipe / two-finger tap
//! to close. Ported from ui.rs::frontlight_dialog with the hit-testing
//! extracted into pure functions.

use ybdev::frontlight::Frontlight;
use ybdev::input::{Gesture, SwipeDir};

use crate::painter::{pt, Painter, Rect};
use crate::screen::{Action, Screen};

const BAR_H_PT: f32 = 18.0;
/// Generous hit margin around each bar: a near-miss used to fall into
/// "tap outside → close" and the dialog kept vanishing mid-adjust.
const HIT_MARGIN_PT: f32 = 6.0;
const BAR_SIDE_PT: f32 = 24.0;
const BRIGHT_OFF_PT: f32 = 34.0;
const TONE_GAP_PT: f32 = 32.0;

#[derive(Clone, Copy, Debug)]
pub struct FlLayout {
    pub bar_left: i32,
    pub bar_right: i32,
    pub bar_h: i32,
    pub bright_top: i32,
    pub tone_top: i32,
}

impl FlLayout {
    pub fn new(w: i32, h: i32) -> FlLayout {
        let bright_top = h / 2 - pt(BRIGHT_OFF_PT);
        FlLayout {
            bar_left: pt(BAR_SIDE_PT),
            bar_right: w - pt(BAR_SIDE_PT),
            bar_h: pt(BAR_H_PT),
            bright_top,
            tone_top: bright_top + pt(TONE_GAP_PT),
        }
    }

    fn margin(&self) -> i32 {
        pt(HIT_MARGIN_PT)
    }

    pub fn in_bright(&self, y: i32) -> bool {
        y >= self.bright_top - self.margin() && y < self.bright_top + self.bar_h + self.margin()
    }

    pub fn in_tone(&self, y: i32) -> bool {
        y >= self.tone_top - self.margin() && y < self.tone_top + self.bar_h + self.margin()
    }

    /// Horizontal fraction (0..1) of a point along the bar track.
    pub fn frac(&self, x: i32) -> f32 {
        ((x - self.bar_left) as f32 / (self.bar_right - self.bar_left) as f32).clamp(0.0, 1.0)
    }
}

/// What a tap at (x, y) means on this layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapTarget {
    Bright,
    Tone,
    Close,
}

/// Which slider a drag adjusts. A separate type from TapTarget: a drag
/// that starts off the bars is a close/ignored gesture, never a "Close
/// bar" — the type rules that nonsense out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Bright,
    Tone,
}

pub fn tap_target(l: &FlLayout, y: i32, tone_on: bool) -> TapTarget {
    if l.in_bright(y) {
        TapTarget::Bright
    } else if tone_on && l.in_tone(y) {
        TapTarget::Tone
    } else {
        TapTarget::Close
    }
}

/// Which bar a swipe drag adjusts (if it started on one).
pub fn drag_bar(l: &FlLayout, y: i32, tone_on: bool) -> Option<Bar> {
    if l.in_bright(y) {
        Some(Bar::Bright)
    } else if tone_on && l.in_tone(y) {
        Some(Bar::Tone)
    } else {
        None
    }
}

pub struct FrontlightScreen {
    fl: Option<Frontlight>,
    /// Layout is a pure function of panel size; draw() runs before any
    /// gesture (Push draws on entry), so cache it there for hit-testing.
    layout: FlLayout,
}

impl FrontlightScreen {
    pub fn new() -> FrontlightScreen {
        FrontlightScreen {
            fl: None,
            layout: FlLayout::new(1236, 1648),
        }
    }
}

impl Default for FrontlightScreen {
    fn default() -> Self {
        FrontlightScreen::new()
    }
}

impl Screen for FrontlightScreen {
    fn on_enter(&mut self) -> Action {
        self.fl = Frontlight::open().ok();
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        p.clear(255);

        let Some(fl) = &self.fl else {
            p.text_center(h / 2, 10.0, 0, "No frontlight device");
            return;
        };

        let max = fl.max().max(1);
        let v = fl.get();
        let tmax = fl.tone_max();
        let t = fl.tone_get();

        let l = FlLayout::new(w, h);
        self.layout = l;
        p.text_center(pt(56.0), 13.0, 0, "Light");
        p.text_center(l.bright_top - pt(7.0), 8.0, 0, "brightness");
        p.bar(
            Rect::new(l.bar_left, l.bright_top, l.bar_right - l.bar_left, l.bar_h),
            v as f32 / max as f32,
        );
        p.text_center(
            l.tone_top - pt(7.0),
            8.0,
            0,
            if tmax > 0 { "tone" } else { "tone (unsupported)" },
        );
        if tmax > 0 {
            p.bar(
                Rect::new(l.bar_left, l.tone_top, l.bar_right - l.bar_left, l.bar_h),
                t as f32 / tmax as f32,
            );
        }
        p.text_center(
            h - pt(46.0),
            8.0,
            80,
            "tap/drag bar: set · tap outside: close",
        );
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let Some(fl) = &mut self.fl else {
            return Action::Pop;
        };
        let l = self.layout;
        let tmax = fl.tone_max();
        let tone_on = tmax > 0;
        let max = fl.max().max(1);

        let set_bright = |fl: &mut Frontlight, x: i32| -> bool {
            let v = (l.frac(x) * max as f32).round() as i32;
            if v == fl.get() {
                return false;
            }
            fl.set(v);
            true
        };
        let set_tone = |fl: &mut Frontlight, x: i32| -> bool {
            let t = (l.frac(x) * tmax as f32).round() as i32;
            if t == fl.tone_get() {
                return false;
            }
            fl.tone_set(t);
            true
        };

        match g {
            Gesture::Tap { x, y } => match tap_target(&l, y as i32, tone_on) {
                TapTarget::Bright => {
                    if set_bright(fl, x as i32) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                }
                TapTarget::Tone => {
                    if set_tone(fl, x as i32) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                }
                TapTarget::Close => Action::Pop,
            },
            // A swipe that starts on a bar is a drag-adjust, not a close
            // command — set from where the finger ended. This arm wins
            // over the vertical-swipe close even for N/S drags.
            Gesture::Swipe { y, ex, .. } => match drag_bar(&l, y as i32, tone_on) {
                Some(Bar::Bright) => {
                    if set_bright(fl, ex as i32) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                }
                Some(Bar::Tone) => {
                    if set_tone(fl, ex as i32) {
                        Action::Redraw
                    } else {
                        Action::Keep
                    }
                }
                None => match g {
                    Gesture::Swipe { dir: SwipeDir::North, .. }
                    | Gesture::Swipe { dir: SwipeDir::South, .. } => Action::Pop,
                    _ => Action::Keep,
                },
            },
            Gesture::TwoFingerTap => Action::Pop,
            _ => Action::Keep,
        }
    }


    // The App-level edge gestures would push a *new* frontlight on top of
    // this one and steal its close gestures — opt out.
    fn default_edges(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> FlLayout {
        FlLayout::new(1236, 1648)
    }

    #[test]
    fn tap_hits_bars_with_margin_and_close_elsewhere() {
        let l = layout();
        // Direct hits.
        assert_eq!(tap_target(&l, l.bright_top + 5, true), TapTarget::Bright);
        assert_eq!(tap_target(&l, l.tone_top + 5, true), TapTarget::Tone);
        // Margin counts as the bar (the old vanishing-dialog bug).
        assert_eq!(tap_target(&l, l.bright_top - pt(6.0) + 1, true), TapTarget::Bright);
        assert_eq!(tap_target(&l, l.bright_top + l.bar_h + pt(6.0) - 1, true), TapTarget::Bright);
        // Between the bars (the gap that remains outside both margins)
        // and everywhere else closes.
        let gap_mid =
            (l.bright_top + l.bar_h + pt(HIT_MARGIN_PT) + l.tone_top - pt(HIT_MARGIN_PT)) / 2;
        assert_eq!(tap_target(&l, gap_mid, true), TapTarget::Close);
        assert_eq!(tap_target(&l, 10, true), TapTarget::Close);
        // Tone unsupported: taps there close, not adjust.
        assert_eq!(tap_target(&l, l.tone_top + 5, false), TapTarget::Close);
    }

    #[test]
    fn drag_only_when_swipe_starts_on_a_bar() {
        let l = layout();
        assert_eq!(drag_bar(&l, l.bright_top + 2, true), Some(Bar::Bright));
        assert_eq!(drag_bar(&l, l.tone_top + 2, true), Some(Bar::Tone));
        assert_eq!(drag_bar(&l, 100, true), None);
        assert_eq!(drag_bar(&l, l.tone_top + 2, false), None);
    }

    #[test]
    fn frac_spans_the_track() {
        let l = layout();
        assert_eq!(l.frac(l.bar_left), 0.0);
        assert_eq!(l.frac(l.bar_right), 1.0);
        assert!((l.frac((l.bar_left + l.bar_right) / 2) - 0.5).abs() < 0.01);
        assert_eq!(l.frac(0), 0.0);
        assert_eq!(l.frac(99999), 1.0);
    }
}
