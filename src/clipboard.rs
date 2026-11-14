//! Clipboard monitoring via the modern clipboard-format-listener API.
//!
//! A hidden window is registered with `AddClipboardFormatListener`.  Whenever
//! the clipboard changes Windows posts `WM_CLIPBOARDUPDATE` to that window; we
//! read the supported formats (text, images, rich text) and store them in the
//! history database.  This avoids the legacy clipboard-viewer chain entirely
//! (no WM_DRAWCLIPBOARD forwarding, no WM_CHANGECBCHAIN repair).

use crate::config::MAX_ITEM_BYTES;
use crate::history::{ClipboardFormat, ClipboardFormatId, ClipboardPayload, History};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Shell::*;
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
                if let Ok(Some(payload)) = read_clipboard_payload() {
                    if should_record(&payload) {
                        history.push_payload(&payload);
                    }
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Well-known clipboard format IDs, mirrored from `Win32.Graphics.Gdi` so the
/// code can compare them without importing the whole GDI module into scope.
const CF_TEXT_ID: u32 = 1;
const CF_BITMAP_ID: u32 = 2;
const CF_DIB_ID: u32 = 8;
const CF_UNICODETEXT_ID: u32 = 13;
const CF_DIBV5_ID: u32 = 17;
/// Explorer's "copy file" puts a file-drop list on the clipboard instead of
/// image pixels.  `CF_HDROP` lives in the shell module but is a plain u32.
const CF_HDROP_ID: u32 = 15;

/// One image file referenced by a CF_HDROP list, with the name the user sees.
struct DroppedImageFile {
    path: String,
    label: String,
}

/// Copy supported clipboard formats into owned memory while the clipboard is open.
unsafe fn read_clipboard_payload() -> Result<Option<ClipboardPayload>> {
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
        return Ok(None);
    }
    let result = (|| -> Result<Option<ClipboardPayload>> {
        let mut ids = Vec::new();
        let mut previous = 0;
        loop {
            let current = EnumClipboardFormats(previous);
            if current == 0 {
                break;
            }
            previous = current;
            ids.push(current);
        }
        ids.sort_unstable();
        ids.dedup();

        let mut formats = Vec::new();
        let mut preview = String::new();
        let mut has_image = false;
        let mut has_rich_text = false;
        let mut total_bytes = 0usize;

        for id in ids {
            if id == CF_BITMAP_ID {
                // A DIB is the portable representation of a device bitmap: it
                // round-trips through the database unchanged, and every app
                // that understands CF_BITMAP also understands CF_DIB.  Convert
                // eagerly here because the bitmap handle is only valid while
                // the clipboard is open.
                if let Some(bytes) = bitmap_to_dib() {
                    if !add_format(&mut formats, &mut total_bytes, CF_DIB_ID, None, bytes) {
                        return Ok(None);
                    }
                    has_image = true;
                }
                continue;
            }
            if id == CF_HDROP_ID {
                // Copying a file in Explorer puts a drop list on the clipboard,
                // not pixels.  Load the referenced image file ourselves so the
                // entry can still be pasted as an image later.
                let files = dropped_image_files();
                for (index, file) in files.iter().enumerate() {
                    let label = if files.len() > 1 {
                        format!("{} ({}/{})", file.label, index + 1, files.len())
                    } else {
                        file.label.clone()
                    };
                    match load_image_file_as_dib(&file.path) {
                        Some(bytes) => {
                            if !add_format(&mut formats, &mut total_bytes, CF_DIB_ID, None, bytes) {
                                return Ok(None);
                            }
                            has_image = true;
                            if preview.is_empty() {
                                preview = format!("[Image] {label}");
                            }
                        }
                        None => {
                            // Not an image (or unreadable): fall back to the
                            // path as text so the copy is not silently lost.
                            if preview.is_empty() {
                                preview = format!("[Files] {}", file.label);
                            }
                            let mut bytes = file
                                .path
                                .encode_utf16()
                                .flat_map(u16::to_le_bytes)
                                .collect::<Vec<u8>>();
                            bytes.extend_from_slice(&[0, 0]);
                            let _ = add_format(&mut formats, &mut total_bytes, CF_UNICODETEXT_ID, None, bytes);
                        }
                    }
                }
                continue;
            }
            let registered_name = if id >= 0xC000 {
                clipboard_format_name(id)
            } else {
                None
            };
            let is_builtin = matches!(id, CF_TEXT_ID | CF_UNICODETEXT_ID | CF_DIB_ID | CF_DIBV5_ID);
            let Some(name) = registered_name.as_deref() else {
                if !is_builtin {
                    continue;
                }
                let Some(bytes) = read_global_format(id) else {
                    continue;
                };
                if bytes.is_empty() {
                    continue;
                }
                if id == CF_UNICODETEXT_ID {
                    preview = decode_utf16_clipboard(&bytes);
                } else if id == CF_TEXT_ID && preview.is_empty() {
                    preview = String::from_utf8_lossy(&bytes)
                        .trim_end_matches('\0')
                        .to_owned();
                } else if matches!(id, CF_DIB_ID | CF_DIBV5_ID) {
                    has_image = true;
                }
                if !add_format(&mut formats, &mut total_bytes, id, None, bytes) {
                    return Ok(None);
                }
                continue;
            };

            let lower = name.to_ascii_lowercase();
            let is_image = ["png", "jpeg", "jfif", "jpg", "gif", "tiff", "heic", "heif"]
                .iter()
                .any(|token| lower.contains(token));
            let is_rich = lower.contains("html format")
                || lower.contains("rich text format")
                || lower == "rtf";
            if !is_image && !is_rich {
                continue;
            }
            let Some(bytes) = read_global_format(id) else {
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            if !add_format(&mut formats, &mut total_bytes, id, Some(name), bytes) {
                return Ok(None);
            }
            has_image |= is_image;
            has_rich_text |= is_rich;
        }

        if formats.is_empty() {
            return Ok(None);
        }
        if preview.trim().is_empty() {
            preview = if has_image {
                "[Image]".to_owned()
            } else if has_rich_text {
                "[Formatted text]".to_owned()
            } else {
                return Ok(None);
            };
        }
        Ok(Some(ClipboardPayload { preview, formats }))
    })();
    let _ = CloseClipboard();
    result
}

/// Append one captured format, enforcing the per-item size cap.  Returns
/// `false` when the cap is exceeded so the caller can drop the whole item
/// rather than persist it partially.
unsafe fn add_format(
    formats: &mut Vec<ClipboardFormat>,
    total_bytes: &mut usize,
    id: u32,
    registered_name: Option<&str>,
    bytes: Vec<u8>,
) -> bool {
    let next = total_bytes.saturating_add(bytes.len());
    if next > MAX_ITEM_BYTES {
        return false;
    }
    *total_bytes = next;
    let format_id = match registered_name {
        Some(name) => ClipboardFormatId::Registered(name.to_owned()),
        None => ClipboardFormatId::Windows(id),
    };
    formats.push(ClipboardFormat { id: format_id, bytes });
    true
}

/// Convert the clipboard's CF_BITMAP into a CF_DIB byte stream.
///
/// Returns `None` when no bitmap is available or the conversion fails, in
/// which case the caller simply skips the image rather than recording it.
unsafe fn bitmap_to_dib() -> Option<Vec<u8>> {
    let handle = GetClipboardData(CF_BITMAP_ID).ok()?;
    let bitmap = HBITMAP(handle.0);
    if bitmap.0.is_null() {
        return None;
    }

    let mut info: BITMAP = BITMAP::default();
    if GetObjectW(
        HGDIOBJ(bitmap.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut info as *mut _ as *mut _),
    ) == 0
    {
        return None;
    }
    if info.bmWidth <= 0 || info.bmHeight <= 0 || info.bmBitsPixel == 0 {
        return None;
    }

    let screen_dc = GetDC(None);
    if screen_dc.0.is_null() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(Some(screen_dc));
    let _ = ReleaseDC(None, screen_dc);
    if mem_dc.0.is_null() {
        return None;
    }

    // 32bpp keeps every source bitmap lossless and avoids palette bookkeeping.
    let height_abs = info.bmHeight.unsigned_abs();
    let Some(pixel_bytes) = dib_pixel_size(info.bmWidth, info.bmHeight) else {
        let _ = DeleteDC(mem_dc);
        return None;
    };
    let mut pixels = vec![0u8; pixel_bytes];

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: info.bmWidth,
            biHeight: info.bmHeight,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD::default(); 1],
    };

    let lines = GetDIBits(
        mem_dc,
        bitmap,
        0,
        height_abs,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    let _ = DeleteDC(mem_dc);
    if lines == 0 {
        return None;
    }

    // GetDIBits filled in biSizeImage and biClrUsed on the header we passed,
    // so read the header back out of the BITMAPINFO for a faithful DIB.
    let header_len = std::mem::size_of::<BITMAPINFOHEADER>();
    let header_bytes = std::slice::from_raw_parts(
        &bmi.bmiHeader as *const BITMAPINFOHEADER as *const u8,
        header_len,
    );
    let mut result = Vec::with_capacity(header_len + pixel_bytes);
    result.extend_from_slice(header_bytes);
    result.extend_from_slice(&pixels);
    Some(result)
}

