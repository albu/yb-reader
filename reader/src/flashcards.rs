use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::vocab::{VocabDb, WordEntry};
use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const FLASHCARDS_PATH: &str = "/mnt/us/extensions/reader/flashcards.json";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A single flashcard tracked with the SuperMemo SM-2 algorithm.
#[derive(Debug, Clone)]
pub struct Flashcard {
    pub word: String,
    pub repetitions: u32,
    pub interval_days: u32,
    pub ease_factor: f32,
    pub due_timestamp: u64,
    pub last_reviewed: u64,
}

impl Flashcard {
    pub fn new(word: &str) -> Self {
        Flashcard {
            word: word.to_lowercase(),
            repetitions: 0,
            interval_days: 0,
            ease_factor: 2.5,
            due_timestamp: now_secs(), // Due immediately on creation
            last_reviewed: 0,
        }
    }

    /// Update card scheduling using the SuperMemo SM-2 algorithm.
    /// Grade: 0 = Again (fail), 1 = Hard, 2 = Good, 3 = Easy
    pub fn apply_sm2(&mut self, grade: u8) {
        let now = now_secs();
        self.last_reviewed = now;

        let q = match grade {
            0 => 0.0, // Again
            1 => 3.0, // Hard
            2 => 4.0, // Good
            _ => 5.0, // Easy
        };

        // Update Ease Factor (bounded to >= 1.3)
        let new_ef = self.ease_factor + (0.1 - (5.0 - q) * (0.08 + (5.0 - q) * 0.02));
        self.ease_factor = new_ef.max(1.3);

        if grade == 0 {
            // Again: Reset interval
            self.repetitions = 0;
            self.interval_days = 1;
        } else {
            // Successful recall
            if self.repetitions == 0 {
                self.interval_days = if grade == 3 { 3 } else { 1 };
            } else if self.repetitions == 1 {
                self.interval_days = if grade == 3 { 8 } else { 4 };
            } else {
                let factor = if grade == 1 {
                    1.2
                } else if grade == 3 {
                    self.ease_factor * 1.3
                } else {
                    self.ease_factor
                };
                self.interval_days = ((self.interval_days as f32 * factor).round() as u32).max(self.interval_days + 1);
            }
            self.repetitions += 1;
        }

        self.due_timestamp = now + (self.interval_days as u64 * 86400);
    }
}

/// The collection of flashcards persisted on the device.
#[derive(Debug, Clone, Default)]
pub struct FlashcardDeck {
    pub cards: HashMap<String, Flashcard>,
}

impl FlashcardDeck {
    pub fn load() -> Self {
        Self::load_from(FLASHCARDS_PATH)
    }

    pub fn load_from<P: AsRef<Path>>(path: P) -> Self {
        let mut cards = HashMap::new();
        if let Ok(data) = fs::read_to_string(path) {
            for line in data.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 6 {
                    let word = parts[0].to_string();
                    let repetitions = parts[1].parse().unwrap_or(0);
                    let interval_days = parts[2].parse().unwrap_or(0);
                    let ease_factor = parts[3].parse().unwrap_or(2.5);
                    let due_timestamp = parts[4].parse().unwrap_or(0);
                    let last_reviewed = parts[5].parse().unwrap_or(0);
                    cards.insert(
                        word.clone(),
                        Flashcard {
                            word,
                            repetitions,
                            interval_days,
                            ease_factor,
                            due_timestamp,
                            last_reviewed,
                        },
                    );
                }
            }
        }
        FlashcardDeck { cards }
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all("/mnt/us/extensions/reader");
        let mut buf = String::new();
        for c in self.cards.values() {
            buf.push_str(&format!(
                "{}\t{}\t{}\t{:.2}\t{}\t{}\n",
                c.word, c.repetitions, c.interval_days, c.ease_factor, c.due_timestamp, c.last_reviewed
            ));
        }
        let _ = fs::write(FLASHCARDS_PATH, buf);
    }


    pub fn add_word(&mut self, word: &str) {
        let w = word.to_lowercase();
        if !self.cards.contains_key(&w) {
            self.cards.insert(w.clone(), Flashcard::new(&w));
            self.save();
        }
    }

    pub fn due_count(&self) -> usize {
        let now = now_secs();
        self.cards.values().filter(|c| c.due_timestamp <= now).count()
    }

    pub fn due_words(&self) -> Vec<String> {
        let now = now_secs();
        let mut due: Vec<&Flashcard> = self.cards.values().filter(|c| c.due_timestamp <= now).collect();
        due.sort_by_key(|c| c.due_timestamp);
        due.into_iter().map(|c| c.word.clone()).collect()
    }

    pub fn all_words(&self) -> Vec<String> {
        let mut words: Vec<String> = self.cards.keys().cloned().collect();
        words.sort();
        words
    }
}

