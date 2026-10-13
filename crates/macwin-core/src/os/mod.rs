//! Pure helper math for the OS backends. Compiled and tested on every OS so
//! the Windows conversions are verified on the Mac.

pub mod macos;
pub mod windows;

/// Tag written into every injected event (`kCGEventSourceUserData` on macOS,
/// `dwExtraInfo` on Windows) so capture can recognise and ignore our own
/// events: "macwin" + 0001.
pub const INJECT_TAG: u64 = 0x6D61_6377_696E_0001;
