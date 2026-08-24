use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

const VOCAB_PATH: &str = "/mnt/us/extensions/reader/data/vocab.bin";
const PROFILE_PATH: &str = "/mnt/us/extensions/reader/vocab_profile.json";
const MAGIC: &[u8; 8] = b"YBVOC01\0";

/// Process-wide dictionary, read once: the blob is ~14 MB and every book
/// open would otherwise re-read it from flash into a fresh heap buffer.
/// Lookups take &self, so every ReaderScreen shares this one instance.
static DB: OnceLock<Option<VocabDb>> = OnceLock::new();

/// An entry looked up from the lexical database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    pub word: String,
    pub difficulty: u8, // 0 (easiest) .. 100 (most advanced/rare)
    pub cefr: u8,       // 1=A1, 2=A2, 3=B1, 4=B2, 5=C1, 6=C2, 0=Unk
    pub gloss_en: String,
    pub gloss_ru: String,
}

impl WordEntry {
    pub fn cefr_str(&self) -> &'static str {
        match self.cefr {
            1 => "A1",
            2 => "A2",
            3 => "B1",
            4 => "B2",
            5 => "C1",
            6 => "C2",
            _ => "Vocab",
        }
    }
}

/// In-memory binary search database over vocab.bin.
pub struct VocabDb {
    data: Vec<u8>,
    count: usize,
    strings_offset: usize,
}

impl VocabDb {
    /// Open the device dictionary, cached for the process lifetime.
    pub fn open() -> Option<&'static Self> {
        DB.get_or_init(|| Self::open_path(VOCAB_PATH)).as_ref()
    }

    pub fn open_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        let data = fs::read(path).ok()?;
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Option<Self> {
        if data.len() < 16 || &data[0..8] != MAGIC {
            return None;
        }

        let count = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
        let strings_offset = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;

        if data.len() < strings_offset {
            return None;
        }

        // Each record is a fixed 20-byte index entry. Clamp `count` to the
        // index region so `16 + mid * 20` in the binary search can never
        // overflow on 32-bit (`usize` = u32 on armv7) and land back inside
        // the buffer — a corrupt/edited vocab.bin must degrade to fewer
        // entries, never silently wrong definitions.
        let count = count.min((strings_offset.saturating_sub(16)) / 20);

        Some(VocabDb {
            data,
            count,
            strings_offset,
        })
    }

    /// Fast binary search lookup in O(log N) microseconds.
    pub fn lookup(&self, word: &str) -> Option<WordEntry> {
        let clean = clean_word(word);
        if clean.is_empty() {
            return None;
        }

        // 1. Direct lookup
        let mut entry = self.lookup_exact(&clean);

        // If we found an entry but it lacks definitions (or we didn't find an entry),
        // search candidate lemmas to populate or find definitions.
        if entry
            .as_ref()
            .is_none_or(|e| e.gloss_en.is_empty() || e.gloss_ru.is_empty())
        {
            for lemma in generate_lemmas(&clean) {
                if let Some(lem_entry) = self.lookup_exact(&lemma) {
                    if let Some(ref mut e) = entry {
                        if e.gloss_en.is_empty() && !lem_entry.gloss_en.is_empty() {
                            e.gloss_en = lem_entry.gloss_en;
                        }
                        if e.gloss_ru.is_empty() && !lem_entry.gloss_ru.is_empty() {
                            e.gloss_ru = lem_entry.gloss_ru;
                        }
                        if !e.gloss_en.is_empty() && !e.gloss_ru.is_empty() {
                            break;
                        }
                    } else if !lem_entry.gloss_en.is_empty() || !lem_entry.gloss_ru.is_empty() {
                        let mut e = lem_entry;
                        e.word = clean.to_string();
                        entry = Some(e);
                        break;
                    }
                }
            }
        }

        // Only return an entry if at least one definition/gloss exists
        entry.filter(|e| !e.gloss_en.is_empty() || !e.gloss_ru.is_empty())
    }

    fn lookup_exact(&self, target: &str) -> Option<WordEntry> {
        if self.count == 0 {
            return None;
        }

        let mut lo: usize = 0;
        let mut hi: usize = self.count.saturating_sub(1);

        while lo <= hi {
            let mid = (lo + hi) / 2;
            let off = 16 + mid * 20;
            if off + 20 > self.strings_offset {
                break;
            }

            let w_off = u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap()) as usize;
            let w_len =
                u16::from_le_bytes(self.data[off + 4..off + 6].try_into().unwrap()) as usize;
            let diff = self.data[off + 6];
            let cefr = self.data[off + 7];

            let en_off =
                u32::from_le_bytes(self.data[off + 8..off + 12].try_into().unwrap()) as usize;
            let en_len =
                u16::from_le_bytes(self.data[off + 12..off + 14].try_into().unwrap()) as usize;

            let tr_off =
                u32::from_le_bytes(self.data[off + 14..off + 18].try_into().unwrap()) as usize;
            let tr_len =
                u16::from_le_bytes(self.data[off + 18..off + 20].try_into().unwrap()) as usize;

            let w_start = self.strings_offset + w_off;
            let w_bytes = self.data.get(w_start..w_start + w_len)?;
            let w_str = std::str::from_utf8(w_bytes).ok()?;

            match w_str.cmp(target) {
                std::cmp::Ordering::Equal => {
                    let en_start = self.strings_offset + en_off;
                    let gloss_en = self
                        .data
                        .get(en_start..en_start + en_len)
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or_default()
                        .to_string();

                    let ru_start = self.strings_offset + tr_off;
                    let gloss_ru = self
                        .data
                        .get(ru_start..ru_start + tr_len)
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or_default()
                        .to_string();

                    return Some(WordEntry {
                        word: w_str.to_string(),
                        difficulty: diff,
                        cefr,
                        gloss_en,
                        gloss_ru,
                    });
                }
                std::cmp::Ordering::Less => {
                    lo = mid + 1;
                }
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
}

/// Strip punctuation and lowercase.
pub fn clean_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphabetic() || *c == '-')
        .collect::<String>()
        .to_lowercase()
}

