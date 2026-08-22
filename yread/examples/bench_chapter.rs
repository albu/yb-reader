//! Bench: where does cold-chapter pagination time go?
//! Run: cargo run --release -p yread --example bench_chapter -- /tmp/sample_book.epub 12

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).map(|s| s.as_str()).unwrap_or("/tmp/sample_book.epub");
    let ch_idx: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(12);

    let t0 = Instant::now();
    let book = yread::epub::parse_epub_file(std::path::Path::new(path)).expect("parse");
    println!("parse: {:?} ({} chapters)", t0.elapsed(), book.chapters.len());

    // Find the biggest chapter for reference
    let (biggest, big_len) = book
        .chapters
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| c.text.len())
        .map(|(i, c)| (i, c.text.len()))
        .unwrap_or((0, 0));
    println!("biggest chapter: idx {} ({} kB)", biggest, big_len / 1024);

    let ch = &book.chapters[ch_idx];
    let words = ch.text.split_whitespace().count();
    println!(
        "chapter {}: {} kB text, {} blocks, {} words",
        ch_idx,
        ch.text.len() / 1024,
        ch.blocks.len(),
        words
    );

    let cfg = yread::paginate::LayoutConfig {
        page_width: 1236,
        page_height: 1648,
        margin_left: 72,
        margin_right: 72,
        margin_top: 72 + 92,
        margin_bottom: 72 + 50,
        font_size: 9.0,
        line_spacing: 1.2,
        paragraph_spacing: 0.25,
        indent_em: 1.2,
        hyphenate: true,
    };

    let fonts = yread::font::FontSystem::default();

    // Cold: fresh shape cache (what a first open pays)
    let mut cache = yread::shape::ShapeCache::new();
    let t1 = Instant::now();
    let (pt, layouts) = yread::paginate::paginate_chapter_with_images(
        ch,
        Some(&book.image_sizes),
        &cfg,
        &fonts,
        &mut cache,
        Some(yread::paginate_bench_lang(&book.meta.language)),
    );
    let cold = t1.elapsed();
    println!(
        "cold paginate: {:?} ({} pages, {:.0} µs/word)",
        cold,
        layouts.len(),
        cold.as_micros() as f64 / words.max(1) as f64
    );

    // Warm: same cache — everything hits; what remains is pure layout math
    let t2 = Instant::now();
    let (pt2, layouts2) = yread::paginate::paginate_chapter_with_images(
        ch,
        Some(&book.image_sizes),
        &cfg,
        &fonts,
        &mut cache,
        Some(yread::paginate_bench_lang(&book.meta.language)),
    );
    let warm = t2.elapsed();
    println!(
        "warm paginate: {:?} ({} pages — must equal {} / {} — must equal {})",
        warm,
        layouts2.len(),
        layouts.len(),
        pt2.pages.len(),
        pt.pages.len()
    );
    let shape_share = 1.0 - warm.as_secs_f64() / cold.as_secs_f64().max(1e-9);
    println!("shaping share of cold: {:.0}%", shape_share * 100.0);
    let _ = (pt.pages.len(), pt2.pages.len());

    // Loop warm runs long enough to profile (~3s)
    // Hyphenation A/B: same chapter, hyphenate=false
    {
        let mut cfg_noh = cfg.clone();
        cfg_noh.hyphenate = false;
        let mut c3 = yread::shape::ShapeCache::new();
        let ta = Instant::now();
        let _ = yread::paginate::paginate_chapter_with_images(
            ch, Some(&book.image_sizes), &cfg_noh, &fonts, &mut c3,
            None,
        );
        let cold_noh = ta.elapsed();
        let tb = Instant::now();
        let _ = yread::paginate::paginate_chapter_with_images(
            ch, Some(&book.image_sizes), &cfg_noh, &fonts, &mut c3,
            None,
        );
        println!("NO-HYPHENATE cold: {:?}  warm: {:?}", cold_noh, tb.elapsed());
    }

    let t3 = Instant::now();
    let mut n = 0;
    while t3.elapsed().as_secs_f64() < 3.0 {
        let mut c2 = yread::shape::ShapeCache::new();
        let _ = yread::paginate::paginate_chapter_with_images(
            ch, Some(&book.image_sizes), &cfg, &fonts, &mut c2,
            Some(yread::paginate_bench_lang(&book.meta.language)),
        );
        n += 1;
    }
    println!("profiling loop: {} cold iterations in {:?}", n, t3.elapsed());
}
