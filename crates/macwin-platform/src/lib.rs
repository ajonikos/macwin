//! Backend traits between the macwin driver and the OS: input capture,
//! input injection and screen enumeration. The real implementations live in
//! `macwin-platform-macos` and `macwin-platform-windows`; the `testing`
//! feature provides in-memory fakes.
//!
//! Locking contract: the driver holds its own lock while it calls into a
//! backend (lock order: driver -> backend). Backends must therefore never
//! hold an internal lock while calling the [`CaptureSink`], and every
//! [`CaptureControl`] method must be callable from inside
//! [`CaptureSink::on_event`] without blocking.
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use macwin_core::types::{CaptureFault, CapturedEvent, GrabCommand, InjectCommand, ScreenInfo};

#[cfg(feature = "testing")]
pub mod testing;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("unsupported on this OS")]
    Unsupported,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("backend error: {0}")]
    Backend(String),
}

/// Receives physical input from a capture backend.
///
/// Called synchronously from the capture thread, so it must return quickly
/// (the OS removes hooks / disables taps that block).
pub trait CaptureSink: Send + Sync + 'static {
    /// Returns true to deliver the event to the local OS, false to swallow it.
    fn on_event(&self, ev: CapturedEvent) -> bool;
    /// A fail-safe fired; the backend has already restored local control.
    fn on_fault(&self, fault: CaptureFault);
}

/// Controls a running capture backend. Every method must be non-blocking and
/// re-entrant (callable from inside [`CaptureSink::on_event`]).
pub trait CaptureControl: Send + Sync {
    fn command(&self, cmd: GrabCommand);
    /// Extends the grab lease; without renewal the backend ungrabs on its own
    /// and reports [`CaptureFault::LeaseExpired`].
    fn renew_lease(&self, until: Instant);
    /// Stops capturing and releases any grab.
    fn stop(&self);
}

/// Captures physical local input. Implementations filter out our own
/// injected events (tagged with `macwin_core::os::INJECT_TAG`) and other
/// synthetic input.
pub trait InputCapture: Send {
    fn start(
        &mut self,
        sink: Arc<dyn CaptureSink>,
    ) -> Result<Arc<dyn CaptureControl>, PlatformError>;
}

/// Injects input into the local OS.
pub trait InputInjector: Send {
    fn inject(&mut self, cmd: InjectCommand) -> Result<(), PlatformError>;
}

/// Enumerates the local screens in native coordinates.
pub trait ScreenSource: Send {
    fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError>;
}

/// Name of the private clipboard format that marks text written by macwin,
/// so a clipboard watcher can ignore its own writes.
pub const CLIPBOARD_ORIGIN_FORMAT: &str = "io.macwin.origin";

/// One format currently on the clipboard. Never carries content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFormat {
    pub name: String,
    /// Size in bytes when the backend could determine it.
    pub size: Option<usize>,
}

/// A content-free description of the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardInfo {
    pub change_count: u64,
    pub formats: Vec<ClipboardFormat>,
    /// The clipboard carries [`CLIPBOARD_ORIGIN_FORMAT`] (macwin wrote it).
    pub ours: bool,
}

/// The system clipboard. Content must never be logged.
pub trait ClipboardBackend: Send {
    /// A counter that changes whenever the clipboard changes (macOS
    /// `changeCount`, Windows sequence number). Never reads content.
    fn change_count(&self) -> Result<u64, PlatformError>;
    /// Format names and sizes. Sizing reads the data, which on macOS 15.4+
    /// counts as a programmatic pasteboard read (privacy alert).
    fn inspect(&self) -> Result<ClipboardInfo, PlatformError>;
    fn read_text(&self) -> Result<Option<String>, PlatformError>;
    /// Replaces the clipboard with `text` plus the origin marker.
    fn write_text(&self, text: &str) -> Result<(), PlatformError>;
}

/// Backend for hosts without an implementation: every call fails with
/// [`PlatformError::Unsupported`].
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedPlatform;

impl InputCapture for UnsupportedPlatform {
    fn start(
        &mut self,
        _sink: Arc<dyn CaptureSink>,
    ) -> Result<Arc<dyn CaptureControl>, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl InputInjector for UnsupportedPlatform {
    fn inject(&mut self, _cmd: InjectCommand) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl ScreenSource for UnsupportedPlatform {
    fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl ClipboardBackend for UnsupportedPlatform {
    fn change_count(&self) -> Result<u64, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn inspect(&self) -> Result<ClipboardInfo, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn read_text(&self) -> Result<Option<String>, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn write_text(&self, _text: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use macwin_core::types::Point;

    use super::*;

    struct NullSink;

    impl CaptureSink for NullSink {
        fn on_event(&self, _ev: CapturedEvent) -> bool {
            true
        }

        fn on_fault(&self, _fault: CaptureFault) {}
    }

    #[test]
    fn unsupported_platform_fails_everywhere() {
        let mut p = UnsupportedPlatform;
        assert!(matches!(
            p.start(Arc::new(NullSink)),
            Err(PlatformError::Unsupported)
        ));
        assert!(matches!(
            p.inject(InjectCommand::MoveTo(Point::new(1, 2))),
            Err(PlatformError::Unsupported)
        ));
        assert!(matches!(p.screens(), Err(PlatformError::Unsupported)));
        assert!(matches!(p.change_count(), Err(PlatformError::Unsupported)));
        assert!(matches!(p.inspect(), Err(PlatformError::Unsupported)));
        assert!(matches!(p.read_text(), Err(PlatformError::Unsupported)));
        assert!(matches!(p.write_text("x"), Err(PlatformError::Unsupported)));
    }
}
