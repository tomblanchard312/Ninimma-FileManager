// Ninimma modification: independent project branding and integration.
//! A passphrase-protected index key, for systems with no usable credential
//! store.
//!
//! The index key is random, as always. The passphrase only wraps it:
//! Argon2id turns the passphrase and a random salt into a wrapping key,
//! which seals the index key with XChaCha20-Poly1305. The vault file holds
//! just the salt, the Argon2 settings, the nonce and the sealed key, never
//! the passphrase or anything that reverses to it.

use super::permissions::{check_private_file, create_private_file};
use super::secret::{from_hex, to_hex, SecretBytes, KEY_LEN};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::XChaCha20Poly1305;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &str = "ninimma-index-vault v1";
pub const FILE_NAME: &str = "key.vault";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
/// Short passphrases are too easy to guess offline from a stolen vault.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

/// Argon2id settings: 64 MiB, 3 passes, 1 lane (above OWASP's minimums;
/// about half a second on a typical laptop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KdfParams {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

const DEFAULT_KDF: KdfParams = KdfParams {
    m_cost_kib: 64 * 1024,
    t_cost: 3,
    p_cost: 1,
};

impl KdfParams {
    /// Settings read from a file are bounded, so a tampered vault can't make
    /// unlocking use gigabytes of memory or run for minutes.
    fn is_reasonable(&self) -> bool {
        (8 * 1024..=1024 * 1024).contains(&self.m_cost_kib)
            && (1..=10).contains(&self.t_cost)
            && (1..=4).contains(&self.p_cost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    WrongPassphrase,
    PassphraseTooShort,
    /// The vault file is missing pieces or has impossible settings.
    Corrupt,
    AlreadyExists,
    Missing,
    Io,
    Random,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VaultError::WrongPassphrase => "that passphrase is wrong",
            VaultError::PassphraseTooShort => "the passphrase must be at least 8 characters",
            VaultError::Corrupt => "the index key file is damaged",
            VaultError::AlreadyExists => "an index key file already exists",
            VaultError::Missing => "there is no index key file",
            VaultError::Io => "the index key file couldn't be read or written",
            VaultError::Random => "the system's random number generator failed",
        })
    }
}

impl std::error::Error for VaultError {}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

pub fn exists(dir: &Path) -> bool {
    fs::symlink_metadata(path_in(dir)).is_ok()
}

/// Everything that's authenticated alongside the sealed key, so the stored
/// settings and database name can't be swapped without unlocking failing.
fn associated_data(database_id: &str, kdf: &KdfParams, salt: &[u8]) -> Vec<u8> {
    format!(
        "{MAGIC}|{database_id}|argon2id m={} t={} p={}|{}",
        kdf.m_cost_kib,
        kdf.t_cost,
        kdf.p_cost,
        to_hex(salt)
    )
    .into_bytes()
}

fn wrapping_key(
    passphrase: &str,
    salt: &[u8],
    kdf: &KdfParams,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let params = Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|_| VaultError::Corrupt)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| VaultError::Corrupt)?;
    Ok(key)
}

/// Makes a new random index key, seals it with `passphrase` into a new
/// vault file in `dir`, and returns the key.
pub fn create(dir: &Path, database_id: &str, passphrase: &str) -> Result<SecretBytes, VaultError> {
    create_with(dir, database_id, passphrase, DEFAULT_KDF)
}

fn create_with(
    dir: &Path,
    database_id: &str,
    passphrase: &str,
    kdf: KdfParams,
) -> Result<SecretBytes, VaultError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(VaultError::PassphraseTooShort);
    }
    let key = SecretBytes::random(KEY_LEN).map_err(|_| VaultError::Random)?;
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| VaultError::Random)?;
    getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;

    let wrap = wrapping_key(passphrase, &salt, &kdf)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(wrap.as_ref()).map_err(|_| VaultError::Corrupt)?;
    let aad = associated_data(database_id, &kdf, &salt);
    let sealed = cipher
        .encrypt(
            &nonce.into(),
            Payload {
                msg: key.expose(),
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Corrupt)?;

    let text = format!(
        "{MAGIC}\nkdf=argon2id m={} t={} p={}\nsalt={}\nnonce={}\nsealed={}\n",
        kdf.m_cost_kib,
        kdf.t_cost,
        kdf.p_cost,
        to_hex(&salt),
        to_hex(&nonce),
        to_hex(&sealed)
    );
    // Written to a new private file, then renamed into place, so a crash
    // never leaves a half-written vault where the real one should be.
    let path = path_in(dir);
    if exists(dir) {
        return Err(VaultError::AlreadyExists);
    }
    let temp = dir.join(format!("{FILE_NAME}.new"));
    let _ = fs::remove_file(&temp);
    let mut file = create_private_file(&temp).map_err(|_| VaultError::Io)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() || fs::rename(&temp, &path).is_err() {
        let _ = fs::remove_file(&temp);
        return Err(VaultError::Io);
    }
    Ok(key)
}

