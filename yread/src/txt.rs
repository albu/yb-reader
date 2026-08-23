//! Plain-text books: bytes → one reflowable chapter. The format has no
//! markup and no metadata, so the title is the filename and paragraphs
//! are blank-line groups whose hard-wrapped lines are joined — the
//! engine reflows them to the panel instead of honoring the 80-column
//! wrap some 1990s terminal imposed. A file without a single blank line
//! is line-per-paragraph (the list/lyrics shape). Non-UTF-8 bytes decode
//! as windows-1251 — the encoding legacy Cyrillic .txt books actually
//! arrive in — and UTF-16 gets a clear error rather than mojibake.

use std::path::Path;

use crate::model::{
    over_cap, read_capped, Block, Book, Chapter, Run, Style, TextAlign, TocEntry, MAX_ENTRY_BYTES,
};

pub fn parse_txt_path(path: &Path) -> Result<Book, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot open TXT '{}': {}", path.display(), e))?;
    let bytes = read_capped(file, MAX_ENTRY_BYTES).map_err(|e| format!("read: {}", e))?;
    if over_cap(bytes.len(), MAX_ENTRY_BYTES) {
        return Err(format!("TXT over {} MB", MAX_ENTRY_BYTES / 1_048_576));
    }
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Text".into());
    parse_txt_bytes(&bytes, &title)
}

/// Parse from memory (tests, future callers). `title` is the display title.
pub fn parse_txt_bytes(bytes: &[u8], title: &str) -> Result<Book, String> {
    let text = decode(bytes)?;
    let mut book = Book::default();
    book.meta.title = title.to_string();
    let chap = chapter_from_text(&text, title);
    // One entry so the TOC dialog has something to show and the scrubber
    // has a chapter name.
    book.toc.push(TocEntry {
        title: title.to_string(),
        chapter_idx: 0,
        byte_offset: 0,
        char_offset: 0,
        level: 0,
    });
    book.chapters.push(chap);
    Ok(book)
}

fn decode(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err("UTF-16 text file — convert to UTF-8 first".into());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]) {
        return Ok(String::from_utf8_lossy(rest).into_owned());
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Ok(decode_cp1251(bytes)),
    }
}

/// windows-1251 high half, index = byte − 0x80. 0x98 is officially
/// undefined in the codec and maps to REPLACEMENT CHARACTER.
const CP1251: [char; 128] = [
    'Ђ', 'Ѓ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ', 'ђ', '‘', '’',
    '“', '”', '•', '–', '—', '\u{FFFD}', '™', 'љ', '›', 'њ', 'ќ', 'ћ', 'џ', '\u{A0}', 'Ў', 'ў',
    'Ј', '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '\u{AD}', '®', 'Ї', '°', '±', 'І', 'і', 'ґ',
    'µ', '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї', 'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З',
    'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', 'П', 'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Ц', 'Ч', 'Ш', 'Щ', 'Ъ',
    'Ы', 'Ь', 'Э', 'Ю', 'Я', 'а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н',
    'о', 'п', 'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', 'я',
];

fn decode_cp1251(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                CP1251[(b - 0x80) as usize]
            }
        })
        .collect()
}

fn chapter_from_text(raw: &str, title: &str) -> Chapter {
    // Normalize newlines once: CRLF and lone CR both become \n.
    let normalized = if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        raw.to_string()
    };
    let mut lines: Vec<&str> = normalized.split('\n').map(str::trim_end).collect();
    // The file's trailing (and leading) newline produces empty edge
    // lines — those are not paragraph structure, and treating them as
    // such collapses a line-per-paragraph file into one giant paragraph.
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }

    let mut paras: Vec<String> = Vec::new();
    if lines.iter().any(|l| l.trim().is_empty()) {
        // Blank-line groups: hard-wrapped lines join with one space so
        // layout can reflow them.
        let mut cur = String::new();
        for line in &lines {
            let t = line.trim();
            if t.is_empty() {
                if !cur.is_empty() {
                    paras.push(std::mem::take(&mut cur));
                }
            } else if cur.is_empty() {
                cur.push_str(t);
            } else {
                cur.push(' ');
                cur.push_str(t);
            }
        }
        if !cur.is_empty() {
            paras.push(cur);
        }
    } else {
        // No blank-line structure: each line is its own paragraph.
        for line in &lines {
            let t = line.trim();
            if !t.is_empty() {
                paras.push(t.to_string());
            }
        }
    }

    let mut chap = Chapter::new("txt", title);
    let style = Style {
        indent: true,
        ..Style::default()
    };
    for p in paras {
        let start = chap.text.len();
        chap.text.push_str(&p);
        let end = chap.text.len();
        chap.blocks.push(Block::Paragraph {
            runs: vec![Run {
                start,
                end,
                style: style.clone(),
            }],
            indent: true,
            align: TextAlign::Justify,
            left_margin_em: 0.0,
            bullet_prefix: None,
            is_quote: false,
        });
    }
    chap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_wrapped_blank_line_paragraphs_join() {
        let book = parse_txt_bytes(
            b"It was the best of times,\r\nit was the worst of times,\r\n\r\nit was the age of wisdom,\r\nit was the age of foolishness,\r\n",
            "A Tale",
        )
        .unwrap();
        assert_eq!(book.meta.title, "A Tale");
        assert_eq!(book.chapters.len(), 1);
        let c = &book.chapters[0];
        assert_eq!(c.blocks.len(), 2);
        // Adjacent paragraphs' flat text concatenates with no separator —
        // the same convention as the EPUB/FB2 parsers: blocks are the
        // layout units, `text` exists for offset math and search.
        assert_eq!(
            c.text,
            "It was the best of times, it was the worst of times,\
             it was the age of wisdom, it was the age of foolishness,"
        );
        // Runs tile the flat text exactly, in order — the layout engine
        // walks blocks and slices text by these ranges.
        let mut at = 0usize;
        for b in &c.blocks {
            if let Block::Paragraph { runs, .. } = b {
                for r in runs {
                    assert_eq!(r.start, at);
                    at = r.end;
                }
            }
        }
        assert_eq!(at, c.text.len());
        // One TOC entry so the dialog isn't empty.
        assert_eq!(book.toc.len(), 1);
        assert_eq!(book.toc[0].title, "A Tale");
    }

    #[test]
    fn no_blank_lines_is_line_per_paragraph() {
        let book = parse_txt_bytes(b"line one\nline two\n", "List").unwrap();
        let c = &book.chapters[0];
        assert_eq!(c.blocks.len(), 2);
        assert_eq!(c.text, "line oneline two");
    }

    #[test]
    fn cp1251_fallback_decodes_cyrillic() {
        // "Привет" in windows-1251 — invalid UTF-8 by construction
        // (the compiler even warns the literal can never decode as one).
        let bytes = b"\xCF\xF0\xE8\xE2\xE5\xF2";
        let book = parse_txt_bytes(bytes, "t").unwrap();
        assert_eq!(book.chapters[0].text, "Привет");
    }

    #[test]
    fn utf16_rejected_with_clear_error() {
        assert!(parse_txt_bytes(b"\xFF\xFE\x00\x00", "t").is_err());
    }
}