#[derive(PartialEq, Eq)]
enum CardSide {
    Front,
    Back,
}

pub struct FlashcardsScreen {
    deck: FlashcardDeck,
    vocab_db: Option<VocabDb>,
    due_queue: Vec<String>,
    current_idx: usize,
    side: CardSide,
    reviewed_count: usize,
    current_entry: Option<WordEntry>,
    dims: (i32, i32),
}

impl FlashcardsScreen {
    pub fn new() -> Self {
        let deck = FlashcardDeck::load();
        let vocab_db = VocabDb::open();
        let mut due_queue = deck.due_words();

        // If no cards due, offer reviewing all words in deck
        if due_queue.is_empty() {
            due_queue = deck.all_words();
        }

        let first_entry = due_queue.first().and_then(|w| {
            vocab_db.as_ref().and_then(|db| db.lookup(w))
        });

        FlashcardsScreen {
            deck,
            vocab_db,
            due_queue,
            current_idx: 0,
            side: CardSide::Front,
            reviewed_count: 0,
            current_entry: first_entry,
            dims: (1236, 1648),
        }
    }

    fn load_current_entry(&mut self) {
        if let Some(word) = self.due_queue.get(self.current_idx) {
            self.current_entry = self.vocab_db.as_ref().and_then(|db| db.lookup(word));
        } else {
            self.current_entry = None;
        }
    }

    fn next_card(&mut self) {
        self.current_idx += 1;
        self.side = CardSide::Front;
        self.load_current_entry();
    }
}

impl Screen for FlashcardsScreen {
    fn default_edges(&self) -> bool {
        false
    }

    fn on_enter(&mut self) -> Action {
        Action::RedrawFull
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        // Header Navigation Bar
        let bar_h = pt(40.0);
        p.rect(Rect::new(0, 0, w, bar_h), 245);
        p.hline_t(bar_h, 0, w, 1, 200);

        // Title + Progress
        p.text(pt(16.0), pt(24.0), 10.0, 0, "🗂 Flashcards");
        
        let total = self.due_queue.len();
        if total > 0 && self.current_idx < total {
            let prog = format!("Card {} of {}", self.current_idx + 1, total);
            p.text_center(pt(24.0), 9.0, 90, &prog);
        }

        // Close Button on top-right
        let close_w = pt(55.0);
        let close_h = pt(24.0);
        let close_x = w - close_w - pt(12.0);
        let close_y = pt(8.0);
        let close_rect = Rect::new(close_x, close_y, close_w, close_h);
        p.rect_outline_t(close_rect, 1, 100);
        p.text_center_in(close_x, close_x + close_w, close_y + pt(16.0), 8.5, 0, "✕ Close");

        // Finished State
        if self.current_idx >= self.due_queue.len() || self.due_queue.is_empty() {
            let card_w = w - pt(36.0);
            let card_h = pt(220.0);
            let card_x = pt(18.0);
            let card_y = (h - card_h) / 2;
            let card_rect = Rect::new(card_x, card_y, card_w, card_h);

            p.rect(card_rect, 255);
            p.rect_outline_t(card_rect, 2, 0);

            p.text_center(card_y + pt(45.0), 16.0, 0, "🎉 All Caught Up!");
            
            let stat_msg = format!("Reviewed {} words today · Total in deck: {}", self.reviewed_count, self.deck.cards.len());
            p.text_center(card_y + pt(75.0), 10.0, 80, &stat_msg);

            // Button: Return to Library
            let btn_w = pt(180.0);
            let btn_h = pt(34.0);
            let btn_x = (w - btn_w) / 2;
            let btn_y = card_y + card_h - pt(50.0);
            let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);

            p.rect(btn_rect, 0);
            p.text_center_in(btn_x, btn_x + btn_w, btn_y + pt(22.0), 10.0, 255, "Return to Library");
            return;
        }

