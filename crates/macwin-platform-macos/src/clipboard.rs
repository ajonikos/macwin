//! NSPasteboard access. Reading content (including `inspect`, which sizes
//! every type) is a programmatic pasteboard read: on recent macOS it shows
//! the "Paste from Other Apps" alert unless the app is allowed in
//! System Settings > Privacy & Security.

use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardFormat, ClipboardInfo, PlatformError,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSData, NSString};

/// The general pasteboard.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacClipboard;

impl ClipboardBackend for MacClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        Ok(NSPasteboard::generalPasteboard().changeCount() as u64)
    }

    fn inspect(&self) -> Result<ClipboardInfo, PlatformError> {
        let pb = NSPasteboard::generalPasteboard();
        let change_count = pb.changeCount() as u64;
        let mut formats = Vec::new();
        if let Some(types) = pb.types() {
            for i in 0..types.count() {
                let t = types.objectAtIndex(i);
                formats.push(ClipboardFormat {
                    name: t.to_string(),
                    size: pb.dataForType(&t).map(|d| d.len()),
                });
            }
        }
        let ours = formats.iter().any(|f| f.name == CLIPBOARD_ORIGIN_FORMAT);
        Ok(ClipboardInfo {
            change_count,
            formats,
            ours,
        })
    }

    fn read_text(&self) -> Result<Option<String>, PlatformError> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: an AppKit constant valid for the process lifetime.
        let string_type = unsafe { NSPasteboardTypeString };
        Ok(pb.stringForType(string_type).map(|s| s.to_string()))
    }

    fn write_text(&self, text: &str) -> Result<(), PlatformError> {
        let pb = NSPasteboard::generalPasteboard();
        // SAFETY: an AppKit constant valid for the process lifetime.
        let string_type = unsafe { NSPasteboardTypeString };
        pb.clearContents();
        if !pb.setString_forType(&NSString::from_str(text), string_type) {
            return Err(PlatformError::Backend(
                "the pasteboard refused the text".into(),
            ));
        }
        let marker = NSString::from_str(CLIPBOARD_ORIGIN_FORMAT);
        if !pb.setData_forType(Some(&NSData::with_bytes(b"1")), &marker) {
            return Err(PlatformError::Backend(
                "the pasteboard refused the origin marker".into(),
            ));
        }
        Ok(())
    }
}
