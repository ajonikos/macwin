//! The real backends of the host OS (macOS or Windows).

use macwin_platform::PlatformError;

pub use imp::{capture, clipboard, init, injector, permissions_report, screens};

use crate::driver::Backends;

/// All backends a KVM session needs.
pub fn backends() -> Result<Backends, PlatformError> {
    Ok(Backends {
        capture: capture(),
        injector: injector()?,
        screens: screens(),
    })
}

#[cfg(target_os = "macos")]
mod imp {
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource,
    };
    use macwin_platform_macos::{MacCapture, MacClipboard, MacInjector, MacScreens, permissions};

    pub fn init() {}

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(MacScreens)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(MacCapture::new())
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Ok(Box::new(MacInjector::new()?))
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(MacClipboard)
    }

    /// `prompt` asks macOS to show the permission prompts first.
    pub fn permissions_report(prompt: bool) -> String {
        let p = if prompt {
            permissions::request()
        } else {
            permissions::check()
        };
        let state = |b: bool| if b { "granted" } else { "MISSING" };
        let mut out = format!(
            "accessibility:    {}\ninput monitoring: {}\npost events:      {}\nsecure input:     {}\n",
            state(p.accessibility),
            state(p.listen_events),
            state(p.post_events),
            if p.secure_input {
                "ON (keystrokes are hidden from macwin until it ends)"
            } else {
                "off"
            },
        );
        if !p.sufficient() {
            out.push_str(
                "\nGrant the missing permissions to the app that runs macwin (MacWin.app, or your terminal) in\n\
                 System Settings > Privacy & Security > Accessibility / Input Monitoring, then restart it.\n",
            );
        }
        out
    }
}

#[cfg(windows)]
mod imp {
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource,
    };
    use macwin_platform_windows::{
        WinCapture, WinClipboard, WinInjector, WinScreens, init_dpi_awareness,
    };

    /// Per-monitor-v2 DPI awareness; must run before any backend is used.
    pub fn init() {
        let _ = init_dpi_awareness();
    }

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(WinScreens)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(WinCapture::new())
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Ok(Box::new(WinInjector::new()))
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(WinClipboard)
    }

    pub fn permissions_report(_prompt: bool) -> String {
        "Windows needs no permission prompts.\n\
         Limits: elevated (admin) windows do not accept input from a non-elevated macwin, and the lock\n\
         screen / UAC prompts are unreachable until the SYSTEM service exists (Phase 4).\n"
            .into()
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use macwin_platform::{
        ClipboardBackend, InputCapture, InputInjector, PlatformError, ScreenSource,
        UnsupportedPlatform,
    };

    pub fn init() {}

    pub fn screens() -> Box<dyn ScreenSource> {
        Box::new(UnsupportedPlatform)
    }

    pub fn capture() -> Box<dyn InputCapture> {
        Box::new(UnsupportedPlatform)
    }

    pub fn injector() -> Result<Box<dyn InputInjector>, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    pub fn clipboard() -> Box<dyn ClipboardBackend> {
        Box::new(UnsupportedPlatform)
    }

    pub fn permissions_report(_prompt: bool) -> String {
        "unsupported OS\n".into()
    }
}
