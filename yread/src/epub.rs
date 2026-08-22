//! EPUB 2/3 container and streaming XHTML parser.
//!
//! Extracts metadata, spine reading order, and converts XHTML content documents
//! directly into the `Book -> Chapter -> Block -> Run` model.

use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

use crate::model::{Block, Book, Chapter, FontStyle, Run, Style, TextAlign};

pub struct EpubParser<R: Read + Seek> {
    archive: ZipArchive<R>,
    root_dir: PathBuf,
    opf_path: String,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    book: Book,
    /// File-backed mode: images are never eagerly loaded; the archive is
    /// moved into the Book for on-demand loads.
    lazy_images: bool,
    /// All manifest image items (id -> archive path), for the lazy store.
    image_entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy)]
enum ListType {
    Unordered,
    Ordered(usize),
}

#[derive(Debug, Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
}

impl<'a> EpubParser<Cursor<&'a [u8]>> {
    pub fn new(data: &'a [u8]) -> Result<Self, String> {
        Self::with_reader(Cursor::new(data))
    }
}

impl EpubParser<std::io::BufReader<std::fs::File>> {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Cannot open EPUB '{}': {}", path.display(), e))?;
        Self::with_reader(std::io::BufReader::new(file))
    }
}

impl<R: Read + Seek> EpubParser<R> {
    pub fn with_reader(reader: R) -> Result<Self, String> {
        let archive = ZipArchive::new(reader).map_err(|e| format!("Invalid EPUB zip archive: {:?}", e))?;
        Ok(Self {
            archive,
            root_dir: PathBuf::new(),
            opf_path: String::new(),
            manifest: HashMap::new(),
            spine: Vec::new(),
            book: Book::default(),
            lazy_images: false,
            image_entries: Vec::new(),
        })
    }

    fn parse_common(&mut self) -> Result<(), String> {
        // Step 1: Locate OPF package file via META-INF/container.xml
        self.locate_opf()?;

        // Step 2: Parse OPF (metadata, manifest, spine)
        self.parse_opf()?;

        // Step 3: Parse spine items (XHTML content documents)
        let spine_ids = self.spine.clone();
        for id in &spine_ids {
            if let Some(item) = self.manifest.get(id).cloned() {
                if item.media_type.contains("html") || item.media_type.contains("xml") || item.href.ends_with(".xhtml") || item.href.ends_with(".html") {
                    self.parse_xhtml_item(&item.href)?;
                }
            }
        }

        // Step 4: Parse Table of Contents (NCX / Nav / Headings)
        self.parse_toc();

        // Step 5: images — eager bytes here, or header-only sizes when the
        // lazy flag is set (file-backed parse_lazy).
        self.load_images();
        Ok(())
    }

    /// Eager parse (in-memory books, tests): all image bytes retained.
    pub fn parse(mut self) -> Result<Book, String> {
        self.parse_common()?;
        Ok(self.book)
    }
}

impl<R: Read + Seek + Send + 'static> EpubParser<R> {
    /// Streaming/lazy parse: image bytes load on demand from the archive,
    /// which is moved into the Book.
    pub fn parse_lazy(mut self) -> Result<Book, String> {
        self.lazy_images = true;
        self.parse_common()?;

        let EpubParser { archive, mut book, image_entries, .. } = self;
        let entries: HashMap<String, String> = image_entries.into_iter().collect();
        let reader = archive.into_inner();
        let boxed = Box::new(reader) as Box<dyn crate::model::ImageSource>;
        let archive = ZipArchive::new(boxed).map_err(|e| format!("Re-open archive: {:?}", e))?;
        book.lazy_images = std::sync::Arc::new(crate::model::LazyImages::from_archive(archive, entries));
        Ok(book)
    }
}

impl<R: Read + Seek> EpubParser<R> {
    fn read_file_to_string(&mut self, path: &str) -> Result<String, String> {
        let mut file = self.archive.by_name(path).map_err(|e| format!("Cannot find file '{}' in EPUB: {:?}", path, e))?;
        let mut content = String::new();
        file.read_to_string(&mut content).map_err(|e| format!("Cannot read file '{}': {:?}", path, e))?;
        Ok(content)
    }

