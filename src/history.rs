//! Persistent clipboard history backed by a sled KV store.
//!
//! Keys are big-endian encoded sequence numbers so lexicographic order
//! matches insertion order.  A single `META` key tracks the monotonic
//! counter, letting us prepend new entries without scanning the DB.

use crate::config::{db_path, MAX_STORED};
use serde::{Deserialize, Serialize};
use sled::{Db, IVec};
use std::sync::Mutex;

/// Well-known key holding the next sequence number.
const META: &[u8] = b"__meta_next_seq__";
const PAYLOAD_MAGIC: &[u8] = b"CLIPYW01";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ClipboardFormatId {
    Windows(u32),
    Registered(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClipboardFormat {
    pub id: ClipboardFormatId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClipboardPayload {
    pub preview: String,
    pub formats: Vec<ClipboardFormat>,
}

impl ClipboardPayload {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        bytes.extend_from_slice(&[0, 0]);
        Self {
            preview: text,
            formats: vec![ClipboardFormat {
                id: ClipboardFormatId::Windows(CF_UNICODETEXT_ID),
                bytes,
            }],
        }
    }
}

const CF_UNICODETEXT_ID: u32 = 13;

/// A single history entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Monotonic insert order; reserved for future features (e.g. pinning).
    #[allow(dead_code)]
    pub seq: u64,
    pub text: String,
    pub payload: ClipboardPayload,
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
        let db = sled::open(path).map_err(std::io::Error::other)?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Insert a new entry at the front of the history.
    ///
    /// If the same text already exists it is promoted to the top instead of
    /// duplicated.  Returns the sequence number assigned to the entry.
    #[cfg(test)]
    pub fn push(&self, text: &str) -> u64 {
        self.push_payload(&ClipboardPayload::from_text(text))
    }

    /// Insert a full clipboard payload, promoting identical payloads instead of duplicating them.
    pub fn push_payload(&self, payload: &ClipboardPayload) -> u64 {
        let db = self.db.lock().expect("sled poisoned");
        let encoded = encode_payload(payload);

        if let Some((old_key, _)) = db
            .iter()
            .filter_map(|r| r.ok())
            .find(|(_, value)| value.as_ref() == encoded.as_slice())
        {
            if old_key.as_ref() != META {
                let _ = db.remove(old_key.as_ref());
            }
        }

        let next = next_seq(&db);
        let key = encode_key(next);
        let _ = db.insert(key, encoded);
        let _ = db.insert(
            META,
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
            .filter_map(|(k, value)| {
                let seq = decode_key(k.as_ref())?;
                let payload = decode_payload(value.as_ref());
                Some(Entry {
                    seq,
                    text: payload.preview.clone(),
                    payload,
                })
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
        let _ = db.insert(META, 0u64.to_be_bytes().to_vec());
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

fn encode_payload(payload: &ClipboardPayload) -> Vec<u8> {
    let encoded = bincode::serialize(payload).expect("clipboard payload serialization failed");
    let mut value = Vec::with_capacity(PAYLOAD_MAGIC.len() + encoded.len());
    value.extend_from_slice(PAYLOAD_MAGIC);
    value.extend_from_slice(&encoded);
    value
}

fn decode_payload(value: &[u8]) -> ClipboardPayload {
    if let Some(encoded) = value.strip_prefix(PAYLOAD_MAGIC) {
        if let Ok(payload) = bincode::deserialize(encoded) {
            return payload;
        }
    }

    ClipboardPayload::from_text(String::from_utf8_lossy(value).into_owned())
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
        let dir = std::env::temp_dir().join(format!("clipy-w-test-{}", nanos));
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
        // Insert more than the configured storage limit.
        for i in 0..(MAX_STORED + 50) {
            h.push(&format!("n-{}", i));
        }
        let items = h.recent(MAX_STORED + 50);
        assert_eq!(items.len(), MAX_STORED, "oldest entries should be trimmed");
        assert_eq!(items[0].text, format!("n-{}", MAX_STORED + 49));
    }

    #[test]
    fn rich_clipboard_formats_roundtrip_through_history() {
        let history = temp_history();
        let payload = ClipboardPayload {
            preview: "Formatted example".to_owned(),
            formats: vec![
                ClipboardFormat {
                    id: ClipboardFormatId::Windows(13),
                    bytes: vec![b't', 0, 0, 0],
                },
                ClipboardFormat {
                    id: ClipboardFormatId::Registered("HTML Format".to_owned()),
                    bytes: b"<b>Formatted example</b>".to_vec(),
                },
            ],
        };

        history.push_payload(&payload);

        let entry = history.recent(1).pop().unwrap();
        assert_eq!(entry.text, "Formatted example");
        assert_eq!(entry.payload, payload);
    }

    #[test]
    fn legacy_plain_text_history_remains_readable() {
        let history = temp_history();
        {
            let db = history.db.lock().unwrap();
            db.insert(encode_key(0), b"legacy copied text").unwrap();
            db.insert(META, 1u64.to_be_bytes().to_vec()).unwrap();
        }

        let entry = history.recent(1).pop().unwrap();
        assert_eq!(entry.text, "legacy copied text");
        assert_eq!(entry.payload.preview, "legacy copied text");
        assert!(entry.payload.formats.iter().any(|format| {
            format.id == ClipboardFormatId::Windows(13)
        }));
    }
}
