//! Per-user Windows sign-in startup registration.

use std::io;
use windows::core::w;
use windows::Win32::System::Registry::*;

const RUN_KEY: windows::core::PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: windows::core::PCWSTR = w!("Clipy-W");

pub fn is_enabled() -> io::Result<bool> {
    unsafe {
        let mut key = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            KEY_QUERY_VALUE,
            &mut key,
        );
        if status.0 == 2 {
            return Ok(false);
        }
        check(status)?;

        let mut value_type = REG_VALUE_TYPE(0);
        let mut byte_count = 0;
        let status = RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            Some(&mut value_type),
            None,
            Some(&mut byte_count),
        );
        let _ = RegCloseKey(key);
        if status.0 == 2 {
            return Ok(false);
        }
        check(status)?;
        Ok(value_type == REG_SZ)
    }
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    unsafe {
        let mut key = HKEY::default();
        if enabled {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                w!(""),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            ))?;
        } else {
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                KEY_SET_VALUE,
                &mut key,
            );
            if status.0 == 2 {
                return Ok(());
            }
            check(status)?;
        }

        let status = if enabled {
            let executable = std::env::current_exe()?;
            let value: Vec<u16> = format!("\"{}\"", executable.display())
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let bytes = std::slice::from_raw_parts(
                value.as_ptr() as *const u8,
                value.len() * std::mem::size_of::<u16>(),
            );
            RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes))
        } else {
            RegDeleteValueW(key, VALUE_NAME)
        };
        let _ = RegCloseKey(key);
        if !enabled && status.0 == 2 {
            return Ok(());
        }
        check(status)
    }
}

fn check(status: windows::Win32::Foundation::WIN32_ERROR) -> io::Result<()> {
    if status.0 == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.0 as i32))
    }
}