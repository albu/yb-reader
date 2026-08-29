//! User dictionaries.
//!
//! The app ships zero dictionary data. Users bring their own free
//! FreeDict dictionaries — the `*.src.tar.xz` release from freedict.org
//! (a TEI XML file inside), dropped into the dictionaries folder (over
//! USB, or through the receive page's Dictionaries tab). English books
//! always get the builtin WordNet definition row; per-language defaults
//! (configured on the receive page) pick the translation dictionary,
//! with a per-book override available from the word card.
//!
//! TEI is the *import* format. On first use each dictionary is converted
//! once into a compact `.ybdict` (translations extracted, sorted by
//! normalized word), so lookups are a binary search over a packed index
//! with meanings read from the blob — no per-tap XML parsing. The TEI
//! file states every field explicitly (<orth>, <cit type="trans">,
//! <pron>, <pos>), so the import needs none of the HTML-heuristic
//! parsing a rendered StarDict build would.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
/// Serializes tests that mutate the process-global YB_DICT_DIR env (the
/// receive tests use the same pattern for their env roots).
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Top-level and USB-obvious, right next to `documents/` and
/// `screensavers/`: unpack a downloaded FreeDict `src` archive here over
/// USB.
pub const DICT_DIR: &str = "/mnt/us/dictionaries";
/// The builtin English dictionary (WordNet glosses, built by
/// tools/build_wordnet) — deployed by deploy.sh, never committed.
pub const BUILTIN_PATH: &str = "/mnt/us/extensions/reader/data/wordnet.ybdict";

/// Bumped when the importer's meaning content changes, so previously
/// imported `.ybdict`s re-convert instead of serving stale text.
const YBDICT_MAGIC: &[u8; 8] = b"YBDICT02";
/// Refuse absurd files instead of OOMing a ~150 MB-RAM device.
const MAX_DICT_BYTES: usize = 64 * 1024 * 1024;

/// One lookup result from the active dictionaries.
#[derive(Debug, Clone)]
pub struct DictEntry {
    pub word: String,
    pub meaning: String,
    pub source: String,
}

/// The two rows a lookup can produce: the English definition (WordNet,
/// always consulted for English books) and the translation (the
/// user-selected dictionary).
#[derive(Debug, Clone, Default)]
pub struct WordResult {
    pub definition: Option<DictEntry>,
    pub translation: Option<DictEntry>,
}

impl WordResult {
    pub fn word(&self) -> &str {
        self.definition
            .as_ref()
            .or(self.translation.as_ref())
            .map(|e| e.word.as_str())
            .unwrap_or("")
    }
}

/// The activated dictionaries, persisted as base names. WordNet is not in
/// here — it is the always-on definition row. Lookups cycle through the
/// active list (in order), and a book can pin one of them via
/// `book_override` (the word card's "next dictionary").
#[derive(Debug, Clone, Default)]
pub struct DictSelection {
    pub active: Vec<String>,
}

/// A discovered dictionary (imported or importable).
#[derive(Debug, Clone)]
pub struct DictInfo {
    pub base: String,
    pub name: String,
    pub word_count: u32,
    #[allow(dead_code)]
    pub imported: bool,
}

fn dict_dir() -> String {
    std::env::var("YB_DICT_DIR").unwrap_or_else(|_| DICT_DIR.to_string())
}

/// Find every `{base}.tei` under the dictionaries dir (a downloaded
/// FreeDict `src` archive nests its files one level deep, and a
/// mis-upload can leave a second copy in a stray folder).
fn find_teis(base: &str) -> Vec<PathBuf> {
    let dir = dict_dir();
    let root = Path::new(&dir);
    let mut stack = vec![root.to_path_buf()];
    let mut hits = vec![];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_stem().and_then(|s| s.to_str()) == Some(base)
                && p.extension().and_then(|x| x.to_str()) == Some("tei")
            {
                hits.push(p);
            }
        }
    }
    hits
}

/// Pick the canonical `.tei` for a base out of every copy found. The
/// FreeDict layout is `{base}/{base}.tei`; prefer a file whose parent
/// directory is named exactly `base`, then the shallowest path, then the
/// newest file. Keeps scan() and the importer agreeing on the same copy.
fn rank_src(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().max_by(|a, b| {
        score_src(a)
            .cmp(&score_src(b))
            .then_with(|| a.cmp(b))
    })
}

fn score_src(p: &Path) -> (bool, usize, std::time::SystemTime) {
    let parent_is_base = p
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|s| s.to_str())
        .map(|d| d == p.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
        .unwrap_or(false);
    let depth = p.components().count();
    let mtime = p
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    (parent_is_base, usize::MAX - depth, mtime)
}

fn find_src(base: &str) -> Option<PathBuf> {
    rank_src(find_teis(base))
}

fn config_path() -> PathBuf {
    Path::new(&dict_dir()).join("selection.txt")
}

fn ybdict_path(base: &str) -> PathBuf {
    Path::new(&dict_dir()).join(format!("{base}.ybdict"))
}

// ------------------------------------------------------------- selection --

pub fn load_selection() -> DictSelection {
    let Ok(s) = fs::read_to_string(config_path()) else {
        return DictSelection::default();
    };
    let mut sel = DictSelection::default();
    for line in s.lines() {
        let mut it = line.splitn(2, ' ');
        let key = it.next().unwrap_or("");
        let val = it.next().unwrap_or("").trim();
        match key {
            "active" => {
                if !val.is_empty() {
                    sel.active.push(val.to_string());
                }
            }
            // Older selection files carried primary/secondary/lang lines;
            // those models are gone, so the lines are simply ignored.
            _ => {}
        }
    }
    sel
}

pub fn save_selection(sel: &DictSelection) {
    if let Some(p) = config_path().parent() {
        let _ = fs::create_dir_all(p);
    }
    let mut body = String::new();
    for base in &sel.active {
        body.push_str(&format!("active {}\n", base));
    }
    // Same durability contract as every other persisted store: a power
    // cut must cost at most the previous contents, never an empty file
    // that silently resets every activation.
    ybdev::atomic::write(config_path(), body.as_bytes());
}

/// Add or remove `base` from the active list. Rebuilding an unbuilt or
/// stale dictionary happens right here, on the activate tap (~2 s on the
/// UI thread — the user waits once, and the dictionary just works).
pub fn toggle_active(sel: &mut DictSelection, base: &str) {
    if sel.active.iter().any(|b| b == base) {
        sel.active.retain(|b| b != base);
    } else {
        if needs_rebuild(base) {
            if rebuild(base) {
                clear_rebuild_failed(base);
            } else {
                mark_rebuild_failed(base);
            }
        }
        sel.active.push(base.to_string());
    }
    save_selection(sel);
    prune_cache(&sel.active);
}

