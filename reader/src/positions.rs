//! Per-book reading positions, persisted as one small file on /mnt/us.
//! Format: one line per book, `file_name<TAB>page<TAB>total<TAB>ts`
//! (ts = unix seconds, so the freshest entry is the "last read" book).
//! Writes go to a .tmp then rename — a power cut must not truncate the
//! whole store mid-write.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const STORE: &str = "/mnt/us/extensions/reader/positions.txt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub page: usize,
    pub total: usize,
    pub ts: u64,
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse the store text. Malformed lines are skipped, not fatal.
fn parse(text: &str) -> HashMap<String, Pos> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(name), Some(page), Some(total), Some(ts)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if let (Ok(page), Ok(total), Ok(ts)) = (page.parse(), total.parse(), ts.parse()) {
            map.insert(name.to_string(), Pos { page, total, ts });
        }
    }
    map
}

fn load_at(path: &str) -> HashMap<String, Pos> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

fn save_at(path: &str, map: &HashMap<String, Pos>) {
    let mut lines: Vec<String> = map
        .iter()
        .map(|(k, p)| format!("{}\t{}\t{}\t{}", k, p.page, p.total, p.ts))
        .collect();
    lines.sort();
    let tmp = format!("{}.tmp", path);
    if std::fs::write(&tmp, lines.join("\n") + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Saved page for a book (0 when never opened).
pub fn resume_page(name: &str) -> usize {
    load_at(STORE).get(name).map(|p| p.page).unwrap_or(0)
}

/// Record progress; stamps the entry now (making it the last-read book).
pub fn record(name: &str, page: usize, total: usize) {
    let mut map = load_at(STORE);
    map.insert(name.to_string(), Pos { page, total, ts: now_ts() });
    save_at(STORE, &map);
}

/// The most recently opened book, if any.
pub fn last_read() -> Option<(String, Pos)> {
    load_at(STORE).into_iter().max_by_key(|(_, p)| p.ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        let text = "a.epub\t12\t340\t1700000000\nb.epub\t0\t0\t1700000005\n";
        let map = parse(text);
        assert_eq!(map["a.epub"], Pos { page: 12, total: 340, ts: 1700000000 });
        assert_eq!(map.len(), 2);

        let path = "/tmp/yb-positions-test.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);
        assert_eq!(load_at(path), map);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let map = parse("garbage\n\nx.epub\t3\t10\ny.epub\t1\t2\t3\t4");
        // "garbage" and empty lines dropped; "x.epub" incomplete;
        // "y.epub" has a trailing extra field — the first four parse.
        assert_eq!(map.len(), 1);
        assert_eq!(map["y.epub"], Pos { page: 1, total: 2, ts: 3 });
    }

    #[test]
    fn last_read_is_freshest() {
        let mut map = HashMap::new();
        map.insert("old.epub".into(), Pos { page: 1, total: 9, ts: 100 });
        map.insert("new.epub".into(), Pos { page: 2, total: 9, ts: 200 });
        let path = "/tmp/yb-positions-last.txt";
        let _ = std::fs::remove_file(path);
        save_at(path, &map);
        let (name, pos) = load_at(path).into_iter().max_by_key(|(_, p)| p.ts).unwrap();
        assert_eq!(name, "new.epub");
        assert_eq!(pos.page, 2);
        let _ = std::fs::remove_file(path);
    }
}
