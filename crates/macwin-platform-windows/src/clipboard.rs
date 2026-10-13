//! Win32 clipboard. `change_count` uses the clipboard sequence number and
//! never opens the clipboard; `inspect` sizes HGLOBAL-based formats only.

use std::ptr;
use std::thread;
use std::time::Duration;

use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardFormat, ClipboardInfo, PlatformError,
};
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

/// The system clipboard.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinClipboard;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn origin_format() -> u32 {
    let name = wide(CLIPBOARD_ORIGIN_FORMAT);
    // SAFETY: NUL-terminated UTF-16 buffer.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Closes the clipboard when dropped.
struct Open;

impl Open {
    /// Another process may hold the clipboard briefly: retry for ~200 ms.
    fn new(owner: HWND) -> Result<Open, PlatformError> {
        for _ in 0..20 {
            // SAFETY: `owner` is null or a window of this thread.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Open);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(PlatformError::Backend("the clipboard is busy".into()))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: we opened it.
        unsafe { CloseClipboard() };
    }
}

/// Names of the predefined formats (registered ones have their own names).
fn standard_name(f: u32) -> Option<&'static str> {
    Some(match f {
        1 => "CF_TEXT",
        2 => "CF_BITMAP",
        3 => "CF_METAFILEPICT",
        7 => "CF_OEMTEXT",
        8 => "CF_DIB",
        13 => "CF_UNICODETEXT",
        14 => "CF_ENHMETAFILE",
        15 => "CF_HDROP",
        16 => "CF_LOCALE",
        17 => "CF_DIBV5",
        _ => return None,
    })
}

fn format_name(f: u32) -> String {
    if let Some(n) = standard_name(f) {
        return n.into();
    }
    let mut buf = [0u16; 256];
    // SAFETY: the buffer length is passed.
    let len = unsafe { GetClipboardFormatNameW(f, buf.as_mut_ptr(), buf.len() as i32) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        format!("#{f}")
    }
}

/// Handles of these formats are GDI objects, not HGLOBALs.
fn is_gdi_handle(f: u32) -> bool {
    matches!(f, 2 | 3 | 9 | 14)
}

impl ClipboardBackend for WinClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        // SAFETY: plain query; does not open the clipboard.
        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
    }

    fn inspect(&self) -> Result<ClipboardInfo, PlatformError> {
        let change_count = self.change_count()?;
        let origin = origin_format();
        let _open = Open::new(ptr::null_mut())?;
        let mut formats = Vec::new();
        let mut ours = false;
        let mut f = 0;
        loop {
            // SAFETY: the clipboard is open on this thread.
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            ours |= f == origin;
            let size = if is_gdi_handle(f) {
                None
            } else {
                // SAFETY: the clipboard is open and `f` is an HGLOBAL format,
                // so a non-null handle can be sized.
                unsafe {
                    let h = GetClipboardData(f);
                    (!h.is_null()).then(|| GlobalSize(h))
                }
            };
            formats.push(ClipboardFormat {
                name: format_name(f),
                size,
            });
        }
        Ok(ClipboardInfo {
            change_count,
            formats,
            ours,
        })
    }

    fn read_text(&self) -> Result<Option<String>, PlatformError> {
        let format = u32::from(CF_UNICODETEXT);
        // SAFETY: plain query.
        if unsafe { IsClipboardFormatAvailable(format) } == 0 {
            return Ok(None);
        }
        let _open = Open::new(ptr::null_mut())?;
        // SAFETY: the clipboard is open; CF_UNICODETEXT is an HGLOBAL holding
        // NUL-terminated UTF-16 within GlobalSize bytes.
        unsafe {
            let h = GetClipboardData(format);
            if h.is_null() {
                return Ok(None);
            }
            let p = GlobalLock(h) as *const u16;
            if p.is_null() {
                return Ok(None);
            }
            let max = GlobalSize(h) / 2;
            let slice = std::slice::from_raw_parts(p, max);
            let len = slice.iter().position(|&c| c == 0).unwrap_or(max);
            let text = String::from_utf16_lossy(&slice[..len]);
            GlobalUnlock(h);
            Ok(Some(text))
        }
    }

    fn write_text(&self, text: &str) -> Result<(), PlatformError> {
        let data = wide(text);
        let class = wide("STATIC");
        // SAFETY: a message-only window owns the clipboard while we write
        // (EmptyClipboard with a NULL owner would make SetClipboardData fail);
        // the HGLOBALs are handed to the system on success and freed on failure.
        unsafe {
            let owner = CreateWindowExW(
                0,
                class.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if owner.is_null() {
                return Err(PlatformError::Backend(
                    "cannot create the clipboard owner window".into(),
                ));
            }
            let result = (|| {
                let _open = Open::new(owner)?;
                EmptyClipboard();
                let bytes = data.len() * 2;
                let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if h.is_null() {
                    return Err(PlatformError::Backend("GlobalAlloc failed".into()));
                }
                let p = GlobalLock(h) as *mut u16;
                if p.is_null() {
                    GlobalFree(h);
                    return Err(PlatformError::Backend("GlobalLock failed".into()));
                }
                ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                GlobalUnlock(h);
                if SetClipboardData(u32::from(CF_UNICODETEXT), h).is_null() {
                    GlobalFree(h);
                    return Err(PlatformError::Backend("SetClipboardData failed".into()));
                }
                let marker = GlobalAlloc(GMEM_MOVEABLE, 1);
                if !marker.is_null() && SetClipboardData(origin_format(), marker).is_null() {
                    GlobalFree(marker);
                }
                Ok(())
            })();
            DestroyWindow(owner);
            result
        }
    }
}
