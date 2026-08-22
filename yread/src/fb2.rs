//! Streaming FB2 (FictionBook 2.0/2.1/2.2) parser using `quick-xml`.
//!
//! Converts XML elements directly into the `Book -> Chapter -> Block -> Run` model
//! with zero intermediate DOM allocation.

use std::io::{BufRead, Cursor, Read};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::{
    Block, Book, Chapter, FontStyle, Run, Style, TextAlign,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    None,
    InDescription,
    InTitleInfo,
    InBookTitle,
    InAuthor,
    InFirstName,
    InLastName,
    InLang,
    InCoverpage,
    InBody,
    InNotesBody,
    InBinary,
}


pub struct Fb2Parser<R: BufRead> {
    reader: Reader<R>,
    book: Book,
    current_chapter: Option<Chapter>,
    current_runs: Vec<Run>,
    current_style: Style,
    style_stack: Vec<Style>,
    state_stack: Vec<ParserState>,
    binary_id: Option<String>,
    binary_buf: String,
    author_first: String,
    author_last: String,
    in_title: bool,
    title_runs: Vec<Run>,
    in_paragraph: bool,
    section_depth: usize,
    in_notes: bool,
    current_note_id: Option<String>,
    current_note_text: String,
}

impl<R: BufRead> Fb2Parser<R> {
    pub fn new(reader: R) -> Self {
        let mut xml_reader = Reader::from_reader(reader);
        xml_reader.config_mut().trim_text(false);
        xml_reader.config_mut().expand_empty_elements = true;

        Self {
            reader: xml_reader,
            book: Book::default(),
            current_chapter: None,
            current_runs: Vec::new(),
            current_style: Style::default(),
            style_stack: Vec::new(),
            state_stack: Vec::new(),
            binary_id: None,
            binary_buf: String::new(),
            author_first: String::new(),
            author_last: String::new(),
            in_title: false,
            title_runs: Vec::new(),
            in_paragraph: false,
            section_depth: 0,
            in_notes: false,
            current_note_id: None,
            current_note_text: String::new(),
        }
    }

    pub fn parse(mut self) -> Result<Book, String> {
        let mut buf = Vec::with_capacity(1024);
        loop {
            match self.reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => self.handle_start_tag(e)?,
                Ok(Event::End(ref e)) => self.handle_end_tag(e)?,
                Ok(Event::Text(ref e)) => {
                    let unescaped = e.unescape().map_err(|err| err.to_string())?;
                    self.handle_text(&unescaped)?;
                }
                Ok(Event::CData(ref e)) => {
                    let text = std::str::from_utf8(e.as_ref()).map_err(|err| err.to_string())?;
                    self.handle_text(text)?;
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("XML parse error at position {}: {:?}", self.reader.error_position(), e)),
                _ => {}
            }
            buf.clear();
        }

