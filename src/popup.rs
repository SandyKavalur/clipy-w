//! Root menu and adjacent item window for clipboard history and snippets.

use crate::config::{
    popup_height, AUTO_PASTE, MAX_STORED, POPUP_HEADER, POPUP_ROW, POPUP_WIDTH, RANGE_WIDTH,
    VISIBLE_ITEMS,
};
use crate::history::{Entry, History};
use crate::snippet_editor;
use crate::snippets::SnippetStore;
use crate::startup;
use crate::theme;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
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

const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
const SNIPPET_TOP_MARGIN: i32 = 8;
const POPUP_DISMISS_TIMER_ID: usize = 0xC1;

#[derive(Clone, Copy)]
enum MenuAction {
    HistoryPage(usize),
    SnippetFolder(usize),
    EditSnippets,
    ToggleStartup,
    Quit,
}

struct MenuRow {
    action: MenuAction,
    top: i32,
}

struct MenuLayout {
    rows: Vec<MenuRow>,
    snippets_heading_top: i32,
    page_count: usize,
    folder_count: usize,
}

struct PopupState {
    entries: Vec<Entry>,
    snippets: Arc<Mutex<SnippetStore>>,
    startup_enabled: bool,
    selected_row: usize,
    selected_item: usize,
    item_scroll_start: usize,
    child: Option<MenuAction>,
}

static STATE: Lazy<Mutex<Option<PopupState>>> = Lazy::new(|| Mutex::new(None));
static ROOT_WINDOW: AtomicIsize = AtomicIsize::new(0);
static ITEMS_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Show the root menu. Selecting a history range or snippet folder opens a neighboring list.
///
/// # Safety
/// Must be called from a thread with a message pump.
pub unsafe fn show(history: &History, snippets: Arc<Mutex<SnippetStore>>) -> Result<()> {
    let entries = history.recent(MAX_STORED);
    let startup_enabled = startup::is_enabled().unwrap_or(false);
    let existing = ROOT_WINDOW.load(Ordering::Acquire);
    if existing != 0 {
        let hwnd = HWND(existing as _);
        if IsWindow(Some(hwnd)).as_bool() {
            if let Some(state) = STATE.lock().unwrap().as_mut() {
                state.entries = entries;
                state.snippets = snippets;
                state.startup_enabled = startup_enabled;
            }
            let _ = InvalidateRect(Some(hwnd), None, true);
            let items = ITEMS_WINDOW.load(Ordering::Acquire);
            if items != 0 {
                let _ = InvalidateRect(Some(HWND(items as _)), None, true);
            }
            let _ = SetForegroundWindow(hwnd);
            return Ok(());
        }
        let _ = ROOT_WINDOW.compare_exchange(existing, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    *STATE.lock().unwrap() = Some(PopupState {
        entries,
        snippets,
        startup_enabled,
        selected_row: 0,
        selected_item: 0,
        item_scroll_start: 0,
        child: None,
    });

    let instance = GetModuleHandleW(None)?;
    let class = wide("ClipyW_Popup");
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(popup_wndproc),
        hInstance: HINSTANCE(instance.0),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hbrBackground: theme::surface_brush(),
        lpszClassName: class,
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 && GetLastError() != WIN32_ERROR(ERROR_CLASS_ALREADY_EXISTS) {
        return Err(Error::from_win32());
    }

    let height = root_height();
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        class,
        wide("Clipy-W"),
        WS_POPUP,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        RANGE_WIDTH,
        height,
        None,
        None,
        Some(HINSTANCE(instance.0)),
        None,
    )?;
    if hwnd.0.is_null() {
        return Err(Error::from_win32());
    }

    ROOT_WINDOW.store(hwnd.0 as isize, Ordering::Release);
    round_window(hwnd, RANGE_WIDTH, height);
    centre_root_window(hwnd, height);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = UpdateWindow(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetActiveWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
    let _ = SetTimer(Some(hwnd), POPUP_DISMISS_TIMER_ID, 100, None);
    Ok(())
}

fn page_count(entry_count: usize) -> usize {
    entry_count.div_ceil(VISIBLE_ITEMS)
}

fn page_bounds(entry_count: usize, page: usize) -> Option<(usize, usize)> {
    let start = page.checked_mul(VISIBLE_ITEMS)?;
    if start >= entry_count {
        return None;
    }
    Some((start, (start + VISIBLE_ITEMS).min(entry_count)))
}

fn mouse_client_y(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) as u16 as i16) as i32
}

