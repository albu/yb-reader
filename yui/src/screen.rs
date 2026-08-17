//! The screen contract. A screen is pure logic plus drawing instructions:
//! it never touches the panel or the input device — it reacts to gestures
//! and ticks and says what should happen next via [`Action`].

use std::time::Duration;

use ybdev::input::Gesture;

use crate::painter::Painter;

pub trait Screen {
    /// Paint the whole screen ( backgrounds included — the buffer is not
    /// cleared for you). Called by App only when a redraw was earned.
    fn draw(&mut self, p: &mut Painter);

    /// A complete gesture (tap / swipe / two-finger tap) in panel px.
    fn on_gesture(&mut self, g: Gesture) -> Action {
        let _ = g;
        Action::Keep
    }

    /// The gesture-wait timed out: a slot for periodic work (keepalives,
    /// polling). Returning `Keep` is free — App refreshes NOTHING on a bare
    /// tick (the no-op-flash/ghosting rule).
    fn on_tick(&mut self) -> Action {
        Action::Keep
    }

    /// This screen was pushed (it is now the top). Runs before the first
    /// draw; heavy setup (opening a document, network) belongs here. The
    /// returned action is applied after the push — e.g. an overlay message
    /// on a failed setup.
    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    /// This screen was popped for good (not merely covered). Release
    /// resources (network, wakelocks) here. No draw follows.
    fn on_leave(&mut self) {}

    /// An overlay above us was popped; we are the top again. Default
    /// repaints with a full flash — screens holding a pixel cache may
    /// prefer `Action::Redraw` to re-present it cheaply.
    fn on_resume(&mut self) -> Action {
        Action::RedrawFull
    }

    /// How long App waits for a gesture before calling on_tick.
    fn tick_interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    /// Accept the App-level edge gestures (top-edge swipe and two-finger
    /// tap open the frontlight, bottom-right corner swipe-up pops)?
    /// Overlays that define their own close gestures must opt out to avoid
    /// intercepting their own dismiss input.
    fn default_edges(&self) -> bool {
        true
    }

    /// Is this screen a sleep/standby overlay?
    fn is_sleep(&self) -> bool {
        false
    }
}


// Manual Debug: the Push variant carries a Box<dyn Screen> that can't
// derive — the variant name is all logs ever need.
impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Action::Keep => "Keep",
            Action::Redraw => "Redraw",
            Action::RedrawFull => "RedrawFull",
            Action::Push(_) => "Push(..)",
            Action::Pop => "Pop",
            Action::Quit => "Quit",
        })
    }
}

pub enum Action {
    /// Nothing changed; no redraw, no refresh.
    Keep,
    /// Repaint and send a partial (flash-less) refresh.
    Redraw,
    /// Repaint and send a full (flashing) refresh — screen change or
    /// ghost cleanup.
    RedrawFull,
    /// Open a screen on top of the current one.
    Push(Box<dyn Screen>),
    /// Close the top screen (the root popping quits the app).
    Pop,
    /// Exit the app entirely.
    Quit,
}