/// Delete a dictionary from the device (removes its .ybdict, .tei source,
/// folder if named after base, active selection, and per-book overrides).
pub fn delete_dictionary(base: &str) -> bool {
    let mut ok = false;
    let yb = ybdict_path(base);
    if yb.exists() {
        if fs::remove_file(&yb).is_ok() {
            ok = true;
        }
    }
    for tei in find_teis(base) {
        let parent = tei.parent();
        if let Some(p) = parent {
            if p.file_name().and_then(|n| n.to_str()) == Some(base) {
                if fs::remove_dir_all(p).is_ok() {
                    ok = true;
                }
                continue;
            }
        }
        if fs::remove_file(&tei).is_ok() {
            ok = true;
        }
    }
    // Also remove from selection
    let mut sel = load_selection();
    if sel.active.iter().any(|b| b == base) {
        sel.active.retain(|b| b != base);
        save_selection(&sel);
    }
    // Remove from book overrides
    let bo_path = book_overrides_path();
    if let Ok(s) = fs::read_to_string(&bo_path) {
        let mut lines: Vec<String> = Vec::new();
        let mut changed = false;
        for line in s.lines() {
            if let Some((_, b)) = line.split_once('\t') {
                if b == base {
                    changed = true;
                    continue;
                }
            }
            lines.push(line.to_string());
        }
        if changed {
            let mut body = lines.join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            let _ = ybdev::atomic::write(&bo_path, body.as_bytes());
        }
    }
    prune_cache(&sel.active);
    ok
}

// ---------------------------------------------------------------- scan --

/// List every dictionary in the folder: importable TEI sources and
/// already-imported `.ybdict`s, deduplicated by base name. A stray copy
/// of the same set (e.g. an older folder left next to the real one)
/// collapses into a single row, and the canonical copy wins.
pub fn scan() -> Vec<DictInfo> {
    let dir = dict_dir();
    let mut src_paths: Vec<(String, PathBuf)> = vec![];
    // Walk the tree (a downloaded FreeDict archive may nest its files).
    let mut stack = vec![dir.clone()];
    let mut seen_ybdict: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.to_string_lossy().into_owned());
                continue;
            }
            let Some(ext) = p.extension().and_then(|x| x.to_str()) else {
                continue;
            };
            let Some(base) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match ext {
                "tei" => {
                    src_paths.push((base.to_string(), p.clone()));
                }
                "ybdict" => {
                    seen_ybdict.insert(base.to_string());
                }
                _ => {}
            }
        }
    }
    // Dedupe: one row per base, keeping the canonical source copy.
    let mut best_path: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();
    for (base, p) in src_paths {
        let better = match best_path.get(&base) {
            Some(cur) => rank_src(vec![p.clone(), cur.clone()])
                .map(|winner| winner == p)
                .unwrap_or(false),
            None => true,
        };
        if better {
            best_path.insert(base, p);
        }
    }
    let mut infos: Vec<DictInfo> = best_path
        .into_iter()
        .map(|(base, p)| info_from_src(&p, &base))
        .collect();
    for base in seen_ybdict {
        // A bare `.ybdict` with no source is listed only when its header
        // is valid — a corrupt/truncated file would otherwise show as
        // "installed" and silently contribute nothing at lookup time.
        if ybdict_valid(&ybdict_path(&base)) && !infos.iter().any(|i| i.base == base) {
            infos.push(DictInfo {
                base: base.clone(),
                name: base.clone(),
                word_count: ybdict_count(&base),
                imported: true,
            });
        }
    }
    infos.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    infos
}

fn info_from_src(src: &Path, base: &str) -> DictInfo {
    let (name, wc) = tei_info(src);
    DictInfo {
        base: base.to_string(),
        name,
        word_count: wc,
        imported: ybdict_fresh(base, src),
    }
}

fn ybdict_fresh(base: &str, src: &Path) -> bool {
    let yb = ybdict_path(base);
    match (fs::metadata(&yb), fs::metadata(src)) {
        (Ok(m1), Ok(m2)) => {
            // The format must also match: a stale-format ybdict (older
            // magic) is not "imported" even when its mtime is newer.
            m1.modified().ok() >= m2.modified().ok() && ybdict_valid(&yb)
        }
        _ => false,
    }
}

/// Read just the 24-byte header (magic + entry count) — never the
/// multi-MB body. scan()/ybdict_fresh run on the System screen's every
/// redraw; opening the whole blob there froze the UI for seconds.
fn ybdict_header(path: &Path) -> Option<u32> {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(path) else {
        return None;
    };
    let mut hdr = [0u8; 24];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..8] != YBDICT_MAGIC {
        return None;
    }
    Some(u32::from_be_bytes(hdr[8..12].try_into().ok()?))
}

fn ybdict_valid(path: &Path) -> bool {
    ybdict_header(path).is_some()
}

fn ybdict_count(base: &str) -> u32 {
    ybdict_header(&ybdict_path(base)).unwrap_or(0)
}

/// A dictionary the lookup chain may load: a `.ybdict` that is not older
/// than its source (or has no source — a bare converted upload). This
/// check never converts — conversion happens in `toggle_active` (on
/// activate) and in `open_active` (a stale active dictionary re-converts
/// in place, a couple of seconds on the UI thread).
fn ybdict_usable(base: &str) -> bool {
    match find_src(base) {
        Some(src) => ybdict_fresh(base, &src),
        None => ybdict_path(base).exists(),
    }
}

/// Does this dictionary need a rebuild? A source `.tei` whose `.ybdict`
/// is missing or older than the source. Surface it in the Dictionaries
/// screen, and let the activate path start it there.
#[allow(dead_code)] // exercised by tests; the screen uses scan().imported
pub fn needs_rebuild(base: &str) -> bool {
    match find_src(base) {
        Some(src) => !ybdict_fresh(base, &src),
        None => false,
    }
}

/// Rebuild the TEI → `.ybdict` conversion. Measured at ~2 s on-device
/// for a typical FreeDict source, so it is run synchronously on the UI
/// thread: from `toggle_active` when activating a not-yet-built
/// dictionary, and from `open_active` when an active dictionary's source
/// has changed since its last import.
pub fn rebuild(base: &str) -> bool {
    convert(base).is_some()
}

// ------------------------------------------------------------- convert --

/// Display name and advertised headword count from a TEI header —
/// teiHeader's <title> and <extent> ("62181 headwords"). scan() calls
/// this on every redraw, so it reads only the first 8 KiB; both elements
/// sit in the header prose at the top of the file.
fn tei_info(path: &Path) -> (String, u32) {
    use std::io::Read;
    let mut head = vec![0u8; 8192];
    let n = fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&head[..n]);

    let name = tag_body(&text, "<title>", "</title>")
        .map(|t| xml_unescape(t.trim()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Dictionary".to_string());
    let wc = tag_body(&text, "<extent>", "</extent>")
        .and_then(|extent| {
            let digits: &str = &extent
                [..extent.find(|c: char| !c.is_ascii_digit()).unwrap_or(extent.len())];
            digits.parse().ok()
        })
        .unwrap_or(0);
    (name, wc)
}

/// The content between the first `open` and the `close` that follows it.
/// Never panics on a closing tag that precedes its opener — a corrupt or
/// foreign .tei must degrade to defaults, not crash the Dictionaries
/// screen.
fn tag_body<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let s = text.find(open)?;
    let rest = &text[s + open.len()..];
    let e = rest.find(close)?;
    Some(&rest[..e])
}

