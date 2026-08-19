//! fb2png — wrap a raw grayscale framebuffer dump as a PNG.
//! `cargo run -p ybdev --example fb2png fb.raw out.png W STRIDE [H]`
//! (H defaults to len/stride). For "see it yourself" sessions: dd the
//! Kindle's /dev/fb0, convert, open.

use std::env;

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: {} <raw> <out.png> <width> <stride> [height]", a[0]);
        std::process::exit(2);
    }
    let data = std::fs::read(&a[1]).expect("read raw");
    let w: usize = a[3].parse().expect("width");
    let stride: usize = a[4].parse().expect("stride");
    let h: usize = match a.get(5) {
        Some(s) => s.parse().expect("height"),
        None => data.len() / stride,
    };
    let mut rows: Vec<u8> = Vec::with_capacity(w * h);
    for y in 0..h {
        let s = y * stride;
        rows.extend_from_slice(&data[s..s + w]);
    }
    let file = std::fs::File::create(&a[2]).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("header")
        .write_image_data(&rows)
        .expect("pixels");
    println!("{}x{} -> {}", w, h, a[2]);
}
