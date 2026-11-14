//! Application configuration constants.

use std::path::PathBuf;

/// Display name of the application.
pub const APP_NAME: &str = "ClipYRS";

/// Hotkey: Win + Shift + V (mirrors Clipy's Cmd+Shift+V on macOS).
pub const HOTKEY_ID: i32 = 1;

/// Virtual-key code of the test hotkey (F9), used when `CLIPYRS_TEST=1` is
/// set so the app can be driven end-to-end without the real Windows key.
/// F9 is used instead of F12 because F12 is claimed by another app on some
/// machines (including the dev box).
pub const TEST_HOTKEY_VK: u32 = 0x78; // VK_F9

/// Maximum number of items kept in the history database.
pub const MAX_STORED: usize = 100;

/// Number of items shown in the popup window.
pub const VISIBLE_ITEMS: usize = 10;

/// Ignore clipboard content larger than this (bytes) to avoid memory bloat.
pub const MAX_ITEM_BYTES: usize = 100 * 1024;

/// When an item is chosen, automatically simulate Ctrl+V to paste it.
pub const AUTO_PASTE: bool = true;

/// Width of the popup window (pixels).
pub const POPUP_WIDTH: i32 = 560;

/// Height of the popup header area (pixels).
pub const POPUP_HEADER: i32 = 34;

/// Height of one history row (pixels).
pub const POPUP_ROW: i32 = 28;

/// Total popup height for [`VISIBLE_ITEMS`] rows.
pub fn popup_height() -> i32 {
    POPUP_HEADER + (VISIBLE_ITEMS as i32) * POPUP_ROW + 12
}

/// Path to the sled database file, e.g. `%APPDATA%\clipyrs\history.db`.
pub fn db_path() -> PathBuf {
    let mut p = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_data_dir());
    p.push("clipyrs");
    p.push("history.db");
    p
}

/// Fallback when APPDATA is missing: `%USERPROFILE%\.clipyrs`.
fn dirs_data_dir() -> PathBuf {
    let mut p = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    p.push(".clipyrs");
    p.push("history.db");
    p
}
