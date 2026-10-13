//! Cursor control for grabs: warp, decouple from the mouse, hide/show.

use macwin_core::types::Point;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDisplayHideCursor, CGDisplayShowCursor,
    CGWarpMouseCursorPosition,
};
use tracing::debug;

/// `kCGNullDirectDisplay`: the cursor calls ignore the display id.
const ANY_DISPLAY: u32 = 0;

pub(crate) fn warp(p: Point) {
    let _ = CGWarpMouseCursorPosition(CGPoint::new(f64::from(p.x), f64::from(p.y)));
}

/// `false` freezes the cursor while the mouse keeps reporting deltas.
/// Re-associating right after a warp also cancels the post-warp event
/// suppression delay.
pub(crate) fn associate(connected: bool) {
    let _ = CGAssociateMouseAndMouseCursorPosition(connected);
}

pub(crate) fn hide() {
    allow_background_hide();
    let _ = CGDisplayHideCursor(ANY_DISPLAY);
}

pub(crate) fn show() {
    let _ = CGDisplayShowCursor(ANY_DISPLAY);
}

/// `CGDisplayHideCursor` only works for the frontmost app unless the
/// connection property "SetsCursorInBackground" is set. This is a private
/// WindowServer API (used the same way by other KVM tools); it keeps working
/// without App Store distribution. Without the feature the cursor stays
/// visible, parked at the edge.
#[cfg(feature = "private-cursor-api")]
fn allow_background_hide() {
    use std::ffi::c_void;
    use std::sync::Once;

    use objc2_core_foundation::{CFString, kCFBooleanTrue};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn _CGSDefaultConnection() -> i32;
        fn CGSSetConnectionProperty(
            cid: i32,
            target: i32,
            key: *const c_void,
            value: *const c_void,
        ) -> i32;
    }

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let key = CFString::from_static_str("SetsCursorInBackground");
        // SAFETY: kCFBooleanTrue is a CF constant valid for the process lifetime.
        let Some(yes) = (unsafe { kCFBooleanTrue }) else {
            return;
        };
        // SAFETY: the connection id comes from CGS itself; `key` and `yes`
        // are valid CF objects for the duration of the call.
        let err = unsafe {
            let cid = _CGSDefaultConnection();
            CGSSetConnectionProperty(
                cid,
                cid,
                (&*key as *const CFString).cast(),
                (yes as *const objc2_core_foundation::CFBoolean).cast(),
            )
        };
        debug!(err, "SetsCursorInBackground");
    });
}

#[cfg(not(feature = "private-cursor-api"))]
fn allow_background_hide() {
    debug!("private-cursor-api disabled: the cursor stays visible in the background");
}
