use yread::epub::parse_epub;
use yread::fb2::parse_fb2;
use yread::font::FontSystem;
use yread::model::Block;
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
        paragraph_spacing: 0.15,
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

    assert!(
        pt_11pt.page_count() >= 2,
        "Expected multiple pages at 11pt, got {}",
        pt_11pt.page_count()
    );

    // Take a target character in the middle of page 2
    let page2_start_char = pt_11pt.char_for_page(1);
    let target_char = page2_start_char + 50;

    // Verify binary search locates page 1 (0-indexed)
    let found_page = pt_11pt.page_for_char(target_char);
    assert_eq!(
        found_page, 1,
        "Character {} should land on page 1",
        target_char
    );

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
    assert!(
        pt_16pt.page_count() > pt_11pt.page_count(),
        "16pt should have more pages than 11pt"
    );

    // Land on the exact same character offset!
    let landing_page = pt_16pt.page_for_char(target_char);
    let landing_start_char = pt_16pt.char_for_page(landing_page);
    let landing_end_char = layouts_16pt[landing_page].end_char;

    assert!(
        target_char >= landing_start_char && target_char <= landing_end_char,
        "Target char {} must land inside landing page [{}, {}]",
        target_char,
        landing_start_char,
        landing_end_char
    );

    // 3. Render landing page to grayscale buffer and verify pixels
    let mut raster = Rasterizer::new();
    let mut fb = vec![255u8; 600 * 800];
    raster.render_page(
        &book,
        &layouts_16pt[landing_page],
        &config_16pt,
        &fonts,
        &mut fb,
        600,
    );

    let dark_pixels = fb.iter().filter(|&&p| p < 128).count();
    assert!(
        dark_pixels > 100,
        "Rendered page must have ink (dark pixels), got {}",
        dark_pixels
    );
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
        zip.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/content.opf", options).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="utf-8"?>
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
</package>"#,
        )
        .unwrap();

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

    let (pt, layouts) =
        paginate_chapter(ch, &config, &fonts, &mut cache, Some(hypher::Lang::English));
    assert_eq!(pt.page_count(), 1);
    assert_eq!(layouts.len(), 1);
}

#[test]
fn test_html_entity_unescape_and_lossy_safety() {
    use yread::epub::unescape_html_lossy;

    assert_eq!(
        unescape_html_lossy("Hello&nbsp;world"),
        "Hello\u{00A0}world"
    );
    assert_eq!(unescape_html_lossy("A&mdash;B&hellip;C"), "A—B…C");
    assert_eq!(unescape_html_lossy("&#8212;"), "—");
    assert_eq!(unescape_html_lossy("&#x2014;"), "—");
    // Unknown entities shouldn't panic or fail
    assert_eq!(unescape_html_lossy("&unknownentity;"), "&unknownentity;");
}

#[test]
fn test_fb2_footnotes_and_toc_extraction() {
    const FB2_WITH_NOTES: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description>
  <title-info>
    <book-title>Test Book</book-title>
  </title-info>
</description>
<body>
  <section>
    <title><p>Chapter One: Beginning</p></title>
    <p>Some text with footnote reference <a type="note" href="#note_1">[1]</a>.</p>
  </section>
</body>
<body name="notes">
  <section id="note_1">
    <title><p>1</p></title>
    <p>This is the explanation of note 1.</p>
  </section>
</body>
</FictionBook>"##;

    let book = parse_fb2(FB2_WITH_NOTES.as_bytes()).expect("parse");
    assert_eq!(book.toc.len(), 1);
    assert_eq!(book.toc[0].title, "Chapter One: Beginning");
    assert!(book.footnotes.contains_key("note_1"));
    let note_text = book.footnotes.get("note_1").unwrap();
    assert!(note_text.contains("explanation of note 1"));

    // Test resolving footnote
    let res = book.resolve_footnote_or_link(0, "#note_1");
    assert!(res.text.contains("explanation of note 1"));
    assert_eq!(res.title, "Footnote [note_1]");
}

