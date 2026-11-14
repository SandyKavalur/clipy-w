//! Clipy-W — a Clipy-style clipboard manager for Windows.
//!
//! Registers a global Ctrl+Shift+H hotkey.  Pressing it pops up a window with
//! the ten most recent clipboard entries; pick one to paste it back into the
//! active application.  Clipboard changes are captured in the background and
//! persisted to a sled database under `%APPDATA%\clipy-w\history.db`.

mod clipboard;
mod config;
mod history;
mod popup;
mod snippet_editor;
mod snippets;
mod theme;

use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct AppState {
    history: Arc<history::History>,
    snippets: Arc<Mutex<snippets::SnippetStore>>,
}

fn main() -> Result<()> {
    let debug = std::env::var_os("CLIPY_W_DEBUG").is_some();
    let log = |msg: &str| {
        if debug {
            eprintln!("[clipy-w] {}", msg);
        }
    };

    let _legacy_mutex = unsafe {
        let mutex = CreateMutexW(None, true, wide("Local\\ClipYRS_SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            eprintln!("[Clipy-W] close the previous ClipYRS instance before launching this version.");
            return Ok(());
        }
        mutex
    };

    let _instance_mutex = unsafe {
        let mutex = CreateMutexW(None, true, wide("Local\\ClipyW_SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            eprintln!("[Clipy-W] another instance is already running.");
            return Ok(());
        }
        mutex
    };

    if let Err(error) = config::migrate_legacy_data() {
        eprintln!("[Clipy-W] failed to migrate existing ClipYRS data: {error}");
        std::process::exit(1);
    }

    let history = match history::History::open() {
        Ok(history) => Arc::new(history),
        Err(error) => {
            eprintln!("[Clipy-W] failed to open history database: {error}");
            eprintln!("[Clipy-W] another process may be using it; close it and try again.");
            std::process::exit(1);
        }
    };
    log("history opened");
    let snippets = match snippets::SnippetStore::load() {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(error) => {
            eprintln!("[Clipy-W] failed to load snippets: {error}");
            Arc::new(Mutex::new(snippets::SnippetStore::default()))
        }
    };
    let app_state = Arc::new(AppState {
        history: history.clone(),
        snippets: snippets.clone(),
    });

    unsafe {
        let instance = GetModuleHandleW(None)?;
        log("got module handle");
        let class = wide(&format!("{}_Main", config::APP_NAME));
        let wc = WNDCLASSW {
            lpfnWndProc: Some(main_wndproc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            wide(config::APP_NAME),
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
        log("main window created");

        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::into_raw(app_state) as _);

        // Prefer Win+Shift+V, falling back to Ctrl+Shift+V if unavailable.
        let test_mode = std::env::var_os("CLIPY_W_TEST").is_some();
        let hotkey_result = if test_mode {
            RegisterHotKey(
                Some(hwnd),
                config::HOTKEY_ID,
                HOT_KEY_MODIFIERS(0),
                config::TEST_HOTKEY_VK,
            )
        } else {
            match RegisterHotKey(
                Some(hwnd),
                config::HOTKEY_ID,
                MOD_WIN | MOD_SHIFT,
                config::HOTKEY_VK,
            ) {
                Ok(()) => {
                    log("Win+Shift+V hotkey registered");
                    Ok(())
                }
                Err(error) => {
                    log(&format!("Win+Shift+V unavailable: {error}; trying Ctrl+Shift+V"));
                    let fallback = RegisterHotKey(
                        Some(hwnd),
                        config::HOTKEY_ID,
                        MOD_CONTROL | MOD_SHIFT,
                        config::HOTKEY_VK,
                    );
                    if fallback.is_ok() {
                        log("Ctrl+Shift+V fallback hotkey registered");
                    }
                    fallback
                }
            }
        };
        if let Err(e) = hotkey_result {
            log(&format!("RegisterHotKey failed: {:?}", e));
        } else if test_mode {
            log("hotkey registered");
        }

        // Start clipboard monitoring.  The monitor must outlive this match:
        // binding it to a variable inside an arm would drop it at the end of
        // the arm, destroying its window and unregistering the listener.
        let monitor = clipboard::ClipboardMonitor::new(history.clone());
        match &monitor {
            Ok(_) => log("clipboard monitor started"),
            Err(e) => log(&format!("ClipboardMonitor::new failed: {:?}", e)),
        }

        // Manual test hook: show the popup once and exit, without needing the
        // global hotkey.  Lets the UI be exercised headlessly.
        if std::env::var_os("CLIPY_W_SHOW").is_some() {
            log("manual show requested");
            let _ = popup::show(&history, snippets.clone());
            log("popup closed, exiting");
            let _ = UnregisterHotKey(Some(hwnd), config::HOTKEY_ID);
            return Ok(());
        }

        // Main message loop.  PeekMessageW + WaitMessage is used (rather than
        // GetMessageW) so the loop can never wedge on a quit message that is
        // never posted, and so hidden-window messages are still dispatched.
        let mut msg = MSG::default();
        loop {
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                let _ = WaitMessage();
            }
        }

        let _ = UnregisterHotKey(Some(hwnd), config::HOTKEY_ID);
    }
    Ok(())
}

/// Window procedure for the hidden main window.
unsafe extern "system" fn main_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 == config::HOTKEY_ID as usize => {
            if std::env::var_os("CLIPY_W_DEBUG").is_some() {
                eprintln!("[clipy-w] WM_HOTKEY received");
            }
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const AppState;
            if !ptr.is_null() {
                let state = &*ptr;
                if let Err(error) = popup::show(&state.history, state.snippets.clone()) {
                    eprintln!("[Clipy-W] failed to show popup: {error}");
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Convert a string literal to a `PCWSTR`. The returned pointer points to a
/// leaked allocation so it stays valid for the lifetime of the program.
fn wide(s: &str) -> PCWSTR {
    let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let ptr = Box::leak(v.into_boxed_slice()).as_ptr();
    PCWSTR(ptr)
}