/// Unseals the index key with `passphrase`.
pub fn unlock(dir: &Path, database_id: &str, passphrase: &str) -> Result<SecretBytes, VaultError> {
    let path = path_in(dir);
    if !exists(dir) {
        return Err(VaultError::Missing);
    }
    check_private_file(&path).map_err(|_| VaultError::Io)?;
    // A real vault is a few hundred bytes; don't read anything huge.
    if fs::metadata(&path).map_err(|_| VaultError::Io)?.len() > 4096 {
        return Err(VaultError::Corrupt);
    }
    let text = fs::read_to_string(&path).map_err(|_| VaultError::Corrupt)?;
    let vault = parse(&text).ok_or(VaultError::Corrupt)?;

    let wrap = wrapping_key(passphrase, &vault.salt, &vault.kdf)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(wrap.as_ref()).map_err(|_| VaultError::Corrupt)?;
    let aad = associated_data(database_id, &vault.kdf, &vault.salt);
    let nonce: [u8; NONCE_LEN] = vault
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Corrupt)?;
    let mut key = cipher
        .decrypt(
            &nonce.into(),
            Payload {
                msg: &vault.sealed,
                aad: &aad,
            },
        )
        // Authentication failure: wrong passphrase (or a tampered file,
        // which can't be told apart and is just as unusable).
        .map_err(|_| VaultError::WrongPassphrase)?;
    if key.len() != KEY_LEN {
        key.zeroize();
        return Err(VaultError::Corrupt);
    }
    Ok(SecretBytes::from_vec(key))
}

pub fn delete(dir: &Path) -> Result<(), VaultError> {
    match fs::remove_file(path_in(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(VaultError::Io),
    }
}

struct ParsedVault {
    kdf: KdfParams,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    sealed: Vec<u8>,
}

fn parse(text: &str) -> Option<ParsedVault> {
    let mut lines = text.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let mut field = |name: &str| lines.next()?.strip_prefix(name)?.strip_prefix('=');
    let kdf_text = field("kdf")?.strip_prefix("argon2id ")?;
    let salt = from_hex(field("salt")?)?;
    let nonce = from_hex(field("nonce")?)?;
    let sealed = from_hex(field("sealed")?)?;

    let mut values = kdf_text.split(' ').map(|part| part.split_once('='));
    let mut number = |name: &str| -> Option<u32> {
        let (k, v) = values.next()??;
        (k == name).then(|| v.parse().ok())?
    };
    let kdf = KdfParams {
        m_cost_kib: number("m")?,
        t_cost: number("t")?,
        p_cost: number("p")?,
    };
    let sizes_ok =
        salt.len() == SALT_LEN && nonce.len() == NONCE_LEN && sealed.len() == KEY_LEN + 16;
    (kdf.is_reasonable() && sizes_ok).then_some(ParsedVault {
        kdf,
        salt,
        nonce,
        sealed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small Argon2 settings, so the tests run quickly.
    const FAST: KdfParams = KdfParams {
        m_cost_kib: 8 * 1024,
        t_cost: 1,
        p_cost: 1,
    };

    #[test]
    fn the_right_passphrase_unlocks_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        let again = unlock(dir.path(), "metadata", "correct horse battery").unwrap();
        assert!(key == again);
    }

    #[test]
    fn a_wrong_passphrase_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        assert_eq!(
            unlock(dir.path(), "metadata", "correct horse batterY").err(),
            Some(VaultError::WrongPassphrase)
        );
    }

    #[test]
    fn the_vault_is_bound_to_its_database() {
        let dir = tempfile::tempdir().unwrap();
        create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        assert!(unlock(dir.path(), "semantic", "correct horse battery").is_err());
    }

    #[test]
    fn the_file_holds_neither_passphrase_nor_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        let text = fs::read_to_string(path_in(dir.path())).unwrap();
        assert!(!text.contains("correct horse"));
        assert!(!text.contains(&to_hex(key.expose())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path_in(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn short_passphrases_and_overwrites_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            create_with(dir.path(), "metadata", "short", FAST).err(),
            Some(VaultError::PassphraseTooShort)
        );
        create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        assert_eq!(
            create_with(dir.path(), "metadata", "another passphrase", FAST).err(),
            Some(VaultError::AlreadyExists)
        );
    }

    #[test]
    fn tampered_or_unreasonable_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        let path = path_in(dir.path());
        let text = fs::read_to_string(&path).unwrap();

        // Raising the cost to absurd values is refused before any work.
        let greedy = text.replace("m=8192", "m=99999999");
        fs::write(&path, &greedy).unwrap();
        assert_eq!(
            unlock(dir.path(), "metadata", "correct horse battery").err(),
            Some(VaultError::Corrupt)
        );

        // Lowering it changes the authenticated data, so unlocking fails.
        let weaker = text.replace("t=1", "t=2");
        fs::write(&path, &weaker).unwrap();
        assert!(unlock(dir.path(), "metadata", "correct horse battery").is_err());

        fs::write(&path, "garbage").unwrap();
        assert_eq!(
            unlock(dir.path(), "metadata", "correct horse battery").err(),
            Some(VaultError::Corrupt)
        );
    }

    #[test]
    fn deleting_removes_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        create_with(dir.path(), "metadata", "correct horse battery", FAST).unwrap();
        delete(dir.path()).unwrap();
        assert!(!exists(dir.path()));
        assert_eq!(
            unlock(dir.path(), "metadata", "x").err(),
            Some(VaultError::Missing)
        );
        delete(dir.path()).unwrap();
    }
}
