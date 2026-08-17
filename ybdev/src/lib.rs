//! ybdev — the Kindle device layer for yb-reader.
//!
//! Panel (e-ink framebuffer + MTK refresh), frontlight, touch input,
//! mirror.conf parsing and the plugin log. No heavy dependencies.

pub mod config;
pub mod frontlight;
pub mod img;
pub mod input;
pub mod log;
pub mod mtk;
pub mod panel;
pub mod ssh;
pub mod sysinfo;