    fn read_file_to_bytes(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let mut file = self.archive.by_name(path).map_err(|e| format!("Cannot find file '{}' in EPUB: {:?}", path, e))?;
        let mut content = Vec::new();
        file.read_to_end(&mut content).map_err(|e| format!("Cannot read file '{}': {:?}", path, e))?;
        Ok(content)
    }

    fn locate_opf(&mut self) -> Result<(), String> {
        let container_xml = self.read_file_to_string("META-INF/container.xml")?;
        let mut reader = Reader::from_str(&container_xml);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name == "rootfile" {
                        for attr in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                            if k == "full-path" {
                                let path_str = String::from_utf8_lossy(&attr.value).to_string();
                                self.opf_path = path_str.clone();
                                if let Some(parent) = Path::new(&path_str).parent() {
                                    self.root_dir = parent.to_path_buf();
                                }
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("Error parsing container.xml: {:?}", e)),
                _ => {}
            }
            buf.clear();
        }

        if self.opf_path.is_empty() {
            Err("No rootfile full-path found in META-INF/container.xml".to_string())
        } else {
            Ok(())
        }
    }

    fn parse_opf(&mut self) -> Result<(), String> {
        let opf_xml = self.read_file_to_string(&self.opf_path.clone())?;
        let mut reader = Reader::from_str(&opf_xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut in_title = false;
        let mut in_creator = false;
        let mut in_language = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name.ends_with("title") {
                        in_title = true;
                    } else if name.ends_with("creator") {
                        in_creator = true;
                    } else if name.ends_with("language") {
                        in_language = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name.ends_with("title") {
                        in_title = false;
                    } else if name.ends_with("creator") {
                        in_creator = false;
                    } else if name.ends_with("language") {
                        in_language = false;
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name == "item" {
                        let mut id = String::new();
                        let mut href = String::new();
                        let mut media_type = String::new();
                        for attr in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                            let v = String::from_utf8_lossy(&attr.value).to_string();
                            if k == "id" {
                                id = v;
                            } else if k == "href" {
                                href = v;
                            } else if k == "media-type" {
                                media_type = v;
                            }
                        }
                        if !id.is_empty() && !href.is_empty() {
                            // Resolve relative href to root_dir
                            let full_href = if self.root_dir.as_os_str().is_empty() {
                                href
                            } else {
                                self.root_dir.join(href).to_string_lossy().to_string()
                            };
                            self.manifest.insert(id, ManifestItem { href: full_href, media_type });
                        }
                    } else if name == "itemref" {
                        for attr in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                            if k == "idref" {
                                let idref = String::from_utf8_lossy(&attr.value).to_string();
                                self.spine.push(idref);
                            }
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    let text = e.unescape().map_err(|err| err.to_string())?;
                    let trimmed = text.trim();
                    if in_title && !trimmed.is_empty() && self.book.meta.title.is_empty() {
                        self.book.meta.title = trimmed.to_string();
                    } else if in_creator && !trimmed.is_empty() {
                        self.book.meta.authors.push(trimmed.to_string());
                    } else if in_language && !trimmed.is_empty() {
                        self.book.meta.language = trimmed.to_string();
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("Error parsing OPF package: {:?}", e)),
                _ => {}
            }
            buf.clear();
        }

        Ok(())
    }

    fn parse_xhtml_item(&mut self, href: &str) -> Result<(), String> {
        let xhtml_content = match self.read_file_to_string(href) {
            Ok(c) => c,
            Err(_) => return Ok(()), // Skip missing spine files gracefully
        };

        let chap_idx = self.book.chapters.len() + 1;
        let mut chapter = Chapter::new(format!("ch_{}", chap_idx), format!("Chapter {}", chap_idx));
        chapter.href = href.to_string();

        let mut reader = Reader::from_str(&xhtml_content);
        reader.config_mut().trim_text(false);
        reader.config_mut().expand_empty_elements = true;

        let mut buf = Vec::new();
        let mut style_stack = Vec::new();
        let mut current_style = Style::default();
        let mut current_runs = Vec::new();
        let mut in_block = false;
        let mut block_align = TextAlign::Justify;
        let mut heading_level = 1u8;
        let mut in_title_tag = false;
        let mut title_tag_buf = String::new();

        let mut list_stack: Vec<ListType> = Vec::new();
        let mut in_blockquote = false;
        let mut in_figure = false;
        let mut in_li = false;
        let mut li_bullet: Option<String> = None;
        let mut li_margin_em: f32 = 0.0;
        let mut cur_bullet: Option<String> = None;
        let mut cur_margin_em: f32 = 0.0;
        let mut cur_is_quote = false;

        let mut current_char_count = 0usize;
        let mut in_pre = false;
        let mut pre_buf = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                        if k == "id" {
                            let id_val = String::from_utf8_lossy(&attr.value).to_string();
                            chapter.anchors.insert(id_val, current_char_count);
                        }
                    }
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    match name.as_str() {
                        "title" => {
                            in_title_tag = true;
                            title_tag_buf.clear();
                        }
                        "br" => {
                            if in_block {
                                let start = chapter.text.len();
                                chapter.text.push('\n');
                                current_char_count += 1;
                                let end = chapter.text.len();
                                current_runs.push(Run {
                                    start,
                                    end,
                                    style: current_style.clone(),
                                });
                            }
                        }
                        "ul" => {
                            list_stack.push(ListType::Unordered);
                        }
                        "ol" => {
                            list_stack.push(ListType::Ordered(1));
                        }
                        "blockquote" | "aside" => {
                            in_blockquote = true;
                        }
                        "figure" => {
                            in_figure = true;
                        }
                        "li" => {
                            in_li = true;
                            let level = list_stack.len().max(1);
                            li_margin_em = (level - 1) as f32 * 1.5;
                            let bullet_str = if let Some(last) = list_stack.last_mut() {
                                match last {
                                    ListType::Ordered(num) => {
                                        let s = format!("{}. ", num);
                                        *num += 1;
                                        s
                                    }
                                    ListType::Unordered => {
                                        if level > 1 { "– ".to_string() } else { "• ".to_string() }
                                    }
                                }
                            } else {
                                "• ".to_string()
                            };
                            li_bullet = Some(bullet_str);

                            current_runs.clear();
                            in_block = true;
                            current_style = Style::default();
                            current_style.indent = false;
                            cur_bullet = li_bullet.clone();
                            cur_margin_em = li_margin_em;
                            cur_is_quote = false;
                            block_align = parse_align_from_attrs(e).unwrap_or(TextAlign::Left);
                            current_style.align = block_align;
                        }
                        "pre" => {
                            in_pre = true;
                            pre_buf.clear();
                        }
                        "p" | "div" => {
                            current_runs.clear();
                            in_block = true;
                            current_style = Style::default();
                            if in_li {
                                current_style.indent = false;
                                cur_bullet = li_bullet.take();
                                cur_margin_em = li_margin_em;
                                cur_is_quote = false;
                            } else if in_blockquote {
                                current_style.indent = false;
                                cur_bullet = None;
                                cur_margin_em = 1.5;
                                cur_is_quote = true;
                                current_style.font_style = FontStyle::Italic;
                            } else {
                                current_style.indent = name == "p";
                                cur_bullet = None;
                                cur_margin_em = 0.0;
                                cur_is_quote = false;
                            }
                            block_align = parse_align_from_attrs(e).unwrap_or(if in_li { TextAlign::Left } else { TextAlign::Justify });
                            current_style.align = block_align;
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "figcaption" => {
                            current_runs.clear();
                            in_block = true;
                            heading_level = if name == "figcaption" {
                                6
                            } else {
                                name.chars().nth(1).and_then(|c| c.to_digit(10)).unwrap_or(1) as u8
                            };
                            current_style = Style::default();
                            current_style.font_style = if in_figure || heading_level == 6 { FontStyle::Italic } else { FontStyle::Bold };
                            current_style.size_mult = if in_figure || heading_level == 6 {
                                0.85
                            } else {
                                match heading_level {
                                    1 => 1.35,
                                    2 => 1.20,
                                    _ => 1.10,
                                }
                            };
                            block_align = parse_align_from_attrs(e).unwrap_or(TextAlign::Center);
                            current_style.align = block_align;
                        }
                        "b" | "strong" => {
                            style_stack.push(current_style.clone());
                            current_style.font_style = match current_style.font_style {
                                FontStyle::Italic | FontStyle::BoldItalic => FontStyle::BoldItalic,
                                _ => FontStyle::Bold,
                            };
                        }
                        "i" | "em" => {
                            style_stack.push(current_style.clone());
                            current_style.font_style = match current_style.font_style {
                                FontStyle::Bold | FontStyle::BoldItalic => FontStyle::BoldItalic,
                                _ => FontStyle::Italic,
                            };
                        }
                        "sup" => {
                            style_stack.push(current_style.clone());
                            current_style.is_sup = true;
                            current_style.size_mult *= 0.75;
                        }
                        "sub" => {
                            style_stack.push(current_style.clone());
                            current_style.is_sub = true;
                            current_style.size_mult *= 0.75;
                        }
                        "code" | "tt" => {
                            style_stack.push(current_style.clone());
                            current_style.is_code = true;
                        }
                        "a" => {
                            style_stack.push(current_style.clone());
                            for attr in e.attributes().flatten() {
                                let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                                if k == "href" {
                                    let href_val = String::from_utf8_lossy(&attr.value).to_string();
                                    if href_val.starts_with('#') {
                                        current_style.footnote_ref = Some(href_val.trim_start_matches('#').to_string());
                                    }
                                }
                            }
                        }
                        "hr" => {
                            chapter.blocks.push(Block::Rule);
                        }
                        "img" | "image" => {
                            let mut target_src = String::new();
                            for attr in e.attributes().flatten() {
                                let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                                if k == "src" || k.ends_with("href") {
                                    target_src = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                            if !target_src.is_empty() {
                                let resolved_src = resolve_relative_path(href, &target_src);
                                chapter.blocks.push(Block::Image {
                                    id: resolved_src,
                                    width: None,
                                    height: None,
                                    caption: None,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    match name.as_str() {
                        "title" => {
                            in_title_tag = false;
                            if !title_tag_buf.trim().is_empty() && chapter.title.starts_with("Chapter ") {
                                chapter.title = title_tag_buf.trim().to_string();
                            }
                        }
                        "ul" | "ol" => {
                            list_stack.pop();
                        }
                        "blockquote" | "aside" => {
                            in_blockquote = false;
                        }
                        "figure" => {
                            in_figure = false;
                        }
                        "li" => {
                            in_li = false;
                            in_block = false;
                            if !current_runs.is_empty() {
                                chapter.blocks.push(Block::Paragraph {
                                    runs: std::mem::take(&mut current_runs),
                                    indent: false,
                                    align: block_align,
                                    left_margin_em: cur_margin_em,
                                    bullet_prefix: cur_bullet.take().or_else(|| li_bullet.take()),
                                    is_quote: cur_is_quote,
                                });
                            }
                            li_bullet = None;
                            li_margin_em = 0.0;
                        }
                        "pre" => {
                            in_pre = false;
                            if !pre_buf.trim().is_empty() {
                                chapter.blocks.push(Block::CodeBlock {
                                    code: std::mem::take(&mut pre_buf),
                                });
                            }
                        }
                        "p" | "div" => {
                            in_block = false;
                            if !current_runs.is_empty() {
                                chapter.blocks.push(Block::Paragraph {
                                    runs: std::mem::take(&mut current_runs),
                                    indent: name == "p" && !in_li && !in_blockquote,
                                    align: block_align,
                                    left_margin_em: cur_margin_em,
                                    bullet_prefix: cur_bullet.take(),
                                    is_quote: cur_is_quote,
                                });
                            }
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "figcaption" => {
                            in_block = false;
                            if !current_runs.is_empty() {
                                let runs = std::mem::take(&mut current_runs);
                                let h_text: String = runs.iter().filter_map(|r| chapter.text.get(r.start..r.end)).collect();
                                if !h_text.trim().is_empty() && chapter.title.starts_with("Chapter ") && heading_level <= 2 {
                                    chapter.title = h_text.trim().to_string();
                                }
                                chapter.blocks.push(Block::Heading {
                                    level: heading_level,
                                    runs,
                                });
                            }
                        }
                        "b" | "strong" | "i" | "em" | "sup" | "sub" | "code" | "tt" | "a" => {
                            if let Some(prev) = style_stack.pop() {
                                current_style = prev;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(ref e)) => {
                    let raw_slice = std::str::from_utf8(e.as_ref()).unwrap_or("");
                    let text = unescape_html_lossy(raw_slice);
                    if in_title_tag {
                        title_tag_buf.push_str(&text);
                    } else if in_pre {
                        pre_buf.push_str(&text);
                    } else if in_block {
                        let normalized = normalize_spaces(&text);
                        if !normalized.is_empty() {
                            let start = chapter.text.len();
                            chapter.text.push_str(&normalized);
                            current_char_count += normalized.chars().count();
                            let end = chapter.text.len();
                            current_runs.push(Run {
                                start,
                                end,
                                style: current_style.clone(),
                            });
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("XHTML parse error in '{}': {:?}", href, e)),
                _ => {}
            }
            buf.clear();
        }

        if !chapter.blocks.is_empty() || !chapter.text.is_empty() {
            self.book.chapters.push(chapter);
        }

        Ok(())
    }

    fn parse_toc(&mut self) {
        // Look for NCX item in manifest
        let ncx_item = self
            .manifest
            .values()
            .find(|it| it.media_type == "application/x-dtbncx+xml" || it.href.ends_with(".ncx"))
            .cloned();

        if let Some(ncx) = ncx_item {
            if let Ok(ncx_xml) = self.read_file_to_string(&ncx.href) {
                self.parse_ncx_xml(&ncx_xml);
                if !self.book.toc.is_empty() {
                    return;
                }
            }
        }

        // Fallback: Generate TOC from chapter titles
        for (ch_idx, ch) in self.book.chapters.iter().enumerate() {
            let title = if ch.title.trim().is_empty() {
                format!("Chapter {}", ch_idx + 1)
            } else {
                ch.title.trim().to_string()
            };
            self.book.toc.push(crate::model::TocEntry {
                title,
                chapter_idx: ch_idx,
                byte_offset: 0,
                char_offset: 0,
                level: 0,
            });
        }
    }

    fn parse_ncx_xml(&mut self, xml: &str) {
        struct NavPointFrame {
            level: usize,
            title: String,
            src: String,
            emitted: bool,
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut current_level = 0usize;
        let mut in_text = false;
        let mut stack: Vec<NavPointFrame> = Vec::new();
        let mut entries: Vec<crate::model::TocEntry> = Vec::new();

        let emit_frame = |frame: &mut NavPointFrame, chapters: &[crate::model::Chapter], out: &mut Vec<crate::model::TocEntry>| {
            if frame.emitted || frame.title.is_empty() {
                return;
            }
            frame.emitted = true;

            // Resolve src to chapter and anchor
            let src_parts: Vec<&str> = frame.src.split('#').collect();
            let file_target = src_parts[0];
            let anchor = src_parts.get(1).copied().unwrap_or("");

            // Match chapter by href filename
            let target_fname = Path::new(file_target).file_name().and_then(|n| n.to_str()).unwrap_or(file_target);
            let matched_ch_idx = chapters.iter().position(|c| {
                let ch_fname = Path::new(&c.href).file_name().and_then(|n| n.to_str()).unwrap_or(&c.href);
                ch_fname.eq_ignore_ascii_case(target_fname) || c.href.eq_ignore_ascii_case(file_target)
            });

            if let Some(ch_idx) = matched_ch_idx {
                let ch = &chapters[ch_idx];
                let char_offset = if !anchor.is_empty() {
                    ch.anchors.get(anchor).copied().unwrap_or(0)
                } else {
                    0
                };
                let byte_offset = ch.char_to_byte(char_offset);

                out.push(crate::model::TocEntry {
                    title: frame.title.clone(),
                    chapter_idx: ch_idx,
                    byte_offset,
                    char_offset,
                    level: frame.level,
                });
            }
        };

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name.ends_with("navpoint") {
                        // Before entering a child, make sure parent is emitted
                        if let Some(parent) = stack.last_mut() {
                            emit_frame(parent, &self.book.chapters, &mut entries);
                        }
                        stack.push(NavPointFrame {
                            level: current_level,
                            title: String::new(),
                            src: String::new(),
                            emitted: false,
                        });
                        current_level += 1;
                    } else if name.ends_with("text") {
                        in_text = true;
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name.ends_with("content") {
                        for attr in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                            if k == "src" {
                                let src_val = String::from_utf8_lossy(&attr.value).to_string();
                                if let Some(frame) = stack.last_mut() {
                                    frame.src = src_val;
                                }
                            }
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if in_text {
                        if let Ok(t) = e.unescape() {
                            let title_str = t.trim().to_string();
                            if let Some(frame) = stack.last_mut() {
                                frame.title = title_str;
                            }
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    if name.ends_with("text") {
                        in_text = false;
                    } else if name.ends_with("navpoint") {
                        current_level = current_level.saturating_sub(1);
                        if let Some(mut frame) = stack.pop() {
                            emit_frame(&mut frame, &self.book.chapters, &mut entries);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }

        if !entries.is_empty() {
            self.book.toc = entries;
        }
    }

    fn load_images(&mut self) {
        let image_items: Vec<(String, String)> = self
            .manifest
            .values()
            .filter(|it| it.media_type.starts_with("image/"))
            .map(|it| (it.href.clone(), it.href.clone()))
            .collect();

        for (id, path) in image_items {
            if self.lazy_images {
                // Header-only: sizes for layout, no bytes retained. The
                // archive itself moves into the Book for on-demand loads.
                self.image_entries.push((id.clone(), path.clone()));
                if let Some((w, h)) = self.read_image_size(&path) {
                    self.book.image_sizes.insert(id, (w, h));
                }
                continue;
            }
            if let Ok(bytes) = self.read_file_to_bytes(&path) {
                // Single copy keyed by the manifest href (what Block::Image
                // ids resolve to). The rasterizer falls back to a file-name
                // lookup for books whose references use bare names.
                self.book.add_image(id, bytes);
            }
        }
    }

    /// Read a small prefix of an archived image and sniff its pixel size
    /// (PNG/GIF/JPEG). 8KB covers JPEG SOF markers incl. most EXIF APP1.
    fn read_image_size(&mut self, path: &str) -> Option<(u32, u32)> {
        let mut file = self.archive.by_name(path).ok()?;
        let mut prefix = [0u8; 8192];
        let mut filled = 0usize;
        while filled < prefix.len() {
            match file.read(&mut prefix[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        sniff_image_size(&prefix[..filled])
    }
}

/// Header-sniff image dimensions without decoding the file.
pub fn sniff_image_size(prefix: &[u8]) -> Option<(u32, u32)> {
    // PNG: IHDR width/height big-endian at bytes 16..24
    if prefix.len() >= 24 && prefix[..8] == *b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes(prefix[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(prefix[20..24].try_into().ok()?);
        return Some((w, h));
    }
    // GIF: little-endian at 6..10
    if prefix.len() >= 10 && (prefix[..6] == *b"GIF87a" || prefix[..6] == *b"GIF89a") {
        let w = u16::from_le_bytes(prefix[6..8].try_into().ok()?) as u32;
        let h = u16::from_le_bytes(prefix[8..10].try_into().ok()?) as u32;
        return Some((w, h));
    }
    // JPEG: walk segments to the first SOF marker
    if prefix.len() >= 4 && prefix[..2] == *b"\xff\xd8" {
        let mut i = 2usize;
        while i + 9 < prefix.len() {
            if prefix[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = prefix[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            if i + 4 > prefix.len() {
                break;
            }
            let seglen = u16::from_be_bytes(prefix[i + 2..i + 4].try_into().ok()?) as usize;
            let is_sof = (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC;
            if is_sof {
                if i + 9 > prefix.len() {
                    break;
                }
                let h = u16::from_be_bytes(prefix[i + 5..i + 7].try_into().ok()?) as u32;
                let w = u16::from_be_bytes(prefix[i + 7..i + 9].try_into().ok()?) as u32;
                return Some((w, h));
            }
            i += 2 + seglen.max(2);
        }
        return None;
    }
    None
}

fn parse_align_from_attrs(e: &BytesStart) -> Option<TextAlign> {
    for attr in e.attributes().flatten() {
        let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let v = String::from_utf8_lossy(&attr.value).to_lowercase();
        if k == "align" {
            let v_trim = v.trim();
            if v_trim == "center" { return Some(TextAlign::Center); }
            if v_trim == "right" { return Some(TextAlign::Right); }
            if v_trim == "justify" { return Some(TextAlign::Justify); }
            if v_trim == "left" { return Some(TextAlign::Left); }
        } else if k == "style" {
            for part in v.split(';') {
                let kv: Vec<&str> = part.split(':').map(|s| s.trim()).collect();
                if kv.len() == 2 && kv[0] == "text-align" {
                    match kv[1] {
                        "center" => return Some(TextAlign::Center),
                        "right" => return Some(TextAlign::Right),
                        "justify" => return Some(TextAlign::Justify),
                        "left" => return Some(TextAlign::Left),
                        _ => {}
                    }
                }
            }
        }
    }
    None
}

pub fn unescape_html_lossy(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            let mut entity = String::with_capacity(10);
            let mut found_semi = false;
            while let Some(&next_c) = chars.peek() {
                if next_c == ';' {
                    chars.next();
                    found_semi = true;
                    break;
                } else if next_c.is_alphanumeric() || next_c == '#' {
                    entity.push(chars.next().unwrap());
                    if entity.len() > 10 {
                        break;
                    }
                } else {
                    break;
                }
            }
            if found_semi {
                if let Some(dec) = decode_entity(&entity) {
                    out.push_str(dec);
                } else if let Some(code) = decode_numeric_entity(&entity) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                    } else {
                        out.push('&');
                        out.push_str(&entity);
                        out.push(';');
                    }
                } else {
                    out.push('&');
                    out.push_str(&entity);
                    out.push(';');
                }
            } else {
                out.push('&');
                out.push_str(&entity);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn decode_numeric_entity(ent: &str) -> Option<u32> {
    let rest = ent.strip_prefix('#')?;
    if let Some(hex) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        rest.parse::<u32>().ok()
    }
}

fn decode_entity(ent: &str) -> Option<&'static str> {
    match ent {
        "quot" => Some("\""),
        "amp" => Some("&"),
        "apos" => Some("'"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "nbsp" => Some("\u{00A0}"),
        "iexcl" => Some("¡"),
        "cent" => Some("¢"),
        "pound" => Some("£"),
        "curren" => Some("¤"),
        "yen" => Some("¥"),
        "brvbar" => Some("¦"),
        "sect" => Some("§"),
        "uml" => Some("¨"),
        "copy" => Some("©"),
        "ordf" => Some("ª"),
        "laquo" => Some("«"),
        "not" => Some("¬"),
        "shy" => Some("\u{00AD}"),
        "reg" => Some("®"),
        "macr" => Some("¯"),
        "deg" => Some("°"),
        "plusmn" => Some("±"),
        "sup2" => Some("²"),
        "sup3" => Some("³"),
        "acute" => Some("´"),
        "micro" => Some("µ"),
        "para" => Some("¶"),
        "middot" => Some("·"),
        "cedil" => Some("¸"),
        "sup1" => Some("¹"),
        "ordm" => Some("º"),
        "raquo" => Some("»"),
        "frac14" => Some("¼"),
        "frac12" => Some("½"),
        "frac34" => Some("¾"),
        "iquest" => Some("¿"),
        "times" => Some("×"),
        "divide" => Some("÷"),
        "ndash" => Some("–"),
        "mdash" => Some("—"),
        "lsquo" => Some("‘"),
        "rsquo" => Some("’"),
        "sbquo" => Some("‚"),
        "ldquo" => Some("“"),
        "rdquo" => Some("”"),
        "bdquo" => Some("„"),
        "dagger" => Some("†"),
        "Dagger" => Some("‡"),
        "bull" => Some("•"),
        "hellip" => Some("…"),
        "permil" => Some("‰"),
        "prime" => Some("′"),
        "Prime" => Some("″"),
        "lsaquo" => Some("‹"),
        "rsaquo" => Some("›"),
        "euro" => Some("€"),
        "trade" => Some("™"),
        "minus" => Some("−"),
        "thinsp" => Some("\u{2009}"),
        "ensp" => Some("\u{2002}"),
        "emsp" => Some("\u{2003}"),
        "zwj" => Some("\u{200D}"),
        "zwnj" => Some("\u{200C}"),
        _ => None,
    }
}

fn resolve_relative_path(base_file: &str, target: &str) -> String {
    if target.starts_with('/') {
        target.trim_start_matches('/').to_string()
    } else if let Some(parent) = Path::new(base_file).parent() {
        parent.join(target).to_string_lossy().to_string()
    } else {
        target.to_string()
    }
}

fn normalize_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// Parse an EPUB from byte slice (eager images — tests, in-memory books)
pub fn parse_epub(data: &[u8]) -> Result<Book, String> {
    let parser = EpubParser::new(data)?;
    parser.parse()
}

/// Parse an EPUB from a file path: streams the archive (no whole-file read),
/// keeps image bytes lazy. This is the reader's path.
pub fn parse_epub_file(path: &Path) -> Result<Book, String> {
    EpubParser::open(path)?.parse_lazy()
}
