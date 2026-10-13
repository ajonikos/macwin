//! A machine's long-term identity: a self-signed certificate and its key.
//!
//! Phase 0 stores both as PEM files (`identity.crt.pem`, `identity.key.pem`)
//! in the config directory, with mode 0600 inside a 0700 directory on Unix.
//! TODO(Phase 1): move the key into the macOS Keychain / Windows DPAPI.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::NetError;
use crate::fingerprint::Fingerprint;

const CERT_FILE: &str = "identity.crt.pem";
const KEY_FILE: &str = "identity.key.pem";
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
        Ok(generate_with_pem()?.0)
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

    /// Loads the identity from `dir`, or creates and stores a new one.
    /// Returns whether it was created. A directory holding only one of the
    /// two files is an error: the identity is never silently replaced.
    pub fn load_or_create(dir: &Path) -> Result<(Identity, bool), NetError> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        match (cert_path.exists(), key_path.exists()) {
            (true, true) => Ok((load(&cert_path, &key_path)?, false)),
            (false, false) => {
                create_private_dir(dir)?;
                let (identity, cert_pem, key_pem) = generate_with_pem()?;
                write_private(&key_path, key_pem.as_bytes())?;
                write_private(&cert_path, cert_pem.as_bytes())?;
                Ok((identity, true))
            }
            _ => Err(NetError::Identity(format!(
                "incomplete identity in {}; remove it to regenerate",
                dir.display()
            ))),
        }
    }
}

fn generate_with_pem() -> Result<(Identity, String, String), NetError> {
    let ck = rcgen::generate_simple_self_signed(vec![CERT_NAME.to_string()])
        .map_err(|e| NetError::Identity(format!("cannot generate a certificate: {e}")))?;
    let identity = Identity {
        cert: ck.cert.der().clone(),
        key: PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()),
    };
    Ok((identity, ck.cert.pem(), ck.signing_key.serialize_pem()))
}

fn load(cert_path: &Path, key_path: &Path) -> Result<Identity, NetError> {
    let read = |p: &Path| {
        fs::read(p).map_err(|e| NetError::Identity(format!("cannot read {}: {e}", p.display())))
    };
    let cert = CertificateDer::from_pem_slice(&read(cert_path)?).map_err(|e| {
        NetError::Identity(format!("bad certificate in {}: {e}", cert_path.display()))
    })?;
    let key = PrivatePkcs8KeyDer::from_pem_slice(&read(key_path)?)
        .map_err(|e| NetError::Identity(format!("bad key in {}: {e}", key_path.display())))?;
    Ok(Identity { cert, key })
}

fn create_private_dir(dir: &Path) -> Result<(), NetError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| NetError::Identity(format!("cannot create {}: {e}", dir.display())))
}

/// Writes `data` to a temporary file (mode 0600 on Unix) and renames it into
/// place, so a crash never leaves a half-written file under the final name.
fn write_private(path: &Path, data: &[u8]) -> Result<(), NetError> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let err =
        |e: std::io::Error| NetError::Identity(format!("cannot write {}: {e}", path.display()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(err)?;
    let written = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err(e));
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
    fn generated_identities_differ_and_debug_hides_the_key() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.clone().fingerprint(), a.fingerprint());
        let dbg = format!("{a:?}");
        assert!(dbg.contains(&a.fingerprint().to_string()) && !dbg.contains("key"));
    }

    #[test]
    fn load_or_create_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("macwin");
        let (first, created) = Identity::load_or_create(&path).unwrap();
        assert!(created);
        let (second, created) = Identity::load_or_create(&path).unwrap();
        assert!(!created);
        assert_eq!(first.fingerprint(), second.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o700);
            assert_eq!(mode(&path.join(KEY_FILE)), 0o600);
            assert_eq!(mode(&path.join(CERT_FILE)), 0o600);
        }
        // No temporary files are left behind.
        let names: Vec<_> = fs::read_dir(&path)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn half_present_identity_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        Identity::load_or_create(dir.path()).unwrap();
        fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let err = Identity::load_or_create(dir.path()).unwrap_err();
        assert!(
            matches!(&err, NetError::Identity(m) if m.contains("incomplete identity")),
            "{err}"
        );
        assert!(
            !dir.path().join(KEY_FILE).exists(),
            "never silently regenerated"
        );
    }

    #[test]
    fn corrupt_files_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CERT_FILE), "not pem").unwrap();
        fs::write(dir.path().join(KEY_FILE), "not pem").unwrap();
        assert!(matches!(
            Identity::load_or_create(dir.path()),
            Err(NetError::Identity(_))
        ));
    }
}
