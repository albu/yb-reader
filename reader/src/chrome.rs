//! Reader chrome: everything painted around the page — the status
//! header (clock · title · battery), the progress footer with
//! chapter-aware time-left, and the selection-mode bookmark ribbon.

use std::process::Command;

use ybdev::sysinfo;
use yui::painter::{pt, Painter, Rect};

use crate::split::ReaderSettings;

pub fn current_time_str() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

/// "Xm in ch · Yh Zm left" from the reader's personal pace. Chapter
/// scope comes from the TOC outline pages; without a TOC the chapter
/// falls back to the whole book.
pub fn time_left_str(
    total: usize,
    page_no: usize,
    toc_chapters: &[usize],
    avg_secs_per_page: f32,
) -> String {
    let pages_left_book = total.saturating_sub(page_no + 1);
    let pages_left_chap = match toc_chapters.iter().find(|&&p| p > page_no) {
        Some(next_chap_page) => next_chap_page.saturating_sub(page_no),
        None => pages_left_book,
    };

    let mins_in_chap = ((pages_left_chap as f32 * avg_secs_per_page) / 60.0).round() as usize;
    let mins_in_book = ((pages_left_book as f32 * avg_secs_per_page) / 60.0).round() as usize;
    if mins_in_book >= 60 {
        format!(
            "{}m in ch · {}h {}m left",
            mins_in_chap,
            mins_in_book / 60,
            mins_in_book % 60
        )
    } else {
        format!("{}m in ch · {}m left", mins_in_chap, mins_in_book)
    }
}

/// The footer line: page progress (split-mode aware) + time left.
pub fn footer_str(
    loading: bool,
    page_no: usize,
    sub_idx: usize,
    total: usize,
    settings: &ReaderSettings,
    time_left: &str,
) -> String {
    if loading {
        format!("page {} · Loading book…", page_no + 1)
    } else if settings.split.total_steps(total) > total {
        format!(
            "page {} ({}/{}) · {}",
            page_no + 1,
            sub_idx + 1,
            settings.split.sub_box_count(),
            time_left
        )
    } else {
        format!("page {} / {} · {}", page_no + 1, total, time_left)
    }
}

/// Status header: clock + truncated title + battery, horizontal in
/// portrait, rotated along the long edge in landscape.
pub fn draw_header(p: &mut Painter, time_str: &str, book: &str, settings: &ReaderSettings, is_night: bool) {
    let (w, _h) = p.size();
    let fg_color = if is_night { 200 } else { 90 };
    let (bat_cap, _) = sysinfo::battery();
    let bat_str = format!("{}%", bat_cap);
    let title_trunc = p.truncate(7.0, book, p.width_pt() - 70.0);

    if settings.split.is_landscape() {
        let rot = settings.split.rotation;
        let header_text = format!("{} · {} · {}", time_str, title_trunc, bat_str);
        let cx = if rot == 270 { pt(10.0) } else { w - pt(10.0) };
        p.text_center_rotated(cx, _h / 2, 6.5, fg_color, &header_text, rot);
    } else {
        p.text(pt(16.0), pt(14.0), 7.0, fg_color, time_str);
        p.text_center(pt(14.0), 7.0, fg_color, &title_trunc);
        p.text_right(w - pt(16.0), pt(14.0), 7.0, fg_color, &bat_str);
        p.hline_t(pt(20.0), pt(16.0), w - pt(16.0), 1, if is_night { 60 } else { 225 });
    }
}

/// Progress footer, rotated along the reading edge in landscape.
pub fn draw_footer(p: &mut Painter, footer: &str, rotation: u16, is_night: bool) {
    let (w, h) = p.size();
    let fg_color = if is_night { 200 } else { 90 };
    match rotation {
        270 => {
            p.text_center_rotated(w - pt(10.0), h / 2, 7.0, fg_color, footer, 270);
        }
        90 => {
            p.text_center_rotated(pt(10.0), h / 2, 7.0, fg_color, footer, 90);
        }
        _ => {
            p.text_center(h - pt(10.0), 7.0, fg_color, footer);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::SplitConfig;

    #[test]
    fn time_left_uses_chapter_scope_and_hours() {
        // 100 pages total, on page 10 (89 left), next chapter at 20: 10
        // pages of chapter left. At 60s/page that's 10m and 89m.
        let s = time_left_str(100, 10, &[20, 60], 60.0);
        assert_eq!(s, "10m in ch · 1h 29m left");
        // No TOC: chapter scope falls back to the whole book (29 pages
        // at 30s = 14.5m, rounds to 15).
        let s = time_left_str(40, 10, &[], 30.0);
        assert_eq!(s, "15m in ch · 15m left");
    }

    #[test]
    fn footer_marks_split_mode_and_loading() {
        let mut settings = ReaderSettings::default();
        settings.split = SplitConfig::for_preset(crate::split::SplitPreset::Horizontal2);
        // H2 doubles the steps, so the footer carries the sub-index.
        let s = footer_str(false, 5, 1, 10, &settings, "X");
        assert!(s.contains("page 6 (2/2)"), "{}", s);
        assert!(s.ends_with("· X"));

        let loading = footer_str(true, 5, 0, 10, &settings, "X");
        assert_eq!(loading, "page 6 · Loading book…");
    }
}
