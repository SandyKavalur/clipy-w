//! Native editor for snippet folders and snippets.

use crate::snippets::{Snippet, SnippetFolder, SnippetStore};
use crate::theme;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: &str = "ClipyW_SnippetEditor";
const FOLDER_COMBO: u16 = 101;
const FOLDER_NAME: u16 = 102;
const ADD_FOLDER: u16 = 103;
const RENAME_FOLDER: u16 = 104;
const DELETE_FOLDER: u16 = 105;
const SNIPPET_COMBO: u16 = 106;
const ADD_SNIPPET: u16 = 107;
const DELETE_SNIPPET: u16 = 108;
const TITLE_EDIT: u16 = 109;
const TEXT_EDIT: u16 = 110;
const SAVE_SNIPPET: u16 = 111;
const STATUS_TEXT: u16 = 112;
const STATUS_PREVIOUS: u16 = 113;

const CBN_SELCHANGE: usize = 1;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;

struct Controls {
    folders: isize,
    folder_name: isize,
    snippets: isize,
    title: isize,
    text: isize,
    status: isize,
    status_previous: isize,
}

struct EditorState {
    store: Arc<Mutex<SnippetStore>>,
    controls: Option<Controls>,
    selected_folder: Option<usize>,
    selected_snippet: Option<usize>,
}

static EDITOR_WINDOW: AtomicIsize = AtomicIsize::new(0);
static EDITOR_STATE: Lazy<Mutex<Option<EditorState>>> = Lazy::new(|| Mutex::new(None));

