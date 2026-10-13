//! Display enumeration in CG global coordinates (points, y down, origin at
//! the top-left of the main display).

use macwin_core::geometry::MAX_SCREENS;
use macwin_core::types::{Rect, ScreenInfo};
use macwin_platform::{PlatformError, ScreenSource};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGError, CGGetActiveDisplayList, CGMainDisplayID,
};

/// The active displays. `native_per_logical` is 1.0: macOS coordinates are
/// already logical points.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacScreens;

impl ScreenSource for MacScreens {
    fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError> {
        let mut ids: [CGDirectDisplayID; MAX_SCREENS] = [0; MAX_SCREENS];
        let mut count: u32 = 0;
        // SAFETY: `ids` has room for MAX_SCREENS ids and that is the capacity
        // we pass; `count` is a valid out pointer.
        let err =
            unsafe { CGGetActiveDisplayList(MAX_SCREENS as u32, ids.as_mut_ptr(), &mut count) };
        if err != CGError::Success {
            return Err(PlatformError::Backend(format!(
                "CGGetActiveDisplayList failed: {err:?}"
            )));
        }
        let main = CGMainDisplayID();
        let mut screens = Vec::with_capacity(count as usize);
        for &id in ids.iter().take(count as usize) {
            let b = CGDisplayBounds(id);
            let rect = Rect::new(
                b.origin.x.round() as i32,
                b.origin.y.round() as i32,
                b.size.width.round() as i32,
                b.size.height.round() as i32,
            )
            .map_err(|e| {
                PlatformError::Backend(format!("display {id} has unusable bounds: {e}"))
            })?;
            screens.push(ScreenInfo {
                id,
                rect,
                native_per_logical: 1.0,
                primary: id == main,
            });
        }
        if screens.is_empty() {
            return Err(PlatformError::Backend("no active displays".into()));
        }
        Ok(screens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only query; no permission is involved.
    #[test]
    fn lists_valid_screens() {
        let screens = match MacScreens.screens() {
            Ok(s) => s,
            // Headless CI machines may have no active display.
            Err(PlatformError::Backend(m)) if m == "no active displays" => return,
            Err(e) => panic!("{e}"),
        };
        assert!(!screens.is_empty());
        assert!(screens.iter().filter(|s| s.primary).count() <= 1);
        assert!(macwin_core::geometry::ScreenSet::new(screens).is_ok());
    }
}
