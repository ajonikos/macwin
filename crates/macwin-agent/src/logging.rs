//! Logging: to stderr for commands, to a file for the tray (it has no
//! terminal). `RUST_LOG` overrides the default `info` level.
//!
//! Key identities are only ever logged at TRACE, never above.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

/// A log above this size is moved aside when the tray starts.
const MAX_LOG: u64 = 4 * 1024 * 1024;

fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

pub fn init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_writer(std::io::stderr)
        .try_init();
}

/// The tray's log file: `~/Library/Logs/macwin/macwin.log` on macOS (where
/// `scripts/macos/run.sh` follows it), `macwin.log` in the cache directory
/// elsewhere or when the directories are overridden.
pub fn log_path(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let overridden = ["MACWIN_CONFIG_DIR", "MACWIN_CACHE_DIR"]
        .iter()
        .any(|k| env(k).is_some_and(|v| !v.is_empty()));
    if cfg!(target_os = "macos") && !overridden {
        let home = env("HOME").filter(|h| !h.is_empty())?;
        return Some(PathBuf::from(home).join("Library/Logs/macwin/macwin.log"));
    }
    macwin_net::cache_dir(env)
        .ok()
        .map(|d| d.join("macwin.log"))
}

/// Appends the log to `path`; a log above 4 MiB is first renamed to
/// `macwin.log.old`. Panics are logged too.
pub fn init_file(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = fs::rename(path, path.with_extension("log.old"));
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default(info);
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn overridden_directories_keep_the_log_with_the_cache() {
        let p = log_path(env_of(&[("MACWIN_CONFIG_DIR", "/tmp/cfg"), ("HOME", "/h")])).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/tmp/cfg").join("cache").join("macwin.log")
        );
    }
}
