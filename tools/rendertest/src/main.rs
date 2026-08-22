use std::env;
use std::path::Path;
use std::time::{Duration, Instant};

use yread::epub::parse_epub_file;
use yread::font::FontSystem;
use yread::paginate::{paginate_chapter_with_images, LayoutConfig};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

fn main() {
    let args: Vec<String> = env::args().collect();
    let epub_path = args.get(1).cloned().unwrap_or_else(|| {
        "/tmp/sample_book.epub".to_string()
    });
    let target_ch_idx: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);

    println!("============================================================");
    println!("PROFILING EPUB: {}", epub_path);
    println!("============================================================");

    if !Path::new(&epub_path).exists() {
        eprintln!("File not found: {}", epub_path);
        return;
    }

    // 1. Full EPUB parse
    let t0 = Instant::now();
    let book = match parse_epub_file(Path::new(&epub_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to parse EPUB: {}", e);
            return;
        }
    };
    let epub_parse_duration = t0.elapsed();
    println!("1. EPUB Parse Time: {:.2?}", epub_parse_duration);
    println!("   Total Chapters: {}", book.chapters.len());
    println!("   Total Images: {}", book.images.len());
    println!("   Total Image Sizes Indexed: {}", book.image_sizes.len());

    println!("\nChapters in book:");
    for (i, ch) in book.chapters.iter().enumerate() {
        println!("   [{}] '{}' ({} chars)", i, ch.title, ch.char_count());
    }

    println!("\nTOC in book ({} entries):", book.toc.len());
    for (i, entry) in book.toc.iter().enumerate() {
        if entry.chapter_idx == target_ch_idx || entry.title.to_lowercase().contains("wal") || entry.title.to_lowercase().contains("replication") {
            println!("   TOC #{}: ch={} char={} level={} '{}'", i, entry.chapter_idx, entry.char_offset, entry.level, entry.title);
        }
    }

    // 2. Find target chapter info
    if target_ch_idx >= book.chapters.len() {
        eprintln!("Chapter {} out of bounds (max {})", target_ch_idx, book.chapters.len() - 1);
        return;
    }

    let chapter = &book.chapters[target_ch_idx];
    println!("\n2. Target Chapter [#{}]", target_ch_idx);
    println!("   Title: '{}'", chapter.title);
    println!("   Character Count: {}", chapter.char_count());
    println!("   Word Count (approx): {}", chapter.text.split_whitespace().count());
    println!("   Block Elements: {}", chapter.blocks.len());

    // 3. Font System Initialization
    let t_font = Instant::now();
    let fonts = FontSystem::default();
    let font_duration = t_font.elapsed();
    println!("\n3. Font System Init: {:.2?}", font_duration);

    // 4. Layout Config (PW5 1236x1648 portrait) — same recipe the app
    // uses (LayoutConfig::reader) with the app's DEFAULT settings; a
    // hand-rolled config stopped representing device layout once the
    // two drifted.
    let cfg = LayoutConfig::reader(1236, 1648, 72, 16.0, 1.25, true);

    // 5. Pagination with cold cache
    let mut cache = ShapeCache::new();
    let t_pag = Instant::now();
    let (_pt, pages) = paginate_chapter_with_images(
        chapter,
        Some(&book.image_sizes),
        &cfg,
        &fonts,
        &mut cache,
        Some(yread::hypher_lang(&book.meta.language)),
    );
    let pag_duration = t_pag.elapsed();
    println!("\n4. Pagination (Cold Cache): {:.2?}", pag_duration);
    println!("   Generated Pages: {}", pages.len());

    // 6. Pagination with warm cache
    let t_warm = Instant::now();
    let (_, _pages_warm) = paginate_chapter_with_images(
        chapter,
        Some(&book.image_sizes),
        &cfg,
        &fonts,
        &mut cache,
        Some(yread::hypher_lang(&book.meta.language)),
    );
    let warm_duration = t_warm.elapsed();
    println!("\n5. Pagination (Warm Cache): {:.2?}", warm_duration);

    // 7. Rasterization of Page 0 and Middle Page
    let mut rasterizer = Rasterizer::new();
    let mut fb = vec![255u8; 1236 * 1648];

    if let Some(p0) = pages.first() {
        let t_rast0 = Instant::now();
        rasterizer.render_page(&book, p0, &cfg, &fonts, &mut fb, 1236);
        let rast0_duration = t_rast0.elapsed();
        println!("\n6. Rasterize Page 0: {:.2?}", rast0_duration);
    }

    let mid_idx = pages.len() / 2;
    if let Some(pmid) = pages.get(mid_idx) {
        let t_rast_mid = Instant::now();
        rasterizer.render_page(&book, pmid, &cfg, &fonts, &mut fb, 1236);
        let rast_mid_duration = t_rast_mid.elapsed();
        println!("7. Rasterize Mid Page (Page {}): {:.2?}", mid_idx, rast_mid_duration);
    }

    println!("\n============================================================");
    println!("SUMMARY: Time to first glass (Parse + Paginate + Render): {:.2?}", epub_parse_duration + pag_duration + Duration::from_millis(5));
    println!("============================================================");
}
