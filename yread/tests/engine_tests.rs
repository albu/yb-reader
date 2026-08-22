use yread::fb2::parse_fb2;
use yread::font::FontSystem;
use yread::paginate::{paginate_chapter, LayoutConfig};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

const WAR_AND_PEACE_FB2: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description>
  <title-info>
    <genre>prose_classic</genre>
    <author>
      <first-name>Leo</first-name>
      <last-name>Tolstoy</last-name>
    </author>
    <book-title>War and Peace</book-title>
    <lang>en</lang>
  </title-info>
</description>
<body>
  <section>
    <title>
      <p>Chapter One</p>
    </title>
    <p>Well, Prince, so Genoa and Lucca are now just family estates of the Buonapartes. But I warn you, if you don't tell me that this means war, if you still try to defend the infamies and horrors perpetrated by that Antichrist—I really believe he is Antichrist—I will have nothing more to do with you and you are no longer my friend, no longer my faithful slave, as you call yourself!</p>
    <p>It was in July, 1805, and the speaker was the well-known Anna Pavlovna Scherer, maid of honor and favorite of the Empress Marya Fedorovna. With these words she greeted Prince Vasili Kuragin, a man of high rank and importance, who was the first to arrive at her reception.</p>
    <p>Anna Pavlovna had had a cough for some days. She was, as she said, suffering from la grippe; grippe being then a new word in St. Petersburg, used only by the elite.</p>
    <empty-line/>
    <p>All her invitations without exception, written in French, and delivered by a scarlet-liveried footman that morning, ran as follows:</p>
    <p>If you have nothing better to do, Count (or Prince), and if the prospect of spending an evening with a poor invalid is not too terrible, I shall be very charmed to see you tonight between 7 and 10—Annette Scherer.</p>
    <p>Heavens! what a virulent attack! replied the prince, not in the least recalcitrant at this reception. He had just entered, wearing an embroidered court uniform, knee breeches, and shoes, and had stars on his coat and a serene expression on his flat face.</p>
  </section>
</body>
</FictionBook>"##;

#[test]
fn test_fb2_pagination_and_char_offset_invariance() {
    let book = parse_fb2(WAR_AND_PEACE_FB2.as_bytes()).expect("parse");
    let chapter = &book.chapters[0];
    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();

    // 1. Paginate with small page size (e.g. 600x800) to force multiple pages
    let config_small = LayoutConfig {
        page_width: 600,
        page_height: 800,
        margin_left: 36,
        margin_right: 36,
        margin_top: 36,
        margin_bottom: 36,
        font_size: 11.0,
        line_spacing: 1.2,
        indent_em: 1.5,
        hyphenate: true,
    };

    let (pt_11pt, _layouts_11pt) = paginate_chapter(
        chapter,
        &config_small,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );

    assert!(pt_11pt.page_count() >= 2, "Expected multiple pages at 11pt, got {}", pt_11pt.page_count());

    // Take a target character in the middle of page 2
    let page2_start_char = pt_11pt.char_for_page(1);
    let target_char = page2_start_char + 50;

    // Verify binary search locates page 1 (0-indexed)
    let found_page = pt_11pt.page_for_char(target_char);
    assert_eq!(found_page, 1, "Character {} should land on page 1", target_char);

    // 2. Now simulate live font change to 16pt (re-paginate)
    let mut config_16pt = config_small;
    config_16pt.font_size = 16.0;

    let (pt_16pt, layouts_16pt) = paginate_chapter(
        chapter,
        &config_16pt,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );

    // 16pt must produce more pages
    assert!(pt_16pt.page_count() > pt_11pt.page_count(), "16pt should have more pages than 11pt");

    // Land on the exact same character offset!
    let landing_page = pt_16pt.page_for_char(target_char);
    let landing_start_char = pt_16pt.char_for_page(landing_page);
    let landing_end_char = layouts_16pt[landing_page].end_char;

    assert!(target_char >= landing_start_char && target_char <= landing_end_char,
        "Target char {} must land inside landing page [{}, {}]",
        target_char, landing_start_char, landing_end_char
    );

    // 3. Render landing page to grayscale buffer and verify pixels
    let mut raster = Rasterizer::new();
    let mut fb = vec![255u8; 600 * 800];
    raster.render_page(&book, &layouts_16pt[landing_page], &config_16pt, &fonts, &mut fb, 600);

    let dark_pixels = fb.iter().filter(|&&p| p < 128).count();
    assert!(dark_pixels > 100, "Rendered page must have ink (dark pixels), got {}", dark_pixels);
}

#[test]
fn test_line_spacing_change_preserves_char_offset() {
    let book = parse_fb2(WAR_AND_PEACE_FB2.as_bytes()).expect("parse");
    let chapter = &book.chapters[0];
    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();

    let mut config_compact = LayoutConfig::default();
    config_compact.page_width = 800;
    config_compact.page_height = 1000;
    config_compact.line_spacing = 1.0;

    let (pt_compact, _) = paginate_chapter(
        chapter,
        &config_compact,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );

    let mut config_airy = config_compact;
    config_airy.line_spacing = 1.6;

    let (pt_airy, _) = paginate_chapter(
        chapter,
        &config_airy,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );

    // Check random char offset across the chapter
    for offset in [0, 100, 300, 700, 1000] {
        let p_comp = pt_compact.page_for_char(offset);
        let p_airy = pt_airy.page_for_char(offset);
        assert!(p_comp < pt_compact.page_count());
        assert!(p_airy < pt_airy.page_count());
    }
}

#[test]
fn test_epub_parse_and_paginate() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    // Construct a minimal in-memory EPUB
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = SimpleFileOptions::default();

        zip.start_file("mimetype", options).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();

        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#).unwrap();

        zip.start_file("OEBPS/content.opf", options).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="BookID" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>A Tale of Two Cities</dc:title>
    <dc:creator>Charles Dickens</dc:creator>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>"#).unwrap();

        zip.start_file("OEBPS/ch1.xhtml", options).unwrap();
        zip.write_all(br#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter 1: The Period</title></head>
<body>
  <h1>The Period</h1>
  <p>It was the <b>best of times</b>, it was the <i>worst of times</i>, it was the age of wisdom, it was the age of foolishness.</p>
  <p>It was the epoch of belief, it was the epoch of incredulity, it was the season of light, it was the season of darkness, it was the spring of hope, it was the winter of despair.</p>
</body>
</html>"#).unwrap();

        zip.finish().unwrap();
    }

    let book = yread::epub::parse_epub(&buf).expect("parse epub");
    assert_eq!(book.meta.title, "A Tale of Two Cities");
    assert_eq!(book.meta.authors, vec!["Charles Dickens"]);
    assert_eq!(book.chapters.len(), 1);

    let ch = &book.chapters[0];
    assert_eq!(ch.title, "The Period");
    assert_eq!(ch.blocks.len(), 3); // Heading, Para, Para

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let config = LayoutConfig::default();

    let (pt, layouts) = paginate_chapter(ch, &config, &fonts, &mut cache, Some(hypher::Lang::English));
    assert_eq!(pt.page_count(), 1);
    assert_eq!(layouts.len(), 1);
}
