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

/// Status header: clock + truncated title + battery along the visual top
/// edge — one code path for every orientation (Painter handles the grip).
pub fn draw_header(p: &mut Painter, time_str: &str, book: &str, is_night: bool) {
    let (w, _h) = p.size();
    let fg_color = if is_night { 200 } else { 90 };
    let (bat_cap, _) = sysinfo::battery();
    let bat_str = format!("{}%", bat_cap);
    let title_trunc = p.truncate(7.0, book, p.width_pt() - 70.0);
    p.text(pt(16.0), pt(14.0), 7.0, fg_color, time_str);
    p.text_center(pt(14.0), 7.0, fg_color, &title_trunc);
    p.text_right(w - pt(16.0), pt(14.0), 7.0, fg_color, &bat_str);
    p.hline_t(pt(20.0), pt(16.0), w - pt(16.0), 1, if is_night { 60 } else { 225 });
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

/// Selection-mode bookmark: a ribbon hanging from the top edge, left of
/// the battery — outline when off, filled when on.
pub fn draw_bookmark_ribbon(p: &mut Painter, w: i32, sel_mode: bool) {
    let bw = pt(13.0);
    let bh = pt(20.0);
    let x = w - pt(58.0);
    let y = 0;
    let body_h = bh - pt(4.0);
    let seg = bw / 3;
    if sel_mode {
        p.rect(Rect::new(x, y, bw, body_h), 0);
        p.rect(Rect::new(x, y + body_h, seg, pt(4.0)), 0);
        p.rect(Rect::new(x + 2 * seg, y + body_h, seg, pt(4.0)), 0);
    } else {
        p.rect_outline_t(Rect::new(x, y, bw, body_h), 2, 130);
        p.line_w(x, y + body_h, x + bw / 2, y + bh, 2, 130);
        p.line_w(x + bw, y + body_h, x + bw / 2, y + bh, 2, 130);
    }
}
