//! Per-monitor-v2 DPI awareness: coordinates are physical pixels on every
//! monitor. Must run before any window or hook is created.

use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Makes the process per-monitor-v2 DPI aware. Returns false if Windows
/// refused (for example because awareness was already set), which is fine
/// when it was set to the same mode by a manifest.
pub fn init_dpi_awareness() -> bool {
    // SAFETY: plain Win32 call with a predefined context constant.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}
