//! Regression and repro test for glyph weight consistency across styles and multi-chapter sessions.
use yread::epub::parse_epub;
use yread::font::FontSystem;
use yread::model::FontStyle;
use yread::paginate::{paginate_chapter_with_images, LayoutConfig};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

const PROBE: &str = "Pacific Ocean";

/// Compute mean ink darkness (0.0 = white, 1.0 = solid black) inside a bounding box on the framebuffer.
fn glyph_darkness(fb: &[u8], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> (f32, usize) {
    let mut total_ink: u64 = 0;
    let mut pixel_count: usize = 0;
    for y in y0..y1 {
        let row = y * stride;
        for x in x0..x1 {
            let gray = fb[row + x];
            total_ink += (255 - gray) as u64;
            pixel_count += 1;
        }
    }
    if pixel_count == 0 {
        return (0.0, 0);
    }
    let mean_ink = (total_ink as f64 / (pixel_count as f64 * 255.0)) as f32;
    (mean_ink, total_ink as usize)
}

#[test]
fn test_glyph_weight_consistency_across_chapters() {
    let data = std::fs::read("/tmp/sample_book.epub").expect("scp the book to /tmp/sample_book.epub first");
    let book = parse_epub(&data).expect("parse");

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let cfg = LayoutConfig {
        page_width: 1236,
        page_height: 1648,
        margin_left: 72,
        margin_right: 72,
        margin_top: 164,
        margin_bottom: 122,
        font_size: 9.0,
        line_spacing: 1.2,
        paragraph_spacing: 0.25,
        indent_em: 1.2,
        hyphenate: true,
    };

    let (ci, ch) = book
        .chapters
        .iter()
        .enumerate()
        .find(|(_, c)| c.text.contains(PROBE))
        .expect("no chapter contains probe");

    // Render chapters 0..6 first on shared rasterizer, simulating a full reading session
    let mut raster = Rasterizer::new();
    for i in 0..ci {
        let (_pt, ch_layouts) = paginate_chapter_with_images(&book.chapters[i], Some(&book.image_sizes), &cfg, &fonts, &mut cache, None);
        for l in &ch_layouts {
            let mut fb = vec![255u8; 1236 * 1648];
            raster.render_page(&book, l, &cfg, &fonts, &mut fb, 1236);
        }
    }

    let (_pt, layouts) = paginate_chapter_with_images(ch, Some(&book.image_sizes), &cfg, &fonts, &mut cache, None);
    let mut fb_session = vec![255u8; 1236 * 1648];
    raster.render_page(&book, &layouts[0], &cfg, &fonts, &mut fb_session, 1236);

    // Fresh isolated render
    let mut clean_cache = ShapeCache::new();
    let (_pt_clean, clean_layouts) = paginate_chapter_with_images(ch, Some(&book.image_sizes), &cfg, &fonts, &mut clean_cache, None);
    let mut clean_raster = Rasterizer::new();
    let mut fb_clean = vec![255u8; 1236 * 1648];
    clean_raster.render_page(&book, &clean_layouts[0], &cfg, &fonts, &mut fb_clean, 1236);

    // Assert zero drift between long-running session and clean render
    let diff_count = fb_session.iter().zip(fb_clean.iter()).filter(|(a, b)| a != b).count();
    assert_eq!(diff_count, 0, "Rasterizer state leaked across chapters (pixel diff count: {})", diff_count);

    // Save rendered PNG for visual verification
    let img = image::GrayImage::from_raw(1236, 1648, fb_session.clone()).unwrap();
    img.save("/tmp/glyph_probe_p0.png").unwrap();

    // Verify per-glyph darkness metrics for probe words: "Pacific", "Ocean", "When"
    let origin_x = cfg.margin_left as f32;
    let origin_y = cfg.margin_top as f32;
    let mut probed_words = 0;

    for elem in &layouts[0].elements {
        if let yread::paginate::PageElement::Line { line, x, y } = elem {
            let mut cur_x = origin_x + x;
            let mut extra_space_per_gap = 0.0f32;
            match line.align {
                yread::model::TextAlign::Center => {
                    let slack = (line.max_width - line.width).max(0.0);
                    cur_x += slack / 2.0;
                }
                yread::model::TextAlign::Right => {
                    let slack = (line.max_width - line.width).max(0.0);
                    cur_x += slack;
                }
                yread::model::TextAlign::Justify => {
                    if !line.is_last_in_paragraph && line.width < line.max_width {
                        let space_count = line.items.iter().filter(|it| it.is_space()).count();
                        if space_count > 0 {
                            let slack = line.max_width - line.width;
                            if slack < line.max_width * 0.40 {
                                extra_space_per_gap = slack / space_count as f32;
                            }
                        }
                    }
                }
                yread::model::TextAlign::Left => {}
            }

            for item in &line.items {
                match item {
                    yread::line::LineItem::Word { byte_start, byte_end, shaped, style, .. } => {
                        let w_str = ch.text.get(*byte_start..*byte_end).unwrap_or("");
                        if w_str == "Pacific" || w_str == "Ocean," || w_str == "When" {
                            probed_words += 1;
                            let mut glyph_metrics = Vec::new();
                            let mut gx = cur_x;
                            for glyph in &shaped.glyphs {
                                let x0 = (gx + glyph.x_offset).max(0.0).round() as usize;
                                let x1 = (gx + glyph.x_offset + glyph.x_advance.max(5.0)).min(1235.0).round() as usize;
                                let y0 = (origin_y + y - 25.0).max(0.0).round() as usize;
                                let y1 = (origin_y + y + 10.0).min(1647.0).round() as usize;
                                let (mean, total) = glyph_darkness(&fb_session, 1236, x0, y0, x1, y1);
                                glyph_metrics.push((glyph.glyph_id, mean, total));
                                gx += glyph.x_advance;
                            }
                            eprintln!("Probe word {:?} ({:?}): {:?}", w_str, style.font_style, glyph_metrics);
                            // Ensure all non-space glyphs have sensible ink levels and no extreme spikes
                            for (gid, mean, total) in &glyph_metrics {
                                assert!(*total > 0, "Glyph {} in {:?} has no ink", gid, w_str);
                                assert!(*mean < 0.60, "Glyph {} in {:?} is excessively dark (mean: {})", gid, w_str, mean);
                            }
                        }
                        cur_x += shaped.advance;
                    }
                    yread::line::LineItem::HyphenatedPrefix { prefix_shaped, hyphen_adv, .. } => {
                        cur_x += prefix_shaped.advance + hyphen_adv;
                    }
                    yread::line::LineItem::Space { adv, .. } => {
                        cur_x += *adv + extra_space_per_gap;
                    }
                    yread::line::LineItem::HardBreak => {}
                }
            }
        }
    }
    assert!(probed_words >= 3, "Did not find all probe words on page 0");
}

#[test]
fn test_font_style_routing() {
    let fonts = FontSystem::default();
    assert_ne!(fonts.regular.data.as_ptr(), fonts.bold.data.as_ptr());
    assert_ne!(fonts.regular.data.as_ptr(), fonts.italic.data.as_ptr());
    assert_ne!(fonts.bold.data.as_ptr(), fonts.bold_italic.data.as_ptr());

    assert_eq!(fonts.face_for_style(FontStyle::Regular).data.as_ptr(), fonts.regular.data.as_ptr());
    assert_eq!(fonts.face_for_style(FontStyle::Bold).data.as_ptr(), fonts.bold.data.as_ptr());
    assert_eq!(fonts.face_for_style(FontStyle::Italic).data.as_ptr(), fonts.italic.data.as_ptr());
    assert_eq!(fonts.face_for_style(FontStyle::BoldItalic).data.as_ptr(), fonts.bold_italic.data.as_ptr());
}
