use std::fs;
use std::path::Path;
use std::time::Instant;

use yread::epub::parse_epub;
use yread::font::FontSystem;
use yread::paginate::LayoutConfig;
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

#[test]
fn test_render_real_sample_book() {
    let book_path = "/tmp/sample_book.epub";
    if !Path::new(book_path).exists() {
        println!("Book not found at '{}', skipping real book test.", book_path);
        return;
    }

    // 1. Time EPUB reading and parsing
    let t0 = Instant::now();
    let data = fs::read(book_path).expect("read epub file");
    let read_time = t0.elapsed();

    let t1 = Instant::now();
    let book = parse_epub(&data).expect("parse sample epub");
    let parse_time = t1.elapsed();

    println!("=== SAMPLE EPUB Parse Info ===");
    println!("File size: {} MB", data.len() / (1024 * 1024));
    println!("File read time: {:?}", read_time);
    println!("Book parse time: {:?}", parse_time);
    println!("Title: '{}'", book.meta.title);
    println!("Authors: {:?}", book.meta.authors);
    println!("Language: '{}'", book.meta.language);
    println!("Chapters: {}", book.chapters.len());
    println!("Total images: {}", book.images.len());
    println!("Total chars: {}", book.total_chars());

    for (idx, ch) in book.chapters.iter().enumerate().take(15) {
        println!("  Chapter #{}: '{}' (blocks={}, chars={})", idx, ch.title, ch.blocks.len(), ch.char_count());
    }

    assert!(!book.chapters.is_empty(), "Book should have chapters");

    // 2. Paginate a chapter (e.g. Chapter 7)
    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let config = LayoutConfig {
        page_width: 1236,
        page_height: 1648,
        margin_left: 54,
        margin_right: 54,
        margin_top: 54,
        margin_bottom: 54,
        font_size: 8.5,
        line_spacing: 1.10,
        paragraph_spacing: 0.15,
        indent_em: 1.2,
        hyphenate: true,
    };

    // Target Chapter 7: "Reliable, Scalable, and Maintainable Applications"
    let target_chap = &book.chapters[7];

    let t2 = Instant::now();
    let (page_table, layouts) = yread::paginate::paginate_chapter_with_images(
        target_chap,
        Some(&book.image_sizes),
        &config,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );
    let paginate_time = t2.elapsed();

    println!("=== Pagination Info for Chapter 7: '{}' ===", target_chap.title);
    println!("Chapter char count: {}", target_chap.char_count());
    println!("Chapter block count: {}", target_chap.blocks.len());
    println!("Pagination time (cold cache): {:?}", paginate_time);
    println!("Page count: {}", page_table.page_count());

    // 3. Render page 0, page 1, and any page containing a diagram
    let mut raster = Rasterizer::new();
    let mut fb = vec![255u8; 1236 * 1648];

    // Find a page with an image
    let img_page_idx = layouts.iter().position(|layout| {
        layout.elements.iter().any(|el| matches!(el, yread::paginate::PageElement::Image { .. }))
    });

    let mut pages_to_render = vec![0, 1];
    if let Some(ip) = img_page_idx {
        if !pages_to_render.contains(&ip) {
            pages_to_render.push(ip);
        }
        println!("Found diagram on page {}", ip);
    }

    for p in pages_to_render {
        if p >= layouts.len() {
            continue;
        }
        let t3 = Instant::now();
        raster.render_page(
            &book,
            &layouts[p],
            &config,
            &fonts,
            &mut fb,
            1236,
        );
        let render_time = t3.elapsed();

        let dark_px = fb.iter().filter(|&&px| px < 128).count();
        println!("Page {} render time: {:?}, dark pixels: {}", p, render_time, dark_px);
        assert!(dark_px > 500, "Page should have rendered ink");

        let img_gray = image::GrayImage::from_raw(1236, 1648, fb.clone()).expect("GrayImage");
        let out_path = format!("/tmp/sample_ch7_page_{}.png", p);
        img_gray.save(&out_path).expect("save png");
        println!("Saved rendered page to {}", out_path);
    }
}
