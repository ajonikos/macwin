//! A machine's long-term identity: a self-signed certificate and its key.
//!
//! The identity is persisted as a PEM bundle (certificate + PKCS#8 key)
//! in a secret store chosen by the agent (macOS Keychain, Windows DPAPI or a
//! private file). Phase 0 kept two PEM files in the config directory; the
//! `legacy_*` functions read and remove them so the agent can migrate.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;

use crate::NetError;
use crate::fingerprint::Fingerprint;

/// Phase 0 certificate file in the config directory.
pub const LEGACY_CERT_FILE: &str = "identity.crt.pem";
/// Phase 0 private-key file in the config directory.
pub const LEGACY_KEY_FILE: &str = "identity.key.pem";
/// Subject alternative name of every macwin certificate (pinning, not names,
/// is what authenticates peers).
pub const CERT_NAME: &str = "macwin";

/// Certificate + private key of this machine.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Identity {
        Identity {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
        }
    }
}

/// Never prints the key.
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// A fresh ECDSA P-256 self-signed identity.
    pub fn generate() -> Result<Identity, NetError> {
        Ok(Identity::generate_bundle()?.0)
    }

    /// A fresh identity and its PEM bundle (secret: it holds the key).
    pub fn generate_bundle() -> Result<(Identity, String), NetError> {
        let ck = rcgen::generate_simple_self_signed(vec![CERT_NAME.to_string()])
            .map_err(|e| NetError::Identity(format!("cannot generate a certificate: {e}")))?;
        let identity = Identity {
            cert: ck.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()),
        };
        let bundle = format!("{}{}", ck.cert.pem(), ck.signing_key.serialize_pem());
        Ok((identity, bundle))
    }

    /// Parses a PEM bundle holding a CERTIFICATE and a PKCS#8 PRIVATE KEY
    /// that belong together.
    pub fn from_pem_bundle(pem: &[u8]) -> Result<Identity, NetError> {
        let cert = CertificateDer::from_pem_slice(pem)
            .map_err(|e| NetError::Identity(format!("no usable certificate: {e}")))?;
        let key = PrivatePkcs8KeyDer::from_pem_slice(pem)
            .map_err(|e| NetError::Identity(format!("no usable private key: {e}")))?;
        let provider = rustls::crypto::ring::default_provider();
        CertifiedKey::from_der(
            vec![cert.clone()],
            PrivateKeyDer::Pkcs8(key.clone_key()),
            &provider,
        )
        .map_err(|e| NetError::Identity(format!("the key does not match the certificate: {e}")))?;
        Ok(Identity { cert, key })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_cert(self.cert.as_ref())
    }

    pub fn cert(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

/// Reads a Phase 0 identity from `dir`: `Ok(None)` when neither file exists,
/// the identity and its PEM bundle when both do, an error when only one
/// does or they are unreadable.
pub fn legacy_read(dir: &Path) -> Result<Option<(Identity, String)>, NetError> {
    let cert_path = dir.join(LEGACY_CERT_FILE);
    let key_path = dir.join(LEGACY_KEY_FILE);
    match (cert_path.exists(), key_path.exists()) {
        (false, false) => Ok(None),
        (true, true) => {
            let read = |p: &Path| {
                fs::read_to_string(p)
                    .map_err(|e| NetError::Identity(format!("cannot read {}: {e}", p.display())))
            };
            let bundle = format!("{}\n{}", read(&cert_path)?, read(&key_path)?);
            let identity = Identity::from_pem_bundle(bundle.as_bytes()).map_err(|e| {
                NetError::Identity(format!("bad identity files in {}: {e}", dir.display()))
            })?;
            Ok(Some((identity, bundle)))
        }
        _ => Err(NetError::Identity(format!(
            "incomplete identity files in {}; remove {LEGACY_CERT_FILE} and {LEGACY_KEY_FILE} to regenerate",
            dir.display()
        ))),
    }
}

/// Removes the Phase 0 identity files (after a verified migration).
pub fn legacy_remove(dir: &Path) -> Result<(), NetError> {
    for name in [LEGACY_KEY_FILE, LEGACY_CERT_FILE] {
        let path = dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(NetError::Identity(format!(
                    "cannot remove {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Host OS families with different config-directory conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOs {
    MacOs,
    Windows,
    OtherUnix,
}

impl ConfigOs {
    pub fn current() -> ConfigOs {
        if cfg!(target_os = "macos") {
            ConfigOs::MacOs
        } else if cfg!(windows) {
            ConfigOs::Windows
        } else {
            ConfigOs::OtherUnix
        }
    }
}

/// The config directory, resolved through `env` (an environment lookup, so
/// tests never mutate the process environment). A non-empty
/// `MACWIN_CONFIG_DIR` always wins.
pub fn config_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, NetError> {
    config_dir_for(ConfigOs::current(), env)
}

/// The cache directory (received files wait there until they are pasted).
/// A non-empty `MACWIN_CACHE_DIR` wins; with only `MACWIN_CONFIG_DIR` set
/// it is `<config dir>/cache`, so test setups stay self-contained.
pub fn cache_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, NetError> {
    cache_dir_for(ConfigOs::current(), env)
}

/// [`cache_dir`] for an explicit OS family.
pub fn cache_dir_for(
    os: ConfigOs,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, NetError> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("MACWIN_CACHE_DIR") {
        return Ok(dir);
    }
    if let Some(dir) = var("MACWIN_CONFIG_DIR") {
        return Ok(dir.join("cache"));
    }
    let missing = |k: &str| NetError::ConfigDir(format!("{k} is not set; set MACWIN_CACHE_DIR"));
    match os {
        ConfigOs::MacOs => Ok(var("HOME")
            .ok_or_else(|| missing("HOME"))?
            .join("Library/Caches/macwin")),
        ConfigOs::Windows => Ok(var("LOCALAPPDATA")
            .ok_or_else(|| missing("LOCALAPPDATA"))?
            .join("macwin")),
        ConfigOs::OtherUnix => match var("XDG_CACHE_HOME") {
            Some(xdg) => Ok(xdg.join("macwin")),
            None => Ok(var("HOME")
                .ok_or_else(|| missing("HOME"))?
                .join(".cache/macwin")),
        },
    }
}

/// [`config_dir`] for an explicit OS family.
pub fn config_dir_for(
    os: ConfigOs,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, NetError> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("MACWIN_CONFIG_DIR") {
        return Ok(dir);
    }
    let missing = |k: &str| NetError::ConfigDir(format!("{k} is not set; set MACWIN_CONFIG_DIR"));
    match os {
        ConfigOs::MacOs => Ok(var("HOME")
            .ok_or_else(|| missing("HOME"))?
            .join("Library/Application Support/macwin")),
        ConfigOs::Windows => Ok(var("APPDATA")
            .ok_or_else(|| missing("APPDATA"))?
            .join("macwin")),
        ConfigOs::OtherUnix => match var("XDG_CONFIG_HOME") {
            Some(xdg) => Ok(xdg.join("macwin")),
            None => Ok(var("HOME")
                .ok_or_else(|| missing("HOME"))?
                .join(".config/macwin")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn config_dir_branches() {
        let p = |os, pairs| config_dir_for(os, env_of(pairs)).unwrap();
        assert_eq!(
            p(
                ConfigOs::MacOs,
                &[("MACWIN_CONFIG_DIR", "/tmp/x"), ("HOME", "/h")]
            ),
            PathBuf::from("/tmp/x")
        );
        assert_eq!(
            p(
                ConfigOs::MacOs,
                &[("MACWIN_CONFIG_DIR", ""), ("HOME", "/Users/u")]
            ),
            PathBuf::from("/Users/u/Library/Application Support/macwin")
        );
        assert_eq!(
            p(
                ConfigOs::Windows,
                &[("APPDATA", "C:\\Users\\u\\AppData\\Roaming")]
            ),
            PathBuf::from("C:\\Users\\u\\AppData\\Roaming").join("macwin")
        );
        assert_eq!(
            p(
                ConfigOs::OtherUnix,
                &[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")]
            ),
            PathBuf::from("/xdg/macwin")
        );
        assert_eq!(
            p(ConfigOs::OtherUnix, &[("HOME", "/h")]),
            PathBuf::from("/h/.config/macwin")
        );
        assert!(matches!(
            config_dir_for(ConfigOs::MacOs, env_of(&[])),
            Err(NetError::ConfigDir(_))
        ));
        assert!(matches!(
            config_dir_for(ConfigOs::Windows, env_of(&[("HOME", "/h")])),
            Err(NetError::ConfigDir(_))
        ));
    }

    #[test]
    fn cache_dir_branches() {
        let p = |os, pairs| cache_dir_for(os, env_of(pairs)).unwrap();
        assert_eq!(
            p(
                ConfigOs::MacOs,
                &[("MACWIN_CACHE_DIR", "/c"), ("MACWIN_CONFIG_DIR", "/x")]
            ),
            PathBuf::from("/c")
        );
        assert_eq!(
            p(ConfigOs::MacOs, &[("MACWIN_CONFIG_DIR", "/x")]),
            PathBuf::from("/x/cache")
        );
        assert_eq!(
            p(ConfigOs::MacOs, &[("HOME", "/Users/u")]),
            PathBuf::from("/Users/u/Library/Caches/macwin")
        );
        assert_eq!(
            p(ConfigOs::Windows, &[("LOCALAPPDATA", "C:\\L")]),
            PathBuf::from("C:\\L").join("macwin")
        );
        assert_eq!(
            p(ConfigOs::OtherUnix, &[("HOME", "/h")]),
            PathBuf::from("/h/.cache/macwin")
        );
        assert!(cache_dir_for(ConfigOs::Windows, env_of(&[("APPDATA", "x")])).is_err());
    }

    #[test]
    fn generated_identities_differ_and_debug_hides_the_key() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.clone().fingerprint(), a.fingerprint());
        let dbg = format!("{a:?}");
        assert!(dbg.contains(&a.fingerprint().to_string()) && !dbg.contains("key"));
    }

    #[test]
    fn bundle_round_trip_and_mismatch() {
        let (a, bundle) = Identity::generate_bundle().unwrap();
        assert!(bundle.contains("BEGIN CERTIFICATE") && bundle.contains("BEGIN PRIVATE KEY"));
        let back = Identity::from_pem_bundle(bundle.as_bytes()).unwrap();
        assert_eq!(back.fingerprint(), a.fingerprint());
        // A certificate with someone else's key is refused.
        let (_, other) = Identity::generate_bundle().unwrap();
        let cert_end = bundle.find("-----BEGIN PRIVATE KEY").unwrap();
        let key_start = other.find("-----BEGIN PRIVATE KEY").unwrap();
        let mixed = format!("{}{}", &bundle[..cert_end], &other[key_start..]);
        let err = Identity::from_pem_bundle(mixed.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert!(Identity::from_pem_bundle(b"garbage").is_err());
        assert!(Identity::from_pem_bundle(&bundle.as_bytes()[..cert_end]).is_err());
    }

    #[test]
    fn legacy_files_are_read_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(legacy_read(dir.path()).unwrap().is_none());
        let (id, bundle) = Identity::generate_bundle().unwrap();
        let split = bundle.find("-----BEGIN PRIVATE KEY").unwrap();
        fs::write(dir.path().join(LEGACY_CERT_FILE), &bundle[..split]).unwrap();
        fs::write(dir.path().join(LEGACY_KEY_FILE), &bundle[split..]).unwrap();
        let (read, read_bundle) = legacy_read(dir.path()).unwrap().unwrap();
        assert_eq!(read.fingerprint(), id.fingerprint());
        assert_eq!(
            Identity::from_pem_bundle(read_bundle.as_bytes())
                .unwrap()
                .fingerprint(),
            id.fingerprint()
        );
        legacy_remove(dir.path()).unwrap();
        legacy_remove(dir.path()).unwrap();
        assert!(legacy_read(dir.path()).unwrap().is_none());
    }

    #[test]
    fn half_present_or_corrupt_legacy_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(LEGACY_CERT_FILE), "x").unwrap();
        let err = legacy_read(dir.path()).unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err}");
        fs::write(dir.path().join(LEGACY_KEY_FILE), "y").unwrap();
        assert!(matches!(
            legacy_read(dir.path()),
            Err(NetError::Identity(_))
        ));
    }
}
