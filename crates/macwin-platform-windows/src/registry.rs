//! String values under `HKEY_CURRENT_USER` (the login `Run` key).

use std::ptr;

use macwin_platform::PlatformError;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(what: &str, code: u32) -> PlatformError {
    PlatformError::Backend(format!("{what} failed with Win32 error {code}"))
}

/// The string value `name` of `HKCU\subkey`, or `None` when the key or the
/// value does not exist.
pub fn read_string(subkey: &str, name: &str) -> Result<Option<String>, PlatformError> {
    let (key, value) = (wide(subkey), wide(name));
    let mut buf: Vec<u16> = vec![0; 260];
    loop {
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: both names are NUL-terminated; `bytes` is the buffer size in
        // bytes and RegGetValueW writes at most that much (NUL included).
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        match r {
            ERROR_SUCCESS => {
                let len = (bytes as usize / 2).min(buf.len());
                let text = &buf[..len];
                let end = text.iter().position(|&c| c == 0).unwrap_or(len);
                return Ok(Some(String::from_utf16_lossy(&text[..end])));
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            ERROR_MORE_DATA if buf.len() < 32_768 => buf.resize(bytes as usize / 2 + 1, 0),
            code => return Err(failed("RegGetValueW", code)),
        }
    }
}

/// Sets the string value `name` of `HKCU\subkey`, creating the key.
pub fn write_string(subkey: &str, name: &str, data: &str) -> Result<(), PlatformError> {
    let (key, value, data) = (wide(subkey), wide(name), wide(data));
    let mut hkey: HKEY = ptr::null_mut();
    // SAFETY: plain registry calls with NUL-terminated strings; the opened key
    // is closed below.
    unsafe {
        let r = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            ptr::null(),
            &mut hkey,
            ptr::null_mut(),
        );
        if r != ERROR_SUCCESS {
            return Err(failed("RegCreateKeyExW", r));
        }
        let r = RegSetValueExW(
            hkey,
            value.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        );
        RegCloseKey(hkey);
        if r != ERROR_SUCCESS {
            return Err(failed("RegSetValueExW", r));
        }
    }
    Ok(())
}

/// Deletes the value `name` of `HKCU\subkey`; a missing value is fine.
pub fn delete_value(subkey: &str, name: &str) -> Result<(), PlatformError> {
    let (key, value) = (wide(subkey), wide(name));
    // SAFETY: both names are NUL-terminated.
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        code => Err(failed("RegDeleteKeyValueW", code)),
    }
}

/// Deletes `HKCU\subkey` with everything below it; a missing key is fine.
pub fn delete_tree(subkey: &str) -> Result<(), PlatformError> {
    let key = wide(subkey);
    // SAFETY: the name is NUL-terminated.
    match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        code => Err(failed("RegDeleteTreeW", code)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_under_a_scratch_key() {
        let key = format!(r"Software\macwin-tests\registry-{}", std::process::id());
        assert_eq!(read_string(&key, "v").unwrap(), None);
        write_string(&key, "v", r#""C:\Program Files\é\macwin-tray.exe""#).unwrap();
        assert_eq!(
            read_string(&key, "v").unwrap().as_deref(),
            Some(r#""C:\Program Files\é\macwin-tray.exe""#)
        );
        let long = "x".repeat(1000);
        write_string(&key, "v", &long).unwrap();
        assert_eq!(read_string(&key, "v").unwrap(), Some(long));
        delete_value(&key, "v").unwrap();
        delete_value(&key, "v").unwrap();
        assert_eq!(read_string(&key, "v").unwrap(), None);
        delete_tree(&key).unwrap();
        delete_tree(&key).unwrap();
    }
}
