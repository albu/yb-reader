//! EPUB 2/3 container and streaming XHTML parser.
//!
//! Extracts metadata, spine reading order, and converts XHTML content documents
//! directly into the `Book -> Chapter -> Block -> Run` model.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

use crate::model::{Block, Book, Chapter, FontStyle, Run, Style, TextAlign};

pub struct EpubParser<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    root_dir: PathBuf,
    opf_path: String,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    book: Book,
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

impl<'a> EpubParser<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, String> {
        let cursor = Cursor::new(data);
        let archive = ZipArchive::new(cursor).map_err(|e| format!("Invalid EPUB zip archive: {:?}", e))?;
        Ok(Self {
            archive,
            root_dir: PathBuf::new(),
            opf_path: String::new(),
            manifest: HashMap::new(),
            spine: Vec::new(),
            book: Book::default(),
        })
    }

    pub fn parse(mut self) -> Result<Book, String> {
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

        // Step 4: Load embedded images from manifest
        self.load_images();

        Ok(self.book)
    }

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

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();
                    match name.as_str() {
                        "title" => {
                            in_title_tag = true;
                            title_tag_buf.clear();
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
                            li_margin_em = (level as f32) * 1.2;
                            let bullet_str = if let Some(last) = list_stack.last_mut() {
                                match last {
                                    ListType::Ordered(count) => {
                                        let b = format!("{}. ", count);
                                        *count += 1;
                                        b
                                    }
                                    ListType::Unordered => {
                                        if level > 1 { "– ".to_string() } else { "• ".to_string() }
                                    }
                                }
                            } else {
                                "• ".to_string()
                            };
                            li_bullet = Some(bullet_str);

                            // Start a block for the list item if not already in one
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
                        "p" | "div" | "pre" => {
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
                                    current_style.footnote_ref = Some(href_val);
                                }
                            }
                        }
                        "hr" => {
                            chapter.blocks.push(Block::Rule);
                        }
                        "img" | "image" => {
                            let mut img_src = None;
                            for attr in e.attributes().flatten() {
                                let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                                if k == "src" || k.ends_with("href") {
                                    let raw_src = String::from_utf8_lossy(&attr.value).to_string();
                                    img_src = Some(raw_src);
                                }
                            }
                            if let Some(src) = img_src {
                                let resolved = resolve_relative_path(href, &src);
                                chapter.blocks.push(Block::Image {
                                    id: resolved,
                                    caption: None,
                                    width: None,
                                    height: None,
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
                        "p" | "div" | "pre" => {
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
                    let text = e.unescape().map_err(|err| err.to_string())?;
                    if in_title_tag {
                        title_tag_buf.push_str(&text);
                    } else if in_block {
                        let normalized = normalize_spaces(&text);
                        if !normalized.is_empty() {
                            let start = chapter.text.len();
                            chapter.text.push_str(&normalized);
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

    fn load_images(&mut self) {
        let image_items: Vec<(String, String)> = self
            .manifest
            .values()
            .filter(|it| it.media_type.starts_with("image/"))
            .map(|it| (it.href.clone(), it.href.clone()))
            .collect();

        for (id, path) in image_items {
            if let Ok(bytes) = self.read_file_to_bytes(&path) {
                if let Some(fname) = Path::new(&id).file_name().and_then(|n| n.to_str()) {
                    self.book.add_image(fname.to_string(), bytes.clone());
                }
                self.book.add_image(id, bytes);
            }
        }
    }
}

fn parse_align_from_attrs(e: &BytesStart) -> Option<TextAlign> {
    for attr in e.attributes().flatten() {
        let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let v = String::from_utf8_lossy(&attr.value).to_lowercase();
        if k == "align" || k == "style" {
            if v.contains("center") {
                return Some(TextAlign::Center);
            } else if v.contains("right") {
                return Some(TextAlign::Right);
            } else if v.contains("left") {
                return Some(TextAlign::Left);
            } else if v.contains("justify") {
                return Some(TextAlign::Justify);
            }
        }
    }
    None
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

/// Parse an EPUB from byte slice
pub fn parse_epub(data: &[u8]) -> Result<Book, String> {
    let parser = EpubParser::new(data)?;
    parser.parse()
}