pub unsafe fn show(store: Arc<Mutex<SnippetStore>>) -> Result<()> {
    let active = EDITOR_WINDOW.load(Ordering::Acquire);
    if active != 0 {
        let hwnd = HWND(active as _);
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = SetForegroundWindow(hwnd);
            return Ok(());
        }
        let _ = EDITOR_WINDOW.compare_exchange(active, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    *EDITOR_STATE.lock().unwrap() = Some(EditorState {
        store,
        controls: None,
        selected_folder: None,
        selected_snippet: None,
    });

    let instance = GetModuleHandleW(None)?;
    let class_name = wide_null(CLASS_NAME);
    let wc = WNDCLASSW {
        lpfnWndProc: Some(editor_wndproc),
        hInstance: HINSTANCE(instance.0),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hbrBackground: theme::background_brush(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 && GetLastError() != WIN32_ERROR(1410) {
        *EDITOR_STATE.lock().unwrap() = None;
        return Err(Error::from_win32());
    }

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW,
        PCWSTR(class_name.as_ptr()),
        PCWSTR(wide_null("Edit Snippets").as_ptr()),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        760,
        530,
        None,
        None,
        Some(HINSTANCE(instance.0)),
        None,
    )?;
    if hwnd.0.is_null() {
        *EDITOR_STATE.lock().unwrap() = None;
        return Err(Error::from_win32());
    }

    EDITOR_WINDOW.store(hwnd.0 as isize, Ordering::Release);
    let _ = theme::apply_backdrop(hwnd);
    centre_window(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

unsafe extern "system" fn editor_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            create_controls(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u16;
            let notification = (wparam.0 >> 16) & 0xffff;
            match id {
                FOLDER_COMBO if notification == CBN_SELCHANGE => change_folder(),
                SNIPPET_COMBO if notification == CBN_SELCHANGE => change_snippet(),
                ADD_FOLDER => add_folder(),
                RENAME_FOLDER => rename_folder(),
                DELETE_FOLDER => delete_folder(),
                ADD_SNIPPET => add_snippet(),
                DELETE_SNIPPET => delete_snippet(),
                SAVE_SNIPPET => save_selected_snippet(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DRAWITEM => draw_button(lparam),
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as _);
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_ok() {
                FillRect(hdc, &rect, theme::background_brush());
            }
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wparam.0 as _);
            SetTextColor(hdc, theme::MUTED_TEXT);
            let control_id = GetDlgCtrlID(HWND(lparam.0 as _));
            if control_id == STATUS_TEXT as i32 || control_id == STATUS_PREVIOUS as i32 {
                SetBkMode(hdc, OPAQUE);
                SetBkColor(hdc, theme::BACKGROUND);
                return LRESULT(theme::background_brush().0 as isize);
            }
            SetBkMode(hdc, TRANSPARENT);
            let brush = if theme::glass_enabled() {
                HBRUSH(GetStockObject(NULL_BRUSH).0)
            } else {
                theme::background_brush()
            };
            LRESULT(brush.0 as isize)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let hdc = HDC(wparam.0 as _);
            SetTextColor(hdc, theme::TEXT);
            SetBkColor(hdc, theme::INPUT);
            LRESULT(theme::input_brush().0 as isize)
        }
        WM_CLOSE => {
            save_selected_snippet();
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            EDITOR_WINDOW.store(0, Ordering::Release);
            *EDITOR_STATE.lock().unwrap() = None;
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_controls(parent: HWND) {
    let Ok(module) = GetModuleHandleW(None) else {
        return;
    };
    let instance = HINSTANCE(module.0);
    let common = WS_CHILD | WS_VISIBLE;
    create_control(parent, instance, "STATIC", "Folder", common, 16, 14, 100, 20, 0);
    let folders = create_control(
        parent,
        instance,
        "COMBOBOX",
        "",
        WINDOW_STYLE(common.0 | 0x0003 | WS_VSCROLL.0),
        16,
        36,
        210,
        240,
        FOLDER_COMBO,
    );
    let folder_name = create_control(
        parent,
        instance,
        "EDIT",
        "",
        WINDOW_STYLE(common.0 | WS_BORDER.0 | 0x0080),
        240,
        36,
        210,
        26,
        FOLDER_NAME,
    );
    create_control(parent, instance, "BUTTON", "Add", common, 462, 36, 80, 28, ADD_FOLDER);
    create_control(parent, instance, "BUTTON", "Rename", common, 548, 36, 82, 28, RENAME_FOLDER);
    create_control(parent, instance, "BUTTON", "Delete", common, 636, 36, 88, 28, DELETE_FOLDER);

    create_control(parent, instance, "STATIC", "Snippet", common, 16, 82, 100, 20, 0);
    let snippets = create_control(
        parent,
        instance,
        "COMBOBOX",
        "",
        WINDOW_STYLE(common.0 | 0x0003 | WS_VSCROLL.0),
        16,
        104,
        320,
        240,
        SNIPPET_COMBO,
    );
    create_control(parent, instance, "BUTTON", "Add new snippet", common, 350, 104, 120, 28, ADD_SNIPPET);
    create_control(parent, instance, "BUTTON", "Delete snippet", common, 478, 104, 140, 28, DELETE_SNIPPET);

    create_control(parent, instance, "STATIC", "Title", common, 16, 154, 80, 20, 0);
    let title = create_control(
        parent,
        instance,
        "EDIT",
        "",
        WINDOW_STYLE(common.0 | WS_BORDER.0 | 0x0080),
        16,
        176,
        708,
        28,
        TITLE_EDIT,
    );
    create_control(parent, instance, "STATIC", "Text", common, 16, 220, 80, 20, 0);
    let text = create_control(
        parent,
        instance,
        "EDIT",
        "",
        WINDOW_STYLE(common.0 | WS_BORDER.0 | WS_VSCROLL.0 | 0x0004 | 0x0040 | 0x1000),
        16,
        242,
        708,
        190,
        TEXT_EDIT,
    );
    let status = create_control(parent, instance, "STATIC", "", common, 16, 442, 520, 20, STATUS_TEXT);
    let status_previous = create_control(
        parent,
        instance,
        "STATIC",
        "",
        common,
        16,
        464,
        520,
        20,
        STATUS_PREVIOUS,
    );
    create_control(parent, instance, "BUTTON", "Save snippet", common, 600, 444, 124, 32, SAVE_SNIPPET);

    let mut state = EDITOR_STATE.lock().unwrap();
    let Some(state) = state.as_mut() else {
        return;
    };
    state.controls = Some(Controls {
        folders: folders.0 as isize,
        folder_name: folder_name.0 as isize,
        snippets: snippets.0 as isize,
        title: title.0 as isize,
        text: text.0 as isize,
        status: status.0 as isize,
        status_previous: status_previous.0 as isize,
    });
    let folder_count = state.store.lock().unwrap().folders.len();
    state.selected_folder = (folder_count > 0).then_some(0);
    populate_folders(state);
    state.selected_snippet = None;
    populate_snippets(state);
    load_selected(state);
}

unsafe fn create_control(
    parent: HWND,
    instance: HINSTANCE,
    class: &str,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: u16,
) -> HWND {
    let is_button = class == "BUTTON";
    let class = wide_null(class);
    let text = wide_null(text);
    let menu = if id == 0 { None } else { Some(HMENU(id as _)) };
    let style = if is_button {
        WINDOW_STYLE(style.0 | BS_OWNERDRAW as u32)
    } else {
        style
    };
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        PCWSTR(class.as_ptr()),
        PCWSTR(text.as_ptr()),
        style,
        x,
        y,
        width,
        height,
        Some(parent),
        menu,
        Some(instance),
        None,
    )
    .unwrap_or_default();
    if !hwnd.0.is_null() {
        let font = theme::body_font();
        let _ = SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    hwnd
}

unsafe fn draw_button(lparam: LPARAM) -> LRESULT {
    let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
    let is_primary = matches!(item.CtlID as u16, ADD_FOLDER | ADD_SNIPPET | SAVE_SNIPPET);
    let pressed = item.itemState.0 & 1 != 0;
    let color = if is_primary {
        if pressed {
            theme::rgb(73, 113, 190)
        } else {
            theme::ACCENT
        }
    } else if pressed {
        theme::HIGHLIGHT
    } else {
        theme::SURFACE
    };
    let brush = CreateSolidBrush(color);
    let pen = CreatePen(PS_SOLID, 1, color);
    let old_brush = SelectObject(item.hDC, HGDIOBJ(brush.0));
    let old_pen = SelectObject(item.hDC, HGDIOBJ(pen.0));
    let old_font = SelectObject(item.hDC, theme::heading_font());
    let rect = item.rcItem;
    let _ = RoundRect(
        item.hDC,
        rect.left + 1,
        rect.top + 1,
        rect.right - 1,
        rect.bottom - 1,
        10,
        10,
    );
    let _ = SelectObject(item.hDC, old_brush);
    let _ = SelectObject(item.hDC, old_pen);
    let _ = SelectObject(item.hDC, old_font);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));

    SetBkMode(item.hDC, TRANSPARENT);
    SetTextColor(item.hDC, theme::TEXT);
    let mut label = wide_null(&get_text(item.hwndItem));
    let mut text_rect = rect;
    let _ = DrawTextW(
        item.hDC,
        &mut label,
        &mut text_rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    LRESULT(1)
}

unsafe fn populate_folders(state: &EditorState) {
    let Some(controls) = &state.controls else {
        return;
    };
    let folders = control(controls.folders);
    let _ = SendMessageW(folders, CB_RESETCONTENT, None, None);
    let names: Vec<String> = state
        .store
        .lock()
        .unwrap()
        .folders
        .iter()
        .map(|folder| folder.name.clone())
        .collect();
    for name in names {
        combo_add(folders, &name);
    }
    if let Some(index) = state.selected_folder {
        let _ = SendMessageW(
            folders,
            CB_SETCURSEL,
            Some(WPARAM(index)),
            None,
        );
    }
    let name = state
        .selected_folder
        .and_then(|index| state.store.lock().unwrap().folders.get(index).map(|folder| folder.name.clone()))
        .unwrap_or_default();
    set_text(control(controls.folder_name), &name);
}

unsafe fn populate_snippets(state: &EditorState) {
    let Some(controls) = &state.controls else {
        return;
    };
    let snippets = control(controls.snippets);
    let _ = SendMessageW(snippets, CB_RESETCONTENT, None, None);
    let titles: Vec<String> = state
        .selected_folder
        .and_then(|folder| {
            state.store.lock().unwrap().folders.get(folder).map(|folder| {
                folder.snippets.iter().map(|snippet| snippet.title.clone()).collect()
            })
        })
        .unwrap_or_default();
    for title in titles {
        combo_add(snippets, &title);
    }
    if let Some(index) = state.selected_snippet {
        let _ = SendMessageW(
            snippets,
            CB_SETCURSEL,
            Some(WPARAM(index)),
            None,
        );
    }
}

unsafe fn load_selected(state: &EditorState) {
    let Some(controls) = &state.controls else {
        return;
    };
    let snippet = state.selected_folder.and_then(|folder_index| {
        state.selected_snippet.and_then(|snippet_index| {
            state
                .store
                .lock()
                .unwrap()
                .folders
                .get(folder_index)
                .and_then(|folder| folder.snippets.get(snippet_index))
                .cloned()
        })
    });
    let has_selection = snippet.is_some();
    let title_control = control(controls.title);
    let text_control = control(controls.text);
    let _ = EnableWindow(title_control, has_selection);
    let _ = EnableWindow(text_control, has_selection);
    set_text(
        title_control,
        snippet.as_ref().map(|snippet| snippet.title.as_str()).unwrap_or(""),
    );
    set_text(
        text_control,
        snippet.as_ref().map(|snippet| snippet.text.as_str()).unwrap_or(""),
    );
}

unsafe fn save_selected(state: &mut EditorState) -> bool {
    save_selected_with_status(state, true)
}

unsafe fn save_selected_quietly(state: &mut EditorState) -> bool {
    save_selected_with_status(state, false)
}

unsafe fn save_selected_with_status(state: &mut EditorState, show_saved: bool) -> bool {
    let (Some(folder_index), Some(snippet_index), Some(controls)) = (
        state.selected_folder,
        state.selected_snippet,
        state.controls.as_ref(),
    ) else {
        return true;
    };
    let title = get_text(control(controls.title));
    let text = get_text(control(controls.text));
    {
        let mut store = state.store.lock().unwrap();
        let Some(snippet) = store
            .folders
            .get_mut(folder_index)
            .and_then(|folder| folder.snippets.get_mut(snippet_index))
        else {
            return true;
        };
        snippet.title = if title.trim().is_empty() {
            "Untitled snippet".to_owned()
        } else {
            title
        };
        snippet.text = text;
        if let Err(error) = store.save() {
            set_status(state, &format!("Could not save snippets: {error}"));
            return false;
        }
    }
    if show_saved {
        set_status(state, "Snippet saved.");
    }
    true
}

unsafe fn change_folder() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let Some(folder_combo) = state.controls.as_ref().map(|controls| control(controls.folders)) else {
        return;
    };
    if !save_selected(state) {
        let previous = state.selected_folder.unwrap_or(0);
        let _ = SendMessageW(
            folder_combo,
            CB_SETCURSEL,
            Some(WPARAM(previous)),
            None,
        );
        return;
    }
    let selected = combo_selection(folder_combo);
    state.selected_folder = selected;
    state.selected_snippet = selected.and_then(|index| {
        (!state.store.lock().unwrap().folders[index].snippets.is_empty()).then_some(0)
    });
    populate_folders(state);
    populate_snippets(state);
    load_selected(state);
    set_status(state, "");
}

unsafe fn change_snippet() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let Some(snippet_combo) = state.controls.as_ref().map(|controls| control(controls.snippets)) else {
        return;
    };
    if !save_selected(state) {
        let previous = state.selected_snippet.unwrap_or(0);
        let _ = SendMessageW(
            snippet_combo,
            CB_SETCURSEL,
            Some(WPARAM(previous)),
            None,
        );
        return;
    }
    state.selected_snippet = combo_selection(snippet_combo);
    load_selected(state);
    set_status(state, "");
}

unsafe fn add_folder() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    if !save_selected(state) {
        return;
    }
    let name = state
        .controls
        .as_ref()
        .map(|controls| get_text(control(controls.folder_name)))
        .unwrap_or_default();
    if name.trim().is_empty() {
        set_status(state, "Enter a folder name first.");
        return;
    }
    let mut store = state.store.lock().unwrap();
    store.folders.push(SnippetFolder {
        name: name.trim().to_owned(),
        snippets: Vec::new(),
    });
    let index = store.folders.len() - 1;
    if let Err(error) = store.save() {
        set_status(state, &format!("Could not save snippets: {error}"));
        return;
    }
    drop(store);
    state.selected_folder = Some(index);
    state.selected_snippet = None;
    populate_folders(state);
    populate_snippets(state);
    load_selected(state);
    set_status(state, "Folder added.");
}

unsafe fn rename_folder() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    if !save_selected(state) {
        return;
    }
    let (Some(index), Some(controls)) = (state.selected_folder, state.controls.as_ref()) else {
        set_status(state, "Select a folder first.");
        return;
    };
    let name = get_text(control(controls.folder_name));
    if name.trim().is_empty() {
        set_status(state, "Folder name cannot be empty.");
        return;
    }
    let mut store = state.store.lock().unwrap();
    store.folders[index].name = name.trim().to_owned();
    if let Err(error) = store.save() {
        set_status(state, &format!("Could not save snippets: {error}"));
        return;
    }
    drop(store);
    populate_folders(state);
    set_status(state, "Folder renamed.");
}