/// Read the CF_HDROP list on the open clipboard into (path, file name) pairs.
///
/// Only the first file is loaded as an image today; the rest are recorded as
/// text paths so a multi-file copy is never silently lost.
unsafe fn dropped_image_files() -> Vec<DroppedImageFile> {
    let Ok(handle) = GetClipboardData(CF_HDROP_ID) else {
        return Vec::new();
    };
    let query = HDROP(handle.0);
    let count = DragQueryFileW(query, 0xFFFFFFFF, None);
    if count == 0 {
        return Vec::new();
    }
    let mut files = Vec::with_capacity(count.min(32) as usize);
    for index in 0..count.min(32) {
        let mut buf = vec![0u16; 520];
        let written = DragQueryFileW(query, index, Some(&mut buf));
        if written == 0 {
            continue;
        }
        buf.truncate(written as usize);
        let path = String::from_utf16_lossy(&buf);
        let label = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        files.push(DroppedImageFile { path, label });
    }
    files
}

/// Decode an image file into a 32bpp BGRA CF_DIB byte stream via WIC.
///
/// Returns `None` when COM/WIC is unavailable or the file is not a decodable
/// image, letting the caller fall back to storing the path as text.
unsafe fn load_image_file_as_dib(path: &str) -> Option<Vec<u8>> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let decoder = factory
        .CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        )
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;

    let mut width = 0u32;
    let mut height = 0u32;
    frame.GetSize(&mut width, &mut height).ok()?;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let converter: IWICFormatConverter = factory.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;

    let stride = width.checked_mul(4)?;
    let row_bytes = stride as usize;
    let pixel_bytes = row_bytes.checked_mul(height as usize)?;
    if pixel_bytes > MAX_ITEM_BYTES {
        return None;
    }
    let mut pixels = vec![0u8; pixel_bytes];
    converter
        .CopyPixels(std::ptr::null(), stride, pixels.as_mut_slice())
        .ok()?;

    let header_len = std::mem::size_of::<BITMAPINFOHEADER>();
    let mut result = Vec::with_capacity(header_len + pixel_bytes);
    let header = BITMAPINFOHEADER {
        biSize: header_len as u32,
        biWidth: width as i32,
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: pixel_bytes as u32,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let header_bytes = std::slice::from_raw_parts(&header as *const BITMAPINFOHEADER as *const u8, header_len);
    result.extend_from_slice(header_bytes);
    result.extend_from_slice(&pixels);
    Some(result)
}

