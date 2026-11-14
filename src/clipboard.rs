//! Clipboard monitoring via the modern clipboard-format-listener API.
//!
//! A hidden window is registered with `AddClipboardFormatListener`.  Whenever
//! the clipboard changes Windows posts `WM_CLIPBOARDUPDATE` to that window; we
//! read the text (if any) and store it in the history database.  This avoids
//! the legacy clipboard-viewer chain entirely (no WM_DRAWCLIPBOARD forwarding,
//! no WM_CHANGECBCHAIN repair).

use crate::config::{MAX_ITEM_BYTES, MAX_STORED};
use crate::history::History;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Global pointer to the shared history store (set once at startup).
static HISTORY_PTR: AtomicPtr<History> = AtomicPtr::new(std::ptr::null_mut());

/// Opaque handle to the hidden monitor window.
pub struct ClipboardMonitor {
    hwnd: HWND,
}

impl ClipboardMonitor {
    /// Create the hidden window and register it as a clipboard listener.
    ///
    /// # Safety
    /// Must be called on a thread that will pump messages.
    pub unsafe fn new(history: Arc<History>) -> Result<Self> {
        // Publish the Arc pointer for the static wndproc to find.
        HISTORY_PTR.store(Arc::into_raw(history) as *mut History, Ordering::SeqCst);

        let instance = GetModuleHandleW(None)?;
        let class = wide("ClipyW_ClipMonitor");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(monitor_wndproc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: class,
            ..Default::default()
        };
        let atom = RegisterClassW(&wc);
        if atom == 0 {
            let err = Error::from_win32();
            if err.code().0 as u32 != ERROR_CLASS_ALREADY_EXISTS {
                return Err(err);
            }
        }

        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE,
            class,
            wide("ClipyW_Monitor"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )?;
        if hwnd.0.is_null() {
            return Err(Error::from_win32());
        }

        AddClipboardFormatListener(hwnd)?;
        Ok(Self { hwnd })
    }

    /// Window handle of the monitor.  Reserved for future features (e.g. a
    /// system-tray context menu that needs the hwnd).
    #[allow(dead_code)]
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for ClipboardMonitor {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveClipboardFormatListener(self.hwnd);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Message handler for the hidden monitor window.
unsafe extern "system" fn monitor_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            let ptr = HISTORY_PTR.load(Ordering::SeqCst);
            if !ptr.is_null() {
                // The pointer is to the History data itself (Arc::into_raw
                // returns *const T), so use a plain reference — not &Arc.
                let history: &History = &*(ptr as *const History);
                if let Ok(text) = read_clipboard_text() {
                    if should_record(&text) {
                        history.push(&text);
                    }
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Extract UTF-16 text from the clipboard, if present.
unsafe fn read_clipboard_text() -> Result<String> {
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
        return Ok(String::new());
    }
    // The clipboard may be held briefly by another app; retry a few times.
    let mut opened = false;
    for _ in 0..10 {
        if OpenClipboard(None).is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return Ok(String::new());
    }
    let result = (|| -> Result<String> {
        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32)?;
        if handle.0.is_null() {
            return Ok(String::new());
        }
        let hglobal: HGLOBAL = HGLOBAL(handle.0);

        // NOTE: GlobalFlags on a clipboard handle reads 0 — the flags only
        // apply to handles we allocated ourselves — so there is nothing to
        // validate here.  The read is bounded by GlobalSize + the NUL scan.

        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            return Ok(String::new());
        }

        // Walk u16 words until NUL, never reading past the block size.
        let bytes = GlobalSize(hglobal) as usize;
        let words = bytes / 2;
        let mut len = 0usize;
        while len < words {
            if *(ptr as *const u16).add(len) == 0 {
                break;
            }
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, len));
        let _ = GlobalUnlock(hglobal);
        Ok(text)
    })();
    let _ = CloseClipboard();
    result
}

/// Decide whether a clipboard payload is worth recording.
fn should_record(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_ITEM_BYTES
        && text.len() <= MAX_STORED * 4 * 1024
}

/// Convert a string literal to a `PCWSTR`. The returned pointer points to a
/// leaked allocation so it stays valid for the lifetime of the program.
fn wide(s: &str) -> PCWSTR {
    let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let ptr = Box::leak(v.into_boxed_slice()).as_ptr();
    PCWSTR(ptr)
}

/// `ERROR_CLASS_ALREADY_EXISTS` — our window class was registered earlier in
/// this process (only possible if [`ClipboardMonitor::new`] runs twice).
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
