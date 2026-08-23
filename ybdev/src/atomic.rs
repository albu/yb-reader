//! Atomic durable writes for the plain-file stores (positions,
//! flashcards, vocab profile, metadata cache).
//!
//! Contract: after `write` returns true, `path` holds exactly `data` and
//! the bytes are on the medium — a crash or power loss at any point
//! costs at most the *previous* contents, never a truncated/empty store.
//! That needs all three steps: tmp file (a failed write never touches
//! the real name), fsync (the rename is a directory operation that a
//! power cut can otherwise persist while the data blocks are not —
//! a valid name over empty content), rename (atomic swap).

use std::io::Write as _;
use std::path::Path;

/// Atomically replace `path` with `data`. False on any I/O failure (the
/// previous file, if any, is left untouched; a `.tmp` may remain — it is
/// inert and the next write overwrites it).
pub fn write(path: impl AsRef<Path>, data: &[u8]) -> bool {
    let path = path.as_ref();
    let tmp = format!("{}.tmp", path.display());
    let Ok(mut f) = std::fs::File::create(&tmp) else {
        return false;
    };
    if f.write_all(data).is_err() || f.sync_all().is_err() {
        return false;
    }
    drop(f);
    std::fs::rename(&tmp, path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_content_and_leaves_no_tmp() {
        let path = std::env::temp_dir().join("yb-atomic-write-test.txt");
        let _ = std::fs::remove_file(&path);
        assert!(write(&path, b"one"));
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert!(write(&path, b"two"));
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!path.with_extension("txt.tmp").exists(), "no tmp left behind");
        let _ = std::fs::remove_file(&path);
    }
}
