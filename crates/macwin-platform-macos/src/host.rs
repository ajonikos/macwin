//! The machine's name as users know it.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::NSProcessInfo;

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyComputerName(
        store: *const c_void,
        name_encoding: *mut u32,
    ) -> *mut CFString;
}

/// The "Computer Name" from System Settings > General > Sharing (for
/// example "Dmitriy's MacBook Pro"), falling back to the host name without
/// `.local`.
pub fn device_name() -> Option<String> {
    // SAFETY: a NULL store asks for a temporary session and a NULL encoding
    // pointer is allowed; the result follows the Copy rule (+1 or NULL).
    let raw = unsafe { SCDynamicStoreCopyComputerName(ptr::null(), ptr::null_mut()) };
    if let Some(raw) = NonNull::new(raw) {
        // SAFETY: a +1 CFString that we now own.
        let name = unsafe { CFRetained::from_raw(raw) }.to_string();
        if !name.trim().is_empty() {
            return Some(name);
        }
    }
    let host = NSProcessInfo::processInfo().hostName().to_string();
    let host = host
        .strip_suffix(".local")
        .unwrap_or(&host)
        .trim()
        .to_string();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_name_is_available() {
        let name = super::device_name().expect("a computer or host name");
        assert!(!name.is_empty() && !name.ends_with(".local"), "{name:?}");
    }
}