/// Number of pixel bytes a 32bpp DIB needs for the given bitmap dimensions.
///
/// `height` may be negative (a top-down DIB); the sign is preserved in the
/// DIB header but does not affect the buffer size.  Returns `None` when the
/// dimensions are invalid or the buffer would exceed the per-item cap.
fn dib_pixel_size(width: i32, height: i32) -> Option<usize> {
    if width <= 0 || height == 0 {
        return None;
    }
    let row_width = (width as u32).saturating_mul(32).div_ceil(32) * 4;
    let pixels = (row_width as usize).saturating_mul(height.unsigned_abs() as usize);
    (pixels <= MAX_ITEM_BYTES).then_some(pixels)
}

unsafe fn read_global_format(format: u32) -> Option<Vec<u8>> {
    let handle = GetClipboardData(format).ok()?;
    let global = HGLOBAL(handle.0);
    let size = GlobalSize(global);
    if size == 0 || size > MAX_ITEM_BYTES {
        return None;
    }
    let ptr = GlobalLock(global);
    if ptr.is_null() {
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
    let _ = GlobalUnlock(global);
    Some(bytes)
}

unsafe fn clipboard_format_name(format: u32) -> Option<String> {
    let mut name = vec![0u16; 128];
    let length = GetClipboardFormatNameW(format, &mut name);
    (length > 0).then(|| String::from_utf16_lossy(&name[..length as usize]))
}

fn decode_utf16_clipboard(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>().0.iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}

fn should_record(payload: &ClipboardPayload) -> bool {
    !payload.formats.is_empty()
        && payload.formats.iter().map(|format| format.bytes.len()).sum::<usize>() <= MAX_ITEM_BYTES
}

/// Restore all stored formats so the receiving app can choose its preferred
/// representation.  Returns `false` when the clipboard could not be opened or
/// emptied, or when a format failed to set; the caller can then fall back to
/// plain text so the user still gets something on the clipboard.
pub unsafe fn set_clipboard_payload(payload: &ClipboardPayload) -> bool {
    if OpenClipboard(None).is_err() {
        return false;
    }
    if EmptyClipboard().is_err() {
        let _ = CloseClipboard();
        return false;
    }
    let mut all_ok = true;
    for format in &payload.formats {
        let id = match &format.id {
            ClipboardFormatId::Windows(id) => *id,
            ClipboardFormatId::Registered(name) => {
                let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                RegisterClipboardFormatW(PCWSTR(name.as_ptr()))
            }
        };
        if id == 0 || format.bytes.is_empty() {
            continue;
        }
        let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, format.bytes.len()) else {
            all_ok = false;
            continue;
        };
        let destination = GlobalLock(global);
        if destination.is_null() {
            let _ = GlobalFree(Some(global));
            all_ok = false;
            continue;
        }
        std::ptr::copy_nonoverlapping(format.bytes.as_ptr(), destination as *mut u8, format.bytes.len());
        let _ = GlobalUnlock(global);
        if SetClipboardData(id, Some(HANDLE(global.0))).is_err() {
            let _ = GlobalFree(Some(global));
            all_ok = false;
        }
    }
    let _ = CloseClipboard();
    all_ok
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The system clipboard is a process-wide resource: serialise the tests
    /// that touch it so they cannot race each other.
    static CLIPBOARD_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn dib_pixel_size_accounts_for_row_padding() {
        // A 3-pixel-wide 32bpp row pads to 12 bytes (4-byte alignment).
        assert_eq!(dib_pixel_size(3, 1), Some(12));
        // 100x50: each row is 400 bytes (already aligned), 50 rows.
        assert_eq!(dib_pixel_size(100, 50), Some(20_000));
        // Negative height (top-down DIB) uses the absolute value.
        assert_eq!(dib_pixel_size(100, -50), Some(20_000));
        // Zero or negative dimensions are invalid.
        assert_eq!(dib_pixel_size(0, 10), None);
        assert_eq!(dib_pixel_size(10, 0), None);
    }

    #[test]
    fn add_format_tracks_total_and_rejects_oversized_items() {
        let mut formats = Vec::new();
        let mut total = 0usize;

        let small = vec![7u8; 16];
        assert!(unsafe { add_format(&mut formats, &mut total, CF_DIB_ID, None, small.clone()) });
        assert_eq!(total, 16);
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].id, ClipboardFormatId::Windows(CF_DIB_ID));

        // Registering under a name records it as a registered format.
        let rich = vec![9u8; 4];
        assert!(unsafe {
            add_format(&mut formats, &mut total, 0, Some("HTML Format"), rich)
        });
        assert_eq!(formats[1].id, ClipboardFormatId::Registered("HTML Format".to_owned()));
        assert_eq!(total, 20);

        // Pushing past the cap rejects the whole item and leaves state intact.
        let huge = vec![0u8; MAX_ITEM_BYTES];
        assert!(!unsafe { add_format(&mut formats, &mut total, CF_DIB_ID, None, huge) });
        assert_eq!(formats.len(), 2, "oversized format must not be appended");
        assert_eq!(total, 20, "total must not count the rejected format");
    }

    #[test]
    fn should_record_accepts_representable_payloads_only() {
        let empty = ClipboardPayload { preview: String::new(), formats: vec![] };
        assert!(!should_record(&empty));

        let text = ClipboardPayload::from_text("hello");
        assert!(should_record(&text));
        assert_eq!(text.formats.len(), 1);
        assert_eq!(text.formats[0].id, ClipboardFormatId::Windows(CF_UNICODETEXT_ID));
        // UTF-16 code units plus the NUL terminator.
        assert_eq!(text.formats[0].bytes.len(), ("hello".len() + 1) * 2);
    }

    #[test]
    fn text_and_html_roundtrip_through_the_real_clipboard() {
        let _guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
        let utf16: Vec<u8> = "hello"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain([0, 0])
            .collect();
        let payload = ClipboardPayload {
            preview: "hello".to_owned(),
            formats: vec![
                ClipboardFormat {
                    id: ClipboardFormatId::Windows(CF_UNICODETEXT_ID),
                    bytes: utf16.clone(),
                },
                ClipboardFormat {
                    id: ClipboardFormatId::Registered("HTML Format".to_owned()),
                    bytes: b"<b>hello</b>".to_vec(),
                },
            ],
        };
        assert!(unsafe { set_clipboard_payload(&payload) }, "setting the clipboard failed");

        let captured = unsafe { read_clipboard_payload() }
            .unwrap()
            .expect("payload should be captured");
        assert_eq!(captured.preview, "hello");
        let text = captured
            .formats
            .iter()
            .find(|format| matches!(format.id, ClipboardFormatId::Windows(id) if id == CF_UNICODETEXT_ID))
            .expect("CF_UNICODETEXT should be captured");
        assert_eq!(text.bytes, utf16);
        let html = captured
            .formats
            .iter()
            .find(|format| matches!(&format.id, ClipboardFormatId::Registered(name) if name == "HTML Format"))
            .expect("HTML Format should be captured");
        assert_eq!(html.bytes, b"<b>hello</b>");
    }

    #[test]
    fn cf_bitmap_is_captured_as_a_dib() {
        let _guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
        unsafe {
            OpenClipboard(None).unwrap();
            EmptyClipboard().unwrap();
            let screen = GetDC(None);
            let bitmap = CreateCompatibleBitmap(screen, 4, 4);
            ReleaseDC(None, screen);
            SetClipboardData(CF_BITMAP_ID, Some(HANDLE(bitmap.0))).unwrap();
            CloseClipboard().unwrap();
        }

        let captured = unsafe { read_clipboard_payload() }
            .unwrap()
            .expect("payload should be captured");
        assert_eq!(captured.preview, "[Image]");
        let dib = captured
            .formats
            .iter()
            .find(|format| matches!(format.id, ClipboardFormatId::Windows(id) if id == CF_DIB_ID))
            .expect("CF_BITMAP should be converted to CF_DIB");
        // BITMAPINFOHEADER layout: size, width, height, planes, bit count...
        assert_eq!(u32::from_le_bytes(dib.bytes[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib.bytes[4..8].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(dib.bytes[14..16].try_into().unwrap()), 32);
        // 4x4 at 32bpp: header + 4 rows of 16 bytes.
        assert_eq!(dib.bytes.len(), 40 + 4 * 16);
        assert_eq!(
            u32::from_le_bytes(dib.bytes[20..24].try_into().unwrap()),
            64,
            "biSizeImage should describe the pixel block"
        );
    }

    #[test]
    fn explorer_file_copy_is_captured_as_an_image() {
        let _guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
        unsafe {
            OpenClipboard(None).unwrap();
            EmptyClipboard().unwrap();
            // Write a real PNG to a temp file, then publish its path as a
            // CF_HDROP list, exactly like Explorer's "Copy" does.
            let path = std::env::temp_dir().join("clipy-w-hdrop-test.png");
            let png = include_bytes!("../tests/assets/1x1.png");
            std::fs::write(&path, png).unwrap();
            let wide: Vec<u16> = path
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // DROPFILES header: pFiles offset, POINT, fNC, fWide.
            let header_len = 20usize;
            let byte_len = header_len + wide.len() * 2;
            let hglobal = GlobalAlloc(GMEM_MOVEABLE, byte_len).unwrap();
            let dst = GlobalLock(hglobal) as *mut u8;
            std::ptr::write_bytes(dst, 0, byte_len);
            let p_files = header_len as u32;
            let f_wide = 1u32;
            std::ptr::copy_nonoverlapping(&p_files as *const u32 as *const u8, dst, 4);
            std::ptr::copy_nonoverlapping(&f_wide as *const u32 as *const u8, dst.add(16), 4);
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, dst.add(header_len), wide.len() * 2);
            let _ = GlobalUnlock(hglobal);
            assert!(SetClipboardData(CF_HDROP_ID, Some(HANDLE(hglobal.0))).is_ok());
            CloseClipboard().unwrap();
        }

        let captured = unsafe { read_clipboard_payload() }
            .unwrap()
            .expect("a dropped image file should be captured");
        assert!(captured.preview.starts_with("[Image]"), "preview was: {}", captured.preview);
        let dib = captured
            .formats
            .iter()
            .find(|format| matches!(format.id, ClipboardFormatId::Windows(id) if id == CF_DIB_ID))
            .expect("CF_HDROP should be converted to a CF_DIB payload");
        assert_eq!(u32::from_le_bytes(dib.bytes[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib.bytes[4..8].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(dib.bytes[8..12].try_into().unwrap()), -1);
        assert_eq!(u16::from_le_bytes(dib.bytes[14..16].try_into().unwrap()), 32);
    }

    #[test]
    fn registered_image_format_is_captured() {
        let _guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
        let payload = ClipboardPayload {
            preview: "[Image]".to_owned(),
            formats: vec![ClipboardFormat {
                id: ClipboardFormatId::Registered("PNG Image".to_owned()),
                bytes: vec![0x89, b'P', b'N', b'G'],
            }],
        };
        assert!(unsafe { set_clipboard_payload(&payload) }, "setting the clipboard failed");

        let captured = unsafe { read_clipboard_payload() }
            .unwrap()
            .expect("payload should be captured");
        assert_eq!(captured.preview, "[Image]");
        assert_eq!(captured.formats.len(), 1);
        assert_eq!(captured.formats[0].bytes, vec![0x89, b'P', b'N', b'G']);
        assert!(matches!(
            captured.formats[0].id,
            ClipboardFormatId::Registered(ref name) if name == "PNG Image"
        ));
    }
}


