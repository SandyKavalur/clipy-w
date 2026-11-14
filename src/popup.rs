//! Popup window that lists the most recent clipboard entries.
//!
//! The window is borderless, always-on-top, and centred on the primary
//! monitor.  Arrow keys move the selection, Enter copies the selection back
//! to the clipboard and (optionally) simulates Ctrl+V, Esc dismisses the
//! popup.  Clicking a row selects it.  Clicking outside the popup dismisses
//! it (handled via WM_ACTIVATE).

use crate::config::{
    popup_height, AUTO_PASTE, POPUP_HEADER, POPUP_ROW, POPUP_WIDTH, VISIBLE_ITEMS,
};
use crate::history::{Entry, History};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// `ERROR_CLASS_ALREADY_EXISTS` — the popup class was registered by an
/// earlier call to [`show`] in this process.
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

/// Currently selected row index (0-based).
static SELECTED: Lazy<Mutex<usize>> = Lazy::new(|| Mutex::new(0));

/// Snapshot of entries shown by the popup, shared with the wndproc.
static ENTRIES: Lazy<Mutex<Vec<Entry>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Handle of the currently open popup, or zero when it is closed.
static ACTIVE_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Show the popup centred on the primary monitor, listing the newest entries.
///
/// # Safety
/// Must be called from a thread with a message pump.
pub unsafe fn show(history: &History) -> Result<()> {
    let entries = history.recent(VISIBLE_ITEMS);
    if entries.is_empty() {
        return Ok(());
    }
    *ENTRIES.lock().unwrap() = entries;
    *SELECTED.lock().unwrap() = 0;

    let active = ACTIVE_WINDOW.load(Ordering::Acquire);
    if active != 0 {
        let active_hwnd = HWND(active as _);
        if IsWindow(Some(active_hwnd)).as_bool() {
            let _ = InvalidateRect(Some(active_hwnd), None, true);
            let _ = SetForegroundWindow(active_hwnd);
            return Ok(());
        }
        let _ = ACTIVE_WINDOW.compare_exchange(active, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    let instance = GetModuleHandleW(None)?;
    let class = wide("ClipYRS_Popup");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_wndproc),
        hInstance: HINSTANCE(instance.0),
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as _),
        lpszClassName: class,
        ..Default::default()
    };
    // The popup is shown repeatedly, so the class may already be registered.
    if RegisterClassW(&wc) == 0 {
        if GetLastError() != WIN32_ERROR(ERROR_CLASS_ALREADY_EXISTS) {
            return Err(Error::from_win32());
        }
    }

    let height = popup_height();
    let title = wide("ClipYRS");
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        class,
        title,
        WS_POPUP | WS_BORDER,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        POPUP_WIDTH,
        height,
        None,
        None,
        Some(HINSTANCE(instance.0)),
        None,
    )?;
    if hwnd.0.is_null() {
        return Err(Error::from_win32());
    }

    ACTIVE_WINDOW.store(hwnd.0 as isize, Ordering::Release);
    centre_on_screen(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = UpdateWindow(hwnd);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

/// Position a window at the centre of the primary work area.
unsafe fn centre_on_screen(hwnd: HWND) {
    let mut rect = RECT::default();
    if SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some(&mut rect as *mut _ as *mut _),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
    .is_ok()
    {
        let w = POPUP_WIDTH;
        let h = popup_height();
        let x = rect.left + (rect.right - rect.left - w) / 2;
        let y = rect.top + (rect.bottom - rect.top - h) / 2;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            w,
            h,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
    }
}

/// Window procedure for the popup.
unsafe extern "system" fn popup_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_rows(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let mut sel = SELECTED.lock().unwrap();
            let count = ENTRIES.lock().unwrap().len();
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            match vk {
                VK_UP => {
                    if *sel > 0 {
                        *sel -= 1;
                    }
                }
                VK_DOWN => {
                    if *sel + 1 < count {
                        *sel += 1;
                    }
                }
                VK_RETURN => {
                    let idx = *sel;
                    drop(sel);
                    choose_and_close(hwnd, idx);
                    return LRESULT(0);
                }
                VK_ESCAPE | VK_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                    return LRESULT(0);
                }
                _ => {}
            }
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = (lparam.0 as u16) as i32;
            let row = ((y - POPUP_HEADER) / POPUP_ROW).max(0) as usize;
            let count = ENTRIES.lock().unwrap().len();
            if row < count {
                choose_and_close(hwnd, row);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ACTIVATE => {
            // Dismiss when the user clicks away to another window.
            if wparam.0 == WA_INACTIVE as usize {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = ACTIVE_WINDOW.compare_exchange(
                hwnd.0 as isize,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Copy entry `idx` to the clipboard, close the popup, and (optionally)
/// simulate Ctrl+V so the text lands in whatever app regains focus.
unsafe fn choose_and_close(hwnd: HWND, idx: usize) {
    let text = ENTRIES.lock().unwrap().get(idx).map(|e| e.text.clone());
    let _ = DestroyWindow(hwnd);
    if let Some(text) = text {
        set_clipboard_text(&text);
        if AUTO_PASTE {
            // Give Windows a moment to move focus back to the previous app.
            std::thread::sleep(Duration::from_millis(80));
            simulate_ctrl_v();
        }
    }
}

/// Draw the header bar and the visible rows.
unsafe fn paint_rows(hdc: HDC) {
    let entries = ENTRIES.lock().unwrap();
    let sel = *SELECTED.lock().unwrap();

    // Header background.
    let header = RECT {
        left: 0,
        top: 0,
        right: POPUP_WIDTH,
        bottom: POPUP_HEADER,
    };
    FillRect(hdc, &header, HBRUSH((COLOR_HOTLIGHT.0 + 1) as _));
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, rgb(255, 255, 255));
    let _ = TextOutW(
        hdc,
        12,
        8,
        &wide_null("Clipboard history — last 10 items"),
    );

    for (i, entry) in entries.iter().enumerate() {
        let top = POPUP_HEADER + (i as i32) * POPUP_ROW;
        let row = RECT {
            left: 0,
            top,
            right: POPUP_WIDTH,
            bottom: top + POPUP_ROW,
        };
        if i == sel {
            FillRect(hdc, &row, HBRUSH((COLOR_HIGHLIGHT.0 + 1) as _));
            SetTextColor(hdc, rgb(255, 255, 255));
        } else {
            FillRect(hdc, &row, HBRUSH((COLOR_WINDOW.0 + 1) as _));
            SetTextColor(hdc, rgb(0, 0, 0));
        }

        // One-line preview: strip newlines and truncate.
        let preview: String = entry.text.chars().take(60).collect();
        let label = format!("{}. {}", i + 1, preview.replace('\n', " "));
        let _ = TextOutW(hdc, 12, top + 6, &wide_null(&label));
    }
}

/// Replace the clipboard contents with `text`.
unsafe fn set_clipboard_text(text: &str) {
    if OpenClipboard(None).is_err() {
        return;
    }
    let _ = EmptyClipboard();
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * 2;
    if let Ok(hglob) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
        let ptr = GlobalLock(hglob);
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
            let _ = GlobalUnlock(hglob);
            let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hglob.0)));
        }
    }
    let _ = CloseClipboard();
}

/// Simulate Ctrl+V so the chosen text is pasted into the active app.
unsafe fn simulate_ctrl_v() {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_CONTROL,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    ..Default::default()
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x56), // 'V'
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    ..Default::default()
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x56),
                    dwFlags: KEYEVENTF_KEYUP,
                    ..Default::default()
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_CONTROL,
                    dwFlags: KEYEVENTF_KEYUP,
                    ..Default::default()
                },
            },
        },
    ];
    let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
}

/// Convert a Rust string to a NUL-terminated UTF-16 vector.
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Convert a string literal to a `PCWSTR`. The returned pointer points to a
/// leaked allocation so it stays valid for the lifetime of the program;
/// acceptable because this is only used for a few class/window names at
/// startup.
fn wide(s: &str) -> PCWSTR {
    let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let ptr = Box::leak(v.into_boxed_slice()).as_ptr();
    PCWSTR(ptr)
}

/// Build a `COLORREF` from red, green, blue components.
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}
