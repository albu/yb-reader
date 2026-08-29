//! Reader chrome: everything painted around the page — the status
//! header (clock · title · battery), the progress footer with
//! chapter-aware time-left, and the selection-mode bookmark ribbon.

use std::process::Command;
use std::sync::{Mutex, OnceLock};

use ybdev::sysinfo;
use yui::painter::{pt, Painter, Rect};

/// Cached (epoch-minute, rendered "HH:MM"). The reader screen re-renders
/// its header on busy ticks; forking /bin/date each time cost ~10 execs/sec
/// while a chapter laid out. The shell-out stays — it is what makes the
/// clock respect the device timezone — but runs at most once per minute.
fn clock_cache() -> &'static Mutex<(u128, String)> {
    static CACHE: OnceLock<Mutex<(u128, String)>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((u128::MAX, "--:--".to_string())))
}

pub fn current_time_str() -> String {
    let now_minute = ybdev::log::now_ms() / 60_000;
    let mut cache = clock_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.0 != now_minute {
        let fresh = Command::new("date")
            .arg("+%H:%M")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "--:--".to_string());
        *cache = (now_minute, fresh);
    }
    cache.1.clone()
}

/// Status header: clock + truncated title + the right status cluster
/// (bookmark · wifi · battery, even gutters) along the visual top edge —
/// one code path for every orientation (Painter handles the grip).
pub fn draw_header(p: &mut Painter, time_str: &str, book: &str, is_night: bool, sel_mode: bool) {
    let (w, _h) = p.size();
    let fg_color = if is_night { 200 } else { 90 };
    let (bat_cap, _) = sysinfo::battery();
    let bat_str = format!("{}%", bat_cap);
    let title_trunc = p.truncate(7.0, book, p.width_pt() - 70.0);
    p.text(pt(16.0), pt(14.0), 7.0, fg_color, time_str);
    p.text_center(pt(14.0), 7.0, fg_color, &title_trunc);
    // The right cluster, laid out right to left from the margin with a
    // uniform 6pt gutter — one composed group, not three stray marks.
    // Worst case ("100%", bars) reaches ~67pt in, inside the title's
    // 70pt truncation budget.
    let xr = w - pt(16.0);
    let bat_w = p.text_width(7.0, &bat_str) as i32;
    p.text_right(xr, pt(14.0), 7.0, fg_color, &bat_str);
    let gw = draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg_color);
    draw_bookmark_ribbon(p, xr - bat_w - pt(6.0) - gw - pt(6.0), sel_mode, fg_color);
    p.hline_t(
        pt(20.0),
        pt(16.0),
        w - pt(16.0),
        1,
        if is_night { 60 } else { 225 },
    );
}

// ---- Settings screens (System and everything it pushes) ----
//
// The settings-style screens share one persistent header, one status
// badge, and one row grid; this is the single source of truth and
// screens contribute only their title and rows. Five hand-copied
// versions had already drifted (footer baseline, gray level, badge
// styles), which is why the copies are gone.

pub const PAD_PT: f32 = 18.0;

// Persistent header / summary band
pub const HDR_RULE_PT: f32 = 20.0;
pub const TRIVIA_BASE_PT: f32 = 34.0;
pub const TRIVIA_RULE_PT: f32 = 42.0;

// Shared row grid
pub const ROW_H_PT: f32 = 38.0;
pub const ICON_BOX_PT: f32 = 16.0;
pub const ICON_GAP_PT: f32 = 10.0;

pub const FOOTER_BASE_PT: f32 = 372.0;

pub const INK: u8 = 0;
pub const DIM: u8 = 110;
pub const MUTED: u8 = 160;
pub const DIVIDER: u8 = 220;

/// The persistent ambient header on the settings screens: time · title ·
/// (wifi · battery) over a rule. `title` may carry a page tracker — the
/// guide passes "How to Use · 1/2 Reading".
pub fn draw_settings_header(p: &mut Painter, w: i32, pad: i32, title: &str) {
    let t = current_time_str();
    let (cap, plugged) = sysinfo::battery();
    let bat = if plugged {
        format!("+{}%", cap)
    } else {
        format!("{}%", cap)
    };
    let fg = 120;

    // Left: Time
    p.text(pad, pt(14.0), 7.0, fg, &t);

    // Center: Title
    p.text_center(pt(14.0), 7.5, INK, title);

    // Right: Wi-Fi glyph + Battery
    let xr = w - pad;
    let bat_w = p.text_width(7.0, &bat) as i32;
    p.text_right(xr, pt(14.0), 7.0, fg, &bat);
    draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, fg);

    // Top divider rule
    p.hline_t(pt(HDR_RULE_PT), pad, w - pad, 1, 225);
}

