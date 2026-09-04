use std::collections::HashSet;
use std::fs;
use std::path::Path;
const PROFILE_PATH: &str = "/mnt/us/extensions/reader/vocab_profile.json";

/// Strip punctuation and lowercase. Preserves internal hyphens (for compound
/// words like "built-in"), but trims leading and trailing hyphens/dashes,
/// and strips English possessive suffixes ('s, ’s).
pub fn clean_word(w: &str) -> String {
    let base = w.strip_suffix("'s").or_else(|| w.strip_suffix("’s")).unwrap_or(w);
    let s: String = base
        .chars()
        .filter(|c| c.is_alphabetic() || *c == '-')
        .collect::<String>()
        .to_lowercase();
    s.trim_matches('-').to_string()
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

    // 5. Common English contractions & negative auxiliary verbs
    // (Apostrophes were stripped by clean_word: "didn't" -> "didnt", "couldn't" -> "couldnt")
    match w {
        "didnt" => {
            lemmas.push("did".to_string());
            lemmas.push("do".to_string());
        }
        "doesnt" | "dont" => lemmas.push("do".to_string()),
        "couldnt" => lemmas.push("could".to_string()),
        "wouldnt" => lemmas.push("would".to_string()),
        "shouldnt" => lemmas.push("should".to_string()),
        "havent" | "hasnt" | "hadnt" => lemmas.push("have".to_string()),
        "cant" => lemmas.push("can".to_string()),
        "wont" => lemmas.push("will".to_string()),
        "wasnt" | "werent" | "isnt" | "arent" => lemmas.push("be".to_string()),
        "theyll" | "youll" | "hell" | "shell" | "itll" => {
            if let Some(base) = w.strip_suffix("ll") {
                lemmas.push(base.to_string());
            }
        }
        "theyve" | "youve" | "weve" | "couldve" | "wouldve" | "shouldve" => {
            if let Some(base) = w.strip_suffix("ve") {
                lemmas.push(base.to_string());
            }
        }
        "theyd" | "youd" | "hed" | "shed" => {
            if let Some(base) = w.strip_suffix('d') {
                lemmas.push(base.to_string());
            }
        }
        _ => {}
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

        // Hyphen / dash trimming & possessive stripping
        assert_eq!(clean_word("--interrupted"), "interrupted");
        assert_eq!(clean_word("said--"), "said");
        assert_eq!(clean_word("--built-in--"), "built-in");
        assert_eq!(clean_word("reader's"), "reader");
        assert_eq!(clean_word("reader’s"), "reader");
        assert_eq!(clean_word("James's"), "james");

        // Contractions & auxiliary negative verbs
        assert!(generate_lemmas(&clean_word("didn't")).contains(&"did".to_string()));
        assert!(generate_lemmas(&clean_word("couldn't")).contains(&"could".to_string()));
        assert!(generate_lemmas(&clean_word("haven't")).contains(&"have".to_string()));
        assert!(generate_lemmas(&clean_word("they'll")).contains(&"they".to_string()));
        assert!(generate_lemmas(&clean_word("we've")).contains(&"we".to_string()));
        assert!(generate_lemmas(&clean_word("won't")).contains(&"will".to_string()));
    }
}
