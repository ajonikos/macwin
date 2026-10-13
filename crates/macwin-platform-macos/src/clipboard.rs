//! NSPasteboard access. Reading content (including `inspect`, which sizes
//! every type) is a programmatic pasteboard read: on recent macOS it shows
//! the "Paste from Other Apps" alert unless the app is allowed in
//! System Settings > Privacy & Security. `changeCount` and the list of
//! types are metadata and never trigger it, so [`MacClipboard::snapshot`]
//! reads the text only when it is going to be shared.

use macwin_platform::{
    CLIPBOARD_ORIGIN_FORMAT, ClipboardBackend, ClipboardFormat, ClipboardInfo, ClipboardSnapshot,
    LocalClipboard, MAC_SENSITIVE_TYPES, PlatformError,
};
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSData, NSString};

/// The general pasteboard.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacClipboard;

fn string_type() -> &'static NSString {
    // SAFETY: an AppKit constant valid for the process lifetime.
    unsafe { NSPasteboardTypeString }
}

/// The pasteboard's type names (metadata only).
fn type_names(pb: &NSPasteboard) -> Vec<String> {
    let Some(types) = pb.types() else {
        return Vec::new();
    };
    (0..types.count())
        .map(|i| types.objectAtIndex(i).to_string())
        .collect()
}

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
        Ok(pb.stringForType(string_type()).map(|s| s.to_string()))
    }

    fn snapshot(&self, max_text_bytes: usize) -> Result<ClipboardSnapshot, PlatformError> {
        let pb = NSPasteboard::generalPasteboard();
        let change_count = pb.changeCount() as u64;
        let types = type_names(&pb);
        let has = |name: &str| types.iter().any(|t| t == name);
        let content = if has(CLIPBOARD_ORIGIN_FORMAT) {
            LocalClipboard::Ours
        } else if MAC_SENSITIVE_TYPES.iter().any(|t| has(t)) {
            LocalClipboard::Sensitive
        } else if !has(&string_type().to_string()) {
            LocalClipboard::NoText
        } else {
            match pb.stringForType(string_type()) {
                None => LocalClipboard::NoText,
                Some(s) => text_content(&s, max_text_bytes),
            }
        };
        Ok(ClipboardSnapshot {
            change_count,
            content,
        })
    }

    fn write_text(&self, text: &str) -> Result<u64, PlatformError> {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if !pb.setString_forType(&NSString::from_str(text), string_type()) {
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
        Ok(pb.changeCount() as u64)
    }
}

/// Sizes the string before converting it, so a huge text is never copied.
fn text_content(s: &Retained<NSString>, max_text_bytes: usize) -> LocalClipboard {
    let bytes = s.len();
    if bytes > max_text_bytes {
        LocalClipboard::TooLarge { bytes }
    } else {
        LocalClipboard::Text(s.to_string())
    }
}
