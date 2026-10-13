//! macOS backend for macwin (CGEventTap capture, CGEventPost injection, display
//! enumeration). Compiles to an empty library on other operating systems.
#![cfg(target_os = "macos")]
