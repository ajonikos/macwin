//! Win32 clipboard. `change_count` uses the clipboard sequence number and
//! never opens the clipboard; `inspect` sizes HGLOBAL-based formats only.
//!
//! Files travel as CF_HDROP (plus "Preferred DropEffect" = copy when
//! written), so an Explorer paste copies them. Images: the registered "PNG"
//! format is preferred (browsers and Office put it), otherwise CF_DIBV5 /
//! CF_DIB is converted to PNG (after the clipboard is closed again: the
//! conversion of a large bitmap takes long enough to lock other apps out);
//! images are written as "PNG" plus CF_DIB.

use std::path::PathBuf;
use std::ptr;
use std::thread;
use std::time::Duration;

use macwin_platform::image::{decode_dib, decode_png, encode_dib, encode_png};
use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardContent, ClipboardFormat, ClipboardInfo,
    ClipboardSnapshot, PlatformError, SnapshotLimits, WIN_CLOUD_FORMAT, WIN_SENSITIVE_FORMATS,
};
use tracing::debug;
use windows_sys::Win32::Foundation::{GlobalFree, HWND, POINT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{
    CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY,
};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

/// The registered PNG format of browsers and Office.
const PNG_FORMAT: &str = "PNG";
/// Tells Explorer whether pasting copies or moves.
const DROP_EFFECT_FORMAT: &str = "Preferred DropEffect";
/// Bitmaps above this size are not converted (a 8K 32-bit DIB is ~133 MB).
const MAX_DIB_BYTES: usize = 256 * 1024 * 1024;

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

/// A copy of an HGLOBAL format's bytes, if present and at most `max` long.
///
/// # Safety
/// The clipboard must be open on this thread and `format` HGLOBAL-based.
unsafe fn read_open_bytes(format: u32, max: usize) -> Option<Result<Vec<u8>, usize>> {
    // SAFETY: the clipboard is open (caller); the handle is an HGLOBAL of
    // GlobalSize bytes while locked.
    unsafe {
        let h = GetClipboardData(format);
        if h.is_null() {
            return None;
        }
        let size = GlobalSize(h);
        if size > max {
            return Some(Err(size));
        }
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let data = std::slice::from_raw_parts(p, size).to_vec();
        GlobalUnlock(h);
        Some(Ok(data))
    }
}

/// The DWORD of a format such as `CanUploadToCloudClipboard`.
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn read_open_dword(format: u32) -> Option<u32> {
    // SAFETY: as in `read_open_bytes`.
    let data = unsafe { read_open_bytes(format, 64) }?.ok()?;
    let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// The paths in CF_HDROP.
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn read_open_files() -> Vec<PathBuf> {
    // SAFETY: the clipboard is open (caller); CF_HDROP is an HDROP that
    // DragQueryFileW reads with the buffer sizes passed.
    unsafe {
        let h = GetClipboardData(u32::from(CF_HDROP)) as HDROP;
        if h.is_null() {
            return Vec::new();
        }
        let count = DragQueryFileW(h, u32::MAX, ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(h, i, ptr::null_mut(), 0);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
            if got > 0 {
                paths.push(PathBuf::from(String::from_utf16_lossy(
                    &buf[..got as usize],
                )));
            }
        }
        paths
    }
}

/// What the clipboard holds once it is closed again: bitmaps still need
/// converting.
enum Read {
    Content(ClipboardContent),
    Dib(Vec<u8>),
}

/// The clipboard image: PNG as is, or a copy of the bitmap (CF_DIBV5, which
/// keeps alpha, else CF_DIB; Windows synthesizes each from the other).
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn read_open_image(max_png: usize) -> Read {
    let png_format = registered(PNG_FORMAT);
    if available(png_format) {
        // SAFETY: open clipboard (caller), HGLOBAL format.
        match unsafe { read_open_bytes(png_format, max_png) } {
            Some(Ok(png)) => return Read::Content(ClipboardContent::Image(png)),
            Some(Err(size)) => {
                return Read::Content(ClipboardContent::TooLarge { bytes: size as u64 });
            }
            None => {}
        }
    }
    for dib_format in [u32::from(CF_DIBV5), u32::from(CF_DIB)] {
        if !available(dib_format) {
            continue;
        }
        // SAFETY: open clipboard (caller), HGLOBAL format.
        match unsafe { read_open_bytes(dib_format, MAX_DIB_BYTES) } {
            Some(Ok(dib)) => return Read::Dib(dib),
            Some(Err(size)) => {
                return Read::Content(ClipboardContent::TooLarge { bytes: size as u64 });
            }
            None => {}
        }
    }
    Read::Content(ClipboardContent::Unsupported)
}

/// Converts a bitmap read from the clipboard into PNG.
fn dib_to_png(dib: &[u8], max_png: usize) -> ClipboardContent {
    match decode_dib(dib).and_then(|img| encode_png(&img)) {
        Ok(png) if png.len() > max_png => ClipboardContent::TooLarge {
            bytes: png.len() as u64,
        },
        Ok(png) => ClipboardContent::Image(png),
        Err(e) => {
            debug!(error = %e, "cannot convert the bitmap");
            ClipboardContent::Unsupported
        }
    }
}

/// Copies `data` into a movable HGLOBAL and hands it to the clipboard.
///
/// # Safety
/// The clipboard must be open (and emptied) by this thread's owner window.
unsafe fn set_global(format: u32, data: &[u8]) -> Result<(), PlatformError> {
    // SAFETY: the allocation is `data.len()` bytes (at least 1); on success
    // the system owns it, on failure we free it.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
        if h.is_null() {
            return Err(PlatformError::Backend("GlobalAlloc failed".into()));
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            return Err(PlatformError::Backend("GlobalLock failed".into()));
        }
        ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        GlobalUnlock(h);
        if SetClipboardData(format, h).is_null() {
            GlobalFree(h);
            return Err(PlatformError::Backend(format!(
                "SetClipboardData({}) failed",
                format_name(format)
            )));
        }
        Ok(())
    }
}

/// Replaces the clipboard: opens it with a message-only owner window (a
/// NULL owner would make SetClipboardData fail after EmptyClipboard),
/// empties it, runs `fill`, adds the origin marker, and returns the
/// sequence number after closing.
fn replace(fill: impl FnOnce() -> Result<(), PlatformError>) -> Result<u64, PlatformError> {
    let class = wide("STATIC");
    // SAFETY: a message-only STATIC window with no parent menu or params;
    // destroyed below on every path.
    let owner = unsafe {
        CreateWindowExW(
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
        )
    };
    if owner.is_null() {
        return Err(PlatformError::Backend(
            "cannot create the clipboard owner window".into(),
        ));
    }
    let result = (|| {
        let _open = Open::new(owner)?;
        // SAFETY: the clipboard is open by `owner`.
        unsafe { EmptyClipboard() };
        fill()?;
        // SAFETY: still open; a failed marker only weakens echo detection.
        if unsafe { set_global(origin_format(), b"1") }.is_err() {
            debug!("the clipboard refused the origin marker");
        }
        Ok(())
    })();
    // SAFETY: our window.
    unsafe { DestroyWindow(owner) };
    result?;
    // SAFETY: plain query.
    Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
}

/// A DROPFILES block followed by the double-NUL-terminated wide paths.
fn hdrop(paths: &[PathBuf]) -> Vec<u8> {
    let header = std::mem::size_of::<DROPFILES>();
    let mut data = vec![0u8; header];
    let dropfiles = DROPFILES {
        pFiles: header as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: 0,
        fWide: 1,
    };
    // SAFETY: DROPFILES is plain old data; `data` holds exactly its size.
    unsafe {
        ptr::copy_nonoverlapping(
            (&dropfiles as *const DROPFILES).cast::<u8>(),
            data.as_mut_ptr(),
            header,
        );
    }
    for p in paths {
        for unit in p.to_string_lossy().encode_utf16().chain(std::iter::once(0)) {
            data.extend_from_slice(&unit.to_le_bytes());
        }
    }
    data.extend_from_slice(&[0, 0]);
    data
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

    fn snapshot(&self, limits: &SnapshotLimits) -> Result<ClipboardSnapshot, PlatformError> {
        let change_count = self.change_count()?;
        let has_files = available(u32::from(CF_HDROP));
        let has_text = available(u32::from(CF_UNICODETEXT));
        let has_image = available(registered(PNG_FORMAT))
            || available(u32::from(CF_DIBV5))
            || available(u32::from(CF_DIB));
        let content = if available(origin_format()) {
            ClipboardContent::Ours
        } else if WIN_SENSITIVE_FORMATS
            .iter()
            .any(|name| available(registered(name)))
        {
            ClipboardContent::Sensitive
        } else if (has_files && !limits.files)
            || !(has_files || has_text || (has_image && limits.images))
        {
            // File names are never shared as text.
            ClipboardContent::Unsupported
        } else {
            let cloud = registered(WIN_CLOUD_FORMAT);
            let read = {
                let _open = Open::new(ptr::null_mut())?;
                // SAFETY: the clipboard is open on this thread.
                if available(cloud) && unsafe { read_open_dword(cloud) } == Some(0) {
                    Read::Content(ClipboardContent::Sensitive)
                } else if has_files {
                    // SAFETY: the clipboard is open on this thread.
                    Read::Content(match unsafe { read_open_files() } {
                        paths if paths.is_empty() => ClipboardContent::Unsupported,
                        paths => ClipboardContent::Files(paths),
                    })
                } else if has_text {
                    // SAFETY: the clipboard is open on this thread.
                    Read::Content(match unsafe { read_open_text(limits.max_text_bytes) } {
                        TextRead::Missing => ClipboardContent::Unsupported,
                        TextRead::TooLarge(bytes) => ClipboardContent::TooLarge {
                            bytes: bytes as u64,
                        },
                        TextRead::Text(t) => ClipboardContent::Text(t),
                    })
                } else {
                    // SAFETY: the clipboard is open on this thread.
                    unsafe { read_open_image(limits.max_image_bytes) }
                }
            };
            match read {
                Read::Content(content) => content,
                Read::Dib(dib) => dib_to_png(&dib, limits.max_image_bytes),
            }
        };
        Ok(ClipboardSnapshot {
            change_count,
            content,
        })
    }

    fn write_text(&self, text: &str) -> Result<u64, PlatformError> {
        let data: Vec<u8> = wide(text).iter().flat_map(|u| u.to_le_bytes()).collect();
        // SAFETY: called while `replace` holds the clipboard open.
        replace(|| unsafe { set_global(u32::from(CF_UNICODETEXT), &data) })
    }

    fn write_image(&self, png: &[u8]) -> Result<u64, PlatformError> {
        let dib = decode_png(png)
            .and_then(|img| encode_dib(&img))
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        replace(|| {
            // SAFETY: called while `replace` holds the clipboard open.
            unsafe {
                set_global(registered(PNG_FORMAT), png)?;
                set_global(u32::from(CF_DIB), &dib)
            }
        })
    }

    fn write_files(&self, paths: &[PathBuf]) -> Result<u64, PlatformError> {
        let drop = hdrop(paths);
        replace(|| {
            // SAFETY: called while `replace` holds the clipboard open.
            unsafe {
                set_global(u32::from(CF_HDROP), &drop)?;
                set_global(
                    registered(DROP_EFFECT_FORMAT),
                    &DROPEFFECT_COPY.to_le_bytes(),
                )
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdrop_layout() {
        let data = hdrop(&[PathBuf::from("C:\\a"), PathBuf::from("D:\\b c")]);
        assert_eq!(&data[..4], &20u32.to_le_bytes(), "pFiles");
        assert_eq!(&data[16..20], &1u32.to_le_bytes(), "fWide");
        let units: Vec<u16> = data[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&units).unwrap();
        assert_eq!(text, "C:\\a\0D:\\b c\0\0");
    }
}
