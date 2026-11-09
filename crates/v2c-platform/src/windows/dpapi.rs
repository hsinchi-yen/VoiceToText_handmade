//! Windows DPAPI [`Protector`] for the input history (T1.9).
//!
//! `CryptProtectData` with the current user's credentials (no `CRYPTPROTECT_LOCAL_MACHINE`),
//! so only the same Windows user can decrypt; `CRYPTPROTECT_UI_FORBIDDEN` because this runs
//! without a UI prompt. A fixed application entropy keeps other DPAPI users from decrypting
//! v2c blobs by accident (it is not a secret). Output buffers are freed with `LocalFree`.

use v2c_core::history::Protector;
use v2c_core::{Result, V2cError};
use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// Default entropy for v2c history blobs.
const ENTROPY: &[u8] = b"v2c-history-v1";

/// [`Protector`] backed by DPAPI, bound to the current Windows user.
#[derive(Debug, Clone)]
pub struct DpapiProtector {
    entropy: Vec<u8>,
}

impl Default for DpapiProtector {
    fn default() -> Self {
        Self::new()
    }
}

impl DpapiProtector {
    pub fn new() -> Self {
        Self::with_entropy(ENTROPY)
    }

    /// Uses a different entropy value; blobs only decrypt with the same one.
    pub fn with_entropy(entropy: &[u8]) -> Self {
        Self {
            entropy: entropy.to_vec(),
        }
    }

    fn entropy_blob(&self) -> CRYPT_INTEGER_BLOB {
        blob_in(&self.entropy)
    }
}

impl Protector for DpapiProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let input = blob_in(plain);
        let entropy = self.entropy_blob();
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` and `entropy` point at live slices for the call (DPAPI only reads
        // them); `out` is a valid out pointer that DPAPI fills with a LocalAlloc'd buffer.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(os_error("CryptProtectData"));
        }
        // SAFETY: on success `out` holds a buffer allocated by DPAPI.
        Ok(unsafe { take_blob(out) })
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        let input = blob_in(sealed);
        let entropy = self.entropy_blob();
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`; no description is requested (null out pointer).
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(os_error("CryptUnprotectData"));
        }
        // SAFETY: on success `out` holds a buffer allocated by DPAPI.
        Ok(unsafe { take_blob(out) })
    }
}

/// Read-only view of `data` as a DPAPI input blob.
fn blob_in(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        // DPAPI does not write through input blobs.
        pbData: data.as_ptr().cast_mut(),
    }
}

/// Copies a DPAPI output blob into a `Vec` and frees it.
///
/// # Safety
/// `blob` must come from a successful `CryptProtectData`/`CryptUnprotectData` call.
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI returned `cbData` readable bytes at `pbData`.
    let v = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: the buffer was allocated with LocalAlloc by DPAPI and is freed exactly once.
    unsafe { LocalFree(blob.pbData.cast()) };
    v
}

/// Error with the Win32 code only (never data).
fn os_error(call: &str) -> V2cError {
    // SAFETY: no arguments.
    let code = unsafe { GetLastError() };
    V2cError::Storage(format!("{call} failed (Win32 error {code})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTENCE: &str = "明天下午三點跟 Alice 開會";

    #[test]
    fn round_trip() {
        let p = DpapiProtector::new();
        let sealed = p.protect(SENTENCE.as_bytes()).unwrap();
        assert_ne!(sealed, SENTENCE.as_bytes());
        assert!(!sealed
            .windows(SENTENCE.len())
            .any(|w| w == SENTENCE.as_bytes()));
        assert_eq!(p.unprotect(&sealed).unwrap(), SENTENCE.as_bytes());
    }

    #[test]
    fn empty_input_round_trips() {
        let p = DpapiProtector::new();
        let sealed = p.protect(b"").unwrap();
        assert!(!sealed.is_empty());
        assert_eq!(p.unprotect(&sealed).unwrap(), b"");
    }

    #[test]
    fn wrong_entropy_or_garbage_fails() {
        let sealed = DpapiProtector::new().protect(b"hello").unwrap();
        assert!(DpapiProtector::with_entropy(b"other")
            .unprotect(&sealed)
            .is_err());
        assert!(DpapiProtector::new()
            .unprotect(b"not a dpapi blob")
            .is_err());
    }

    #[test]
    fn same_plaintext_encrypts_differently() {
        let p = DpapiProtector::new();
        assert_ne!(p.protect(b"hello").unwrap(), p.protect(b"hello").unwrap());
    }
}
