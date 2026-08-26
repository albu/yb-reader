use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use yread::epub::parse_epub_file;
use yread::font::FontSystem;
use yread::model::TextAlign;
use yread::paginate::{paginate_chapter_with_images, LayoutConfig};
use yread::paginate::{PageElement, PageLayout};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

fn main() {
    let args: Vec<String> = env::args().collect();
    let Some(epub_path) = args.get(1) else {
        eprintln!("Usage: rendertest <book.epub> [chapter_idx] [gallery [outdir]]");
        return;
    };
    let target_ch_idx: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let gallery = args.get(3).map(|s| s.as_str()) == Some("gallery");
    let gallery_dir = args.get(4).cloned().unwrap_or_else(|| "/tmp/gallery".to_string());

    println!("============================================================");
    println!("PROFILING EPUB: {}", epub_path);
    println!("============================================================");

    if !Path::new(epub_path).exists() {
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
        if entry.chapter_idx == target_ch_idx
            || entry.title.to_lowercase().contains("wal")
            || entry.title.to_lowercase().contains("replication")
        {
            println!(
                "   TOC #{}: ch={} char={} level={} '{}'",
                i, entry.chapter_idx, entry.char_offset, entry.level, entry.title
            );
        }
    }

    // 2. Find target chapter info
    if target_ch_idx >= book.chapters.len() {
        eprintln!(
            "Chapter {} out of bounds (max {})",
            target_ch_idx,
            book.chapters.len() - 1
        );
        return;
    }

    let chapter = &book.chapters[target_ch_idx];
    println!("\n2. Target Chapter [#{}]", target_ch_idx);
    println!("   Title: '{}'", chapter.title);
    println!("   Character Count: {}", chapter.char_count());
    println!(
        "   Word Count (approx): {}",
        chapter.text.split_whitespace().count()
    );
    println!("   Block Elements: {}", chapter.blocks.len());

    if gallery {
        let fonts = FontSystem::default();
        gallery_mode(&book, chapter, target_ch_idx, &fonts, &gallery_dir);
        return;
    }

    // 3. Font System Initialization
    let t_font = Instant::now();
    let fonts = FontSystem::default();
    let font_duration = t_font.elapsed();
    println!("\n3. Font System Init: {:.2?}", font_duration);

    // 4. Layout Config (PW5 1236x1648 portrait) — same recipe the app
    // uses (LayoutConfig::reader) with the app's DEFAULT settings; a
    // hand-rolled config stopped representing device layout once the
    // two drifted.
    let cfg = LayoutConfig::reader(
        1236,
        1648,
        72,
        16.0,
        1.25,
        true,
        0.25,
        1.2,
        true,
        yread::model::TextAlign::Justify,
        1.0,
        0.0,
    );

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
        println!(
            "7. Rasterize Mid Page (Page {}): {:.2?}",
            mid_idx, rast_mid_duration
        );
    }

    println!("\n============================================================");
    println!(
        "SUMMARY: Time to first glass (Parse + Paginate + Render): {:.2?}",
        epub_parse_duration + pag_duration + Duration::from_millis(5)
    );
    println!("============================================================");
}

