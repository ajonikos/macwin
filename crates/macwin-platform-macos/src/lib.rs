//! macOS backend for macwin: CGEventTap capture, CGEventPost injection,
//! display enumeration and NSPasteboard. Compiles to an empty library on
//! other operating systems.
//!
//! Permissions (System Settings > Privacy & Security): capture needs
//! Accessibility (active event tap) and may need Input Monitoring;
//! injection needs Accessibility. Grant them to the app bundle
//! (`MacWin.app`), not to a Terminal, so they survive rebuilds when the app
//! is signed with a stable identity.
#![cfg(target_os = "macos")]

mod capture;
mod clipboard;
mod convert;
mod cursor;
mod inject;
pub mod permissions;
mod screens;

pub use capture::MacCapture;
pub use clipboard::MacClipboard;
pub use inject::MacInjector;
pub use screens::MacScreens;
