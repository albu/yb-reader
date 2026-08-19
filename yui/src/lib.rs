//! yui — the small e-ink UI toolkit shared by yb-reader's screens.
//!
//! Layers:
//! - [`Painter`]: immediate drawing onto any grayscale buffer (the live
//!   framebuffer or a `Vec<u8>` in tests). All text sizes are in POINTS.
//! - [`Screen`] / [`Action`]: a screen handles gestures and ticks and
//!   returns what should happen next; it never touches Panel or Input.
//! - [`App`]: owns Panel + Input + the screen stack, applies actions,
//!   decides partial-vs-full refresh, and routes the edge gestures
//!   (top-edge swipe / corner-back) uniformly.
//!
//! The discipline from the ad-hoc screens this replaces: NEVER refresh on a
//! bare tick or a no-op event — every e-ink update must be earned by a
//! returned `Redraw*` action or a stack transition.

pub mod app;
pub mod font;
pub mod frontlight;
pub mod nav;
pub mod orientation;
pub mod painter;
pub mod screen;
pub mod widgets;

pub use app::App;
pub use font::Font;
pub use frontlight::FrontlightScreen;
pub use orientation::Orientation;
pub use painter::{Painter, Rect};
pub use screen::{Action, Screen};
pub use widgets::{MenuScreen, MessageScreen};