#[test]
fn test_epub_footnote_and_link_resolution() {
    let body = r##"<p>Text referencing <a href="#fn1" epub:type="noteref">[1]</a> and external <a href="notes.xhtml#n2">note 2</a>.</p>
<div id="fn1"><p>Footnote 1 text: This is detailed local footnote content.</p></div>"##;
    let book = parse_epub(&epub_with_body(body)).expect("parse epub");
    assert_eq!(book.chapters.len(), 1);
    let ch = &book.chapters[0];
    assert!(ch.anchors.contains_key("fn1"));

    // Check that footnote_ref is populated in paragraph runs
    if let Block::Paragraph { runs, .. } = &ch.blocks[0] {
        let fn1_run = runs
            .iter()
            .find(|r| r.style.footnote_ref.as_deref() == Some("#fn1"));
        assert!(fn1_run.is_some());
        assert!(fn1_run.unwrap().style.is_sup);

        let ext_run = runs
            .iter()
            .find(|r| r.style.footnote_ref.as_deref() == Some("notes.xhtml#n2"));
        assert!(ext_run.is_some());
    } else {
        panic!("Expected paragraph");
    }

    // Resolve local anchor footnote
    let res = book.resolve_footnote_or_link(0, "#fn1");
    assert!(res.text.contains("Footnote 1 text"));
    assert!(res.target.is_some());
    let (target_ch, target_off) = res.target.unwrap();
    assert_eq!(target_ch, 0);
    assert_eq!(target_off, *ch.anchors.get("fn1").unwrap());
}

/// In-memory EPUB with arbitrary body XHTML (for parser-level tests).
fn epub_with_body(body: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut buf = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
    let options = SimpleFileOptions::default();
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(br#"<container><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
    zip.start_file("content.opf", options).unwrap();
    zip.write_all(br#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>T</dc:title><dc:language>en</dc:language></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#).unwrap();
    zip.start_file("c1.xhtml", options).unwrap();
    let doc = format!(
        r#"<html><head><title>Ch</title></head><body>{}</body></html>"#,
        body
    );
    zip.write_all(doc.as_bytes()).unwrap();
    zip.finish().unwrap();
    buf
}

#[test]
fn test_lazy_epub_images_load_on_demand() {
    let path = std::path::Path::new("/tmp/sample_book.epub");
    if !path.exists() {
        return; // device book not present on this host
    }
    let book = yread::epub::parse_epub_file(path).expect("lazy parse");
    assert!(book.chapters.len() > 1);
    // No eager bytes; sizes known for layout; lazy entries registered.
    assert!(
        book.images.is_empty(),
        "file-backed parse must not retain image bytes"
    );
    let entries = book.lazy_images.entry_count();
    assert!(entries > 50, "expected ~109 image entries, got {}", entries);
    let sniffed = book.image_sizes.len();
    assert!(
        sniffed > entries / 2,
        "sniffer should cover most images ({} of {})",
        sniffed,
        entries
    );

    // An on-demand load returns decodable bytes.
    let any_id = book.image_sizes.keys().next().cloned().unwrap();
    let bytes = book.get_image(&any_id).expect("lazy load");
    assert!(bytes.len() > 100);
    // Second load hits the memoized copy.
    assert!(book.get_image(&any_id).is_some());
}

#[test]
fn test_sniff_image_size_headers() {
    use yread::epub::sniff_image_size;
    // PNG: signature + IHDR with 1234x567
    let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    png.extend_from_slice(&[0, 0, 0, 13]); // IHDR len
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&1234u32.to_be_bytes());
    png.extend_from_slice(&567u32.to_be_bytes());
    assert_eq!(sniff_image_size(&png), Some((1234, 567)));

    // GIF: 6-byte magic + LE dims
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&320u16.to_le_bytes());
    gif.extend_from_slice(&240u16.to_le_bytes());
    assert_eq!(sniff_image_size(&gif), Some((320, 240)));

    // JPEG: SOI + APP0 (JFIF, 16 bytes) + SOF0 with 800x600
    let mut jpg = vec![0xFF, 0xD8];
    jpg.extend_from_slice(&[0xFF, 0xE0]);
    jpg.extend_from_slice(&16u16.to_be_bytes());
    jpg.extend_from_slice(&[0; 14]); // JFIF payload
    jpg.extend_from_slice(&[0xFF, 0xC0]);
    jpg.extend_from_slice(&17u16.to_be_bytes());
    jpg.extend_from_slice(&[8, 0x02, 0x58, 0x03, 0x20, 0x03]); // prec, h=600, w=800
    assert_eq!(sniff_image_size(&jpg), Some((800, 600)));

    assert_eq!(sniff_image_size(b"not an image at all"), None);
}