unsafe fn delete_folder() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    if !save_selected(state) {
        return;
    }
    let Some(index) = state.selected_folder else {
        set_status(state, "Select a folder first.");
        return;
    };
    let mut store = state.store.lock().unwrap();
    store.folders.remove(index);
    if let Err(error) = store.save() {
        set_status(state, &format!("Could not save snippets: {error}"));
        return;
    }
    let count = store.folders.len();
    drop(store);
    state.selected_folder = (count > 0).then_some(index.min(count.saturating_sub(1)));
    state.selected_snippet = state.selected_folder.and_then(|folder| {
        (!state.store.lock().unwrap().folders[folder].snippets.is_empty()).then_some(0)
    });
    populate_folders(state);
    populate_snippets(state);
    load_selected(state);
    set_status(state, "Folder deleted.");
}

unsafe fn add_snippet() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    set_status(state, "");
    if !save_selected_quietly(state) {
        return;
    }
    let Some(folder) = state.selected_folder else {
        set_status(state, "Create or select a folder first.");
        return;
    };
    let mut store = state.store.lock().unwrap();
    store.folders[folder].snippets.push(Snippet {
        title: "New snippet".to_owned(),
        text: String::new(),
    });
    let index = store.folders[folder].snippets.len() - 1;
    if let Err(error) = store.save() {
        set_status(state, &format!("Could not save snippets: {error}"));
        return;
    }
    drop(store);
    state.selected_snippet = Some(index);
    populate_snippets(state);
    load_selected(state);
    set_status(state, "Snippet added. Edit its title and text, then save.");
}

