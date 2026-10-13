//! Pure helper math for the OS backends. Compiled and tested on every OS so
//! the Windows conversions are verified on the Mac.

pub mod macos;
pub mod windows;

/// Tag written into every injected event (`kCGEventSourceUserData` on macOS,
/// `dwExtraInfo` on Windows) so capture can recognise and ignore our own
/// events: "macwin" + 0001.
pub const INJECT_TAG: u64 = 0x6D61_6377_696E_0001;

/// Pixels per wheel notch (120 units) when converting between the two
/// units: close to the 3 lines Windows scrolls per notch.
pub const PIXELS_PER_NOTCH: i32 = 40;

/// A pixel wheel delta in 1/120 notch units.
pub fn pixels_to_notch120(px: i32) -> i32 {
    px.saturating_mul(120 / PIXELS_PER_NOTCH)
}
