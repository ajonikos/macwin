//! Secrets in the user's login keychain, as generic passwords.
//!
//! The keychain grants silent access to the app that created an item (by
//! its code signature). MacWin.app signed with a stable identity keeps that
//! access across rebuilds; any other binary (for example an unbundled
//! `macwin` run from a terminal) triggers a one-time "allow access"
//! prompt.

use macwin_platform::{PlatformError, SecretStore, check_secret_name};
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, generic_password, set_generic_password,
};

/// `errSecItemNotFound`.
const ITEM_NOT_FOUND: i32 = -25300;
/// `errSecInteractionNotAllowed`: the keychain is locked and no UI may be
/// shown (for example over SSH).
const INTERACTION_NOT_ALLOWED: i32 = -25308;

/// Keychain items `service` / `<name>`.
#[derive(Debug, Clone)]
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// Items of the macwin app (`io.macwin.MacWin`).
    pub fn new() -> KeychainStore {
        KeychainStore::with_service("io.macwin.MacWin")
    }

    pub fn with_service(service: &str) -> KeychainStore {
        KeychainStore {
            service: service.into(),
        }
    }
}

impl Default for KeychainStore {
    fn default() -> KeychainStore {
        KeychainStore::new()
    }
}

fn keychain_error(what: &str, e: &security_framework::base::Error) -> PlatformError {
    let hint = if e.code() == INTERACTION_NOT_ALLOWED {
        " (the keychain is locked; unlock it, or set MACWIN_KEY_STORE=file)"
    } else {
        ""
    };
    PlatformError::Backend(format!("keychain {what} failed: {e}{hint}"))
}

impl SecretStore for KeychainStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, PlatformError> {
        check_secret_name(name)?;
        match generic_password(PasswordOptions::new_generic_password(&self.service, name)) {
            Ok(secret) => Ok(Some(secret)),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(keychain_error("read", &e)),
        }
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), PlatformError> {
        check_secret_name(name)?;
        set_generic_password(&self.service, name, secret).map_err(|e| keychain_error("write", &e))
    }

    fn delete(&self, name: &str) -> Result<(), PlatformError> {
        check_secret_name(name)?;
        match delete_generic_password(&self.service, name) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(keychain_error("delete", &e)),
        }
    }

    fn describe(&self, name: &str) -> String {
        format!("login keychain, item \"{}\" / \"{name}\"", self.service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real login keychain, so it only runs on request:
    /// `cargo test -p macwin-platform-macos -- --ignored keychain`.
    #[test]
    #[ignore = "uses the real login keychain"]
    fn keychain_round_trip() {
        let store = KeychainStore::with_service("io.macwin.test");
        store.delete("roundtrip").unwrap();
        assert_eq!(store.load("roundtrip").unwrap(), None);
        store.store("roundtrip", b"one").unwrap();
        store.store("roundtrip", b"two").unwrap();
        assert_eq!(
            store.load("roundtrip").unwrap().as_deref(),
            Some(&b"two"[..])
        );
        store.delete("roundtrip").unwrap();
        assert_eq!(store.load("roundtrip").unwrap(), None);
    }

    #[test]
    fn invalid_names_are_rejected_before_the_keychain_is_touched() {
        let store = KeychainStore::new();
        assert!(store.load("../x").is_err());
        assert!(store.store("A", b"x").is_err());
        assert!(store.describe("identity").contains("io.macwin.MacWin"));
    }
}