/// Status pill on a settings row, right-aligned ending at `rx` and
/// centered on `cy`: filled ink with white text when active, outline
/// with dim text otherwise.
pub fn draw_badge(p: &mut Painter, rx: i32, cy: i32, text: &str, active: bool) {
    let text_w = p.text_width(7.0, text) as i32;
    let bw = text_w + pt(12.0);
    let bh = pt(14.0);
    let r = Rect::new(rx - bw, cy - bh / 2, bw, bh);
    if active {
        p.rect(r, INK);
        p.text_center_in(r.x, r.x + r.w, cy + pt(2.5), 7.0, 255, text);
    } else {
        p.rect_outline_t(r, 1, MUTED);
        p.text_center_in(r.x, r.x + r.w, cy + pt(2.5), 7.0, DIM, text);
    }
}

/// Radio status glyph for the ambient top rows, right-aligned ending at
/// `x_right`, vertically centered on `cy`: filled bars when associated,
/// hollow bars while the radio powers up without a link yet, and an
/// airplane when it is down (sysfs only — safe on busy ticks). Returns
/// its rendered width for cluster layout.
pub fn draw_wifi_glyph(p: &mut Painter, x_right: i32, cy: i32, size_pt: f32, color: u8) -> i32 {    wifi_glyph(p, x_right, cy, size_pt, color, sysinfo::wifi_radio())
}

/// The drawing, parameterized over the state so host tests can render
/// every variant (the host has no wlan0 to read).
pub fn wifi_glyph(
    p: &mut Painter,
    x_right: i32,
    cy: i32,
    size_pt: f32,
    color: u8,
    radio: sysinfo::WifiRadio,
) -> i32 {
    let s = pt(size_pt);
    match radio {
        radio @ (sysinfo::WifiRadio::Connected | sysinfo::WifiRadio::Searching) => {
            // The curtain's bar language, scaled to row size. Hollow
            // bars = powered but not associated yet.
            let hollow = matches!(radio, sysinfo::WifiRadio::Searching);
            let n = 4;
            let bw = (s as f32 * 0.26).round() as i32;
            let gap = (s as f32 * 0.14).round() as i32;
            let total_w = n * bw + (n - 1) * gap;
            let x0 = x_right - total_w;
            for i in 0..n {
                let bh = (s as f32 * (0.25 + 0.25 * i as f32)).round() as i32;
                let r = Rect::new(x0 + i * (bw + gap), cy - bh / 2, bw, bh);
                if hollow {
                    p.rect_outline_t(r, 1, color);
                } else {
                    p.rect(r, color);
                }
            }
            total_w
        }
        sysinfo::WifiRadio::Off => {
            // Flight-mode pictogram pointing up: fuselage, swept wings,
            // tail — three rects read as a plane at this size.
            let w = (s as f32 * 1.1).round() as i32;
            let x0 = x_right - w;
            let thick = ((s as f32 * 0.18).round() as i32).max(2);
            let cx = x0 + w / 2;
            p.rect(Rect::new(cx - thick / 2, cy - s / 2, thick, s), color); // fuselage
            p.rect(Rect::new(x0, cy - s / 8, w, thick), color); // wings
            let tw = w / 2;
            p.rect(Rect::new(cx - tw / 2, cy + s / 3, tw, thick), color); // tail
            w
        }
    }
}

/// Progress footer along the visual bottom edge: centered status text +
/// minimal progress track. `page_no` is 1-based (the number shown in the
/// text), `toc_chapters` holds 0-based outline pages for the tick marks.
pub fn draw_footer(
    p: &mut Painter,
    footer: &str,
    page_no: usize,
    total: usize,
    toc_chapters: &[usize],
    is_night: bool,
) {
    let (w, h) = p.size();
    let fg_color = if is_night { 200 } else { 90 };
    let track_color = if is_night { 60 } else { 220 };
    let fill_color = if is_night { 180 } else { 120 };
    let tick_color = if is_night { 100 } else { 180 };

    // Text status line: e.g. "page 42 / 318 · 12m in ch · 4h 15m left"
    p.text_center(h - pt(10.5), 7.0, fg_color, footer);

    // Micro progress bar along the bottom edge
    if total > 1 {
        let pad = pt(16.0);
        let bar_x = pad;
        let bar_w = w - 2 * pad;
        let bar_y = h - pt(2.5);
        let bar_h = 2;

        p.rect(Rect::new(bar_x, bar_y, bar_w, bar_h), track_color);

        let frac = (page_no as f32 / total as f32).clamp(0.0, 1.0);
        let fill_w = ((bar_w as f32) * frac).round() as i32;
        if fill_w > 0 {
            p.rect(Rect::new(bar_x, bar_y, fill_w, bar_h), fill_color);
        }

        // Chapter ticks
        for &chap_page in toc_chapters {
            if chap_page > 0 && chap_page + 1 < total {
                let chap_frac = ((chap_page + 1) as f32 / total as f32).clamp(0.0, 1.0);
                let tx = bar_x + ((bar_w as f32) * chap_frac).round() as i32;
                p.rect(Rect::new(tx, bar_y - 1, 1, bar_h + 2), tick_color);
            }
        }
    }
}

