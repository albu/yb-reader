use std::collections::HashSet;
use std::fs;
use std::path::Path;

const VOCAB_PATH: &str = "/mnt/us/extensions/reader/data/vocab.bin";
const PROFILE_PATH: &str = "/mnt/us/extensions/reader/vocab_profile.json";
const MAGIC: &[u8; 8] = b"YBVOC01\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationStyle {

    /// Small superscript definition directly above the word
    Interlinear,
    /// Clean 1-2 line footer list at the bottom of the page
    Margin,
    /// Subtle dotted underline under words on the frontier; tap to expand
    DottedUnderline,
    /// Disabled
    Off,
}

impl Default for AnnotationStyle {
    fn default() -> Self {
        AnnotationStyle::Interlinear
    }
}

/// An entry looked up from the lexical database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    pub word: String,
    pub difficulty: u8, // 0 (easiest) .. 100 (most advanced/rare)
    pub cefr: u8,       // 1=A1, 2=A2, 3=B1, 4=B2, 5=C1, 6=C2, 0=Unk
    pub gloss_en: String,
    pub gloss_tr: String,
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
    pub fn open() -> Option<Self> {
        Self::open_path(VOCAB_PATH)
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
        if let Some(entry) = self.lookup_exact(&clean) {
            return Some(entry);
        }

        // 2. Lemmatization fallbacks (plural -s, past -ed, participle -ing, adverb -ly)
        for lemma in generate_lemmas(&clean) {
            if let Some(mut entry) = self.lookup_exact(&lemma) {
                entry.word = clean.to_string(); // Keep queried surface form
                return Some(entry);
            }
        }

        None
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
            let w_len = u16::from_le_bytes(self.data[off + 4..off + 6].try_into().unwrap()) as usize;
            let diff = self.data[off + 6];
            let cefr = self.data[off + 7];

            let en_off = u32::from_le_bytes(self.data[off + 8..off + 12].try_into().unwrap()) as usize;
            let en_len = u16::from_le_bytes(self.data[off + 12..off + 14].try_into().unwrap()) as usize;

            let tr_off = u32::from_le_bytes(self.data[off + 14..off + 18].try_into().unwrap()) as usize;
            let tr_len = u16::from_le_bytes(self.data[off + 18..off + 20].try_into().unwrap()) as usize;

            let w_start = self.strings_offset + w_off;
            let w_bytes = self.data.get(w_start..w_start + w_len)?;
            let w_str = std::str::from_utf8(w_bytes).ok()?;

            match w_str.cmp(target) {
                std::cmp::Ordering::Equal => {
                    let en_start = self.strings_offset + en_off;
                    let gloss_en = self.data.get(en_start..en_start + en_len)
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or_default()
                        .to_string();

                    let tr_start = self.strings_offset + tr_off;
                    let gloss_tr = self.data.get(tr_start..tr_start + tr_len)
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or_default()
                        .to_string();

                    return Some(WordEntry {
                        word: w_str.to_string(),
                        difficulty: diff,
                        cefr,
                        gloss_en,
                        gloss_tr,
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

/// Simple rule-based English lemmatization for high recall.
pub fn generate_lemmas(w: &str) -> Vec<String> {
    let mut lemmas = Vec::new();
    let len = w.len();

    // Plurals / Verb 3rd person (-ies -> -y, -es -> -, -s -> -)
    if w.ends_with("ies") && len > 4 {
        lemmas.push(format!("{}y", &w[..len - 3]));
    }
    if w.ends_with("es") && len > 3 {
        lemmas.push(w[..len - 2].to_string());
    }
    if w.ends_with('s') && !w.ends_with("ss") && len > 2 {
        lemmas.push(w[..len - 1].to_string());
    }

    // Past / Adjectives (-ied -> -y, -ed -> -e, -ed -> -)
    if w.ends_with("ied") && len > 4 {
        lemmas.push(format!("{}y", &w[..len - 3]));
    }
    if w.ends_with("ed") && len > 3 {
        lemmas.push(format!("{}e", &w[..len - 2]));
        lemmas.push(w[..len - 2].to_string());
    }

    // Participles (-ing -> -e, -ing -> -)
    if w.ends_with("ing") && len > 4 {
        lemmas.push(format!("{}e", &w[..len - 3]));
        lemmas.push(w[..len - 3].to_string());
    }

    // Adverbs (-ly -> -)
    if w.ends_with("ly") && len > 3 {
        lemmas.push(w[..len - 2].to_string());
    }

    lemmas
}

/// User's persistent vocabulary profile and adaptive learning model.
#[derive(Debug, Clone)]
pub struct VocabProfile {

    /// Estimated user vocabulary level (0..100). Default: 65 (B2)
    pub user_level: u8,
    /// Words explicitly marked as known or dismissed
    pub known_words: HashSet<String>,
    /// Words explicitly looked up or starred for learning
    pub learning_words: HashSet<String>,
    /// Display style for inline Word Wise glosses
    pub style: AnnotationStyle,
    /// Max number of annotated words budgeted per page (1..5)
    pub max_per_page: usize,
}

impl Default for VocabProfile {
    fn default() -> Self {
        VocabProfile {
            user_level: 65, // B2 Upper-Intermediate default
            known_words: HashSet::new(),
            learning_words: HashSet::new(),
            style: AnnotationStyle::Interlinear,
            max_per_page: 2,
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
                "style" => {
                    prof.style = match val {
                        "interlinear" => AnnotationStyle::Interlinear,
                        "margin" => AnnotationStyle::Margin,
                        "dotted" => AnnotationStyle::DottedUnderline,
                        "off" => AnnotationStyle::Off,
                        _ => AnnotationStyle::Interlinear,
                    };
                }
                "max" => {
                    if let Ok(m) = val.parse::<usize>() {
                        prof.max_per_page = m.clamp(1, 10);
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
        let style_str = match self.style {
            AnnotationStyle::Interlinear => "interlinear",
            AnnotationStyle::Margin => "margin",
            AnnotationStyle::DottedUnderline => "dotted",
            AnnotationStyle::Off => "off",
        };

        let known_list: Vec<&str> = self.known_words.iter().map(|s| s.as_str()).collect();
        let learning_list: Vec<&str> = self.learning_words.iter().map(|s| s.as_str()).collect();

        let mut out = String::new();
        out.push_str(&format!("level {}\n", self.user_level));
        out.push_str(&format!("style {}\n", style_str));
        out.push_str(&format!("max {}\n", self.max_per_page));
        out.push_str(&format!("known {}\n", known_list.join(",")));
        out.push_str(&format!("learning {}\n", learning_list.join(",")));

        let _ = fs::write(path, out);
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

    /// Mark a word as known / dismiss it: suppresses future annotations.
    pub fn mark_known(&mut self, word: &str, difficulty: u8) {
        let clean = clean_word(word);
        self.learning_words.remove(&clean);
        self.known_words.insert(clean);

        // If you marked an advanced word as known, gently adapt frontier up
        if difficulty > self.user_level && self.user_level < 95 {
            self.user_level = (self.user_level + 1).min(98);
        }
        self.save();
    }

    /// Decide whether a candidate word should be annotated on this page.
    pub fn should_annotate(&self, entry: &WordEntry) -> bool {
        if self.style == AnnotationStyle::Off {
            return false;
        }
        if entry.gloss_en.is_empty() && entry.gloss_tr.is_empty() {
            return false;
        }
        let clean = clean_word(&entry.word);
        if self.known_words.contains(&clean) {
            return false;
        }
        if self.learning_words.contains(&clean) {
            return true;
        }

        // Annotated if word difficulty exceeds user level
        entry.difficulty >= self.user_level
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

        // Mark advanced word (difficulty 85) as known -> adapts user level up
        prof.mark_known("ubiquitous", 85);
        assert_eq!(prof.user_level, 66);
        assert!(prof.known_words.contains("ubiquitous"));

        // Lookup easier word (difficulty 40) -> adapts user level down
        prof.record_lookup("gloomy", 40);
        assert_eq!(prof.user_level, 65);
        assert!(prof.learning_words.contains("gloomy"));

        prof.save_to(path);

        let loaded = VocabProfile::load_from(path);
        assert_eq!(loaded.user_level, 65);
        assert!(loaded.known_words.contains("ubiquitous"));
        assert!(loaded.learning_words.contains("gloomy"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_lemmatizer_rules() {
        assert_eq!(clean_word("Ubiquitous!"), "ubiquitous");
        assert_eq!(clean_word("half-hearted,"), "half-hearted");

        let lemmas = generate_lemmas("tenaciously");
        assert!(lemmas.contains(&"tenacious".to_string()));

        let lemmas = generate_lemmas("computed");
        assert!(lemmas.contains(&"compute".to_string()));
    }

    #[test]
    fn test_vocab_db_lookup() {
        let bin_path = "tools/build_vocab/vocab.bin";
        if !Path::new(bin_path).exists() {
            return;
        }

        let db = VocabDb::open_path(bin_path).expect("open vocab.bin");
        assert!(db.count > 10000);

        let entry = db.lookup("ubiquitous").expect("lookup ubiquitous");
        assert_eq!(entry.word, "ubiquitous");
        assert!(entry.difficulty >= 70);
        assert!(!entry.gloss_en.is_empty());

        let entry_inflected = db.lookup("ephemerally").expect("lookup ephemerally");
        assert_eq!(entry_inflected.word, "ephemerally");
        assert!(entry_inflected.difficulty >= 70);
    }
}

