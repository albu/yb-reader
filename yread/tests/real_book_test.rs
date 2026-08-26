use std::fs;
use std::path::Path;
use std::time::Instant;

use yread::epub::parse_epub;
use yread::font::FontSystem;
use yread::model::TextAlign;
use yread::paginate::{paginate_chapter_with_images, LayoutConfig, PageElement};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

#[test]
fn test_render_real_sample_book() {
    let default_path = "/tmp/sample_book.epub".to_string();
    let book_path = std::env::var("YB_TEST_EPUB").unwrap_or(default_path);
    if !Path::new(&book_path).exists() {
        return;
    }

    // 1. Time EPUB reading and parsing
    let t0 = Instant::now();
    let data = fs::read(book_path).expect("read epub file");
    let read_time = t0.elapsed();

    let t1 = Instant::now();
    let book = parse_epub(&data).expect("parse sample epub");
    let parse_time = t1.elapsed();

    println!("=== Sample Book EPUB Parse Info ===");
    println!("File size: {} MB", data.len() / (1024 * 1024));
    println!("File read time: {:?}", read_time);
    println!("Book parse time: {:?}", parse_time);
    println!("Title: '{}'", book.meta.title);
    println!("Authors: {:?}", book.meta.authors);
    println!("Language: '{}'", book.meta.language);
    println!("Chapters: {}", book.chapters.len());
    println!("TOC Entries: {}", book.toc.len());
    for (t_idx, entry) in book.toc.iter().enumerate().take(20) {
        println!(
            "  TOC #{}: '{:indent$}{}' (ch={}, char={})",
            t_idx,
            "",
            entry.title,
            entry.chapter_idx,
            entry.char_offset,
            indent = entry.level * 2
        );
    }
    println!("Total images: {}", book.images.len());
    println!("Total chars: {}", book.total_chars());

    for (idx, ch) in book.chapters.iter().enumerate().take(15) {
        println!(
            "  Chapter #{}: '{}' (blocks={}, chars={})",
            idx,
            ch.title,
            ch.blocks.len(),
            ch.char_count()
        );
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
        body_align: TextAlign::Justify,
        word_spacing_mult: 1.0,
        letter_spacing_px: 0.0,
    };

    // Target Chapter 7: "Reliable, Scalable, and Maintainable Applications"
    // (guard: books with fewer chapters — including this test's own
    // synthetic fixtures — must not panic on the hardcoded index).
    let Some(target_chap) = book.chapters.get(7) else {
        println!("Chapter 7 absent — skipping chapter-7 render");
        return;
    };

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

    println!(
        "=== Pagination Info for Chapter 7: '{}' ===",
        target_chap.title
    );
    println!("Chapter char count: {}", target_chap.char_count());
    println!("Chapter block count: {}", target_chap.blocks.len());
    println!("Pagination time (cold cache): {:?}", paginate_time);
    println!("Page count: {}", page_table.page_count());

    // 3. Render page 0, page 1, and any page containing a diagram
    let mut raster = Rasterizer::new();
    let mut fb = vec![255u8; 1236 * 1648];

    // Find a page with an image
    let img_page_idx = layouts.iter().position(|layout| {
        layout
            .elements
            .iter()
            .any(|el| matches!(el, yread::paginate::PageElement::Image { .. }))
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
        raster.render_page(&book, &layouts[p], &config, &fonts, &mut fb, 1236);
        let render_time = t3.elapsed();

        let dark_px = fb.iter().filter(|&&px| px < 128).count();
        println!(
            "Page {} render time: {:?}, dark pixels: {}, element count: {}",
            p,
            render_time,
            dark_px,
            layouts[p].elements.len()
        );
        for elem in &layouts[p].elements {
            match elem {
                yread::paginate::PageElement::CircleBullet { x, y, radius } => {
                    println!(
                        "  -> CircleBullet at x={:.1}, y={:.1}, r={:.1}",
                        x, y, radius
                    );
                }
                yread::paginate::PageElement::Bullet { x, y, size_pt, .. } => {
                    println!(
                        "  -> TextBullet at x={:.1}, y={:.1}, size={:.1}",
                        x, y, size_pt
                    );
                }
                yread::paginate::PageElement::QuoteBar { x, y0, y1 } => {
                    println!("  -> QuoteBar at x={:.1}, y0={:.1}, y1={:.1}", x, y0, y1);
                }
                _ => {}
            }
        }
    }

    // Search for "Data Structures That Power" across all chapters
    println!("\n=== Searching for 'Data Structures That Power' across all chapters ===");
    for (idx, ch) in book.chapters.iter().enumerate() {
        if let Some(pos) = ch.text.to_lowercase().find("data structures that power") {
            println!(
                "Found match in Chapter #{}: '{}' at char {}",
                idx, ch.title, pos
            );
            let snippet_start = pos.saturating_sub(50);
            let snippet_end = (pos + 100).min(ch.text.len());
            println!(
                "Context snippet:\n\"{}\"\n",
                &ch.text[snippet_start..snippet_end]
            );

            // Paginate this chapter and find which page it lands on
            let (pt, louts) = yread::paginate::paginate_chapter_with_images(
                ch,
                Some(&book.image_sizes),
                &config,
                &fonts,
                &mut cache,
                Some(hypher::Lang::English),
            );
            let p_idx = pt.page_for_char(pos);
            println!("Match lands on Page {} of Chapter #{}", p_idx, idx);

            // Render that page to see how it looks
            let mut page_fb = vec![255u8; 1236 * 1648];
            raster.render_page(&book, &louts[p_idx], &config, &fonts, &mut page_fb, 1236);
            let out_p = format!("/tmp/outline_ch{}_page_{}.png", idx, p_idx);
            let img = image::GrayImage::from_raw(1236, 1648, page_fb).unwrap();
            img.save(&out_p).unwrap();
            println!("Rendered and saved to {}", out_p);

            // Inspect elements on this page
            for elem in &louts[p_idx].elements {
                if let yread::paginate::PageElement::Line { line, .. } = elem {
                    let mut s = String::new();
                    for it in &line.items {
                        match it {
                            yread::line::LineItem::Word {
                                byte_start,
                                byte_end,
                                ..
                            } => {
                                if let Some(w) = ch.text.get(*byte_start..*byte_end) {
                                    s.push_str(w);
                                }
                            }
                            yread::line::LineItem::Space { .. } => s.push(' '),
                            yread::line::LineItem::SoftHyphen { .. } => {}
                            yread::line::LineItem::Hyphen { .. } => s.push('-'),
                            yread::line::LineItem::HardBreak => {}
                            yread::line::LineItem::HyphenatedPrefix {
                                byte_start,
                                byte_end,
                                ..
                            } => {
                                if let Some(w) = ch.text.get(*byte_start..*byte_end) {
                                    s.push_str(w);
                                    s.push('-');
                                }
                            }
                        }
                    }
                    println!("  Line: \"{}\"", s);
                    println!(
                        "  Line details: align={:?}, width={}, max_width={}",
                        line.align, line.width, line.max_width
                    );
                    if false {
                        for it in &line.items {
                            match it {
                                yread::line::LineItem::Word {
                                    byte_start,
                                    byte_end,
                                    shaped,
                                    style,
                                    ..
                                } => {
                                    let w = ch.text.get(*byte_start..*byte_end).unwrap_or("");
                                    println!("    Word '{}' (bytes {}..{}, adv={}, size_mult={}, font_style={:?})", w, byte_start, byte_end, shaped.advance, style.size_mult, style.font_style);
                                    for g in &shaped.glyphs {
                                        println!(
                                            "      glyph id={}, adv={}",
                                            g.glyph_id, g.x_advance
                                        );
                                    }
                                }
                                yread::line::LineItem::Space { adv, .. } => {
                                    println!("    Space (adv={})", adv);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The real-book verification gate. Opt-in via YB_TEST_EPUB (same pattern
/// as test_render_real_sample_book): every breaker/justification change
/// must keep the width-model invariants green on a real book across
/// several chapters × a settings matrix — the repo's own "revive only
/// against per-font-size real-book verification" gate from the
/// Knuth-Plass removal commit.
#[test]
fn test_real_book_typography_properties() {
    let default_path = "/tmp/sample_book.epub".to_string();
    let book_path = std::env::var("YB_TEST_EPUB").unwrap_or(default_path);
    if !Path::new(&book_path).exists() {
        return;
    }
    let data = fs::read(&book_path).expect("read epub");
    let book = parse_epub(&data).expect("parse");
    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();

    // Same recipe as the app (LayoutConfig::reader): a hand-rolled config
    // stopped representing device layout once the two drifted — the exact
    // drift the gallery exists to prevent.
    let mk = |fs: f32, margin: u32, ls: f32, align: TextAlign, hyphen: bool| {
        LayoutConfig::reader(
            1236, 1648, margin, fs, ls, true, 0.25, 1.2, hyphen, align, 1.0, 0.0,
        )
    };
    let configs: Vec<(&str, LayoutConfig)> = vec![
        ("base", mk(11.0, 72, 1.2, TextAlign::Justify, true)),
        ("fs13", mk(13.0, 72, 1.2, TextAlign::Justify, true)),
        ("fs16", mk(16.0, 72, 1.2, TextAlign::Justify, true)),
        ("margin36", mk(11.0, 36, 1.2, TextAlign::Justify, true)),
        ("margin108", mk(11.0, 108, 1.2, TextAlign::Justify, true)),
        ("align_left", mk(11.0, 72, 1.2, TextAlign::Left, true)),
        ("hyphen_off", mk(11.0, 72, 1.2, TextAlign::Justify, false)),
    ];

    let chapter_idx: Vec<usize> = (0..book.chapters.len().min(8)).collect();
    let lang = yread::hypher_lang(&book.meta.language);

    for &ci in &chapter_idx {
        let chapter = &book.chapters[ci];
        for (name, config) in &configs {
            let (_pt, layouts) = paginate_chapter_with_images(
                chapter,
                Some(&book.image_sizes),
                config,
                &fonts,
                &mut cache,
                Some(lang),
            );
            assert!(
                !layouts.is_empty(),
                "ch{} {}: empty pagination",
                ci,
                name
            );
            for (pi, layout) in layouts.iter().enumerate() {
                for elem in &layout.elements {
                    let PageElement::Line { line, .. } = elem else {
                        continue;
                    };
                    // A single item wider than the measure (URL, code
                    // token) is the pre-existing over-wide-word class —
                    // no gaps can pull it back, so it is exempt.
                    let max_item = line.items.iter().map(|it| it.advance()).fold(0.0, f32::max);
                    let over = line.width - line.max_width;
                    if over > 2.5 && max_item <= line.max_width {
                        panic!(
                            "ch{} {} p{}: natural width {:.1} exceeds measure {:.1} by {:.1}px",
                            ci,
                            name,
                            pi,
                            line.width,
                            line.max_width,
                            over
                        );
                    }

                    if line.align != TextAlign::Justify || line.is_last_in_paragraph {
                        continue;
                    }
                    let gaps = line.items.iter().filter(|it| it.is_space()).count() as f32;
                    if gaps == 0.0 {
                        continue;
                    }
                    let rendered = line.width + gaps * line.extra_space;
                    if max_item <= line.max_width {
                        assert!(
                            rendered <= line.max_width + 0.01,
                            "ch{} {} p{}: justified rendered width {:.2} exceeds measure {:.2}",
                            ci,
                            name,
                            pi,
                            rendered,
                            line.max_width
                        );
                        if line.extra_space != 0.0 {
                            assert!(
                                (rendered - line.max_width).abs() < 0.01,
                                "ch{} {} p{}: justified line renders {:.2}, measure {:.2}",
                                ci,
                                name,
                                pi,
                                rendered,
                                line.max_width
                            );
                        }
                    }
                }
            }
        }
    }
}
