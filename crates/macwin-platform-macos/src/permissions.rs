//! Privacy permission checks. [`check`] never prompts; [`request`] asks
//! macOS to show the permission prompts (only for a proper app bundle).

use objc2_core_graphics::{
    CGPreflightListenEventAccess, CGPreflightPostEventAccess, CGRequestListenEventAccess,
    CGRequestPostEventAccess,
};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// What this process may do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Accessibility (needed for an active event tap and for posting events).
    pub accessibility: bool,
    /// Input Monitoring (listen-only access to keyboard events).
    pub listen_events: bool,
    /// Event posting.
    pub post_events: bool,
    /// Secure Event Input is on (a password field or "Secure Keyboard
    /// Entry" is active): event taps cannot see keystrokes until it ends.
    pub secure_input: bool,
}

impl Permissions {
    /// True when capture and injection can both work.
    pub fn sufficient(&self) -> bool {
        self.accessibility && self.post_events
    }
}

/// Checks without prompting.
pub fn check() -> Permissions {
    // SAFETY: both functions take no arguments and only report state.
    let (ax, secure) = unsafe { (AXIsProcessTrusted() != 0, IsSecureEventInputEnabled() != 0) };
    Permissions {
        accessibility: ax,
        listen_events: CGPreflightListenEventAccess(),
        post_events: CGPreflightPostEventAccess(),
        secure_input: secure,
    }
}

/// Asks macOS to prompt for Input Monitoring and Accessibility, then checks
/// again. The prompts are attributed to the app bundle (or to the Terminal
/// when run from one).
pub fn request() -> Permissions {
    let _ = CGRequestListenEventAccess();
    let _ = CGRequestPostEventAccess();
    check()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `check` only reads state and never prompts.
    #[test]
    fn check_does_not_panic() {
        let p = check();
        assert_eq!(p.sufficient(), p.accessibility && p.post_events);
    }
}