/// Rule-based English lemmatization for dictionary fallback lookups.
/// Handles regular inflectional suffixes (-s, -es, -ies, -ves, -ed, -ied, -ing, -ying, -ly, -ily, -ally)
/// and guards against over-stemming non-inflected words.
pub fn generate_lemmas(w: &str) -> Vec<String> {
    let len = w.len();
    if len < 3 {
        return Vec::new();
    }

    // Common non-inflected English words that should not have terminal suffixes stripped
    const S_STOP_WORDS: &[&str] = &[
        "as", "is", "us", "was", "his", "this", "thus", "yes", "gas", "bus", "plus",
        "lens", "always", "news", "series", "species", "crisis", "basis", "analysis",
        "thesis", "genius", "status", "focus", "virus", "canvas", "chaos", "abyss",
    ];
    const LY_STOP_WORDS: &[&str] = &[
        "only", "early", "ugly", "holy", "daily", "jelly", "silly", "belly",
        "ally", "rely", "apply", "supply", "fly", "family", "imply", "italy",
    ];
    const ED_STOP_WORDS: &[&str] = &[
        "red", "bed", "fed", "led", "shed", "bleed", "breed", "feed", "need", "seed", "speed", "steed", "weed",
    ];
    const ING_STOP_WORDS: &[&str] = &[
        "king", "ring", "wing", "sing", "bring", "spring", "string", "thing", "during", "morning", "evening", "ceiling", "bling",
    ];

    let mut lemmas = Vec::new();

    // Helper to test if a consonant is doubled (e.g. "stopped", "running")
    let is_double_consonant = |s: &str| -> bool {
        let b = s.as_bytes();
        let l = b.len();
        if l >= 2 && b[l - 1] == b[l - 2] {
            !matches!(b[l - 1], b'a' | b'e' | b'i' | b'o' | b'u' | b's' | b'y')
        } else {
            false
        }
    };

    // 1. Plurals & 3rd-person verbs
    if !S_STOP_WORDS.contains(&w) && !w.ends_with("ss") {
        if w.ends_with("ies") && len > 4 {
            lemmas.push(format!("{}y", &w[..len - 3]));
            lemmas.push(format!("{}ie", &w[..len - 3]));
        } else if w.ends_with("ves") && len > 4 {
            lemmas.push(format!("{}fe", &w[..len - 3]));
            lemmas.push(format!("{}f", &w[..len - 3]));
        } else if (w.ends_with("sses") || w.ends_with("shes") || w.ends_with("ches") || w.ends_with("xes") || w.ends_with("zes")) && len > 4 {
            lemmas.push(w[..len - 2].to_string());
        } else if w.ends_with("es") && len > 3 {
            lemmas.push(w[..len - 1].to_string()); // makes -> make
            lemmas.push(w[..len - 2].to_string()); // heroes -> hero
        } else if w.ends_with('s') && len > 3 {
            lemmas.push(w[..len - 1].to_string());
        }
    }

    // 2. Past tense & adjectives
    if !ED_STOP_WORDS.contains(&w) {
        if w.ends_with("ied") && len > 4 {
            lemmas.push(format!("{}y", &w[..len - 3]));
            lemmas.push(format!("{}ie", &w[..len - 3]));
        } else if w.ends_with("ed") && len > 3 {
            let base = &w[..len - 2];
            if is_double_consonant(base) {
                lemmas.push(base[..base.len() - 1].to_string()); // stopped -> stop
            }
            lemmas.push(format!("{}e", base)); // created -> create
            lemmas.push(base.to_string()); // walked -> walk
        }
    }

    // 3. Present participles (-ing)
    if !ING_STOP_WORDS.contains(&w) {
        if w.ends_with("ying") && len > 4 {
            lemmas.push(format!("{}ie", &w[..len - 4])); // lying -> lie
        } else if w.ends_with("ing") && len > 4 {
            let base = &w[..len - 3];
            if is_double_consonant(base) {
                lemmas.push(base[..base.len() - 1].to_string()); // running -> run
            }
            lemmas.push(format!("{}e", base)); // making -> make
            lemmas.push(base.to_string()); // walking -> walk
        }
    }

    // 4. Adverbs (-ly)
    if !LY_STOP_WORDS.contains(&w) {
        if w.ends_with("ily") && len > 4 {
            lemmas.push(format!("{}y", &w[..len - 3])); // happily -> happy
        } else if w.ends_with("ally") && len > 5 {
            lemmas.push(w[..len - 4].to_string()); // basically -> basic
            lemmas.push(w[..len - 2].to_string()); // legally -> legal
        } else if w.ends_with("ly") && len > 4 {
            lemmas.push(w[..len - 2].to_string()); // quickly -> quick
        }
    }

    // Retain only unique, non-empty lemmas that differ from the input
    let mut out = Vec::new();
    for l in lemmas {
        if !l.is_empty() && l != w && !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// User's persistent vocabulary profile and reading difficulty tracker.
#[derive(Debug, Clone)]
pub struct VocabProfile {
    /// Estimated user vocabulary level (0..100). Default: 65 (B2)
    pub user_level: u8,
    /// Words explicitly marked as known or dismissed
    pub known_words: HashSet<String>,
    /// Words explicitly looked up or starred for learning
    pub learning_words: HashSet<String>,
}

impl Default for VocabProfile {
    fn default() -> Self {
        VocabProfile {
            user_level: 65, // B2 Upper-Intermediate default
            known_words: HashSet::new(),
            learning_words: HashSet::new(),
        }
    }
}

impl VocabProfile {
    pub fn load() -> Self {
        Self::load_from(PROFILE_PATH)
    }

    pub fn load_from<P: AsRef<Path>>(path: P) -> Self {
        let Ok(content) = fs::read_to_string(path) else {
            return VocabProfile::default();
        };

        let mut prof = VocabProfile::default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, ' ');
            let key = parts.next().unwrap_or("");
            let val = parts.next().unwrap_or("").trim();

            match key {
                "level" => {
                    if let Ok(lvl) = val.parse::<u8>() {
                        prof.user_level = lvl.clamp(1, 100);
                    }
                }
                "known" => {
                    for w in val.split(',') {
                        let clean = clean_word(w);
                        if !clean.is_empty() {
                            prof.known_words.insert(clean);
                        }
                    }
                }
                "learning" => {
                    for w in val.split(',') {
                        let clean = clean_word(w);
                        if !clean.is_empty() {
                            prof.learning_words.insert(clean);
                        }
                    }
                }
                _ => {}
            }
        }
        prof
    }

    pub fn save(&self) {
        self.save_to(PROFILE_PATH);
    }

    pub fn save_to<P: AsRef<Path>>(&self, path: P) {
        if let Some(parent) = path.as_ref().parent() {
            let _ = fs::create_dir_all(parent);
        }

        let known_list: Vec<&str> = self.known_words.iter().map(|s| s.as_str()).collect();
        let learning_list: Vec<&str> = self.learning_words.iter().map(|s| s.as_str()).collect();

        let mut out = String::new();
        out.push_str(&format!("level {}\n", self.user_level));
        out.push_str(&format!("known {}\n", known_list.join(",")));
        out.push_str(&format!("learning {}\n", learning_list.join(",")));

        // Atomic + fsync'd swap (ybdev::atomic): a power loss mid-write
        // must cost at most the previous state, not the whole learning
        // history.
        let _ = ybdev::atomic::write(path, out.as_bytes());
    }

    /// Record a word lookup: promotes word to learning and slightly adjusts frontier.
    pub fn record_lookup(&mut self, word: &str, difficulty: u8) {
        let clean = clean_word(word);
        self.known_words.remove(&clean);
        self.learning_words.insert(clean);

        // If you looked up a word below your current level, gently adapt frontier down
        if difficulty < self.user_level && self.user_level > 20 {
            self.user_level = self.user_level.saturating_sub(1);
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vocab_profile_learning_adaptation() {
        let path = "/tmp/yb_test_vocab_profile.json";
        let _ = fs::remove_file(path);

        let mut prof = VocabProfile::default();
        assert_eq!(prof.user_level, 65);

        // Lookup below the frontier -> recorded for learning, frontier dips.
        prof.record_lookup("gloomy", 40);
        assert_eq!(prof.user_level, 64);
        assert!(prof.learning_words.contains("gloomy"));

        // Lookup above the frontier -> recorded, no further dip.
        prof.record_lookup("lofty", 90);
        assert_eq!(prof.user_level, 64);
        assert!(prof.learning_words.contains("lofty"));

        prof.save_to(path);

        let loaded = VocabProfile::load_from(path);
        assert_eq!(loaded.user_level, 64);
        assert!(loaded.learning_words.contains("gloomy"));
        assert!(loaded.learning_words.contains("lofty"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_lemmatizer_rules() {
        assert_eq!(clean_word("Ubiquitous!"), "ubiquitous");
        assert_eq!(clean_word("half-hearted,"), "half-hearted");

        // Non-inflected stop words must never be stripped
        for stop in ["was", "this", "his", "news", "series", "species", "crisis", "only", "early", "ugly", "king"] {
            let lemmas = generate_lemmas(stop);
            assert!(lemmas.is_empty(), "stop word '{stop}' should not produce stripped lemmas: {lemmas:?}");
        }

        // Regular plurals and verbs
        assert!(generate_lemmas("addresses").contains(&"address".to_string()));
        assert!(generate_lemmas("processes").contains(&"process".to_string()));
        assert!(generate_lemmas("parties").contains(&"party".to_string()));
        assert!(generate_lemmas("knives").contains(&"knife".to_string()));
        assert!(generate_lemmas("makes").contains(&"make".to_string()));

        // Past tense & participles
        assert!(generate_lemmas("stopped").contains(&"stop".to_string()));
        assert!(generate_lemmas("running").contains(&"run".to_string()));
        assert!(generate_lemmas("computed").contains(&"compute".to_string()));
        assert!(generate_lemmas("making").contains(&"make".to_string()));

        // Adverbs
        assert!(generate_lemmas("tenaciously").contains(&"tenacious".to_string()));
        assert!(generate_lemmas("happily").contains(&"happy".to_string()));
        assert!(generate_lemmas("basically").contains(&"basic".to_string()));
    }

    #[test]
    fn test_vocab_db_lookup() {
        let default_bin = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/build_vocab/vocab.bin");
        let bin_path = std::env::var("YB_VOCAB_PATH").unwrap_or_else(|_| {
            if Path::new(VOCAB_PATH).exists() {
                VOCAB_PATH.to_string()
            } else {
                default_bin.to_string()
            }
        });
        if !Path::new(&bin_path).exists() {
            return;
        }

        let db = VocabDb::open_path(&bin_path).expect("open vocab.bin");
        assert!(db.count > 10000);

        let entry = db.lookup("ubiquitous").expect("lookup ubiquitous");
        assert_eq!(entry.word, "ubiquitous");
        assert!(entry.difficulty >= 70);
        assert!(!entry.gloss_en.is_empty());

        let entry_inflected = db.lookup("ephemerally").expect("lookup ephemerally");
        assert_eq!(entry_inflected.word, "ephemerally");
        assert!(entry_inflected.difficulty >= 70);

        // Inflected words whose direct entry had empty glosses must inherit lemma definitions
        let entry_switches = db.lookup("switches").expect("lookup switches");
        assert_eq!(entry_switches.word, "switches");
        assert!(!entry_switches.gloss_en.is_empty());
        assert!(!entry_switches.gloss_ru.is_empty());

        let entry_databases = db.lookup("databases").expect("lookup databases");
        assert_eq!(entry_databases.word, "databases");
        assert!(!entry_databases.gloss_en.is_empty());
    }

    #[test]
    fn open_is_cached_for_process_lifetime() {
        // Same &'static handle both times (None on the host, where the
        // device path doesn't exist — also cached, also identical).
        let a = VocabDb::open();
        let b = VocabDb::open();
        assert_eq!(a.is_some(), b.is_some());
        if let (Some(a), Some(b)) = (a, b) {
            assert!(std::ptr::eq(a, b));
        }
    }
}