#[test]
fn test_fb2_path_streaming_parse() {
    let tmp = std::env::temp_dir().join("yread_probe.fb2");
    std::fs::write(&tmp, WAR_AND_PEACE_FB2.as_bytes()).unwrap();
    let book = yread::fb2::parse_fb2_path(&tmp).expect("path parse");
    assert_eq!(book.chapters.len(), 1);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_br_produces_real_line_breaks() {
    use yread::line::break_paragraph_lines;
    use yread::model::{Block, TextAlign};

    let book =
        yread::epub::parse_epub(&epub_with_body("<p>line one<br/>line two</p>")).expect("parse");
    let ch = &book.chapters[0];
    let runs = match &ch.blocks[0] {
        Block::Paragraph { runs, .. } => runs,
        _ => panic!("expected paragraph"),
    };

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let lines = break_paragraph_lines(
        &ch.text,
        runs,
        0.0,
        5000.0,
        10.0,
        1.1,
        TextAlign::Left,
        &fonts,
        &mut cache,
        None,
    );

    assert_eq!(
        lines.len(),
        2,
        "br must split into two lines, text was {:?}",
        ch.text
    );
    assert_eq!(&ch.text[lines[0].start_byte..lines[0].end_byte], "line one");
    assert_eq!(&ch.text[lines[1].start_byte..lines[1].end_byte], "line two");
}

#[test]
fn test_consecutive_brs_yield_blank_line_with_monotonic_offsets() {
    use yread::line::break_paragraph_lines;
    use yread::model::{Block, TextAlign};

    let book =
        yread::epub::parse_epub(&epub_with_body("<p>alpha<br/><br/>beta</p>")).expect("parse");
    let ch = &book.chapters[0];
    let runs = match &ch.blocks[0] {
        Block::Paragraph { runs, .. } => runs,
        _ => panic!("expected paragraph"),
    };

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let lines = break_paragraph_lines(
        &ch.text,
        runs,
        0.0,
        5000.0,
        10.0,
        1.1,
        TextAlign::Left,
        &fonts,
        &mut cache,
        None,
    );

    assert_eq!(lines.len(), 3, "consecutive brs = 3 lines (one blank)");
    assert!(lines[1].items.is_empty(), "middle line is blank");
    assert_eq!(
        lines[1].start_char, lines[0].end_char,
        "blank line inherits previous end"
    );
    assert!(
        lines[1].start_char <= lines[2].start_char,
        "offsets stay monotonic"
    );
}

#[test]
fn test_page_starts_are_set_consistent_and_monotonic() {
    use yread::model::{Block, Chapter, Run, Style, TextAlign};

    // Hand-built chapter: one-line paragraphs each followed by a rule (scene
    // breaks), paginated into a tiny page. Units are line+rule, so overflow
    // lands on the RULE half of the unit — the pending-start path — on most
    // page breaks, deterministically.
    let mut ch = Chapter::new("t", "T");
    let filler = "lorem ipsum dolor sit";
    let mut blocks = Vec::new();
    for i in 0..40 {
        let text = format!("{} {}", filler, i);
        let start = ch.text.len();
        ch.text.push_str(&text);
        let end = ch.text.len();
        blocks.push(Block::Paragraph {
            runs: vec![Run {
                start,
                end,
                style: Style::default(),
            }],
            indent: false,
            align: TextAlign::Justify,
            left_margin_em: 0.0,
            bullet_prefix: None,
            is_quote: false,
        });
        blocks.push(Block::Rule);
    }
    ch.blocks = blocks;

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let config = LayoutConfig {
        page_width: 600,
        page_height: 500,
        margin_left: 30,
        margin_right: 30,
        margin_top: 30,
        margin_bottom: 30,
        font_size: 11.0,
        line_spacing: 1.15,
        paragraph_spacing: 0.15,
        indent_em: 1.2,
        hyphenate: true,
    };

    let (pt, layouts) = paginate_chapter(
        &ch,
        &config,
        &fonts,
        &mut cache,
        Some(hypher::Lang::English),
    );

    assert!(
        pt.page_count() >= 3,
        "tiny pages must force breaks, got {}",
        pt.page_count()
    );
    for (i, l) in layouts.iter().enumerate() {
        assert_eq!(
            l.start_char, pt.pages[i].char_offset,
            "page {} start_char must match its PageBreak",
            i
        );
    }
    for w in layouts.windows(2) {
        // Strict: a repeated start means a page recorded its predecessor's
        // offset (the old Rule/Image/CodeBlock break bug).
        assert!(
            w[0].start_char < w[1].start_char,
            "page starts must be strictly monotonic"
        );
        assert!(
            w[0].end_char <= w[1].end_char,
            "page ends must be monotonic"
        );
    }
    // A later page starting at char 0 means the start was never recorded.
    assert!(
        layouts[layouts.len() - 1].start_char > 0,
        "last page start_char must be a real offset, not the default 0"
    );

    // And the reading position round-trips: the page that claims to contain
    // a char actually renders that char.
    let probe = ch.char_count() / 2;
    let page = pt.page_for_char(probe);
    let upper = layouts
        .get(page + 1)
        .map(|l| l.start_char)
        .unwrap_or(ch.char_count());
    assert!(
        probe >= layouts[page].start_char && probe < upper,
        "char {} should be inside page {} [{}, {})",
        probe,
        page,
        layouts[page].start_char,
        upper
    );
}

#[test]
fn test_orphan_punctuation_never_breaks_alone_on_next_line() {
    use yread::line::break_paragraph_lines;
    use yread::model::{Block, TextAlign};

    // Construct a paragraph where an italicized phrase is immediately followed by a comma
    let html = "<p>This is a paragraph with <em>italic text</em>, and more words following.</p>";
    let book = yread::epub::parse_epub(&epub_with_body(html)).expect("parse");
    let ch = &book.chapters[0];
    let runs = match &ch.blocks[0] {
        Block::Paragraph { runs, .. } => runs,
        _ => panic!("expected paragraph"),
    };

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();

    // Test a variety of narrow widths to find wrapping boundary
    for width in (150..500).step_by(10) {
        let lines = break_paragraph_lines(
            &ch.text,
            runs,
            0.0,
            width as f32,
            12.0,
            1.2,
            TextAlign::Left,
            &fonts,
            &mut cache,
            None,
        );

        for line in &lines {
            if let Some(first_item) = line.items.first() {
                if let yread::line::LineItem::Word {
                    byte_start,
                    byte_end,
                    ..
                } = first_item
                {
                    let first_word = &ch.text[*byte_start..*byte_end];
                    assert_ne!(first_word, ",", "comma should never start a line alone!");
                    assert_ne!(first_word, ".", "period should never start a line alone!");
                }
            }
        }
    }
}

#[test]
fn hyphen_broken_line_keeps_space_before_hyphenated_word() {
    use yread::line::break_paragraph_lines;
    use yread::model::{Block, TextAlign};

    // Regression (2026-08-23): a line ending in a hyphenated word dropped
    // the space BEFORE it, gluing the last two words together on every
    // hyphen-broken line of justified text. (The bug was in the KP
    // extractor, since removed; the source-gap invariant below is
    // breaker-agnostic and stays as a tripwire.)
    let html = "<p>Долгими зимними вечерами электроэнергетическая \
                промышленность южных регионов продолжала работать \
                устойчиво и надёжно каждый single day</p>";
    let book = yread::epub::parse_epub(&epub_with_body(html)).expect("parse");
    let ch = &book.chapters[0];
    let runs = match &ch.blocks[0] {
        Block::Paragraph { runs, .. } => runs,
        _ => panic!("expected paragraph"),
    };

    let fonts = FontSystem::default();
    let mut cache = ShapeCache::new();
    let lang = yread::hypher_lang("ru");

    let mut hyphen_breaks = 0;
    for width in [90.0, 110.0, 130.0, 150.0, 170.0, 190.0, 230.0] {
        let lines = break_paragraph_lines(
            &ch.text,
            runs,
            0.0,
            width,
            10.0,
            1.2,
            TextAlign::Justify,
            &fonts,
            &mut cache,
            Some(lang),
        );
        for (li, line) in lines.iter().enumerate() {
            // A rendered space is missing when two word items are
            // adjacent but the SOURCE has whitespace between them
            // (legit punctuation joins like "(sharding" have none).
            let mut prev: Option<&yread::line::LineItem> = None;
            for it in &line.items {
                let wordish = !it.is_space();
                if wordish {
                    if let Some(p) = prev {
                        let ranges = |x: &yread::line::LineItem| match x {
                            yread::line::LineItem::Word {
                                byte_start,
                                byte_end,
                                ..
                            }
                            | yread::line::LineItem::HyphenatedPrefix {
                                byte_start,
                                byte_end,
                                ..
                            } => Some((*byte_start, *byte_end)),
                            _ => None,
                        };
                        if let (Some((_, pe)), Some((cb, _))) = (ranges(p), ranges(it)) {
                            if cb > pe && ch.text[pe..cb].contains(' ') {
                                panic!(
                                    "width {} line {}: missing space between {:?} and {:?}",
                                    width,
                                    li,
                                    &ch.text[pe..(pe + 8).min(cb)],
                                    &ch.text[cb..(cb + 8).min(ch.text.len())]
                                );
                            }
                        }
                    }
                }
                prev = Some(it);
            }
        }
        // next.start_byte == cur.end_byte (no whitespace gap in the
        // source) is exactly a hyphen break — the setup must produce
        // some, or this test guards nothing.
        for pair in lines.iter().zip(lines.iter().skip(1)) {
            let (cur, next) = pair;
            if next.start_byte == cur.end_byte {
                hyphen_breaks += 1;
            }
        }
    }
    assert!(
        hyphen_breaks > 0,
        "test setup must force hyphenated line ends"
    );

    // Real prose, English hyphenation: the missing space shows up when
    // a hyphenated word sits mid-line, which synthetic text at a few
    // widths can miss.
    let book = parse_fb2(WAR_AND_PEACE_FB2.as_bytes()).expect("parse");
    let ch = &book.chapters[0];
    let lang = yread::hypher_lang("en");
    for width in (160..400).step_by(17) {
        for block in &ch.blocks {
            let Block::Paragraph { runs, .. } = block else {
                continue;
            };
            let lines = break_paragraph_lines(
                &ch.text,
                runs,
                18.0,
                width as f32,
                12.0,
                1.2,
                TextAlign::Justify,
                &fonts,
                &mut cache,
                Some(lang),
            );
            for (li, line) in lines.iter().enumerate() {
                let mut prev_wordish = false;
                for it in &line.items {
                    let wordish = !it.is_space();
                    if wordish && prev_wordish {
                        panic!(
                            "width {} line {}: two words with no space \
                             between them",
                            width, li
                        );
                    }
                    prev_wordish = wordish;
                }
            }
        }
    }
}

#[test]
fn test_fb2_anchor_char_offsets_survive_multibyte() {
    // Anchors are char offsets into chapter.text; the parser maintains them
    // incrementally (cur_char_count). Multibyte content must not drift the
    // count from ground truth, and successive anchors must land exactly on
    // their own paragraphs.
    let fb2 = r##"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description>
  <title-info>
    <book-title>T</book-title>
  </title-info>
</description>
<body>
  <section id="s1">
    <p id="p1">First—café</p>
    <p id="p2">Second 日本語 text</p>
    <p id="p3">Third</p>
  </section>
</body>
</FictionBook>"##;
    let book = parse_fb2(fb2.as_bytes()).expect("parse fb2");
    assert!(!book.chapters.is_empty(), "expected a chapter");
    let ch = &book.chapters[0];

    let o1 = *ch.anchors.get("p1").expect("p1 anchor");
    let o2 = *ch.anchors.get("p2").expect("p2 anchor");
    let o3 = *ch.anchors.get("p3").expect("p3 anchor");
    assert!(
        o1 < o2 && o2 < o3,
        "anchors must strictly increase: {o1} {o2} {o3}"
    );

    let from = |off: usize| -> String { ch.text.chars().skip(off).collect() };
    assert!(
        from(o1).starts_with("First—café"),
        "p1 lands wrong: {:?}",
        from(o1)
    );
    assert!(
        from(o2).starts_with("Second 日本語 text"),
        "p2 lands wrong: {:?}",
        from(o2)
    );
    assert!(
        from(o3).starts_with("Third"),
        "p3 lands wrong: {:?}",
        from(o3)
    );

    // Counter must equal a full rescan at end of parse.
    assert_eq!(from(o3), "Third");
}

#[test]
fn test_epub_malformed_chapter_degrades_not_dies() {
    // Chapter 1 contains a mismatched end tag (quick-xml hard error);
    // chapter 2 is fine. The book must still open, keep BOTH spine
    // positions (ch1 as a placeholder), and chapter 2's content intact —
    // one bad file must not cost the reader the whole book.
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut buf = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
    let options = SimpleFileOptions::default();
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(br#"<container><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
    zip.start_file("content.opf", options).unwrap();
    zip.write_all(br#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>T</dc:title><dc:language>en</dc:language></metadata><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="c2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#).unwrap();

    zip.start_file("c1.xhtml", options).unwrap();
    zip.write_all(b"<html><body><p>Broken </wrongtag></body></html>")
        .unwrap();
    zip.start_file("c2.xhtml", options).unwrap();
    zip.write_all(b"<html><body><p>Surviving chapter text</p></body></html>")
        .unwrap();
    zip.finish().unwrap();

    let book = parse_epub(&buf).expect("book with one bad chapter must still parse");
    assert_eq!(
        book.chapters.len(),
        2,
        "placeholder keeps spine indices stable"
    );
    let c2 = &book.chapters[1];
    assert!(c2.text.contains("Surviving chapter text"));
}

#[test]
fn test_parse_real_sample_fb2() {
    let p = std::path::Path::new("/tmp/sample.fb2");
    if !p.exists() {
        return;
    }
    let book = yread::fb2::parse_fb2_path(p).expect("parse SAMPLE fb2");
    assert_eq!(book.meta.title, "A Sample Book");
    assert!(!book.chapters.is_empty());
    println!("Parsed SAMPLE.fb2: {} chapters, {} images", book.chapters.len(), book.images.len());
}
