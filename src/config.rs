//! Application configuration constants.

use std::io;
use std::path::{Path, PathBuf};

/// Display name of the application.
pub const APP_NAME: &str = "Clipy-W";

pub const HOTKEY_ID: i32 = 1;

/// Hotkey key code for Ctrl + Shift + V.
pub const HOTKEY_VK: u32 = 0x56; // 'V'

/// Virtual-key code of the test hotkey (F9), used when `CLIPY_W_TEST=1` is
/// set so the app can be driven end-to-end without the real Windows key.
/// F9 is used instead of F12 because F12 is claimed by another app on some
/// machines (including the dev box).
pub const TEST_HOTKEY_VK: u32 = 0x78; // VK_F9

/// Maximum number of items kept in the history database.
pub const MAX_STORED: usize = 30;

/// Number of items shown in the popup window.
pub const VISIBLE_ITEMS: usize = 10;

/// Maximum total size of clipboard formats retained for one history item.
pub const MAX_ITEM_BYTES: usize = 20 * 1024 * 1024;

/// When an item is chosen, automatically simulate Ctrl+V to paste it.
pub const AUTO_PASTE: bool = true;

/// Width of the popup window (pixels).
pub const POPUP_WIDTH: i32 = 560;

/// Width of the history-range popup (pixels).
pub const RANGE_WIDTH: i32 = 210;

/// Height of the popup header area (pixels).
pub const POPUP_HEADER: i32 = 34;

/// Height of one history row (pixels).
pub const POPUP_ROW: i32 = 28;

/// Height of a popup containing `item_count` rows.
pub fn popup_height(item_count: usize) -> i32 {
    POPUP_HEADER + (item_count.min(VISIBLE_ITEMS) as i32) * POPUP_ROW + 12
}

/// Path to the sled database directory, e.g. `%APPDATA%\clipy-w\history.db`.
pub fn db_path() -> PathBuf {
    data_dir().join("history.db")
}

/// Path to the persisted snippet folders.
pub fn snippets_path() -> PathBuf {
    data_dir().join("snippets.json")
}

/// Move existing ClipYRS data into the renamed application's data directory.
pub fn migrate_legacy_data() -> io::Result<()> {
    migrate_data_dir(&legacy_data_dir(), &data_dir())
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    if std::env::var_os("APPDATA").is_some() {
        base.join("clipy-w")
    } else {
        base.join(".clipy-w")
    }
}

fn legacy_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    if std::env::var_os("APPDATA").is_some() {
        base.join("clipyrs")
    } else {
        base.join(".clipyrs")
    }
}

fn migrate_data_dir(legacy: &Path, current: &Path) -> io::Result<()> {
    if !legacy.exists() {
        return Ok(());
    }
    if !current.exists() {
        return std::fs::rename(legacy, current);
    }

    for name in ["history.db", "snippets.json"] {
        let source = legacy.join(name);
        let destination = current.join(name);
        if source.exists() && !destination.exists() {
            std::fs::rename(source, destination)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::migrate_data_dir;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn migration_preserves_existing_history_and_snippets() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clipy-w-migration-{unique}"));
        let legacy = root.join("clipyrs");
        let current = root.join("clipy-w");
        std::fs::create_dir_all(legacy.join("history.db")).unwrap();
        std::fs::write(legacy.join("snippets.json"), b"{}").unwrap();

        migrate_data_dir(&legacy, &current).unwrap();

        assert!(current.join("history.db").is_dir());
        assert_eq!(std::fs::read(current.join("snippets.json")).unwrap(), b"{}");
        assert!(!legacy.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
