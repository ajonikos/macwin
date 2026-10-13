//! Loading, creating and migrating this machine's identity.
//!
//! The identity (certificate + key, one PEM bundle) lives in a
//! [`SecretStore`]: the login keychain on macOS, a DPAPI-encrypted file on
//! Windows, or a private file when `MACWIN_KEY_STORE=file`. Phase 0 kept it
//! as two PEM files in the config directory; those are imported once,
//! verified, and then deleted.

use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, bail};
use macwin_net::Identity;
use macwin_net::identity::{legacy_read, legacy_remove};
use macwin_platform::SecretStore;

/// Name of the identity secret in the store.
pub const IDENTITY_SECRET: &str = "identity";
/// Environment variable that overrides the store.
pub const KEY_STORE_ENV: &str = "MACWIN_KEY_STORE";

/// Where the identity is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStoreKind {
    /// macOS login keychain (default on macOS).
    Keychain,
    /// DPAPI-encrypted file in the config directory (default on Windows).
    Dpapi,
    /// Unencrypted file in the config directory, mode 0600 (tests, CI and
    /// other systems).
    File,
}

impl KeyStoreKind {
    /// The store selected by `MACWIN_KEY_STORE` (`keychain`, `dpapi` or
    /// `file`), or the OS default.
    pub fn from_env(env: impl Fn(&str) -> Option<OsString>) -> anyhow::Result<KeyStoreKind> {
        let value = env(KEY_STORE_ENV).filter(|v| !v.is_empty());
        let Some(value) = value else {
            return Ok(KeyStoreKind::os_default());
        };
        let kind = match value.to_string_lossy().as_ref() {
            "keychain" if cfg!(target_os = "macos") => KeyStoreKind::Keychain,
            "dpapi" if cfg!(windows) => KeyStoreKind::Dpapi,
            "file" => KeyStoreKind::File,
            other => bail!(
                "{KEY_STORE_ENV}={other:?} is not available here; use {:?} or \"file\"",
                KeyStoreKind::os_default().name()
            ),
        };
        Ok(kind)
    }

    pub fn os_default() -> KeyStoreKind {
        if cfg!(target_os = "macos") {
            KeyStoreKind::Keychain
        } else if cfg!(windows) {
            KeyStoreKind::Dpapi
        } else {
            KeyStoreKind::File
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            KeyStoreKind::Keychain => "keychain",
            KeyStoreKind::Dpapi => "dpapi",
            KeyStoreKind::File => "file",
        }
    }
}

/// How [`load_or_create`] got the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityOrigin {
    Loaded,
    Created,
    /// Imported from the Phase 0 PEM files, which were then deleted.
    Migrated,
}

/// Stores `bundle` and reads it back, so a store that silently drops
/// writes is noticed before anything depends on it.
fn store_verified(store: &dyn SecretStore, id: &Identity, bundle: &str) -> anyhow::Result<()> {
    store
        .store(IDENTITY_SECRET, bundle.as_bytes())
        .context("cannot save the identity")?;
    let back = store
        .load(IDENTITY_SECRET)
        .context("cannot read the identity back")?
        .context("the identity was not saved")?;
    let back = Identity::from_pem_bundle(&back).context("the saved identity is unreadable")?;
    if back.fingerprint() != id.fingerprint() {
        bail!("the saved identity differs from the one written");
    }
    Ok(())
}

/// Loads the identity from `store`; otherwise migrates the Phase 0 files
/// from `config_dir`; otherwise creates a new identity. A stored identity
/// is never replaced silently.
pub fn load_or_create(
    store: &dyn SecretStore,
    config_dir: &Path,
) -> anyhow::Result<(Identity, IdentityOrigin)> {
    if let Some(blob) = store
        .load(IDENTITY_SECRET)
        .context("cannot read the identity")?
    {
        let id = Identity::from_pem_bundle(&blob).with_context(|| {
            format!(
                "the identity in {} is unreadable",
                store.describe(IDENTITY_SECRET)
            )
        })?;
        return Ok((id, IdentityOrigin::Loaded));
    }
    if let Some((id, bundle)) = legacy_read(config_dir)? {
        store_verified(store, &id, &bundle)?;
        legacy_remove(config_dir)?;
        return Ok((id, IdentityOrigin::Migrated));
    }
    let (id, bundle) = Identity::generate_bundle()?;
    store_verified(store, &id, &bundle)?;
    Ok((id, IdentityOrigin::Created))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use macwin_net::identity::{LEGACY_CERT_FILE, LEGACY_KEY_FILE};
    use macwin_platform::testing::MemorySecretStore;

    use super::*;

    #[test]
    fn creates_once_then_loads() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemorySecretStore::new();
        let (a, origin) = load_or_create(&store, dir.path()).unwrap();
        assert_eq!(origin, IdentityOrigin::Created);
        let (b, origin) = load_or_create(&store, dir.path()).unwrap();
        assert_eq!(origin, IdentityOrigin::Loaded);
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn phase0_files_are_migrated_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (legacy, bundle) = Identity::generate_bundle().unwrap();
        let split = bundle.find("-----BEGIN PRIVATE KEY").unwrap();
        fs::write(dir.path().join(LEGACY_CERT_FILE), &bundle[..split]).unwrap();
        fs::write(dir.path().join(LEGACY_KEY_FILE), &bundle[split..]).unwrap();
        let store = MemorySecretStore::new();
        let (id, origin) = load_or_create(&store, dir.path()).unwrap();
        assert_eq!(origin, IdentityOrigin::Migrated);
        assert_eq!(id.fingerprint(), legacy.fingerprint());
        assert!(!dir.path().join(LEGACY_KEY_FILE).exists());
        assert!(!dir.path().join(LEGACY_CERT_FILE).exists());
        let (again, origin) = load_or_create(&store, dir.path()).unwrap();
        assert_eq!(origin, IdentityOrigin::Loaded);
        assert_eq!(again.fingerprint(), legacy.fingerprint());
    }

    #[test]
    fn a_corrupt_stored_identity_is_an_error_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemorySecretStore::new();
        store.store(IDENTITY_SECRET, b"garbage").unwrap();
        assert!(load_or_create(&store, dir.path()).is_err());
        assert_eq!(
            store.load(IDENTITY_SECRET).unwrap().as_deref(),
            Some(&b"garbage"[..])
        );
    }

    #[test]
    fn key_store_selection() {
        let env = |v: &'static str| move |k: &str| (k == KEY_STORE_ENV).then(|| OsString::from(v));
        assert_eq!(
            KeyStoreKind::from_env(env("")).unwrap(),
            KeyStoreKind::os_default()
        );
        assert_eq!(
            KeyStoreKind::from_env(env("file")).unwrap(),
            KeyStoreKind::File
        );
        assert!(KeyStoreKind::from_env(env("vault")).is_err());
        if cfg!(target_os = "macos") {
            assert_eq!(
                KeyStoreKind::from_env(env("keychain")).unwrap(),
                KeyStoreKind::Keychain
            );
            assert!(KeyStoreKind::from_env(env("dpapi")).is_err());
        }
    }
}
