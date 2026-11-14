//! Persistent clipboard history backed by a sled KV store.
//!
//! Keys are big-endian encoded sequence numbers so lexicographic order
//! matches insertion order.  A single `META` key tracks the monotonic
//! counter, letting us prepend new entries without scanning the DB.

use crate::config::{db_path, MAX_STORED};
use sled::{Db, IVec};
use std::sync::Mutex;

/// Well-known key holding the next sequence number.
const META: &[u8] = b"__meta_next_seq__";

/// A single history entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Monotonic insert order; reserved for future features (e.g. pinning).
    #[allow(dead_code)]
    pub seq: u64,
    pub text: String,
}

/// Thread-safe handle to the history store.
pub struct History {
    db: Mutex<Db>,
}

impl History {
    /// Open (or create) the database at the configured path.
    pub fn open() -> std::io::Result<Self> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::open_at(&path)
    }

    /// Open (or create) the database at an explicit path.  Used by tests.
    pub fn open_at(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = sled::open(path).map_err(|e| std::io::Error::other(e))?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Insert a new entry at the front of the history.
    ///
    /// If the same text already exists it is promoted to the top instead of
    /// duplicated.  Returns the sequence number assigned to the entry.
    pub fn push(&self, text: &str) -> u64 {
        let db = self.db.lock().expect("sled poisoned");

        // Promote existing entry if present.
        if let Some((old_key, _)) = db
            .iter()
            .filter_map(|r| r.ok())
            .find(|(_, v)| v.as_ref() == text.as_bytes())
        {
            if old_key.as_ref() != META {
                let _ = db.remove(old_key.as_ref());
            }
        }

        let next = next_seq(&db);
        let key = encode_key(next);
        let _ = db.insert(key.to_vec(), text.as_bytes().to_vec());
        let _ = db.insert(
            META.to_vec(),
            next.wrapping_add(1).to_be_bytes().to_vec(),
        );
        let _ = db.flush();

        self.trim_to_limit(&db);
        next
    }

    /// Return up to `count` most-recent entries, newest first.
    pub fn recent(&self, count: usize) -> Vec<Entry> {
        let db = self.db.lock().expect("sled poisoned");
        db.iter()
            .rev()
            .filter_map(|r| r.ok())
            .filter(|(k, _)| k.as_ref() != META)
            .take(count)
            .filter_map(|(k, v)| {
                let seq = decode_key(k.as_ref())?;
                let text = String::from_utf8_lossy(v.as_ref()).into_owned();
                Some(Entry { seq, text })
            })
            .collect()
    }

    /// Remove every stored entry.  Reserved for a future "clear history"
    /// menu item / hotkey.
    #[allow(dead_code)]
    pub fn clear(&self) {
        let db = self.db.lock().expect("sled poisoned");
        let keys: Vec<IVec> = db.iter().filter_map(|r| r.ok().map(|(k, _)| k)).collect();
        for k in keys {
            let _ = db.remove(k);
        }
        let _ = db.insert(META.to_vec(), 0u64.to_be_bytes().to_vec());
        let _ = db.flush();
    }

    /// Delete entries beyond `MAX_STORED`, keeping the newest ones.
    fn trim_to_limit(&self, db: &Db) {
        let excess = db.len().saturating_sub(MAX_STORED + 1); // +1 for META
        if excess == 0 {
            return;
        }
        // Oldest entries live at the start of the key space.
        let mut to_remove = Vec::new();
        for r in db.iter() {
            if to_remove.len() >= excess {
                break;
            }
            if let Ok((k, _)) = r {
                if k.as_ref() != META {
                    to_remove.push(k);
                }
            }
        }
        for k in to_remove {
            let _ = db.remove(k);
        }
        let _ = db.flush();
    }
}

/// Read the monotonic counter, initialising it to 0 on first run.
fn next_seq(db: &Db) -> u64 {
    match db.get(META) {
        Ok(Some(v)) => u64::from_be_bytes(v.as_ref().try_into().unwrap_or([0u8; 8])),
        _ => 0,
    }
}

/// Encode a sequence number as an 8-byte big-endian key.
fn encode_key(seq: u64) -> [u8; 8] {
    seq.to_be_bytes()
}

/// Decode an 8-byte big-endian key back into a sequence number.
fn decode_key(key: &[u8]) -> Option<u64> {
    let arr: [u8; 8] = key.try_into().ok()?;
    Some(u64::from_be_bytes(arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Open a throwaway database in the temp dir so tests never touch the
    /// user's real history.
    fn temp_history() -> History {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("clipyrs-test-{}", nanos));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let db = sled::open(&path).unwrap();
        drop(db); // reopen through History so the path logic is exercised
        History::open_at(&path).unwrap()
    }

    #[test]
    fn push_and_recent_roundtrip() {
        let h = temp_history();
        h.push("first");
        h.push("second");
        h.push("third");
        let items = h.recent(10);
        assert_eq!(items.len(), 3);
        // Newest first.
        assert_eq!(items[0].text, "third");
        assert_eq!(items[1].text, "second");
        assert_eq!(items[2].text, "first");
    }

    #[test]
    fn duplicate_text_is_promoted_not_duplicated() {
        let h = temp_history();
        h.push("alpha");
        h.push("beta");
        h.push("alpha"); // should move "alpha" to the top
        let items = h.recent(10);
        assert_eq!(items.len(), 2, "no duplicate rows expected");
        assert_eq!(items[0].text, "alpha");
        assert_eq!(items[1].text, "beta");
    }

    #[test]
    fn recent_respects_count() {
        let h = temp_history();
        for i in 0..25 {
            h.push(&format!("item-{}", i));
        }
        assert_eq!(h.recent(10).len(), 10);
        assert_eq!(h.recent(10)[0].text, "item-24");
        assert_eq!(h.recent(10)[9].text, "item-15");
    }

    #[test]
    fn clear_removes_everything() {
        let h = temp_history();
        h.push("one");
        h.push("two");
        h.clear();
        assert!(h.recent(10).is_empty());
    }

    #[test]
    fn trim_to_limit_keeps_newest() {
        let h = temp_history();
        // MAX_STORED is 100; insert far more.
        for i in 0..(MAX_STORED + 50) {
            h.push(&format!("n-{}", i));
        }
        let items = h.recent(MAX_STORED + 50);
        assert_eq!(items.len(), MAX_STORED, "oldest entries should be trimmed");
        assert_eq!(items[0].text, format!("n-{}", MAX_STORED + 49));
    }
}
