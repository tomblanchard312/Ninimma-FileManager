// Ninimma modification: independent project branding and integration.
//! The index key in the operating system's credential store.
//!
//! Each platform has one store, chosen deliberately:
//! - Windows: Credential Manager (protected with DPAPI for the user).
//! - macOS: the login Keychain.
//! - Linux and FreeBSD: the Secret Service (GNOME Keyring, KWallet).
//!
//! The desktop login unlocks these, so the index opens without a prompt.
//! Ninimma never sees, asks for or checks the login password.

use super::secret::SecretBytes;
use std::fmt;
use std::sync::Arc;
#[cfg(test)]
use std::{collections::HashMap, sync::Mutex};
use zeroize::Zeroize;

/// Versioned, so a future key format can live alongside this one.
pub const SERVICE: &str = "ninimma/index-key/v1";

pub trait IndexKeyStore: Send + Sync {
    fn load(&self, database_id: &str) -> Result<Option<SecretBytes>, KeyStoreError>;
    fn store(&self, database_id: &str, key: &SecretBytes) -> Result<(), KeyStoreError>;
    fn delete(&self, database_id: &str) -> Result<(), KeyStoreError>;
}

/// What went wrong, without the underlying error: some carry the stored
/// bytes, which could be the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStoreError {
    /// No credential store is running or installed.
    Unavailable,
    /// The store exists but is locked or refused access.
    Locked,
    /// Something is stored, but it isn't a valid key.
    Corrupt,
    /// Any other failure in the store.
    Failed,
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyStoreError::Unavailable => "no system keyring is available",
            KeyStoreError::Locked => "the system keyring is locked or refused access",
            KeyStoreError::Corrupt => "the saved index key is damaged",
            KeyStoreError::Failed => "the system keyring reported an error",
        })
    }
}

impl std::error::Error for KeyStoreError {}

/// Maps a keyring error to ours, wiping any bytes it carries.
fn map_error(e: keyring_core::Error) -> KeyStoreError {
    use keyring_core::Error as E;
    match e {
        E::NoStorageAccess(_) => KeyStoreError::Locked,
        E::BadEncoding(mut bytes) | E::BadDataFormat(mut bytes, _) => {
            bytes.zeroize();
            KeyStoreError::Corrupt
        }
        E::NoDefaultStore | E::NotSupportedByStore(_) => KeyStoreError::Unavailable,
        E::PlatformFailure(_) => KeyStoreError::Failed,
        _ => KeyStoreError::Failed,
    }
}

/// The platform credential store.
pub struct OsKeyStore {
    store: Arc<keyring_core::CredentialStore>,
}

impl OsKeyStore {
    /// Connects to the platform store. Fails when there isn't one, e.g. no
    /// Secret Service on a minimal Linux or FreeBSD desktop.
    pub fn connect() -> Result<Self, KeyStoreError> {
        Ok(Self {
            store: platform_store()?,
        })
    }

    fn entry(&self, database_id: &str) -> Result<keyring_core::Entry, KeyStoreError> {
        self.store
            .build(SERVICE, database_id, None)
            .map_err(map_error)
    }
}

#[cfg(windows)]
fn platform_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreError> {
    windows_native_keyring_store::Store::new()
        .map(|s| s as Arc<keyring_core::CredentialStore>)
        .map_err(|_| KeyStoreError::Unavailable)
}

#[cfg(target_os = "macos")]
fn platform_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreError> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|s| s as Arc<keyring_core::CredentialStore>)
        .map_err(|_| KeyStoreError::Unavailable)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_store() -> Result<Arc<keyring_core::CredentialStore>, KeyStoreError> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|s| s as Arc<keyring_core::CredentialStore>)
        .map_err(|_| KeyStoreError::Unavailable)
}

impl IndexKeyStore for OsKeyStore {
    fn load(&self, database_id: &str) -> Result<Option<SecretBytes>, KeyStoreError> {
        match self.entry(database_id)?.get_secret() {
            Ok(bytes) => Ok(Some(SecretBytes::from_vec(bytes))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn store(&self, database_id: &str, key: &SecretBytes) -> Result<(), KeyStoreError> {
        self.entry(database_id)?
            .set_secret(key.expose())
            .map_err(map_error)
    }

    fn delete(&self, database_id: &str) -> Result<(), KeyStoreError> {
        match self.entry(database_id)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

/// A key store held in memory, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, Vec<u8>>>,
    /// Makes every call fail like this, to test error handling.
    pub fail_with: Mutex<Option<KeyStoreError>>,
}

#[cfg(test)]
impl MemoryKeyStore {
    fn check(&self) -> Result<(), KeyStoreError> {
        match *self.fail_with.lock().unwrap() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
impl IndexKeyStore for MemoryKeyStore {
    fn load(&self, database_id: &str) -> Result<Option<SecretBytes>, KeyStoreError> {
        self.check()?;
        Ok(self
            .keys
            .lock()
            .unwrap()
            .get(database_id)
            .map(|k| SecretBytes::from_vec(k.clone())))
    }

    fn store(&self, database_id: &str, key: &SecretBytes) -> Result<(), KeyStoreError> {
        self.check()?;
        let old = self
            .keys
            .lock()
            .unwrap()
            .insert(database_id.to_string(), key.expose().to_vec());
        if let Some(mut old) = old {
            old.zeroize();
        }
        Ok(())
    }

    fn delete(&self, database_id: &str) -> Result<(), KeyStoreError> {
        self.check()?;
        if let Some(mut old) = self.keys.lock().unwrap().remove(database_id) {
            old.zeroize();
        }
        Ok(())
    }
}

#[cfg(test)]
impl Drop for MemoryKeyStore {
    fn drop(&mut self) {
        if let Ok(keys) = self.keys.get_mut() {
            keys.values_mut().for_each(|k| k.zeroize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::secret::KEY_LEN;

    #[test]
    fn memory_store_round_trips_and_deletes() {
        let store = MemoryKeyStore::default();
        assert!(store.load("metadata").unwrap().is_none());
        let key = SecretBytes::random(KEY_LEN).unwrap();
        store.store("metadata", &key).unwrap();
        assert!(store.load("metadata").unwrap().unwrap() == key);
        store.delete("metadata").unwrap();
        assert!(store.load("metadata").unwrap().is_none());
        // Deleting what isn't there is fine.
        store.delete("metadata").unwrap();
    }

    #[test]
    fn errors_never_carry_the_key() {
        let leaked = keyring_core::Error::BadEncoding(b"s3cr3t-key-bytes".to_vec());
        let ours = map_error(leaked);
        assert_eq!(ours, KeyStoreError::Corrupt);
        let shown = format!("{ours} {ours:?}");
        assert!(!shown.contains("s3cr3t"));
    }

    #[test]
    fn a_failing_store_reports_a_bounded_state() {
        let store = MemoryKeyStore::default();
        *store.fail_with.lock().unwrap() = Some(KeyStoreError::Locked);
        assert_eq!(store.load("metadata").err(), Some(KeyStoreError::Locked));
    }

    /// Talks to the real keyring, so it's opt-in:
    /// `cargo test -- --ignored os_key_store` on a desktop session.
    #[test]
    #[ignore]
    fn os_key_store_round_trip() {
        let store = OsKeyStore::connect().expect("a system keyring");
        let id = "ninimma-test-key";
        let key = SecretBytes::random(KEY_LEN).unwrap();
        store.store(id, &key).unwrap();
        assert!(store.load(id).unwrap().unwrap() == key);
        store.delete(id).unwrap();
        assert!(store.load(id).unwrap().is_none());
    }
}
