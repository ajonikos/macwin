//! macwin agent library: session wiring between the network link, the core
//! Engine and the platform backends, shared by the `macwin` binary and tests.
#![forbid(unsafe_code)]

pub mod cli;
pub mod commands;
pub mod driver;
pub mod logging;
pub mod platform;
pub mod spike_net;

use macwin_core::types::Os;

/// The OS this binary runs on, if macwin supports it.
pub fn host_os() -> Option<Os> {
    if cfg!(target_os = "macos") {
        Some(Os::MacOs)
    } else if cfg!(windows) {
        Some(Os::Windows)
    } else {
        None
    }
}

/// A human-readable machine name from the environment.
pub fn device_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "unknown".into())
}
