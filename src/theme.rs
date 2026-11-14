//! Shared dark palette, typography, and optional Windows glass backdrop.

use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{s, w};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

#[repr(C)]
struct AccentPolicy {
    state: i32,
    flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

pub const BACKGROUND: COLORREF = rgb(31, 32, 36);
pub const SURFACE: COLORREF = rgb(40, 42, 47);
pub const INPUT: COLORREF = rgb(48, 50, 56);
pub const HIGHLIGHT: COLORREF = rgb(61, 64, 72);
pub const ACCENT: COLORREF = rgb(93, 139, 230);
pub const TEXT: COLORREF = rgb(239, 240, 243);
pub const MUTED_TEXT: COLORREF = rgb(164, 167, 175);
pub const BORDER: COLORREF = rgb(79, 82, 90);
pub const SEPARATOR: COLORREF = rgb(69, 72, 79);

static GLASS_ENABLED: AtomicBool = AtomicBool::new(false);
static BODY_FONT: Lazy<isize> = Lazy::new(|| create_font(15, FW_MEDIUM));
static HEADING_FONT: Lazy<isize> = Lazy::new(|| create_font(15, FW_SEMIBOLD));
static BACKGROUND_BRUSH: Lazy<isize> = Lazy::new(|| unsafe { CreateSolidBrush(BACKGROUND).0 as isize });
static SURFACE_BRUSH: Lazy<isize> = Lazy::new(|| unsafe { CreateSolidBrush(SURFACE).0 as isize });
static INPUT_BRUSH: Lazy<isize> = Lazy::new(|| unsafe { CreateSolidBrush(INPUT).0 as isize });

pub fn apply_backdrop(hwnd: HWND) -> bool {
    unsafe {
        let dark_mode = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let mut policy = AccentPolicy {
            state: 4,
            flags: 2,
            gradient_color: 0xD6232428,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttributeData {
            attribute: 19,
            data: &mut policy as *mut _ as _,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let enabled = GetModuleHandleW(w!("user32.dll"))
            .ok()
            .and_then(|module| GetProcAddress(module, s!("SetWindowCompositionAttribute")))
            .map(|function| {
                let function: SetWindowCompositionAttributeFn = std::mem::transmute(function);
                function(hwnd, &mut data).as_bool()
            })
            .unwrap_or(false);
        GLASS_ENABLED.store(enabled, Ordering::Release);
        enabled
    }
}

pub fn glass_enabled() -> bool {
    GLASS_ENABLED.load(Ordering::Acquire)
}

pub fn background_brush() -> HBRUSH {
    HBRUSH(*BACKGROUND_BRUSH as _)
}

pub fn surface_brush() -> HBRUSH {
    HBRUSH(*SURFACE_BRUSH as _)
}

pub fn input_brush() -> HBRUSH {
    HBRUSH(*INPUT_BRUSH as _)
}

pub fn body_font() -> HGDIOBJ {
    HGDIOBJ(*BODY_FONT as _)
}

pub fn heading_font() -> HGDIOBJ {
    HGDIOBJ(*HEADING_FONT as _)
}

pub const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

fn create_font(height: i32, weight: FONT_WEIGHT) -> isize {
    let face = "Segoe UI".encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            windows::core::PCWSTR(face.as_ptr()),
        )
        .0 as isize
    }
}