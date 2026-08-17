//! Host-side render check: draw the home screen's look (kicker + action
//! rows + the Boox-style bottom nav) into an offscreen buffer and save
//! /tmp/menu.png, so layout/icon changes can be inspected without a
//! device. Run: cargo run -p rendertest

use yui::nav::{self, Icon, NavTab};
use yui::painter::{pt, Painter, Rect};
use yui::Font;

const W: u32 = 1236;
const H: u32 = 1648;
const STRIDE: usize = 1248; // real fb stride, to exercise the padding path

const TABS: [NavTab; 2] = [
    NavTab::new(Icon::Home, "home"),
    NavTab::new(Icon::Books, "library"),
];

fn main() {
    let font = Font::load().expect("embedded font");
    let mut buf = vec![255u8; STRIDE * H as usize];
    {
        let mut p = Painter::new(&mut buf, W, H, STRIDE, &font);
        let (w, h) = p.size();
        let pad = pt(20.0);
        let content_h = h - nav::bar_h_px();

        p.text(pad, pt(26.0), 7.0, 130, "YB READER");
        p.hline_t(pt(40.0), pad, w - pad, 3, 140);
        let rows = ["Mirror to Mac", "Fetch book from Mac", "Exit"];
        for (i, label) in rows.iter().enumerate() {
            let top = pt(56.0) + i as i32 * pt(34.0);
            icon(&mut p, i, pad, top + (pt(34.0) - pt(13.0)) / 2);
            p.text(pad + pt(21.0), top + pt(21.0), 10.0, 0, label);
            p.text_right(w - pad, top + pt(21.0), 12.0, 160, ">");
            if i + 1 < rows.len() {
                p.hline_t(top + pt(34.0), pad, w - pad, 2, 180);
            }
        }
        p.text_center(
            content_h - pt(20.0),
            7.0,
            150,
            "swipe from top: brightness · corner swipe: back",
        );
        nav::draw_nav(&mut p, &TABS, 0);
    }

    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(std::fs::File::create("/tmp/menu.png").unwrap()),
        W,
        H,
    );
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    // Pack the strided buffer into tight rows for the PNG.
    let mut tight = vec![0u8; (W * H) as usize];
    for y in 0..H as usize {
        tight[y * W as usize..(y + 1) * W as usize]
            .copy_from_slice(&buf[y * STRIDE..y * STRIDE + W as usize]);
    }
    wr.write_image_data(&tight).unwrap();
    println!("wrote /tmp/menu.png");
}

/// Same icons the reader's home rows use, mirrored here for the preview.
fn icon(p: &mut Painter, row: usize, x: i32, y: i32) {
    let s = pt(13.0);
    let mid = x + s / 2;
    const T: i32 = 2;
    match row {
        0 => {
            p.rect_outline_t(Rect::new(x, y, s, s - 2), T, 0);
            p.line_w(mid - 3, y + s / 2, mid + 4, y + s / 2 - 1, T, 0);
            p.line_w(mid + 1, y + s / 2 - 4, mid + 4, y + s / 2 - 1, T, 0);
            p.line_w(mid + 1, y + s / 2 + 2, mid + 4, y + s / 2 - 1, T, 0);
        }
        1 => {
            p.line_w(mid, y + 1, mid, y + s - 5, T, 0);
            p.line_w(mid - 3, y + s - 8, mid, y + s - 5, T, 0);
            p.line_w(mid + 3, y + s - 8, mid, y + s - 5, T, 0);
            p.rect_outline_t(Rect::new(x + 1, y + s - 3, s - 2, 2), T, 0);
        }
        _ => {
            p.line_w(x + 2, y + 2, x + s - 3, y + s - 3, T, 0);
            p.line_w(x + s - 3, y + 2, x + 2, y + s - 3, T, 0);
        }
    }
}
