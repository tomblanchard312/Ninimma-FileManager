// Ninimma modification: independent project branding and integration.
//! Key material that is wiped when dropped and never printed.

use std::fmt;
use zeroize::Zeroize;

/// Length of an index database key: 256 bits.
pub const KEY_LEN: usize = 32;

/// Secret bytes (an index key). The memory is locked against swapping where
/// the OS allows it, zeroed on drop, and `Debug` shows only that it's a
/// secret, so it can't leak into logs or panic messages.
pub struct SecretBytes {
    bytes: Box<[u8]>,
    locked: bool,
}

impl SecretBytes {
    /// A new random key of `len` bytes from the OS random number generator.
    pub fn random(len: usize) -> Result<Self, getrandom::Error> {
        let mut secret = Self::zeroed(len);
        getrandom::fill(&mut secret.bytes)?;
        Ok(secret)
    }

    /// Takes ownership of `bytes`, wiping the vector it came in.
    pub fn from_vec(mut bytes: Vec<u8>) -> Self {
        let mut secret = Self::zeroed(bytes.len());
        secret.bytes.copy_from_slice(&bytes);
        bytes.zeroize();
        secret
    }

    fn zeroed(len: usize) -> Self {
        let bytes = vec![0u8; len].into_boxed_slice();
        let locked = lock_memory(&bytes);
        Self { bytes, locked }
    }

    pub fn expose(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.bytes.zeroize();
        if self.locked {
            unlock_memory(&self.bytes);
        }
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([redacted])")
    }
}

impl PartialEq for SecretBytes {
    /// Constant-time comparison.
    fn eq(&self, other: &Self) -> bool {
        self.bytes.len() == other.bytes.len()
            && self
                .bytes
                .iter()
                .zip(other.bytes.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// Keeps the pages holding a key out of swap. Best effort: it fails under a
/// low RLIMIT_MEMLOCK, and the key is still wiped on drop either way.
#[cfg(unix)]
fn lock_memory(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        // SAFETY: the pointer and length describe a live allocation we own.
        && unsafe { libc::mlock(bytes.as_ptr().cast(), bytes.len()) } == 0
}

#[cfg(unix)]
fn unlock_memory(bytes: &[u8]) {
    // SAFETY: as in lock_memory; unlocking memory we locked.
    unsafe {
        libc::munlock(bytes.as_ptr().cast(), bytes.len());
    }
}

#[cfg(windows)]
fn lock_memory(bytes: &[u8]) -> bool {
    use windows_sys::Win32::System::Memory::VirtualLock;
    !bytes.is_empty()
        // SAFETY: the pointer and length describe a live allocation we own.
        && unsafe { VirtualLock(bytes.as_ptr() as _, bytes.len()) } != 0
}

#[cfg(windows)]
fn unlock_memory(bytes: &[u8]) {
    use windows_sys::Win32::System::Memory::VirtualUnlock;
    // SAFETY: as in lock_memory; unlocking memory we locked.
    unsafe {
        VirtualUnlock(bytes.as_ptr() as _, bytes.len());
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_memory(_bytes: &[u8]) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn unlock_memory(_bytes: &[u8]) {}

/// Lowercase hex, for key formats that need text. The caller wipes it.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_secret() {
        let secret = SecretBytes::from_vec(b"hunter2-very-secret".to_vec());
        let shown = format!("{secret:?} {:?}", Some(&secret));
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn random_keys_differ_and_have_the_right_length() {
        let a = SecretBytes::random(KEY_LEN).unwrap();
        let b = SecretBytes::random(KEY_LEN).unwrap();
        assert_eq!(a.len(), KEY_LEN);
        assert!(a != b);
        assert!(a == SecretBytes::from_vec(a.expose().to_vec()));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0x7f, 0x80, 0xff];
        assert_eq!(to_hex(&bytes), "00017f80ff");
        assert_eq!(from_hex("00017f80ff").unwrap(), bytes);
        assert_eq!(from_hex("0g"), None);
        assert_eq!(from_hex("abc"), None);
    }
}
