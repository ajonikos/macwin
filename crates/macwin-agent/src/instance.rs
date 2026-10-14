//! One running macwin (the tray or `macwin run`) per user: an exclusive lock
//! on a file in the cache directory, released when the process exits.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::Context;
use macwin_platform::fs::create_private_dir;

pub const LOCK_FILE: &str = "macwin.lock";

/// Held for as long as this process runs macwin.
pub struct InstanceLock {
    _file: File,
}

/// Takes the lock in `dir`; `None` when another macwin holds it.
pub fn acquire(dir: &Path) -> anyhow::Result<Option<InstanceLock>> {
    create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(&dir.path().join("cache")).unwrap();
        assert!(first.is_some());
        assert!(acquire(&dir.path().join("cache")).unwrap().is_none());
        drop(first);
        assert!(acquire(&dir.path().join("cache")).unwrap().is_some());
    }
}