/// The verification gallery: render one chapter across a curated settings
/// matrix to PNGs, and report the invariants the whole typography arc
/// leans on (extent, justified rendered extent, rivers, hyphen hygiene).
/// This is the "eyes + numbers" gate real-book verification is made of —
/// run it before and after any breaker/justification change.
fn gallery_mode(
    book: &yread::Book,
    chapter: &yread::Chapter,
    chapter_idx: usize,
    fonts: &FontSystem,
    outdir: &str,
) {
    let _ = fs::create_dir_all(outdir);

    let mk = |font_size: f32,
              margin: u32,
              line_spacing: f32,
              align: TextAlign,
              hyphenate: bool,
              indent_em: f32,
              paragraph_spacing: f32,
              word_spacing_mult: f32,
              letter_spacing_px: f32|
     -> LayoutConfig {
        LayoutConfig::reader(
            1236, 1648, margin, font_size, line_spacing, true, paragraph_spacing, indent_em,
            hyphenate, align, word_spacing_mult, letter_spacing_px,
        )
    };

    // Curated matrix: baseline + one-axis variations (12 renders).
    let matrix: Vec<(&str, LayoutConfig)> = vec![
        ("base", mk(11.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("fs09", mk(9.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("fs13", mk(13.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("fs16", mk(16.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("margin36", mk(11.0, 36, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("margin108", mk(11.0, 108, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("ls10", mk(11.0, 72, 1.0, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("ls14", mk(11.0, 72, 1.4, TextAlign::Justify, true, 1.2, 0.25, 1.0, 0.0)),
        ("align_left", mk(11.0, 72, 1.2, TextAlign::Left, true, 1.2, 0.25, 1.0, 0.0)),
        ("hyphen_off", mk(11.0, 72, 1.2, TextAlign::Justify, false, 1.2, 0.25, 1.0, 0.0)),
        ("indent0", mk(11.0, 72, 1.2, TextAlign::Justify, true, 0.0, 0.25, 1.0, 0.0)),
        ("para060", mk(11.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.60, 1.0, 0.0)),
        ("wordspace", mk(11.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.15, 0.0)),
        ("tracking", mk(11.0, 72, 1.2, TextAlign::Justify, true, 1.2, 0.25, 1.0, 1.0)),
    ];

    println!("\n=== GALLERY (ch {}, {} configs -> {}) ===", chapter_idx, matrix.len(), outdir);
    println!(
        "(rivers = per-gap stretch / that line's minimum space advance; ext_* exclude over-wide words)"
    );
    println!(
        "{:<12} {:>5} {:>8} {:>7} {:>9} {:>7} {:>8} {:>7} {:>6}",
        "config", "pages", "ext_over", "max_px", "render_over", "rivers95", "riversmax", "hyphens", "ladder"
    );

    for (slug, config) in matrix {
        let mut cache = ShapeCache::new();
        let (_pt, layouts) = paginate_chapter_with_images(
            chapter,
            Some(&book.image_sizes),
            &config,
            fonts,
            &mut cache,
            Some(yread::hypher_lang(&book.meta.language)),
        );

        // Render the first page to a PNG for eyeballing.
        if let Some(page) = layouts.first() {
            let mut raster = Rasterizer::new();
            let w = config.page_width as usize;
            let h = config.page_height as usize;
            let mut fb = vec![255u8; w * h];
            raster.render_page(book, page, &config, fonts, &mut fb, w);
            let out = format!("{}/ch{}_p{}_{}.png", outdir, chapter_idx, page.page_idx, slug);
            save_gray_png(&out, w, h, &fb);
        }

        let s = collect_stats(&layouts);
        println!(
            "{:<12} {:>5} {:>8} {:>7.1} {:>9} {:>7.2} {:>8.2} {:>7} {:>6}",
            slug,
            s.pages,
            s.ext_nat_over,
            s.ext_nat_max,
            s.ext_render_over,
            s.gap_ratio_p95,
            s.gap_ratio_max,
            s.hyphen_ends,
            s.hyphen_ladder_max,
        );
    }
}

#[derive(Default)]
struct GalleryStats {
    pages: usize,
    ext_nat_over: usize,
    ext_nat_max: f32,
    ext_render_over: usize,
    gap_ratio_p95: f32,
    gap_ratio_max: f32,
    hyphen_ends: usize,
    hyphen_ladder_max: usize,
}

/// Walk every line of every page and measure the invariants the
/// typography arc depends on:
///   - natural extent: line.width > max_width (over-wide words / drift)
///   - rendered extent: justified line lands past max_width (must be 0)
///   - rivers: per-gap stretch as a fraction of the space width
///   - hyphen hygiene: hyphenated line ends and the longest ladder
fn collect_stats(layouts: &[PageLayout]) -> GalleryStats {
    let mut s = GalleryStats::default();
    let mut ratios: Vec<f32> = Vec::new();
    let mut ladder = 0usize;
    let mut cur_ladder = 0usize;

    for layout in layouts {
        s.pages += 1;
        for elem in &layout.elements {
            let PageElement::Line { line, .. } = elem else {
                continue;
            };
            // A single item wider than the measure (URL, code token) is
            // the pre-existing over-wide-word class — no gaps can pull it
            // back, so it must be exempt from BOTH extent counters or the
            // table cries wolf on ragged configs.
            let max_item = line.items.iter().map(|it| it.advance()).fold(0.0, f32::max);
            let over = line.width - line.max_width;
            if over > 0.5 && max_item <= line.max_width {
                s.ext_nat_over += 1;
                s.ext_nat_max = s.ext_nat_max.max(over);
            }
            let gaps = line.items.iter().filter(|it| it.is_space()).count() as f32;
            let rendered = line.width + gaps * line.extra_space;
            if rendered > line.max_width + 0.01 && max_item <= line.max_width {
                s.ext_render_over += 1;
            }
            let hyp_here = line.items.iter().any(|it| {
                matches!(
                    it,
                    yread::line::LineItem::HyphenatedPrefix { .. } | yread::line::LineItem::Hyphen { .. }
                )
            });
            if hyp_here {
                s.hyphen_ends += 1;
                cur_ladder += 1;
                ladder = ladder.max(cur_ladder);
            } else {
                cur_ladder = 0;
            }

            if line.is_last_in_paragraph
                || line.align != TextAlign::Justify
                || line.extra_space <= 0.0
            {
                continue;
            }
            let min_space = line
                .items
                .iter()
                .filter_map(|it| match it {
                    yread::line::LineItem::Space { adv, .. } => Some(*adv),
                    _ => None,
                })
                .fold(f32::MAX, f32::min);
            if min_space != f32::MAX {
                ratios.push(line.extra_space / min_space);
            }
        }
    }

    s.hyphen_ladder_max = ladder;
    if !ratios.is_empty() {
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = ratios.len();
        s.gap_ratio_p95 = ratios[((n as f32) * 0.95).floor().min((n - 1) as f32) as usize];
        s.gap_ratio_max = ratios[n - 1];
    }
    s
}

/// Write an 8-bit grayscale framebuffer as a PNG (same path fb2png uses).
fn save_gray_png(path: &str, w: usize, h: usize, fb: &[u8]) {
    let file = match fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gallery: cannot write {}: {}", path, e);
            return;
        }
    };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = match enc.write_header() {
        Ok(w) => w,
        Err(_) => return,
    };
    let _ = writer.write_image_data(fb);
}