/// Selection-mode bookmark, folded into the header's right cluster. It
/// used to hang alone from the very top edge, straddling the rule line,
/// which read as a foreign object between two status glyphs; it now
/// sits compactly inside the band like its neighbors. Outline when off,
/// filled when on. Returns its width for cluster layout.
pub fn draw_bookmark_ribbon(p: &mut Painter, x_right: i32, sel_mode: bool, color: u8) -> i32 {
    let bw = pt(11.0);
    let body_h = pt(9.0);
    let tail_h = pt(4.0);
    let y = pt(4.0); // hangs inside the band — the rule runs at 20pt
    let x = x_right - bw;
    let seg = bw / 3;
    if sel_mode {
        p.rect(Rect::new(x, y, bw, body_h), color);
        p.rect(Rect::new(x, y + body_h, seg, tail_h), color);
        p.rect(Rect::new(x + 2 * seg, y + body_h, seg, tail_h), color);
    } else {
        p.rect_outline_t(Rect::new(x, y, bw, body_h), 2, color);
        p.line_w(x, y + body_h, x + bw / 2, y + body_h + tail_h, 2, color);
        p.line_w(
            x + bw,
            y + body_h,
            x + bw / 2,
            y + body_h + tail_h,
            2,
            color,
        );
    }
    bw
}

#[cfg(test)]
mod tests {
    use super::*;
    use yui::Font;

    /// Every glyph variant must put ink in its box and stay inside it —
    /// the geometry is hand-tuned pt math, exactly the kind that drifts.
    #[test]
    fn wifi_glyph_states_render_in_bounds() {
        let font = Font::load().unwrap();
        let cases = [
            (sysinfo::WifiRadio::Connected, "bars"),
            (sysinfo::WifiRadio::Searching, "hollow bars"),
            (sysinfo::WifiRadio::Off, "airplane"),
        ];
        for (state, name) in cases {
            let mut buf = vec![255u8; 200 * 100];
            let mut canvas = vec![0u8; 200 * 100];
            {
                let mut p = Painter::new(
                    &mut buf,
                    200,
                    100,
                    200,
                    yui::Orientation::Portrait,
                    &mut canvas,
                    &font,
                );
                wifi_glyph(&mut p, 180, 50, 8.0, 0, state);
                p.flush();
            }
            // The box the glyph may occupy: right edge at 180, ~12pt wide
            // at this size, full glyph height around cy=50.
            let mut ink_out = 0;
            for (y, row) in buf.chunks_exact(200).enumerate() {
                for (x, &b) in row.iter().enumerate() {
                    if b < 140 && !(x >= 120 && x <= 181 && y >= 30 && y <= 70) {
                        ink_out += 1;
                    }
                }
            }
            assert_eq!(ink_out, 0, "{name}: ink outside the glyph box");
            let ink = buf[40 * 200..61 * 200].iter().filter(|&&b| b < 140).count();
            assert!(ink > 10, "{name}: no ink drawn ({ink})");
        }
    }

    /// The compact ribbon must sit inside the header band (nothing below
    /// the rule at pt(20)) in both states, and report its width.
    #[test]
    fn bookmark_ribbon_stays_inside_the_band() {
        let font = Font::load().unwrap();
        for sel_mode in [false, true] {
            let mut buf = vec![255u8; 200 * 100];
            let mut canvas = vec![0u8; 200 * 100];
            {
                let mut p = Painter::new(
                    &mut buf,
                    200,
                    100,
                    200,
                    yui::Orientation::Portrait,
                    &mut canvas,
                    &font,
                );
                let w = draw_bookmark_ribbon(&mut p, 180, sel_mode, 0);
                p.flush();
                assert!(w > 0 && w <= pt(11.0), "ribbon width {w}");
            }
            // The band ends at the rule (pt(20) ≈ row 83); the old
            // full-height ribbon crossed it — that's the regression.
            let below = buf[84 * 200..].iter().filter(|&&b| b < 140).count();
            assert_eq!(below, 0, "ribbon crosses the header rule");
            let ink = buf[..84 * 200].iter().filter(|&&b| b < 140).count();
            assert!(ink > 10, "ribbon drew no ink");
        }
    }
}