        self.finish_chapter();
        Ok(self.book)
    }

    fn current_state(&self) -> ParserState {
        self.state_stack.last().copied().unwrap_or(ParserState::None)
    }

    fn push_state(&mut self, state: ParserState) {
        self.state_stack.push(state);
    }

    fn pop_state(&mut self) {
        self.state_stack.pop();
    }

    fn ensure_chapter(&mut self, title: Option<String>) {
        if self.current_chapter.is_none() {
            let idx = self.book.chapters.len() + 1;
            let chap_title = title.unwrap_or_else(|| format!("Chapter {}", idx));
            self.current_chapter = Some(Chapter::new(format!("ch_{}", idx), chap_title));
        }
    }

    fn finish_chapter(&mut self) {
        if let Some(chap) = self.current_chapter.take() {
            if !chap.blocks.is_empty() || !chap.text.is_empty() {
                self.book.chapters.push(chap);
            }
        }
    }

    fn handle_start_tag(&mut self, e: &BytesStart) -> Result<(), String> {
        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();

        match tag_name.as_str() {
            "description" => self.push_state(ParserState::InDescription),
            "title-info" => self.push_state(ParserState::InTitleInfo),
            "book-title" => self.push_state(ParserState::InBookTitle),
            "author" => {
                self.author_first.clear();
                self.author_last.clear();
                self.push_state(ParserState::InAuthor);
            }
            "first-name" => self.push_state(ParserState::InFirstName),
            "last-name" => self.push_state(ParserState::InLastName),
            "lang" => self.push_state(ParserState::InLang),
            "coverpage" => self.push_state(ParserState::InCoverpage),
            "body" => {
                let mut is_notes = false;
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    let v = String::from_utf8_lossy(&attr.value).to_lowercase();
                    if k == "name" && (v == "notes" || v == "comments") {
                        is_notes = true;
                    }
                }
                if is_notes {
                    self.in_notes = true;
                    self.finish_chapter();
                    self.push_state(ParserState::InNotesBody);
                } else {
                    self.in_notes = false;
                    self.push_state(ParserState::InBody);
                }
            }
            "section" => {
                self.section_depth += 1;
                let mut section_id = None;
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    if k == "id" {
                        section_id = Some(String::from_utf8_lossy(&attr.value).to_string());
                    }
                }

                if !self.in_notes {
                    // Top-level sections start a new chapter if current one has content
                    if self.section_depth == 1 {
                        self.finish_chapter();
                    }
                    self.ensure_chapter(None);
                    if let (Some(id), Some(ref mut chap)) = (section_id, &mut self.current_chapter) {
                        let char_cnt = chap.char_count();
                        chap.anchors.insert(id, char_cnt);
                    }
                } else {
                    // In notes, section might have an id attribute for footnote link
                    if let Some(id) = section_id {
                        if let Some(prev_id) = self.current_note_id.take() {
                            let trimmed = self.current_note_text.trim().to_string();
                            if !trimmed.is_empty() {
                                self.book.footnotes.insert(prev_id, trimmed);
                            }
                            self.current_note_text.clear();
                        }
                        self.current_note_id = Some(id);
                    }
                }
            }
            "title" => {
                self.in_title = true;
                self.title_runs.clear();
            }
            "p" | "v" | "subtitle" | "text-author" => {
                self.in_paragraph = true;
                self.current_runs.clear();
                self.style_stack.clear();

                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    if k == "id" {
                        let id_val = String::from_utf8_lossy(&attr.value).to_string();
                        if self.in_notes {
                            if let Some(prev_id) = self.current_note_id.take() {
                                let trimmed = self.current_note_text.trim().to_string();
                                if !trimmed.is_empty() {
                                    self.book.footnotes.insert(prev_id, trimmed);
                                }
                                self.current_note_text.clear();
                            }
                            self.current_note_id = Some(id_val);
                        } else {
                            let char_cnt = self.current_chapter.as_ref().map(|c| c.char_count()).unwrap_or(0);
                            self.ensure_chapter(None);
                            if let Some(ref mut chap) = self.current_chapter {
                                chap.anchors.insert(id_val, char_cnt);
                            }
                        }
                    }
                }

                let mut style = Style::default();
                if tag_name == "subtitle" {
                    style.size_mult = 1.2;
                    style.font_style = FontStyle::Bold;
                    style.align = TextAlign::Center;
                } else if tag_name == "v" {
                    style.indent = true;
                    style.align = TextAlign::Left;
                } else if tag_name == "text-author" {
                    style.align = TextAlign::Right;
                    style.font_style = FontStyle::Italic;
                }
                self.current_style = style;
            }
            "strong" | "b" => {
                self.style_stack.push(self.current_style.clone());
                self.current_style.font_style = match self.current_style.font_style {
                    FontStyle::Italic | FontStyle::BoldItalic => FontStyle::BoldItalic,
                    _ => FontStyle::Bold,
                };
            }
            "emphasis" | "em" | "i" => {
                self.style_stack.push(self.current_style.clone());
                self.current_style.font_style = match self.current_style.font_style {
                    FontStyle::Bold | FontStyle::BoldItalic => FontStyle::BoldItalic,
                    _ => FontStyle::Italic,
                };
            }
            "a" => {
                self.style_stack.push(self.current_style.clone());
                let mut target: Option<String> = None;
                let mut is_note = false;
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    let v = String::from_utf8_lossy(&attr.value).to_string();
                    if k.ends_with("href") {
                        target = Some(v.trim_start_matches('#').to_string());
                    } else if k == "type" && (v == "note" || v == "footnote" || v == "comment") {
                        is_note = true;
                    }
                }
                if let Some(t) = target {
                    // Note references render as small superscript markers;
                    // the conventional "n…" id covers books omitting type.
                    if is_note || t.starts_with('n') || t.starts_with("note") {
                        self.current_style.is_sup = true;
                        self.current_style.size_mult *= 0.75;
                    }
                    self.current_style.footnote_ref = Some(t);
                }
            }
            "empty-line" => {
                if let Some(ref mut chap) = self.current_chapter {
                    chap.blocks.push(Block::Spacer(16));
                }
            }
            "image" => {
                let mut img_id = None;
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    if k.ends_with("href") {
                        let href = String::from_utf8_lossy(&attr.value).to_string();
                        img_id = Some(href.trim_start_matches('#').to_string());
                    }
                }
                if let Some(id) = img_id {
                    if self.current_state() == ParserState::InCoverpage {
                        self.book.meta.cover_image_id = Some(id);
                    } else if let Some(ref mut chap) = self.current_chapter {
                        chap.blocks.push(Block::Image {
                            id,
                            caption: None,
                            width: None,
                            height: None,
                        });
                    }
                }
            }
            "binary" => {
                self.binary_id = None;
                self.binary_buf.clear();
                for attr in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
                    if k == "id" {
                        self.binary_id = Some(String::from_utf8_lossy(&attr.value).to_string());
                    }
                }
                self.push_state(ParserState::InBinary);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_end_tag(&mut self, e: &quick_xml::events::BytesEnd) -> Result<(), String> {
        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_lowercase();

        match tag_name.as_str() {
            "description" | "title-info" | "book-title" | "first-name" | "last-name"
            | "lang" | "coverpage" => {
                self.pop_state();
            }
            "author" => {
                let full = format!("{} {}", self.author_first, self.author_last).trim().to_string();
                if !full.is_empty() {
                    self.book.meta.authors.push(full);
                }
                self.pop_state();
            }
            "body" => {
                if self.in_notes {
                    if let Some(note_id) = self.current_note_id.take() {
                        let trimmed = self.current_note_text.trim().to_string();
                        if !trimmed.is_empty() {
                            self.book.footnotes.insert(note_id, trimmed);
                        }
                        self.current_note_text.clear();
                    }
                    self.in_notes = false;
                }
                self.pop_state();
            }
            "section" => {
                self.section_depth = self.section_depth.saturating_sub(1);
                if self.in_notes {
                    if let Some(note_id) = self.current_note_id.take() {
                        let trimmed = self.current_note_text.trim().to_string();
                        if !trimmed.is_empty() {
                            self.book.footnotes.insert(note_id, trimmed);
                        }
                        self.current_note_text.clear();
                    }
                }
            }
            "title" => {
                self.in_title = false;
                let title_text = self.extract_runs_text(&self.title_runs);
                if !title_text.is_empty() {
                    let ch_idx = self.book.chapters.len();
                    let char_offset = self.current_chapter.as_ref().map(|c| c.char_count()).unwrap_or(0);
                    self.book.toc.push(crate::model::TocEntry {
                        title: title_text.clone(),
                        chapter_idx: ch_idx,
                        byte_offset: 0,
                        char_offset,
                        level: self.section_depth.saturating_sub(1),
                    });
                    if let Some(ref mut chap) = self.current_chapter {
                        chap.title = title_text.clone();
                        chap.blocks.push(Block::Heading {
                            level: 1,
                            runs: std::mem::take(&mut self.title_runs),
                        });
                    }
                }
            }
            "p" | "v" | "subtitle" | "text-author" => {
                self.in_paragraph = false;
                if self.in_notes {
                    if !self.current_note_text.is_empty() && !self.current_note_text.ends_with('\n') {
                        self.current_note_text.push('\n');
                    }
                } else if !self.current_runs.is_empty() {
                    let runs = std::mem::take(&mut self.current_runs);
                    let block = if tag_name == "subtitle" {
                        Block::Heading { level: 2, runs }
                    } else {
                        Block::Paragraph {
                            runs,
                            indent: tag_name == "p" || tag_name == "v",
                            align: self.current_style.align,
                            left_margin_em: if tag_name == "text-author" { 1.5 } else { 0.0 },
                            bullet_prefix: None,
                            is_quote: tag_name == "text-author",
                        }
                    };

                    if self.in_title {
                        self.title_runs.extend(match block {
                            Block::Paragraph { runs, .. } | Block::Heading { runs, .. } => runs,
                            _ => Vec::new(),
                        });
                    } else {
                        self.ensure_chapter(None);
                        if let Some(ref mut chap) = self.current_chapter {
                            chap.blocks.push(block);
                        }
                    }
                }
            }
            "strong" | "b" | "emphasis" | "em" | "i" | "a" => {
                if let Some(prev) = self.style_stack.pop() {
                    self.current_style = prev;
                }
            }
            "binary" => {
                if let Some(id) = self.binary_id.take() {
                    let clean_base64: String = self.binary_buf.chars().filter(|c| !c.is_whitespace()).collect();
                    if let Ok(decoded) = BASE64_STANDARD.decode(clean_base64) {
                        self.book.add_image(id, decoded);
                    }
                }
                self.pop_state();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_text(&mut self, raw_text: &str) -> Result<(), String> {
        let state = self.current_state();
        match state {
            ParserState::InBookTitle => {
                self.book.meta.title.push_str(raw_text.trim());
            }
            ParserState::InFirstName => {
                self.author_first.push_str(raw_text.trim());
            }
            ParserState::InLastName => {
                self.author_last.push_str(raw_text.trim());
            }
            ParserState::InLang => {
                self.book.meta.language.push_str(raw_text.trim());
            }
            ParserState::InBinary => {
                self.binary_buf.push_str(raw_text);
            }
            _ => {
                // Only collect text when actively inside a paragraph, title, or note
                if self.in_paragraph || self.in_title {
                    let normalized = normalize_spaces(raw_text);
                    if !normalized.is_empty() {
                        if self.in_notes {
                            if !self.current_note_text.is_empty() && !self.current_note_text.ends_with(' ') && !self.current_note_text.ends_with('\n') {
                                self.current_note_text.push(' ');
                            }
                            self.current_note_text.push_str(&normalized);
                        } else {
                            self.ensure_chapter(None);
                            if let Some(ref mut chap) = self.current_chapter {
                                let start = chap.text.len();
                                chap.text.push_str(&normalized);
                                let end = chap.text.len();
                                self.current_runs.push(Run {
                                    start,
                                    end,
                                    style: self.current_style.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn extract_runs_text(&self, runs: &[Run]) -> String {
        if let Some(ref chap) = self.current_chapter {
            runs.iter()
                .filter_map(|r| chap.text.get(r.start..r.end))
                .collect::<Vec<_>>()
                .join("")
        } else {
            String::new()
        }
    }
}

/// Normalize XML whitespace: collapse multiple whitespaces, carriage returns,
/// newlines and tabs into single spaces.
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

/// Parse an FB2 or FB2.ZIP file from byte slice
pub fn parse_fb2(data: &[u8]) -> Result<Book, String> {
    // Check if zip archive (magic PK\x03\x04)
    if data.starts_with(b"PK\x03\x04") {
        let cursor = Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("Zip error: {:?}", e))?;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| format!("Zip file error: {:?}", e))?;
            if file.name().ends_with(".fb2") || file.name().ends_with(".xml") {
                let mut xml_data = Vec::new();
                file.read_to_end(&mut xml_data).map_err(|e| format!("Read zip entry error: {:?}", e))?;
                let parser = Fb2Parser::new(Cursor::new(xml_data));
                return parser.parse();
            }
        }
        Err("No .fb2 file found in zip archive".to_string())
    } else {
        let parser = Fb2Parser::new(Cursor::new(data));
        parser.parse()
    }
}

/// Parse an FB2 / FB2.ZIP file from disk, streaming — no whole-file read
/// (a 4MB novel never materializes as a Vec).
pub fn parse_fb2_path(path: &std::path::Path) -> Result<Book, String> {
    let mut magic = [0u8; 4];
    let is_zip = match std::fs::File::open(path) {
        Ok(mut f) => f.read_exact(&mut magic).is_ok() && &magic == b"PK\x03\x04",
        Err(e) => return Err(format!("Cannot open FB2 '{}': {}", path.display(), e)),
    };
    if is_zip {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Zip error: {:?}", e))?;
        for i in 0..archive.len() {
            let file = archive.by_index(i).map_err(|e| format!("Zip file error: {:?}", e))?;
            if file.name().ends_with(".fb2") || file.name().ends_with(".xml") {
                let parser = Fb2Parser::new(std::io::BufReader::new(file));
                return parser.parse();
            }
        }
        Err("No .fb2 file found in zip archive".to_string())
    } else {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let parser = Fb2Parser::new(std::io::BufReader::new(file));
        parser.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_FB2: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description>
  <title-info>
    <genre>sf</genre>
    <author>
      <first-name>Arthur</first-name>
      <last-name>Clarke</last-name>
    </author>
    <book-title>Rendezvous with Rama</book-title>
    <lang>en</lang>
    <coverpage>
      <image l:href="#cover.jpg"/>
    </coverpage>
  </title-info>
</description>
<body>
  <section>
    <title>
      <p>Chapter 1: Spaceguard</p>
    </title>
    <p>Sooner or later, it was bound to happen.</p>
    <p>On <strong>June 30, 1908</strong>, Moscow escaped destruction by <em>three hours</em> and four thousand kilometers.</p>
    <empty-line/>
    <p>In those days, man could do <a l:href="#n1" type="note">nothing</a> to protect himself against the rocks from space.</p>
  </section>
</body>
<body name="notes">
  <section id="n1">
    <title><p>1</p></title>
    <p>Astronomical footnote.</p>
  </section>
</body>
<binary id="cover.jpg" content-type="image/jpeg">
aGVsbG8gd29ybGQ=
</binary>
</FictionBook>"##;

    #[test]
    fn test_parse_sample_fb2() {
        let book = parse_fb2(SAMPLE_FB2.as_bytes()).expect("parse fb2");
        assert_eq!(book.meta.title, "Rendezvous with Rama");
        assert_eq!(book.meta.authors, vec!["Arthur Clarke"]);
        assert_eq!(book.meta.cover_image_id, Some("cover.jpg".to_string()));
        assert_eq!(book.images.get("cover.jpg").map(|v| v.as_slice()), Some(b"hello world".as_ref()));

        assert_eq!(book.chapters.len(), 1);
        let ch1 = &book.chapters[0];
        assert_eq!(ch1.title, "Chapter 1: Spaceguard");
        assert_eq!(ch1.blocks.len(), 5); // Heading, Para, Para, Spacer, Para

        // Check runs in June 30 paragraph
        if let Block::Paragraph { runs, .. } = &ch1.blocks[2] {
            assert!(runs.iter().any(|r| r.style.font_style == FontStyle::Bold));
            assert!(runs.iter().any(|r| r.style.font_style == FontStyle::Italic));
        } else {
            panic!("Expected paragraph");
        }

        // Check footnote target
        if let Block::Paragraph { runs, .. } = &ch1.blocks[4] {
            let note_run = runs.iter().find(|r| r.style.footnote_ref.is_some());
            assert_eq!(note_run.and_then(|r| r.style.footnote_ref.as_deref()), Some("n1"));
        } else {
            panic!("Expected footnote ref in paragraph");
        }
    }
}
