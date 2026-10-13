//! Win32 clipboard. `change_count` uses the clipboard sequence number and
//! never opens the clipboard; `inspect` sizes HGLOBAL-based formats only.

use std::ptr;
use std::thread;
use std::time::Duration;

use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardFormat, ClipboardInfo, ClipboardSnapshot,
    LocalClipboard, PlatformError, WIN_CLOUD_FORMAT, WIN_SENSITIVE_FORMATS,
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

/// The id of a registered format (registering is idempotent system-wide).
fn registered(name: &str) -> u32 {
    let name = wide(name);
    // SAFETY: NUL-terminated UTF-16 buffer.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

fn origin_format() -> u32 {
    registered(CLIPBOARD_ORIGIN_FORMAT)
}

/// Format availability; does not open the clipboard.
fn available(format: u32) -> bool {
    // SAFETY: plain query.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

/// Plain text read from an open clipboard.
enum TextRead {
    Missing,
    TooLarge(usize),
    Text(String),
}

/// Reads CF_UNICODETEXT, refusing texts over `max_bytes` UTF-8 bytes
/// before converting them when the UTF-16 length already exceeds it.
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn read_open_text(max_bytes: usize) -> TextRead {
    // SAFETY: the clipboard is open (caller); CF_UNICODETEXT is an HGLOBAL
    // holding NUL-terminated UTF-16 within GlobalSize bytes.
    unsafe {
        let h = GetClipboardData(u32::from(CF_UNICODETEXT));
        if h.is_null() {
            return TextRead::Missing;
        }
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            return TextRead::Missing;
        }
        let max_units = GlobalSize(h) / 2;
        let units = std::slice::from_raw_parts(p, max_units);
        let len = units.iter().position(|&c| c == 0).unwrap_or(max_units);
        // Every UTF-16 unit is at least one UTF-8 byte.
        let read = if len > max_bytes {
            TextRead::TooLarge(len)
        } else {
            let text = String::from_utf16_lossy(&units[..len]);
            if text.len() > max_bytes {
                TextRead::TooLarge(text.len())
            } else {
                TextRead::Text(text)
            }
        };
        GlobalUnlock(h);
        read
    }
}

/// The DWORD of a format such as `CanUploadToCloudClipboard`.
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn read_open_dword(format: u32) -> Option<u32> {
    // SAFETY: the clipboard is open (caller); the handle is an HGLOBAL and
    // at least 4 bytes are checked before reading.
    unsafe {
        let h = GetClipboardData(format);
        if h.is_null() || GlobalSize(h) < 4 {
            return None;
        }
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let value = u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]);
        GlobalUnlock(h);
        Some(value)
    }
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
        if !available(u32::from(CF_UNICODETEXT)) {
            return Ok(None);
        }
        let _open = Open::new(ptr::null_mut())?;
        // SAFETY: the clipboard is open on this thread.
        Ok(match unsafe { read_open_text(usize::MAX) } {
            TextRead::Text(t) => Some(t),
            TextRead::Missing | TextRead::TooLarge(_) => None,
        })
    }

    fn snapshot(&self, max_text_bytes: usize) -> Result<ClipboardSnapshot, PlatformError> {
        let change_count = self.change_count()?;
        let content = if available(origin_format()) {
            LocalClipboard::Ours
        } else if WIN_SENSITIVE_FORMATS
            .iter()
            .any(|name| available(registered(name)))
        {
            LocalClipboard::Sensitive
        } else if !available(u32::from(CF_UNICODETEXT)) {
            LocalClipboard::NoText
        } else {
            let cloud = registered(WIN_CLOUD_FORMAT);
            let _open = Open::new(ptr::null_mut())?;
            // SAFETY: the clipboard is open on this thread.
            if available(cloud) && unsafe { read_open_dword(cloud) } == Some(0) {
                LocalClipboard::Sensitive
            } else {
                // SAFETY: the clipboard is open on this thread.
                match unsafe { read_open_text(max_text_bytes) } {
                    TextRead::Missing => LocalClipboard::NoText,
                    TextRead::TooLarge(bytes) => LocalClipboard::TooLarge { bytes },
                    TextRead::Text(t) => LocalClipboard::Text(t),
                }
            }
        };
        Ok(ClipboardSnapshot {
            change_count,
            content,
        })
    }

    fn write_text(&self, text: &str) -> Result<u64, PlatformError> {
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
            result?;
        }
        self.change_count()
    }
}