        // Active Card Display
        let card_w = w - pt(36.0);
        let card_h = h - bar_h - pt(100.0);
        let card_x = pt(18.0);
        let card_y = bar_h + pt(18.0);
        let card_rect = Rect::new(card_x, card_y, card_w, card_h);

        p.rect(card_rect, 255);
        p.rect_outline_t(card_rect, 2, 0);

        let cur_word = &self.due_queue[self.current_idx];
        let entry = self.current_entry.as_ref();
        let cefr_badge = entry.map(|e| format!("{} · lvl {}", e.cefr_str(), e.difficulty)).unwrap_or_else(|| "Vocab".to_string());

        match self.side {
            CardSide::Front => {
                // Front Side: Large Word + CEFR Badge + Tap Hint
                let mid_y = card_y + card_h / 2;
                p.text_center(mid_y - pt(25.0), 22.0, 0, cur_word);
                
                let badge_w = pt(70.0);
                let badge_x = (w - badge_w) / 2;
                let badge_y = mid_y + pt(5.0);
                p.rect(Rect::new(badge_x, badge_y, badge_w, pt(18.0)), 240);
                p.text_center_in(badge_x, badge_x + badge_w, badge_y + pt(13.0), 8.5, 60, &cefr_badge);

                p.text_center(card_y + card_h - pt(30.0), 9.0, 120, "Tap card to flip");
            }
            CardSide::Back => {
                // Back Side: Word + Russian Translation + English Definition
                let mut top_y = card_y + pt(30.0);
                p.text(card_x + pt(20.0), top_y, 16.0, 0, cur_word);
                p.text_right(card_x + card_w - pt(20.0), top_y - pt(2.0), 8.5, 90, &cefr_badge);
                top_y += pt(10.0);
                p.hline_t(top_y, card_x + pt(18.0), card_x + card_w - pt(18.0), 1, 220);

                top_y += pt(24.0);
                if let Some(e) = entry {
                    if !e.gloss_ru.is_empty() {
                        p.text(card_x + pt(20.0), top_y, 8.0, 110, "РУССКИЙ ПЕРЕВОД");
                        top_y += pt(16.0);
                        p.text(card_x + pt(20.0), top_y, 13.0, 0, &e.gloss_ru);
                        top_y += pt(26.0);
                    }

                    if !e.gloss_en.is_empty() {
                        p.text(card_x + pt(20.0), top_y, 8.0, 110, "ENGLISH DEFINITION");
                        top_y += pt(16.0);

                        // Wrap English definition text
                        let max_w = (card_w - pt(40.0)) as f32;
                        let mut cur_line = String::new();
                        for word in e.gloss_en.split_whitespace() {
                            let test = if cur_line.is_empty() { word.to_string() } else { format!("{} {}", cur_line, word) };
                            if p.text_width(10.0, &test) > max_w {
                                if !cur_line.is_empty() {
                                    p.text(card_x + pt(20.0), top_y, 10.0, 30, &cur_line);
                                    top_y += pt(15.0);
                                }
                                cur_line = word.to_string();
                            } else {
                                cur_line = test;
                            }
                        }
                        if !cur_line.is_empty() {
                            p.text(card_x + pt(20.0), top_y, 10.0, 30, &cur_line);
                        }
                    }
                }

                // 4 SM-2 Grade Buttons docked at screen bottom
                let btn_h = pt(32.0);
                let btn_y = h - btn_h - pt(12.0);
                let spacing = pt(8.0);
                let btn_w = (w - pt(36.0) - (spacing * 3)) / 4;

                let grades = [
                    ("1. Again", "1d"),
                    ("2. Hard", "3d"),
                    ("3. Good", "6d"),
                    ("4. Easy", "12d"),
                ];

                for (i, (label, interval)) in grades.iter().enumerate() {
                    let bx = pt(18.0) + i as i32 * (btn_w + spacing);
                    let brect = Rect::new(bx, btn_y, btn_w, btn_h);

                    if i == 0 {
                        p.rect(brect, 240);
                        p.rect_outline_t(brect, 1, 0);
                    } else if i == 2 {
                        p.rect(brect, 0);
                    } else {
                        p.rect(brect, 255);
                        p.rect_outline_t(brect, 1, 100);
                    }

                    let fg = if i == 2 { 255 } else { 0 };
                    let sub_fg = if i == 2 { 200 } else { 100 };

                    p.text_center_in(bx, bx + btn_w, btn_y + pt(13.0), 8.0, fg, label);
                    p.text_center_in(bx, bx + btn_w, btn_y + pt(24.0), 7.0, sub_fg, interval);
                }
            }
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = self.dims;

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // Close Button tap
                let close_w = pt(55.0);
                let close_h = pt(24.0);
                let close_x = w - close_w - pt(12.0);
                let close_y = pt(8.0);
                let close_rect = Rect::new(close_x, close_y, close_w, close_h);
                if close_rect.contains(px, py) {
                    return Action::Pop;
                }

                // If finished, Return to Library button
                if self.current_idx >= self.due_queue.len() || self.due_queue.is_empty() {
                    let _card_w = w - pt(36.0);
                    let card_h = pt(220.0);
                    let card_y = (h - card_h) / 2;
                    let btn_w = pt(180.0);
                    let btn_h = pt(34.0);
                    let btn_x = (w - btn_w) / 2;
                    let btn_y = card_y + card_h - pt(50.0);
                    let btn_rect = Rect::new(btn_x, btn_y, btn_w, btn_h);
                    if btn_rect.contains(px, py) {
                        return Action::Pop;
                    }
                    return Action::Keep;
                }


                // Active Card interaction
                if self.side == CardSide::Front {
                    // Tap to flip card
                    self.side = CardSide::Back;
                    return Action::Redraw;
                }

                // Back side: Grade button hit test
                let btn_h = pt(32.0);
                let btn_y = h - btn_h - pt(12.0);
                let spacing = pt(8.0);
                let btn_w = (w - pt(36.0) - (spacing * 3)) / 4;

                for i in 0..4 {
                    let bx = pt(18.0) + i as i32 * (btn_w + spacing);
                    let brect = Rect::new(bx, btn_y, btn_w, btn_h);
                    if brect.contains(px, py) {
                        let cur_word = self.due_queue[self.current_idx].clone();
                        if let Some(card) = self.deck.cards.get_mut(&cur_word) {
                            card.apply_sm2(i as u8);
                        } else {
                            let mut card = Flashcard::new(&cur_word);
                            card.apply_sm2(i as u8);
                            self.deck.cards.insert(cur_word, card);
                        }
                        self.deck.save();
                        self.reviewed_count += 1;
                        self.next_card();
                        return Action::Redraw;
                    }
                }

                Action::Keep
            }
            Gesture::Swipe { .. } => Action::Pop,
            _ => Action::Keep,
        }
    }
}