fn child_header_height(action: Option<MenuAction>) -> i32 {
    if matches!(action, Some(MenuAction::HistoryPage(_))) {
        POPUP_HEADER
    } else {
        0
    }
}

fn child_content_top(action: Option<MenuAction>) -> i32 {
    if child_header_height(action) > 0 {
        POPUP_HEADER
    } else if matches!(action, Some(MenuAction::SnippetFolder(_))) {
        SNIPPET_TOP_MARGIN
    } else {
        0
    }
}

fn menu_layout(state: &PopupState) -> MenuLayout {
    let pages = page_count(state.entries.len());
    let folder_count = state.snippets.lock().unwrap().folders.len();
    let mut rows = Vec::new();
    let mut top = POPUP_HEADER + 3;

    for page in 0..pages {
        rows.push(MenuRow {
            action: MenuAction::HistoryPage(page),
            top,
        });
        top += POPUP_ROW;
    }
    let snippets_heading_top = top + if pages > 0 { 8 } else { 0 };
    top = snippets_heading_top + POPUP_ROW;
    for folder in 0..folder_count {
        rows.push(MenuRow {
            action: MenuAction::SnippetFolder(folder),
            top,
        });
        top += POPUP_ROW;
    }
    if pages > 0 || folder_count > 0 {
        top += 9;
    }
    rows.push(MenuRow {
        action: MenuAction::EditSnippets,
        top,
    });
    top += POPUP_ROW + 4;
    rows.push(MenuRow {
        action: MenuAction::ToggleStartup,
        top,
    });
    top += POPUP_ROW + 4;
    rows.push(MenuRow {
        action: MenuAction::Quit,
        top,
    });
    MenuLayout {
        rows,
        snippets_heading_top,
        page_count: pages,
        folder_count,
    }
}

fn root_height() -> i32 {
    let state = STATE.lock().unwrap();
    state
        .as_ref()
        .and_then(|state| {
            menu_layout(state)
                .rows
                .last()
                .map(|row| row.top + POPUP_ROW + 8)
        })
        .unwrap_or(POPUP_HEADER + POPUP_ROW + 12)
}

