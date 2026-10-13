//! Monitor enumeration in virtual-screen physical pixels.

use macwin_core::geometry::MAX_SCREENS;
use macwin_core::types::{Rect, ScreenInfo};
use macwin_platform::{PlatformError, ScreenSource};
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_sys::core::BOOL;

/// The attached monitors. Call `init_dpi_awareness` first, otherwise
/// Windows reports scaled coordinates.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinScreens;

unsafe extern "system" fn collect(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `Vec` passed by `screens`, alive for the whole
    // EnumDisplayMonitors call.
    let list = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
    list.push(monitor);
    1
}

impl ScreenSource for WinScreens {
    fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError> {
        let mut monitors: Vec<HMONITOR> = Vec::new();
        // SAFETY: `collect` only writes into `monitors`, which outlives the call.
        let ok = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut monitors as *mut Vec<HMONITOR> as LPARAM,
            )
        };
        if ok == 0 {
            return Err(PlatformError::Backend("EnumDisplayMonitors failed".into()));
        }
        let mut screens = Vec::new();
        for (i, &m) in monitors.iter().take(MAX_SCREENS).enumerate() {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: `info` is a properly sized MONITORINFO.
            if unsafe { GetMonitorInfoW(m, &mut info) } == 0 {
                continue;
            }
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            // SAFETY: valid monitor handle and out pointers.
            let hr = unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
            let dpi = if hr >= 0 && dpi_x > 0 { dpi_x } else { 96 };
            let r = info.rcMonitor;
            let rect =
                Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top).map_err(|e| {
                    PlatformError::Backend(format!("monitor {i} has unusable bounds: {e}"))
                })?;
            screens.push(ScreenInfo {
                id: i as u32 + 1,
                rect,
                native_per_logical: f64::from(dpi) / 96.0,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        if screens.is_empty() {
            return Err(PlatformError::Backend("no monitors".into()));
        }
        Ok(screens)
    }
}
