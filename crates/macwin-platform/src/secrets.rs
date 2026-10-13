//! Storage for this machine's secrets (its identity key).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::PlatformError;
use crate::fs::{create_private_dir, write_atomic};

/// A small named-secret store: the macOS Keychain, a DPAPI-encrypted file
/// on Windows, or plain files ([`FileSecretStore`]).
pub trait SecretStore: Send + Sync {
    /// The secret called `name`, or `None` when there is none.
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, PlatformError>;
    /// Creates or replaces the secret.
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), PlatformError>;
    /// Removes the secret; a missing secret is not an error.
    fn delete(&self, name: &str) -> Result<(), PlatformError>;
    /// Where the secret `name` is kept, for users.
    fn describe(&self, name: &str) -> String;
}

/// Checks that a secret name is a plain file stem: lowercase ASCII letters,
/// digits and `-` (internal names only, but a name must never escape a
/// store's directory).
pub fn check_secret_name(name: &str) -> Result<(), PlatformError> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PlatformError::Backend(format!(
            "invalid secret name {name:?}"
        )))
    }
}

/// Unencrypted files `<dir>/<name>.secret`, mode 0600 inside a 0700
/// directory on Unix. For tests, CI and systems without a keychain.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    pub fn new(dir: impl Into<PathBuf>) -> FileSecretStore {
        FileSecretStore { dir: dir.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf, PlatformError> {
        check_secret_name(name)?;
        Ok(self.dir.join(format!("{name}.secret")))
    }
}

fn io_err(what: &str, path: &Path, e: io::Error) -> PlatformError {
    PlatformError::Backend(format!("cannot {what} {}: {e}", path.display()))
}

impl SecretStore for FileSecretStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, PlatformError> {
        let path = self.path(name)?;
        match fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err("read", &path, e)),
        }
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), PlatformError> {
        let path = self.path(name)?;
        create_private_dir(&self.dir).map_err(|e| io_err("create", &self.dir, e))?;
        write_atomic(&path, secret, true).map_err(|e| io_err("write", &path, e))
    }

    fn delete(&self, name: &str) -> Result<(), PlatformError> {
        let path = self.path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("delete", &path, e)),
        }
    }

    fn describe(&self, name: &str) -> String {
        match self.path(name) {
            Ok(p) => format!("unencrypted file {}", p.display()),
            Err(_) => "unencrypted file".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("cfg"));
        assert_eq!(store.load("identity").unwrap(), None);
        store.store("identity", b"one").unwrap();
        store.store("identity", b"two").unwrap();
        assert_eq!(
            store.load("identity").unwrap().as_deref(),
            Some(&b"two"[..])
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.path().join("cfg").join("identity.secret");
            assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        store.delete("identity").unwrap();
        store.delete("identity").unwrap();
        assert_eq!(store.load("identity").unwrap(), None);
        assert!(store.describe("identity").contains("identity.secret"));
    }

    #[test]
    fn names_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        for bad in ["", "../x", "a/b", "A", "x.y"] {
            assert!(store.store(bad, b"s").is_err(), "{bad:?}");
            assert!(store.load(bad).is_err(), "{bad:?}");
        }
    }
}