fn child_items(state: &PopupState) -> Vec<(String, String)> {
    match state.child {
        Some(MenuAction::HistoryPage(page)) => {
            let Some((start, end)) = page_bounds(state.entries.len(), page) else {
                return Vec::new();
            };
            state.entries[start..end]
                .iter()
                .enumerate()
                .map(|(offset, entry)| (format!("{}", start + offset + 1), entry.text.clone()))
                .collect()
        }
        Some(MenuAction::SnippetFolder(folder)) => state
            .snippets
            .lock()
            .unwrap()
            .folders
            .get(folder)
            .map(|folder| {
                folder
                    .snippets
                    .iter()
                    .map(|snippet| (snippet.title.clone(), snippet.text.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn child_header(state: &PopupState) -> String {
    match state.child {
        Some(MenuAction::HistoryPage(page)) => page_bounds(state.entries.len(), page)
            .map(|(start, end)| format!("History {}-{}", start + 1, end))
            .unwrap_or_else(|| "History".to_owned()),
        Some(MenuAction::SnippetFolder(folder)) => state
            .snippets
            .lock()
            .unwrap()
            .folders
            .get(folder)
            .map(|folder| folder.name.clone())
            .unwrap_or_else(|| "Snippets".to_owned()),
        _ => String::new(),
    }
}

unsafe fn centre_root_window(hwnd: HWND, height: i32) {
    if let Some(area) = work_area() {
        let x = area.left + (area.right - area.left - RANGE_WIDTH) / 2;
        let y = area.top + (area.bottom - area.top - height) / 2;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            RANGE_WIDTH,
            height,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
    }
}

unsafe fn work_area() -> Option<RECT> {
    let mut rect = RECT::default();
    SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some(&mut rect as *mut _ as *mut _),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    )
    .ok()?;
    Some(rect)
}

unsafe fn open_child_window(parent: HWND, action: MenuAction) -> Result<()> {
    let item_count = {
        let mut guard = STATE.lock().unwrap();
        let Some(state) = guard.as_mut() else {
            return Ok(());
        };
        state.child = Some(action);
        state.selected_item = 0;
        state.item_scroll_start = 0;
        child_items(state).len()
    };
    let _ = InvalidateRect(Some(parent), None, true);

    let previous = ITEMS_WINDOW.swap(0, Ordering::AcqRel);
    if previous != 0 {
        let hwnd = HWND(previous as _);
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }

    let instance = GetModuleHandleW(None)?;
    let class = wide("ClipyW_Popup");
    let content_top = child_content_top(Some(action));
    let height = popup_height(item_count.max(1)) - (POPUP_HEADER - content_top);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        class,
        wide("Clipy-W items"),
        WS_POPUP,
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
    ITEMS_WINDOW.store(hwnd.0 as isize, Ordering::Release);
    round_window(hwnd, POPUP_WIDTH, height);

    if let Some(area) = work_area() {
        let mut parent_rect = RECT::default();
        if GetWindowRect(parent, &mut parent_rect).is_ok() {
            let x = if parent_rect.right + POPUP_WIDTH <= area.right {
                parent_rect.right
            } else {
                (parent_rect.left - POPUP_WIDTH).max(area.left)
            };
            let y = parent_rect.top.clamp(area.top, (area.bottom - height).max(area.top));
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                POPUP_WIDTH,
                height,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }
    }

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = UpdateWindow(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetActiveWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
    Ok(())
}

unsafe fn open_action(parent: HWND, action: MenuAction) {
    match action {
        MenuAction::Quit => {
            close_popup_windows();
            PostQuitMessage(0);
        }
        MenuAction::ToggleStartup => {
            let enabled = STATE
                .lock()
                .unwrap()
                .as_ref()
                .map(|state| !state.startup_enabled);
            if let Some(enabled) = enabled {
                match startup::set_enabled(enabled) {
                    Ok(()) => {
                        if let Some(state) = STATE.lock().unwrap().as_mut() {
                            state.startup_enabled = enabled;
                        }
                        let _ = InvalidateRect(Some(parent), None, true);
                    }
                    Err(error) => {
                        eprintln!("[Clipy-W] could not update startup setting: {error}");
                    }
                }
            }
        }
        MenuAction::EditSnippets => {
            let store = STATE
                .lock()
                .unwrap()
                .as_ref()
                .map(|state| state.snippets.clone());
            close_popup_windows();
            if let Some(store) = store {
                if let Err(error) = snippet_editor::show(store) {
                    eprintln!("[Clipy-W] failed to open snippet editor: {error}");
                }
            }
        }
        _ => {
            if let Err(error) = open_child_window(parent, action) {
                eprintln!("[Clipy-W] failed to show popup items: {error}");
            }
        }
    }
}

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
            let old_font = SelectObject(hdc, theme::body_font());
            paint_surface(hwnd, hdc);
            if hwnd.0 as isize == ROOT_WINDOW.load(Ordering::Acquire) {
                paint_root(hdc);
            } else {
                paint_child(hdc);
            }
            let _ = SelectObject(hdc, old_font);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            if hwnd.0 as isize == ROOT_WINDOW.load(Ordering::Acquire) {
                match vk {
                    VK_UP | VK_DOWN => {
                        if let Some(state) = STATE.lock().unwrap().as_mut() {
                            let count = menu_layout(state).rows.len();
                            if vk == VK_UP {
                                state.selected_row = state.selected_row.saturating_sub(1);
                            } else if state.selected_row + 1 < count {
                                state.selected_row += 1;
                            }
                            let _ = InvalidateRect(Some(hwnd), None, true);
                        }
                    }
                    VK_RIGHT | VK_RETURN => {
                        let action = STATE.lock().unwrap().as_ref().and_then(|state| {
                            menu_layout(state)
                                .rows
                                .get(state.selected_row)
                                .map(|row| row.action)
                        });
                        if let Some(action) = action {
                            open_action(hwnd, action);
                        }
                    }
                    VK_ESCAPE => close_popup_windows(),
                    _ => {}
                }
            } else {
                match vk {
                    VK_UP | VK_DOWN => {
                        if let Some(state) = STATE.lock().unwrap().as_mut() {
                            let count = child_items(state).len();
                            if vk == VK_UP {
                                state.selected_item = state.selected_item.saturating_sub(1);
                            } else if state.selected_item + 1 < count {
                                state.selected_item += 1;
                            }
                            if state.selected_item < state.item_scroll_start {
                                state.item_scroll_start = state.selected_item;
                            } else if state.selected_item >= state.item_scroll_start + VISIBLE_ITEMS {
                                state.item_scroll_start = state.selected_item + 1 - VISIBLE_ITEMS;
                            }
                            let _ = InvalidateRect(Some(hwnd), None, true);
                        }
                    }
                    VK_RETURN => {
                        let index = STATE.lock().unwrap().as_ref().map(|state| state.selected_item);
                        if let Some(index) = index {
                            choose_item(index);
                        }
                    }
                    VK_ESCAPE | VK_LEFT => {
                        let _ = DestroyWindow(hwnd);
                        let parent = ROOT_WINDOW.load(Ordering::Acquire);
                        if parent != 0 {
                            let _ = SetForegroundWindow(HWND(parent as _));
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = mouse_client_y(lparam);
            if hwnd.0 as isize == ROOT_WINDOW.load(Ordering::Acquire) {
                if y < POPUP_HEADER {
                    return LRESULT(0);
                }
                let action = {
                    let mut guard = STATE.lock().unwrap();
                    guard.as_mut().and_then(|state| {
                        let rows = menu_layout(state).rows;
                        let row_index = rows
                            .iter()
                            .position(|row| y >= row.top && y < row.top + POPUP_ROW)?;
                        state.selected_row = row_index;
                        rows.get(row_index).map(|row| row.action)
                    })
                };
                let _ = InvalidateRect(Some(hwnd), None, true);
                if let Some(action) = action {
                    open_action(hwnd, action);
                }
            } else {
                let state = STATE.lock().unwrap();
                let header_height = state
                    .as_ref()
                    .map(|state| child_content_top(state.child))
                    .unwrap_or(0);
                if y < header_height {
                    return LRESULT(0);
                }
                let row = ((y - header_height) / POPUP_ROW).max(0) as usize;
                let index = state.as_ref().and_then(|state| {
                    let index = state.item_scroll_start + row;
                    (index < child_items(state).len()).then_some(index)
                });
                drop(state);
                if let Some(index) = index {
                    choose_item(index);
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE if (wparam.0 & 0xffff) == WA_INACTIVE as usize => {
            let activated = lparam.0 as isize;
            let root = ROOT_WINDOW.load(Ordering::Acquire);
            let items = ITEMS_WINDOW.load(Ordering::Acquire);
            if activated != root && activated != items {
                close_popup_windows();
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            let next = wparam.0 as isize;
            let root = ROOT_WINDOW.load(Ordering::Acquire);
            let items = ITEMS_WINDOW.load(Ordering::Acquire);
            if next != root && next != items {
                close_popup_windows();
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == POPUP_DISMISS_TIMER_ID => {
            let foreground = GetForegroundWindow().0 as isize;
            let root = ROOT_WINDOW.load(Ordering::Acquire);
            let items = ITEMS_WINDOW.load(Ordering::Acquire);
            if foreground != root && foreground != items {
                close_popup_windows();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let value = hwnd.0 as isize;
            let was_root = ROOT_WINDOW
                .compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            let _ = ITEMS_WINDOW.compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire);
            if was_root {
                let _ = KillTimer(Some(hwnd), POPUP_DISMISS_TIMER_ID);
                let items = ITEMS_WINDOW.swap(0, Ordering::AcqRel);
                if items != 0 {
                    let _ = DestroyWindow(HWND(items as _));
                }
                *STATE.lock().unwrap() = None;
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_root(hdc: HDC) {
    let guard = STATE.lock().unwrap();
    let Some(state) = guard.as_ref() else {
        return;
    };
    paint_header(hdc, "History", RANGE_WIDTH);
    let layout = menu_layout(state);
    for (index, row) in layout.rows.iter().take(layout.page_count).enumerate() {
        let label = match row.action {
            MenuAction::HistoryPage(page) => page_bounds(state.entries.len(), page)
                .map(|(start, end)| format!("{} - {}  >", start + 1, end))
                .unwrap_or_default(),
            _ => String::new(),
        };
        draw_row(hdc, row.top, index == state.selected_row, &label, RANGE_WIDTH);
    }

    if layout.page_count > 0 {
        draw_separator(hdc, layout.snippets_heading_top - 4, RANGE_WIDTH);
    }
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, theme::MUTED_TEXT);
    let _ = TextOutW(
        hdc,
        12,
        layout.snippets_heading_top + 5,
        &wide_null("Snippets"),
    );

    let folder_start = layout.page_count;
    let edit_index = layout
        .rows
        .iter()
        .position(|row| matches!(row.action, MenuAction::EditSnippets))
        .unwrap();
    let startup_index = layout
        .rows
        .iter()
        .position(|row| matches!(row.action, MenuAction::ToggleStartup))
        .unwrap();
    let quit_index = layout
        .rows
        .iter()
        .position(|row| matches!(row.action, MenuAction::Quit))
        .unwrap();
    for index in folder_start..edit_index {
        let row = &layout.rows[index];
        let label = match row.action {
            MenuAction::SnippetFolder(folder) => state
                .snippets
                .lock()
                .unwrap()
                .folders
                .get(folder)
                .map(|folder| format!("{}  >", folder.name))
                .unwrap_or_default(),
            _ => String::new(),
        };
        draw_row(hdc, row.top, index == state.selected_row, &label, RANGE_WIDTH);
    }

    let edit_row = &layout.rows[edit_index];
    if layout.page_count > 0 || layout.folder_count > 0 {
        draw_separator(hdc, edit_row.top - 5, RANGE_WIDTH);
    }
    draw_row(
        hdc,
        edit_row.top,
        edit_index == state.selected_row,
        "Edit Snippets...",
        RANGE_WIDTH,
    );

    let startup_row = &layout.rows[startup_index];
    let startup_label = if state.startup_enabled {
        "[x] Start at sign-in"
    } else {
        "[ ] Start at sign-in"
    };
    draw_row(
        hdc,
        startup_row.top,
        startup_index == state.selected_row,
        startup_label,
        RANGE_WIDTH,
    );

    let quit_row = &layout.rows[quit_index];
    draw_separator(hdc, quit_row.top - 5, RANGE_WIDTH);
    draw_row(
        hdc,
        quit_row.top,
        quit_index == state.selected_row,
        "Quit Clipy-W",
        RANGE_WIDTH,
    );
}

unsafe fn paint_child(hdc: HDC) {
    let guard = STATE.lock().unwrap();
    let Some(state) = guard.as_ref() else {
        return;
    };
    let items = child_items(state);
    let content_top = child_content_top(state.child);
    if child_header_height(state.child) > 0 {
        let header = child_header(state);
        paint_header(hdc, &header, POPUP_WIDTH);
    }
    if items.is_empty() {
        let _ = TextOutW(hdc, 12, content_top + 6, &wide_null("No snippets yet"));
        return;
    }
    for (row, index) in (state.item_scroll_start..items.len().min(state.item_scroll_start + VISIBLE_ITEMS)).enumerate() {
        let (title, text) = &items[index];
        let preview: String = text
            .chars()
            .take(52)
            .map(|character| if character == '\n' || character == '\r' { ' ' } else { character })
            .collect();
        let label = match state.child {
            Some(MenuAction::HistoryPage(_)) => format!("{}. {}", title, preview),
            _ if preview.is_empty() => title.clone(),
            _ => format!("{}: {}", title, preview),
        };
        draw_row(hdc, content_top + row as i32 * POPUP_ROW, state.selected_item == index, &label, POPUP_WIDTH);
    }
}

unsafe fn paint_header(hdc: HDC, title: &str, width: i32) {
    let old_font = SelectObject(hdc, theme::body_font());
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, theme::MUTED_TEXT);
    let _ = TextOutW(hdc, 12, 9, &wide_null(title));
    draw_separator(hdc, POPUP_HEADER - 1, width);
    let _ = SelectObject(hdc, old_font);
}

unsafe fn draw_separator(hdc: HDC, y: i32, width: i32) {
    let line = RECT {
        left: 10,
        top: y,
        right: width - 10,
        bottom: y + 1,
    };
    let brush = CreateSolidBrush(theme::SEPARATOR);
    FillRect(hdc, &line, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

unsafe fn draw_row(hdc: HDC, top: i32, selected: bool, label: &str, width: i32) {
    if selected {
        let color = theme::HIGHLIGHT;
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, color);
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = RoundRect(hdc, 5, top + 2, width - 5, top + POPUP_ROW - 2, 10, 10);
        let _ = SelectObject(hdc, old_brush);
        let _ = SelectObject(hdc, old_pen);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, theme::TEXT);
    let _ = TextOutW(hdc, 15, top + 6, &wide_null(label));
}

unsafe fn paint_surface(hwnd: HWND, hdc: HDC) {
    let mut rect = RECT::default();
    if GetClientRect(hwnd, &mut rect).is_err() {
        return;
    }
    FillRect(hdc, &rect, theme::surface_brush());
    let pen = CreatePen(PS_SOLID, 1, theme::BORDER);
    let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
    let _ = RoundRect(hdc, 0, 0, rect.right, rect.bottom, 28, 28);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn round_window(hwnd: HWND, width: i32, height: i32) {
    let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, 28, 28);
    if SetWindowRgn(hwnd, Some(region), true) == 0 {
        let _ = DeleteObject(HGDIOBJ(region.0));
    }
}

unsafe fn choose_item(index: usize) {
    let text = STATE.lock().unwrap().as_ref().and_then(|state| {
        match state.child {
            Some(MenuAction::HistoryPage(page)) => {
                let (start, end) = page_bounds(state.entries.len(), page)?;
                (index < end - start).then(|| state.entries[start + index].text.clone())
            }
            Some(MenuAction::SnippetFolder(folder)) => state
                .snippets
                .lock()
                .unwrap()
                .folders
                .get(folder)
                .and_then(|folder| folder.snippets.get(index))
                .map(|snippet| snippet.text.clone()),
            _ => None,
        }
    });
    close_popup_windows();
    if let Some(text) = text {
        set_clipboard_text(&text);
        if AUTO_PASTE {
            std::thread::sleep(Duration::from_millis(80));
            simulate_ctrl_v();
        }
    }
}

unsafe fn close_popup_windows() {
    let items = ITEMS_WINDOW.swap(0, Ordering::AcqRel);
    let root = ROOT_WINDOW.swap(0, Ordering::AcqRel);
    if root != 0 {
        let _ = KillTimer(Some(HWND(root as _)), POPUP_DISMISS_TIMER_ID);
    }
    for value in [items, root] {
        if value != 0 {
            let hwnd = HWND(value as _);
            if IsWindow(Some(hwnd)).as_bool() {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

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
                    wVk: VIRTUAL_KEY(0x56),
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

fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide(text: &str) -> PCWSTR {
    let value = wide_null(text);
    PCWSTR(Box::leak(value.into_boxed_slice()).as_ptr())
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::LPARAM;
    use super::{
        child_content_top, child_header_height, mouse_client_y, page_bounds, page_count, MenuAction,
    };

    #[test]
    fn full_history_splits_into_three_groups() {
        assert_eq!(page_count(30), 3);
        assert_eq!(page_bounds(30, 0), Some((0, 10)));
        assert_eq!(page_bounds(30, 1), Some((10, 20)));
        assert_eq!(page_bounds(30, 2), Some((20, 30)));
    }

    #[test]
    fn partial_last_group_ends_at_history_length() {
        assert_eq!(page_count(12), 2);
        assert_eq!(page_bounds(12, 1), Some((10, 12)));
        assert_eq!(page_bounds(12, 2), None);
    }

    #[test]
    fn snippet_windows_omit_the_header_but_history_windows_keep_it() {
        assert_eq!(child_header_height(Some(MenuAction::SnippetFolder(0))), 0);
        assert_eq!(child_content_top(Some(MenuAction::SnippetFolder(0))), 8);
        assert_eq!(
            child_header_height(Some(MenuAction::HistoryPage(0))),
            crate::config::POPUP_HEADER
        );
        assert_eq!(
            child_content_top(Some(MenuAction::HistoryPage(0))),
            crate::config::POPUP_HEADER
        );
    }

    #[test]
    fn mouse_click_y_is_read_from_the_high_word() {
        let packed = (73u32 << 16) | 19u32;
        assert_eq!(mouse_client_y(LPARAM(packed as isize)), 73);

        let packed = ((-3i16 as u16 as u32) << 16) | 19u32;
        assert_eq!(mouse_client_y(LPARAM(packed as isize)), -3);
    }
}