/// Read a file but refuse anything over [`MAX_DICT_BYTES`]: a bigger one
/// is corrupt/absurd on a device with a ~150 MB budget. Returns None both
/// on I/O failure and on an over-limit file.
fn read_capped(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut out = Vec::new();
    f.take(MAX_DICT_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > MAX_DICT_BYTES {
        return None;
    }
    Some(out)
}



/// Resolve the five XML predefined entities plus numeric refs. The TEI
/// is machine-generated, so anything else would be a surprise worth
/// seeing verbatim rather than silently mishandled.
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if let Some(v) = [
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&apos;", "'"),
            ("&amp;", "&"),
        ]
        .iter()
        .find(|(ent, _)| tail.starts_with(ent))
        .map(|(_, rep)| *rep)
        {
            out.push_str(v);
            rest = &tail[tail.find(';').map_or(5, |p| p + 1).min(tail.len())..];
            continue;
        }
        // &#NN; / &#xHH;
        if tail.len() > 3 && tail[1..].starts_with('#') {
            if let Some(semi) = tail.find(';') {
                let body = &tail[2..semi];
                let cp = if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X'))
                {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    body.parse::<u32>().ok()
                }
                .and_then(char::from_u32);
                if let Some(ch) = cp {
                    out.push(ch);
                    rest = &tail[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

fn decode_field(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    xml_unescape(&s)
}

/// Parse a FreeDict TEI dictionary into (clean, word, meaning) rows.
/// Uses precompiled memmem finders to parse multi-megabyte XML files
/// in hundreds of milliseconds.
fn parse_tei(xml: &[u8]) -> Option<Vec<(String, String, String)>> {
    let entry_finder = memchr::memmem::Finder::new(b"<entry");
    let entry_end_finder = memchr::memmem::Finder::new(b"</entry>");
    let orth_finder = memchr::memmem::Finder::new(b"<orth>");
    let orth_end_finder = memchr::memmem::Finder::new(b"</orth>");
    let cit_finder = memchr::memmem::Finder::new(b"<cit");
    let cit_end_finder = memchr::memmem::Finder::new(b"</cit>");
    let quote_finder = memchr::memmem::Finder::new(b"<quote>");
    let quote_end_finder = memchr::memmem::Finder::new(b"</quote>");

    if entry_finder.find(xml).is_none() {
        return None;
    }
    let mut items: Vec<(String, String, String)> = Vec::new();
    let mut pos = 0usize;
    while let Some(e_start_rel) = entry_finder.find(&xml[pos..]) {
        let e_start = pos + e_start_rel;
        let Some(e_end_rel) = entry_end_finder.find(&xml[e_start..]) else {
            break;
        };
        let e_end = e_start + e_end_rel + 8;
        let entry = &xml[e_start..e_end];

        let word_opt = if let Some(s) = orth_finder.find(entry) {
            let body = &entry[s + 6..];
            if let Some(e) = orth_end_finder.find(body) {
                Some(decode_field(&body[..e]))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(word) = word_opt {
            let clean = crate::vocab::clean_word(&word);
            if !clean.is_empty() {
                let mut quotes: Vec<String> = Vec::new();
                let mut cpos = 0usize;
                while let Some(c_rel) = cit_finder.find(&entry[cpos..]) {
                    let c = cpos + c_rel;
                    let tag_end = memchr::memchr(b'>', &entry[c..]).map_or(entry.len(), |p| c + p + 1);
                    let Some(body_end_rel) = cit_end_finder.find(&entry[tag_end..]) else {
                        break;
                    };
                    let body_end = tag_end + body_end_rel + 6;
                    let is_trans = entry[c..tag_end.min(entry.len())]
                        .windows(12)
                        .any(|w| w == b"type=\"trans\"");
                    if is_trans {
                        let mut qpos = tag_end;
                        while let Some(qs_rel) = quote_finder.find(&entry[qpos..]) {
                            let qs = qpos + qs_rel;
                            if qs >= body_end {
                                break;
                            }
                            let Some(qe_rel) = quote_end_finder.find(&entry[qs..]) else {
                                break;
                            };
                            let qe = qs + qe_rel;
                            if qe > body_end {
                                break;
                            }
                            let q = decode_field(&entry[qs + 7..qe]);
                            let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !q.is_empty() && !quotes.contains(&q) {
                                quotes.push(q);
                            }
                            qpos = qe + 8;
                        }
                    }
                    cpos = body_end;
                }
                if !quotes.is_empty() {
                    items.push((clean, word, quotes.join(", ")));
                }
            }
        }
        if e_end >= xml.len() {
            break;
        }
        pos = e_end;
    }
    Some(items)
}

fn convert(base: &str) -> Option<()> {
    let t0 = std::time::Instant::now();
    let src = find_src(base)?;
    let xml = read_capped(&src)?;
    let t_read = t0.elapsed();
    let mut items = parse_tei(&xml)?;
    let t_parse = t0.elapsed() - t_read;
    if items.is_empty() {
        return None;
    }
    // WikDict splits homonyms into one entry per part of speech, all
    // with the same headword. Merge their translation rows instead of
    // keeping only the first: "house" must show дом (noun entry) as
    // well as вмеща́ть (verb entry).
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let mut merged: Vec<(String, String, String)> = Vec::with_capacity(items.len());
    for (clean, word, meaning) in items {
        match merged.last_mut() {
            Some(last) if last.0 == clean => {
                for part in meaning.split(", ") {
                    if !last.2.split(", ").any(|p| p == part) {
                        if !last.2.is_empty() {
                            last.2.push_str(", ");
                        }
                        last.2.push_str(part);
                    }
                }
            }
            _ => merged.push((clean, word, meaning)),
        }
    }
    let wrote = write_ybdict(base, &merged).is_some();
    let total_ms = t0.elapsed().as_millis();
    let msg = format!(
        "dictionary: convert {base} in {total_ms}ms (read {:.2?}, parse {:.2?}, merge+write {:.2?}, {} entries, {})",
        t_read,
        t_parse,
        t0.elapsed() - t_read - t_parse,
        merged.len(),
        if wrote { "ok" } else { "write failed" }
    );
    eprintln!("yb-reader: {msg}");
    ybdev::log::plog(&msg);
    if wrote { Some(()) } else { None }
}

fn write_ybdict(base: &str, items: &[(String, String, String)]) -> Option<()> {
    use std::io::{BufWriter, Write};
    let count = items.len();
    let index_len = count * 20;
    let total_words: usize = items.iter().map(|(_, w, _)| w.len()).sum();
    let total_keys: usize = items.iter().map(|(c, _, _)| c.len()).sum();
    let words_off = 24 + index_len;
    let keys_off = words_off + total_words;
    let data_off = keys_off + total_keys;
    let tmp = ybdict_path(base).with_extension("ybdict.tmp");
    let result = (|| -> Option<()> {
        let f = std::fs::File::create(&tmp).ok()?;
        let mut writer = BufWriter::with_capacity(128 * 1024, f);
        writer.write_all(YBDICT_MAGIC).ok()?;
        writer.write_all(&(count as u32).to_be_bytes()).ok()?;
        writer.write_all(&(words_off as u32).to_be_bytes()).ok()?;
        writer.write_all(&(keys_off as u32).to_be_bytes()).ok()?;
        writer.write_all(&(data_off as u32).to_be_bytes()).ok()?;
        let mut w_run = 0u32;
        let mut k_run = 0u32;
        let mut d_run = 0u32;
        let mut rec = [0u8; 20];
        for (clean, word, meaning) in items {
            rec[0..4].copy_from_slice(&w_run.to_be_bytes());
            rec[4..6].copy_from_slice(&(word.len() as u16).to_be_bytes());
            rec[6..10].copy_from_slice(&k_run.to_be_bytes());
            rec[10..12].copy_from_slice(&(clean.len() as u16).to_be_bytes());
            rec[12..16].copy_from_slice(&d_run.to_be_bytes());
            rec[16..20].copy_from_slice(&(meaning.len() as u32).to_be_bytes());
            writer.write_all(&rec).ok()?;
            w_run += word.len() as u32;
            k_run += clean.len() as u32;
            d_run += meaning.len() as u32;
        }
        for (_, w, _) in items {
            writer.write_all(w.as_bytes()).ok()?;
        }
        for (c, _, _) in items {
            writer.write_all(c.as_bytes()).ok()?;
        }
        for (_, _, m) in items {
            writer.write_all(m.as_bytes()).ok()?;
        }
        writer.flush().ok()?;
        let f = writer.into_inner().ok()?;
        f.sync_all().ok()?;
        drop(f);
        std::fs::rename(&tmp, ybdict_path(base)).ok()?;
        Some(())
    })();
    if result.is_none() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// ------------------------------------------------------------- runtime --

/// An imported dictionary: the whole `.ybdict` in memory, binary-searched.
/// The blob is shared via `Rc` so clones (per open, per trainer screen)
/// are cheap and the parsed blobs are cached process-wide per thread.
#[derive(Clone)]
pub struct Dictionary {
    data: std::rc::Rc<Vec<u8>>,
    count: usize,
    words_off: usize,
    keys_off: usize,
    data_off: usize,
    name: String,
}

thread_local! {
    /// Keyed by (path, len, mtime): a re-import changes the mtime, so a
    /// stale blob is never served, while opening the same 15 MB WordNet
    /// over and over (every book, every trainer) reads the file once.
    static DICT_CACHE: std::cell::RefCell<
        std::collections::HashMap<(PathBuf, u64, u64), std::rc::Rc<Dictionary>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// (base, source mtime) pairs whose TEI → `.ybdict` conversion already
/// failed in this process. `open_active()` skips a rebuild it has already
/// attempted for the same source, so a dictionary that can never convert
/// (oversize, unparseable, unwritable) doesn't re-pay a multi-second
/// parse on every book open or dialog pop. The attempt is retried only
/// when the source changes.
static FAILED_REBUILDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(String, u64), ()>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn rebuild_attempt_key(base: &str) -> Option<(String, u64)> {
    let src = find_src(base)?;
    let mtime = src
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as u64;
    Some((base.to_string(), mtime))
}

fn rebuild_failed(base: &str) -> bool {
    rebuild_attempt_key(base)
        .map(|k| FAILED_REBUILDS.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&k))
        .unwrap_or(false)
}

fn mark_rebuild_failed(base: &str) {
    if let Some(k) = rebuild_attempt_key(base) {
        FAILED_REBUILDS.lock().unwrap_or_else(|e| e.into_inner()).insert(k, ());
    }
}

fn clear_rebuild_failed(base: &str) {
    if let Some(k) = rebuild_attempt_key(base) {
        FAILED_REBUILDS.lock().unwrap_or_else(|e| e.into_inner()).remove(&k);
    }
}

/// Prune cached dictionaries that are no longer active, releasing RAM.
/// Also drops superseded generations of still-active paths: the cache is
/// keyed (path, len, mtime), so re-importing a dictionary leaves the old
/// blob behind under the same path — only the generation that matches the
/// file on disk right now is kept.
pub fn prune_cache(active_bases: &[String]) {
    let builtin_path = PathBuf::from(BUILTIN_PATH);
    let active_paths: Vec<PathBuf> = active_bases.iter().map(|b| ybdict_path(b)).collect();
    let (before, after) = DICT_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let before = cache.len();
        cache.retain(|(path, len, mtime), _| {
            let is_active = path == &builtin_path || active_paths.contains(path);
            if !is_active {
                return false;
            }
            // Keep only the generation that matches the file on disk.
            match fs::metadata(path) {
                Ok(m) => {
                    let cur_mtime = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64);
                    m.len() == *len && cur_mtime == Some(*mtime)
                }
                Err(_) => false,
            }
        });
        (before, cache.len())
    });
    // Only ask the allocator to give memory back when something actually
    // left the cache — on the common no-op pop this is a no-op too.
    if after < before {
        ybdev::sysinfo::trim_memory();
    }
}

impl Dictionary {
    pub fn open(base: &str) -> Option<Dictionary> {
        Self::open_path(ybdict_path(base), base)
    }

    pub fn open_path<P: AsRef<Path>>(path: P, name: &str) -> Option<Dictionary> {
        let path = path.as_ref();
        let m = fs::metadata(path).ok()?;
        let mtime = m
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos() as u64;
        let key = (path.to_path_buf(), m.len(), mtime);
        DICT_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            if let Some(d) = cache.get(&key) {
                return Some((**d).clone());
            }
            let data = fs::read(path).ok()?;
            let d = Self::from_bytes(data, name)?;
            let rc = std::rc::Rc::new(d);
            let out = (*rc).clone();
            cache.insert(key, rc);
            Some(out)
        })
    }

    fn from_bytes(data: Vec<u8>, name: &str) -> Option<Dictionary> {
        if data.len() < 24 || &data[0..8] != YBDICT_MAGIC {
            return None;
        }
        let count = u32::from_be_bytes(data[8..12].try_into().ok()?) as usize;
        let words_off = u32::from_be_bytes(data[12..16].try_into().ok()?) as usize;
        let keys_off = u32::from_be_bytes(data[16..20].try_into().ok()?) as usize;
        let data_off = u32::from_be_bytes(data[20..24].try_into().ok()?) as usize;
        if words_off > keys_off || keys_off > data_off || data_off > data.len() {
            return None;
        }
        // Clamp to the index region: a corrupt file degrades to fewer
        // entries, never an out-of-bounds read.
        let count = count.min((words_off.saturating_sub(24)) / 20);
        Some(Dictionary {
            data: std::rc::Rc::new(data),
            count,
            words_off,
            keys_off,
            data_off,
            name: name.to_string(),
        })
    }

    /// Used by tests; production reads the count from the header directly.
    #[allow(dead_code)]
    pub fn word_count(&self) -> usize {
        self.count
    }

    fn key_at(&self, rec: usize) -> Option<&str> {
        let off = 24 + rec * 20;
        if off + 20 > self.words_off {
            return None;
        }
        let k_off = u32::from_be_bytes(self.data[off + 6..off + 10].try_into().ok()?) as usize;
        let k_len = u16::from_be_bytes(self.data[off + 10..off + 12].try_into().ok()?) as usize;
        let start = self.keys_off + k_off;
        std::str::from_utf8(self.data.get(start..start + k_len)?).ok()
    }

    fn entry_at(&self, rec: usize) -> Option<DictEntry> {
        let off = 24 + rec * 20;
        let w_off = u32::from_be_bytes(self.data[off..off + 4].try_into().ok()?) as usize;
        let w_len = u16::from_be_bytes(self.data[off + 4..off + 6].try_into().ok()?) as usize;
        let d_off = u32::from_be_bytes(self.data[off + 12..off + 16].try_into().ok()?) as usize;
        let d_len = u32::from_be_bytes(self.data[off + 16..off + 20].try_into().ok()?) as usize;
        let w_start = self.words_off + w_off;
        let d_start = self.data_off + d_off;
        let word = std::str::from_utf8(self.data.get(w_start..w_start + w_len)?)
            .ok()?
            .to_string();
        let meaning = std::str::from_utf8(self.data.get(d_start..d_start + d_len)?)
            .ok()?
            .to_string();
        Some(DictEntry {
            word,
            meaning,
            source: self.name.clone(),
        })
    }

    pub fn lookup_exact(&self, key: &str) -> Option<DictEntry> {
        if self.count == 0 {
            return None;
        }
        let (mut lo, mut hi) = (0usize, self.count - 1);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            match self.key_at(mid)?.cmp(key) {
                std::cmp::Ordering::Equal => return self.entry_at(mid),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => {
                    if mid == 0 {
                        break;
                    }
                    hi = mid - 1;
                }
            }
        }
        None
    }

    /// Normalize the query, then try direct + lemmatized forms.
    pub fn lookup(&self, word: &str) -> Option<DictEntry> {
        let clean = crate::vocab::clean_word(word);
        if clean.is_empty() {
            return None;
        }
        if let Some(e) = self.lookup_exact(&clean) {
            return Some(e);
        }
        for lemma in crate::vocab::generate_lemmas(&clean) {
            if let Some(e) = self.lookup_exact(&lemma) {
                return Some(e);
            }
        }
        None
    }
}

/// The builtin WordNet dictionary (English glosses) — the always-on
/// definition row for English books.
pub fn open_builtin() -> Option<Dictionary> {
    Dictionary::open_path(BUILTIN_PATH, "WordNet")
}

/// Whether the builtin WordNet dictionary is actually deployed (the repo
/// ships zero data; a fresh clone may lack it until tools/build_wordnet
/// is run). Header-only check, so the Dictionaries screen can be honest.
pub fn builtin_available() -> bool {
    let path = Path::new(BUILTIN_PATH);
    fs::metadata(path).map(|m| m.len() >= 24).unwrap_or(false)
        && ybdict_valid(path)
}

/// The active user dictionaries, deduplicated, with indices for the
/// lookup chain.
pub struct ActiveDicts {
    dicts: Vec<Dictionary>,
    by_base: std::collections::HashMap<String, usize>,
    active: Vec<usize>,
}

impl ActiveDicts {
    /// The translation row: an explicit per-book override, else the first
    /// active dictionary that has the word. WordNet is deliberately not
    /// part of this chain — it is the definition row.
    pub fn translation(&self, word: &str, override_base: Option<&str>) -> Option<DictEntry> {
        if let Some(base) = override_base {
            // Explicitly off, or "wordnet" (the definition row only).
            let hit = if base.is_empty() || base == "wordnet" {
                return None;
            } else if let Some(&i) = self.by_base.get(base) {
                self.dicts[i].lookup(word)
            } else {
                Dictionary::open(base).and_then(|d| d.lookup(word))
            };
            if hit.is_some() {
                return hit;
            }
        }
        for &i in &self.active {
            if let Some(e) = self.dicts[i].lookup(word) {
                return Some(e);
            }
        }
        None
    }

    /// The definition row: WordNet, always attempted (it only has English
    /// glosses, so it naturally applies to English words).
    pub fn definition(&self, builtin: Option<&Dictionary>, word: &str) -> Option<DictEntry> {
        if let Some(b) = builtin {
            return b.lookup(word);
        }
        None
    }

    /// Both rows for one word: definition (WordNet) and translation (the
    /// active chain with the per-book override first).
    pub fn lookup_result(
        &self,
        builtin: Option<&Dictionary>,
        word: &str,
        override_base: Option<&str>,
    ) -> WordResult {
        WordResult {
            definition: self.definition(builtin, word),
            translation: self.translation(word, override_base),
        }
    }

    /// The active dictionary base names, in lookup order — the word
    /// card's "next dictionary" cycles through these.
    pub fn active_bases(&self) -> Vec<String> {
        self.active
            .iter()
            .map(|&i| self.dicts[i].name.clone())
            .collect()
    }
}

/// Load the active user dictionaries whose `.ybdict` is usable (present
/// and not older than its source). An active dictionary that is missing
/// or stale is rebuilt in place here (a couple of seconds on the UI
/// thread — the same cost as the activate tap, and it means a dictionary
/// the user replaced over USB just re-imports on the next open).
pub fn open_active() -> ActiveDicts {
    let sel = load_selection();
    prune_cache(&sel.active);
    let mut dicts: Vec<Dictionary> = Vec::new();
    let mut by_base: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut index = |base: &str| -> Option<usize> {
        if let Some(&i) = by_base.get(base) {
            return Some(i);
        }
        if !base.is_empty() {
            if !ybdict_usable(base) && needs_rebuild(base) {
                // Rebuild once per source generation; a source that can
                // never convert must not be re-attempted on every book
                // open / dialog pop (see FAILED_REBUILDS).
                if !rebuild_failed(base) {
                    if rebuild(base) {
                        clear_rebuild_failed(base);
                    } else {
                        mark_rebuild_failed(base);
                    }
                }
            }
            if ybdict_usable(base) {
                if let Some(d) = Dictionary::open(base) {
                    let i = dicts.len();
                    dicts.push(d);
                    by_base.insert(base.to_string(), i);
                    return Some(i);
                }
            }
        }
        None
    };
    let mut active = Vec::new();
    for base in &sel.active {
        if let Some(i) = index(base) {
            active.push(i);
        }
    }
    ActiveDicts {
        dicts,
        by_base,
        active,
    }
}

fn book_overrides_path() -> PathBuf {
    Path::new(&dict_dir()).join("book_overrides.txt")
}

/// The dictionary a specific book has been pinned to. `None` means
/// automatic (the active chain); `Some("")` means the translation row is
/// explicitly off; `Some(base)` pins one dictionary (or "wordnet").
pub fn book_override(name: &str) -> Option<String> {
    let Ok(s) = fs::read_to_string(book_overrides_path()) else {
        return None;
    };
    for line in s.lines() {
        if let Some((n, base)) = line.split_once('\t') {
            if n == name {
                return Some(base.to_string());
            }
        }
    }
    None
}

/// Pin a book to a dictionary. `Some(base)` stores the choice (an empty
/// base means the translation row is explicitly off); `None` clears the
/// override back to automatic.
pub fn set_book_override(name: &str, base: Option<&str>) {
    let path = book_overrides_path();
    let mut lines: Vec<String> = Vec::new();
    if let Ok(s) = fs::read_to_string(&path) {
        for line in s.lines() {
            if let Some((n, _)) = line.split_once('\t') {
                if n == name {
                    continue;
                }
            }
            lines.push(line.to_string());
        }
    }
    if let Some(base) = base {
        lines.push(format!("{}\t{}", name, base));
    }
    if let Some(p) = path.parent() {
        let _ = fs::create_dir_all(p);
    }
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    ybdev::atomic::write(&path, body.as_bytes());
}

/// The cycle order for the word card's "pick a translation" control:
/// automatic -> each active dictionary -> off -> automatic. `current` is
/// the stored override (`None` = automatic, `Some("")` = off, `Some(base)`
/// = pinned). WordNet is not in the cycle — it is the always-on
/// definition row.
pub fn next_translation_choice(current: Option<&str>, active: &[String]) -> Option<String> {
    let mut list: Vec<Option<String>> = vec![None];
    for b in active {
        list.push(Some(b.clone()));
    }
    list.push(Some(String::new())); // explicit off
    let pos = list
        .iter()
        .position(|x| x.as_deref() == current)
        .unwrap_or(0);
    let next = (pos + 1) % list.len();
    list[next].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("yb_dict_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A minimal FreeDict TEI dictionary: the header fields scan() reads
    /// (<title>, <extent>) and three entries, one carrying an entity.
    fn write_tei(dir: &std::path::Path, base: &str) {
        let entries = [("cat", "ко́шка"), ("dog", "собака &amp; пёс"), ("run", "бежать")]
            .map(|(w, t)| {
                format!(
                    "<entry><form><orth>{w}</orth></form><sense>\
                     <cit type=\"trans\" xml:lang=\"ru\"><quote>{t}</quote></cit>\
                     </sense></entry>"
                )
            })
            .concat();
        fs::write(
            dir.join(format!("{base}.tei")),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\n\
                 <teiHeader><fileDesc><titleStmt><title>Test EN-RU</title></titleStmt>\n\
                 <extent>3 headwords</extent></fileDesc></teiHeader>\n\
                 <text><body>{entries}</body></text></TEI>"
            ),
        )
        .unwrap();
    }

    #[test]
    fn converts_and_looks_up_tei() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("convert");
        write_tei(&dir, "test");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        assert!(convert("test").is_some());
        let dict = Dictionary::open("test").expect("open ybdict");
        assert_eq!(dict.word_count(), 3);

        // The meaning is exactly the translations: XML states the fields,
        // so nothing else can leak in.
        let e = dict.lookup("cat").expect("cat");
        assert_eq!(e.meaning, "ко́шка");
        assert_eq!(e.source, "test");

        // Entities resolve (&amp; in the source quote).
        let d = dict.lookup("dog").expect("dog");
        assert_eq!(d.meaning, "собака & пёс");

        // Lemmatized fallback: "cats" -> lemma "cat".
        let e2 = dict.lookup("cats").expect("cats via lemma");
        assert_eq!(e2.word, "cat");

        // Miss.
        assert!(dict.lookup("zzzz").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_from_a_downloaded_folder() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("folder");
        let sub = dir.join("en-ru");
        fs::create_dir_all(&sub).unwrap();
        write_tei(&sub, "en-ru");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        let infos = scan();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].base, "en-ru");
        assert_eq!(infos[0].name, "Test EN-RU");
        assert!(!infos[0].imported);

        assert!(rebuild("en-ru"));
        let dict = Dictionary::open("en-ru").expect("open");
        assert_eq!(dict.word_count(), 3);
        assert!(dict.lookup("dog").is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_active_auto_rebuilds_active_dictionaries() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("stale");
        let sub = dir.join("en-ru");
        fs::create_dir_all(&sub).unwrap();
        write_tei(&sub, "en-ru");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        // Toggling active auto-rebuilds when activating.
        let mut sel = load_selection();
        toggle_active(&mut sel, "en-ru");
        assert!(!needs_rebuild("en-ru"));
        assert_eq!(open_active().active.len(), 1);

        // Touching source makes it stale, but open_active rebuilds on the fly for active dicts.
        write_tei(&sub, "en-ru");
        assert!(needs_rebuild("en-ru"));
        assert_eq!(open_active().active.len(), 1);
        assert!(!needs_rebuild("en-ru"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_copies_collapse_to_one_row() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("dup");
        // The canonical layout: {base}/{base}.tei.
        let canonical = dir.join("eng-rus");
        fs::create_dir_all(&canonical).unwrap();
        write_tei(&canonical, "eng-rus");
        // A stray copy left in a differently-named folder (e.g. an old
        // scp'd folder next to the uploaded one).
        let stray = dir.join("eng-rus.scp-copy");
        fs::create_dir_all(&stray).unwrap();
        write_tei(&stray, "eng-rus");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        let infos = scan();
        assert_eq!(infos.len(), 1, "duplicate base must collapse: {:?}", infos);
        assert_eq!(infos[0].base, "eng-rus");
        assert_eq!(infos[0].name, "Test EN-RU");

        // The importer must agree with scan() on which copy to use, and a
        // ybdict next to a stale source must not create a second row.
        assert!(rebuild("eng-rus"));
        let infos = scan();
        assert_eq!(infos.len(), 1, "imported duplicate still one row: {:?}", infos);
        assert!(infos[0].imported);
        assert_eq!(Dictionary::open("eng-rus").unwrap().word_count(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merges_homonym_entries_in_tei() {
        // WikDict ships one <entry> per part of speech; "house" must show
        // the noun's дом as well as the verb's вмеща́ть, and the
        // pronunciations and English <def> glosses must stay out.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("homonyms");
        let entries = "\
<entry><form><orth>house</orth><pron>/haʊs/</pron></form><gramGrp><pos>n</pos></gramGrp>\
<sense><cit type=\"trans\" xml:lang=\"ru\"><quote>пала́та</quote><quote>дом</quote></cit>\
<sense><def>A building used for something else.</def></sense></sense></entry>\
<entry><form><orth>house</orth><pron>/haʊz/</pron></form><gramGrp><pos>v</pos></gramGrp>\
<sense><cit type=\"trans\" xml:lang=\"ru\"><quote>вмеща́ть</quote><quote>дом</quote></cit>\
<sense><def>To keep within a structure.</def></sense></sense></entry>";
        fs::write(
            dir.join("t.tei"),
            format!(
                "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\
                 <teiHeader><fileDesc><titleStmt><title>T</title></titleStmt>\
                 <extent>1 headword</extent></fileDesc></teiHeader>\
                 <text><body>{entries}</body></text></TEI>"
            ),
        )
        .unwrap();
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        assert!(rebuild("t"));
        let dict = Dictionary::open("t").expect("open");
        assert_eq!(dict.word_count(), 1, "both entries merge under one key");
        let e = dict.lookup("house").expect("house");
        assert_eq!(e.meaning, "пала́та, дом, вмеща́ть");
        assert!(!e.meaning.contains("building"));
        assert!(!e.meaning.contains("haʊs"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tei_corpus_smoke() {
        // Set YB_TEI_CORPUS to a real eng-rus.tei to run the parser over
        // the full corpus; skipped otherwise (36 MB fixture, not in git).
        let Ok(path) = std::env::var("YB_TEI_CORPUS") else {
            return;
        };
        let xml = read_capped(Path::new(&path)).expect("corpus read");
        let t0 = std::time::Instant::now();
        let items = parse_tei(&xml).expect("parse");
        eprintln!("parse_tei: {:?} for {} bytes", t0.elapsed(), xml.len());
        assert!(!items.is_empty());
        // parse_tei returns UNMERGED rows (convert() merges); match the
        // Python-validated prototype row-for-row. Sort by headword only —
        // stable, so entries of one headword keep document order.
        let mut rows: Vec<(&str, &str)> =
            items.iter().map(|(c, _, m)| (c.as_str(), m.as_str())).collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let house: Vec<&str> =
            rows.iter().filter(|(c, _)| *c == "house").map(|(_, m)| *m).collect();
        assert_eq!(
            house,
            vec!["пала́та, дом, дина́стия", "вмеща́ть, помеща́ть, сели́ть"]
        );
        assert_eq!(
            rows.iter().find(|(c, _)| *c == "replication").map(|(_, m)| *m),
            Some("копи́рование")
        );
        assert_eq!(
            rows.iter().find(|(c, _)| *c == "cat").map(|(_, m)| *m),
            Some("ко́шка, кошка, кот")
        );
        eprintln!("entries: {}", items.len());
    }

    #[test]
    fn selection_roundtrip() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("sel");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        let mut sel = DictSelection::default();
        sel.active.push("en-ru".to_string());
        sel.active.push("en-en".to_string());
        save_selection(&sel);
        let loaded = load_selection();
        assert_eq!(loaded.active, vec!["en-ru".to_string(), "en-en".to_string()]);

        // Old files with primary/secondary/lang lines load cleanly (those
        // models are gone) and a save drops the stale lines.
        fs::write(
            config_path(),
            "primary en-ru\nsecondary en-en\nlang ru user\nactive user\n",
        )
        .unwrap();
        let loaded = load_selection();
        assert_eq!(loaded.active, vec!["user".to_string()]);

        // Toggling adds and removes.
        let mut sel = loaded;
        toggle_active(&mut sel, "en-ru");
        let loaded = load_selection();
        assert!(loaded.active.iter().any(|b| b == "en-ru"));
        assert!(loaded.active.iter().any(|b| b == "user"));
        toggle_active(&mut sel, "en-ru");
        let loaded = load_selection();
        assert!(!loaded.active.iter().any(|b| b == "en-ru"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_list_orders_lookup_and_book_override_wins() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("active");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        // Two user dictionaries; only "first" has "cat".
        write_tei(&dir, "user");
        fs::write(
            dir.join("first.tei"),
            "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\
             <teiHeader><fileDesc><titleStmt><title>First</title></titleStmt>\
             <extent>1 headword</extent></fileDesc></teiHeader>\
             <text><body><entry><form><orth>cat</orth></form>\
             <sense><cit type=\"trans\"><quote>first meaning</quote></cit></sense>\
             </entry></body></text></TEI>",
        )
        .unwrap();
        assert!(convert("first").is_some());
        assert!(convert("user").is_some());

        // Builtin stand-in: only "cat".
        let builtin_dir = dir.join("builtin");
        fs::create_dir_all(&builtin_dir).unwrap();
        fs::write(
            builtin_dir.join("builtin.tei"),
            "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\
             <teiHeader><fileDesc><titleStmt><title>WN</title></titleStmt>\
             <extent>1 headword</extent></fileDesc></teiHeader>\
             <text><body><entry><form><orth>cat</orth></form>\
             <sense><cit type=\"trans\"><quote>builtin meaning</quote></cit></sense>\
             </entry></body></text></TEI>",
        )
        .unwrap();
        assert!(convert("builtin").is_some());

        // Active: user (cat/dog/run), then first (cat).
        let mut sel = DictSelection::default();
        sel.active.push("user".to_string());
        sel.active.push("first".to_string());
        save_selection(&sel);

        let active = open_active();
        let builtin = Dictionary::open("builtin").unwrap();
        assert_eq!(active.active_bases(), vec!["user".to_string(), "first".to_string()]);

        // WordNet is the definition; the active chain is the translation
        // (first active dictionary that has the word wins).
        let r = active.lookup_result(Some(&builtin), "cat", None);
        assert_eq!(r.definition.as_ref().unwrap().source, "builtin");
        assert_eq!(r.translation.as_ref().unwrap().source, "user");

        // WordNet misses: no definition, translation still from the chain.
        let r = active.lookup_result(Some(&builtin), "dog", None);
        assert!(r.definition.is_none());
        assert_eq!(r.translation.as_ref().unwrap().source, "user");

        // A per-book override picks the translation independently.
        let r = active.lookup_result(Some(&builtin), "cat", Some("first"));
        assert_eq!(r.definition.as_ref().unwrap().source, "builtin");
        assert_eq!(r.translation.as_ref().unwrap().source, "first");

        // Inactive dictionaries are not part of the chain.
        let mut sel = DictSelection::default();
        sel.active.push("first".to_string());
        save_selection(&sel);
        let active = open_active();
        let r = active.lookup_result(Some(&builtin), "dog", None);
        assert!(r.translation.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn book_override_roundtrip_and_cycle() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("bookovr");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        assert!(crate::dictionary::book_override("My Book").is_none());
        crate::dictionary::set_book_override("My Book", Some("eng-rus"));
        assert_eq!(
            crate::dictionary::book_override("My Book").as_deref(),
            Some("eng-rus")
        );
        // Explicit off is stored and read back as Some("").
        crate::dictionary::set_book_override("My Book", Some(""));
        assert_eq!(
            crate::dictionary::book_override("My Book").as_deref(),
            Some("")
        );
        crate::dictionary::set_book_override("My Book", None);
        assert!(crate::dictionary::book_override("My Book").is_none());

        let active = vec!["eng-rus".to_string(), "en-en".to_string()];
        // auto -> active1 -> active2 -> off -> auto (WordNet is the
        // definition, not a translation pick).
        assert_eq!(
            crate::dictionary::next_translation_choice(None, &active).as_deref(),
            Some("eng-rus")
        );
        assert_eq!(
            crate::dictionary::next_translation_choice(Some("eng-rus"), &active).as_deref(),
            Some("en-en")
        );
        assert_eq!(
            crate::dictionary::next_translation_choice(Some("en-en"), &active).as_deref(),
            Some("")
        );
        assert_eq!(
            crate::dictionary::next_translation_choice(Some(""), &active),
            None
        );
        assert_eq!(
            crate::dictionary::next_translation_choice(Some("unknown"), &active).as_deref(),
            Some("eng-rus")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tei_info_survives_hostile_headers() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("teiinfo");
        // A closing tag before its opener used to make tei_info slice
        // text[s+7..e] with s > e and panic; it must degrade to defaults.
        fs::write(dir.join("evil.tei"), "<?xml version=\"1.0\"?>\n</title>junk\n<TEI/>")
            .unwrap();
        let (name, wc) = tei_info(&dir.join("evil.tei"));
        assert_eq!(name, "Dictionary");
        assert_eq!(wc, 0);

        // A normal header still parses (with entities unescaped).
        fs::write(
            dir.join("good.tei"),
            "<TEI><teiHeader><fileDesc><titleStmt><title>Eng &amp; Rus</title></titleStmt>\
             <extent>12345 headwords</extent></fileDesc></teiHeader></TEI>",
        )
        .unwrap();
        let (name, wc) = tei_info(&dir.join("good.tei"));
        assert_eq!(name, "Eng & Rus");
        assert_eq!(wc, 12345);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_rebuild_is_not_retried_until_source_changes() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("badconvert");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());

        // A source with <entry> but no usable content: convert() returns
        // None, so the first attempt fails and is remembered.
        fs::write(
            dir.join("bad.tei"),
            "<TEI xmlns=\"http://www.tei-c.org/ns/1.0\">\
             <teiHeader><fileDesc><titleStmt><title>Bad</title></titleStmt>\
             <extent>0 headwords</extent></fileDesc></teiHeader>\
             <text><body><entry><form><orth></orth></form></entry></body></text></TEI>",
        )
        .unwrap();

        let mut sel = DictSelection::default();
        toggle_active(&mut sel, "bad");
        assert!(rebuild_failed("bad"), "failed activate must be remembered");
        assert_eq!(open_active().active.len(), 0, "no retry, chain stays empty");

        // Replacing the source with a buildable one (new mtime) retries
        // and succeeds.
        write_tei(&dir, "bad");
        assert!(!rebuild_failed("bad"), "new source generation resets the attempt");
        assert_eq!(open_active().active.len(), 1, "now it converts and activates");
        assert!(!needs_rebuild("bad"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_cache_drops_superseded_generation() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = tmp_dir("prune_gen");
        std::env::set_var("YB_DICT_DIR", dir.to_str().unwrap());
        write_tei(&dir, "gen");
        assert!(convert("gen").is_some());
        let _ = Dictionary::open("gen").expect("first gen");

        // Re-import the same source: the blob's mtime changes, so the
        // cache holds two generations under the same path.
        write_tei(&dir, "gen");
        assert!(convert("gen").is_some());
        let _ = Dictionary::open("gen").expect("second gen");
        assert_eq!(
            DICT_CACHE.with(|c| c.borrow().len()),
            2,
            "both generations cached before prune"
        );

        prune_cache(&["gen".to_string()]);
        assert_eq!(
            DICT_CACHE.with(|c| c.borrow().len()),
            1,
            "superseded generation evicted by prune"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // The Python↔Rust YBDICT02 contract, pinned in both directions. These
    // bytes were produced once by
    //
    //   python3 tools/build_wordnet/build_wordnet.py \
    //       tools/build_wordnet/testdata /tmp/fixture.ybdict
    //
    // where tools/build_wordnet/testdata/ is a minimal synthetic WordNet
    // tree, then frozen here as hex (no binary artifact in the repo). A
    // change to the magic/header/record layout, the section order, the
    // clean_word key normalization, or the meaning markup on EITHER the
    // Python or the Rust side fails this test instead of surfacing as
    // silent lookup misses on device. Regenerate the fixture only for an
    // intentional two-sided format change.
    const PY_FIXTURE_HEX: &str = concat!(
        "5942444943543032000000020000004000000053000000650000000000080000",
        "00000008000000000000004700000008000b00000008000a000000470000011e",
        "6275696c742d696e706f77657220706c616e746275696c742d696e706f776572",
        "706c616e746578697374696e6720617320616e20657373656e7469616c20636f",
        "6e7374697475656e740a2261206275696c742d696e207175616c697479206f66",
        "207468652073797374656d22312e206275696c64696e677320666f7220636172",
        "7279696e67206f6e20696e647573747269616c206c61626f720a227468657920",
        "6275696c742061206c6172676520666163746f72792220c2b720227468652070",
        "6c616e7420656d706c6f79732035303020776f726b657273220a322e2028626f",
        "74616e792920616e206f7267616e69736d2062656c6f6e67696e6720746f2074",
        "6865206b696e67646f6d20506c616e7461650a226865207468696e6b73207468",
        "6520706c616e7420697320612073756363756c656e74220a332e20707574206f",
        "722073657420287365656473206f7220736565646c696e67732920696e746f20",
        "7468652067726f756e640a22706c616e742074686520736565646c696e677320",
        "696e20737072696e6722",
    );

    fn py_fixture() -> Vec<u8> {
        (0..PY_FIXTURE_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&PY_FIXTURE_HEX[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn python_builtin_fixture_round_trips() {
        let bytes = py_fixture();
        assert_eq!(bytes.len(), 458);
        let dict = Dictionary::from_bytes(bytes, "WordNet").expect("fixture parses");
        assert_eq!(dict.word_count(), 2);
        assert_eq!(dict.words_off, 64);
        assert_eq!(dict.keys_off, 83);
        assert_eq!(dict.data_off, 101);

        // Multi-word lemma: the key is clean_word'd ("power_plant" →
        // "powerplant" style), the display word keeps its spaces, and the
        // meaning carries the numbered-sense + quoted-example markup the
        // word card renders. The noun index line carries two offsets, so
        // this also pins parse_index keeping more than one sense per POS.
        let e = dict.lookup("Power Plant").expect("multi-word lookup");
        assert_eq!(e.word, "power plant");
        assert_eq!(
            e.meaning,
            "1. buildings for carrying on industrial labor\n\
             \"they built a large factory\" · \"the plant employs 500 workers\"\n\
             2. (botany) an organism belonging to the kingdom Plantae\n\
             \"he thinks the plant is a succulent\"\n\
             3. put or set (seeds or seedlings) into the ground\n\
             \"plant the seedlings in spring\""
        );

        // Hyphenated lemma: '-' survives clean_word on both sides.
        let e = dict.lookup("BUILT-IN").expect("hyphenated lookup");
        assert_eq!(e.word, "built-in");
        assert_eq!(
            e.meaning,
            "existing as an essential constituent\n\"a built-in quality of the system\""
        );

        // Normalization lives in lookup(); the stored keys are the clean
        // forms, so an exact case/space-insensitive miss is the contract.
        assert!(dict.lookup_exact("powerplant").is_some());
        assert!(dict.lookup_exact("Power Plant").is_none());
        assert!(dict.lookup("zzz").is_none());
    }
}
