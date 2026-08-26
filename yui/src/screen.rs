//! The screen contract. A screen is pure logic plus drawing instructions:
//! it never touches the panel or the input device — it reacts to gestures
//! and ticks and says what should happen next via [`Action`].

use std::time::Duration;

use ybdev::input::Gesture;

use crate::orientation::Orientation;
use crate::painter::Painter;

pub trait Screen {
    /// Paint the whole screen ( backgrounds included — the buffer is not
    /// cleared for you). Called by App only when a redraw was earned.
    fn draw(&mut self, p: &mut Painter);

    /// The orientation this screen renders and hit-tests in. `None` (the
    /// default) inherits whatever the App currently shows — overlays above
    /// a landscape reader render landscape, like a phone. Screens with an
    /// opinion return it outright: the reader derives it from its split
    /// settings, portrait-designed screens pin `Some(Portrait)`.
    fn orientation(&self) -> Option<Orientation> {
        None
    }

    /// A complete gesture (tap / swipe / two-finger tap) in VISUAL px
    /// (App has already un-rotated panel input into this screen's space).
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

    /// Does this screen have a live reason to keep the device awake —
    /// streaming, running a server, a transfer in flight? App-level
    /// idle sleep (the stock screensaver timeout) skips screens that
    /// do, for as long as they are on top.
    fn holds_awake(&self) -> bool {
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
            Action::RedrawFast => "RedrawFast",
            Action::RedrawFull => "RedrawFull",
            Action::Push(_) => "Push(..)",
            Action::Pop => "Pop",
            Action::PopN(_) => "PopN",
            Action::Quit => "Quit",
        })
    }
}

/// How the panel should repaint after a redraw action. App maps each
/// variant to a refresh waveform; the distinction matters on e-ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshMode {
    /// Fast A2 partial refresh — animation frames. 2 gray levels, ghosts;
    /// screens that return `RedrawFast` must finish with a `RedrawFull`
    /// to clean the panel (the pop / on_resume default does this).
    Fast,
    /// Flash-less partial refresh (GL16), the default for content turns.
    Partial,
    /// Full flashing refresh (GC16) — screen change or ghost cleanup.
    Full,
}

pub enum Action {
    /// Nothing changed; no redraw, no refresh.
    Keep,
    /// Repaint and send a partial (flash-less) refresh.
    Redraw,
    /// Repaint and send a fast A2 partial refresh — animation frames.
    /// See [`RefreshMode::Fast`] for the ghosting caveat.
    RedrawFast,
    /// Repaint and send a full (flashing) refresh — screen change or
    /// ghost cleanup.
    RedrawFull,
    /// Open a screen on top of the current one.
    Push(Box<dyn Screen>),
    /// Close the top screen (the root popping quits the app).
    Pop,
    /// Close the top `n` screens at once — for a dialog that must unwind
    /// through a parent dialog (e.g. a TOC picked on top of a scrubber:
    /// the reader below, not the scrubber, must resume). Never pops the
    /// root; each popped screen still gets `on_leave`.
    PopN(usize),
    /// Exit the app entirely.
    Quit,
}