unsafe fn delete_snippet() {
    let mut guard = EDITOR_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let (Some(folder), Some(snippet)) = (state.selected_folder, state.selected_snippet) else {
        set_status(state, "Select a snippet first.");
        return;
    };
    let mut store = state.store.lock().unwrap();
    store.folders[folder].snippets.remove(snippet);
    if let Err(error) = store.save() {
        set_status(state, &format!("Could not save snippets: {error}"));
        return;
    }
    let count = store.folders[folder].snippets.len();
    drop(store);
    state.selected_snippet = (count > 0).then_some(snippet.min(count.saturating_sub(1)));
    populate_snippets(state);
    load_selected(state);
    set_status(state, "Snippet deleted.");
}

unsafe fn save_selected_snippet() {
    if let Some(state) = EDITOR_STATE.lock().unwrap().as_mut() {
        let _ = save_selected(state);
        if let Some(folder) = state.selected_folder {
            populate_snippets(state);
            if let Some(controls) = &state.controls {
                let _ = SendMessageW(
                    control(controls.snippets),
                    CB_SETCURSEL,
                    Some(WPARAM(state.selected_snippet.unwrap_or(0))),
                    None,
                );
            }
            let _ = folder;
        }
    }
}

unsafe fn set_status(state: &EditorState, message: &str) {
    if let Some(controls) = &state.controls {
        let current = control(controls.status);
        let previous = control(controls.status_previous);
        if message.is_empty() {
            set_text(current, "");
            set_text(previous, "");
            return;
        }
        let current_message = get_text(current);
        if !current_message.is_empty() && current_message != message {
            set_text(previous, &current_message);
        }
        set_text(current, message);
    }
}

unsafe fn combo_selection(combo: HWND) -> Option<usize> {
    let selected = SendMessageW(combo, CB_GETCURSEL, None, None).0;
    (selected >= 0).then_some(selected as usize)
}

unsafe fn combo_add(combo: HWND, value: &str) {
    let value = wide_null(value);
    let _ = SendMessageW(
        combo,
        CB_ADDSTRING,
        None,
        Some(LPARAM(value.as_ptr() as isize)),
    );
}

unsafe fn get_text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut value = vec![0u16; length + 1];
    let read = GetWindowTextW(hwnd, &mut value).max(0) as usize;
    String::from_utf16_lossy(&value[..read])
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide_null(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

unsafe fn centre_window(hwnd: HWND) {
    let mut area = RECT::default();
    if SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some(&mut area as *mut _ as *mut _),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
    .is_ok()
    {
        let width = 760;
        let height = 530;
        let x = area.left + (area.right - area.left - width) / 2;
        let y = area.top + (area.bottom - area.top - height) / 2;
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_SHOWWINDOW,
        );
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn control(handle: isize) -> HWND {
    HWND(handle as _)
}

