//! Secrets encrypted with DPAPI for the current Windows user, stored as
//! `<dir>/<name>.dpapi`. Only the same user account can decrypt them; the
//! file alone is useless on another machine or account.

use std::path::{Path, PathBuf};
use std::{fs, io, ptr, slice};

use macwin_platform::fs::{create_private_dir, write_atomic};
use macwin_platform::{PlatformError, SecretStore, check_secret_name};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Extra entropy: another program of the same user cannot decrypt the
/// blob with a plain `CryptUnprotectData` call.
const ENTROPY: &[u8] = b"io.macwin.secret.v1";

/// DPAPI-encrypted files in one directory.
#[derive(Debug, Clone)]
pub struct DpapiStore {
    dir: PathBuf,
}

impl DpapiStore {
    pub fn new(dir: impl Into<PathBuf>) -> DpapiStore {
        DpapiStore { dir: dir.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf, PlatformError> {
        check_secret_name(name)?;
        Ok(self.dir.join(format!("{name}.dpapi")))
    }
}

fn io_err(what: &str, path: &Path, e: io::Error) -> PlatformError {
    PlatformError::Backend(format!("cannot {what} {}: {e}", path.display()))
}

fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, PlatformError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())
            .map_err(|_| PlatformError::Backend("secret too large".into()))?,
        pbData: data.as_ptr().cast_mut(),
    })
}

/// Copies and frees a buffer that DPAPI allocated with `LocalAlloc`.
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI returned `cbData` initialised bytes at `pbData`, which
    // we own and free exactly once.
    unsafe {
        let data = slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData.cast());
        data
    }
}

fn empty() -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    }
}

/// Encrypts `data` for the current user.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, PlatformError> {
    let input = blob(data)?;
    let entropy = blob(ENTROPY)?;
    let mut out = empty();
    // SAFETY: the input blobs point to live buffers for the duration of the
    // call (DPAPI does not write through them); `out` receives a LocalAlloc
    // buffer that `take` frees.
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(PlatformError::Backend(format!(
            "CryptProtectData failed: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(take(out))
}

/// Decrypts a blob made by [`protect`] for the current user.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, PlatformError> {
    let input = blob(data)?;
    let entropy = blob(ENTROPY)?;
    let mut out = empty();
    // SAFETY: as in `protect`; no description is requested.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(PlatformError::Backend(format!(
            "CryptUnprotectData failed (was the file made by another Windows user?): {}",
            io::Error::last_os_error()
        )));
    }
    Ok(take(out))
}

impl SecretStore for DpapiStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, PlatformError> {
        let path = self.path(name)?;
        match fs::read(&path) {
            Ok(blob) => unprotect(&blob).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err("read", &path, e)),
        }
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), PlatformError> {
        let path = self.path(name)?;
        let blob = protect(secret)?;
        create_private_dir(&self.dir).map_err(|e| io_err("create", &self.dir, e))?;
        write_atomic(&path, &blob, true).map_err(|e| io_err("write", &path, e))
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
            Ok(p) => format!("DPAPI-encrypted file {}", p.display()),
            Err(_) => "DPAPI-encrypted file".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_round_trip_and_store() {
        let blob = protect(b"secret").unwrap();
        assert_ne!(blob, b"secret");
        assert_eq!(unprotect(&blob).unwrap(), b"secret");
        assert!(unprotect(b"not a dpapi blob").is_err());
        let dir = std::env::temp_dir().join(format!("macwin-dpapi-{}", std::process::id()));
        let store = DpapiStore::new(&dir);
        store.store("identity", b"key").unwrap();
        assert_eq!(
            store.load("identity").unwrap().as_deref(),
            Some(&b"key"[..])
        );
        let on_disk = fs::read(dir.join("identity.dpapi")).unwrap();
        assert!(!on_disk.windows(3).any(|w| w == b"key"));
        store.delete("identity").unwrap();
        assert_eq!(store.load("identity").unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
