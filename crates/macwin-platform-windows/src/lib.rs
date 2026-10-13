//! Windows backend for macwin (low-level hooks, SendInput, per-monitor-v2 DPI
//! screen enumeration). Compiles to an empty library on other operating systems.
#![cfg(windows)]
