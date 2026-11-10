//! Linux [`Protector`] for the input history (T3.6): AES-256-GCM with a key kept in the
//! Secret Service (`tasks/plan.md` section 7). Counterpart of `windows::dpapi`.
//!
//! - The key is 256 random bits, created once on first use and stored as a keyring-core
//!   entry (service [`KEY_SERVICE`], user [`KEY_USER`]) in the default store, i.e. the
//!   Secret Service installed by [`super::secret::install_default_store`]. It is cached in
//!   memory for the process lifetime after the first successful read.
//! - If the key exists but cannot be read (locked keyring, prompt dismissed, bus error), the
//!   call fails; a new key is only generated when the store reports "no entry", so a
//!   temporary failure never orphans existing history.
//! - The key source is a trait ([`KeySource`]) so tests use [`MemoryKeySource`].
//!
//! Sealed format, version 1:
//!
//! ```text
//! [0x01][12-byte random nonce][ciphertext || 16-byte GCM tag]
//! ```
//!
//! The AAD is `"v2c-history"` plus the version byte, so a blob cannot be relabelled to a
//! different version. Random 96-bit nonces are fine for the volume of a local history.
//!
//! Keys and plaintext never appear in `Debug` output or in errors.

use std::collections::HashMap;
use std::sync::Mutex;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use v2c_core::history::Protector;
use v2c_core::{Result, V2cError};

/// Format version written by [`AeadProtector::protect`].
pub const FORMAT_V1: u8 = 1;
/// keyring-core entry "service" for the history key. Distinct from the API-key service
/// (`v2c`), so no `CloudService::id` can collide with it.
pub const KEY_SERVICE: &str = "v2c-history";
/// keyring-core entry "user" for the history key.
pub const KEY_USER: &str = "history-key";
/// Label shown in the keyring UI (Seahorse / KWallet).
const KEY_LABEL: &str = "v2c history encryption key";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const AAD_PREFIX: &[u8] = b"v2c-history";

/// A 256-bit history key. `Debug` is redacted; no `Display`, no `PartialEq`.
#[derive(Clone)]
pub struct HistoryKey([u8; KEY_LEN]);

impl HistoryKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// A fresh key from the OS RNG.
    pub fn generate() -> Result<Self> {
        let mut k = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut k)
            .map_err(|_| storage("history key: system RNG failed"))?;
        Ok(Self(k))
    }
}

impl std::fmt::Debug for HistoryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryKey(<redacted>)")
    }
}

/// Where the history key comes from.
pub trait KeySource: Send + Sync {
    /// Returns the key, creating it on first use if the source supports that.
    fn history_key(&self) -> Result<HistoryKey>;
}

/// Fixed in-memory key (tests).
#[derive(Debug, Clone)]
pub struct MemoryKeySource(HistoryKey);

impl MemoryKeySource {
    pub fn new(key: HistoryKey) -> Self {
        Self(key)
    }
}

impl KeySource for MemoryKeySource {
    fn history_key(&self) -> Result<HistoryKey> {
        Ok(self.0.clone())
    }
}

/// History key in the keyring-core default store (the Secret Service on Linux).
#[derive(Default)]
pub struct SecretServiceKeySource {
    cache: Mutex<Option<HistoryKey>>,
}

impl SecretServiceKeySource {
    pub fn new() -> Self {
        Self::default()
    }

    fn load_or_create() -> Result<HistoryKey> {
        let mods = HashMap::from([("label", KEY_LABEL)]);
        let entry = keyring_core::Entry::new_with_modifiers(KEY_SERVICE, KEY_USER, &mods)
            .map_err(key_err)?;
        match entry.get_secret() {
            Ok(bytes) => {
                let arr: [u8; KEY_LEN] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| storage("history key: stored key has the wrong length"))?;
                Ok(HistoryKey(arr))
            }
            Err(keyring_core::Error::NoEntry) => {
                let key = HistoryKey::generate()?;
                entry.set_secret(&key.0).map_err(key_err)?;
                Ok(key)
            }
            Err(e) => Err(key_err(e)),
        }
    }
}

impl std::fmt::Debug for SecretServiceKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached = self.cache.lock().map(|c| c.is_some()).unwrap_or(false);
        f.debug_struct("SecretServiceKeySource")
            .field("cached", &cached)
            .finish()
    }
}

impl KeySource for SecretServiceKeySource {
    fn history_key(&self) -> Result<HistoryKey> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| storage("history key: cache lock poisoned"))?;
        if let Some(k) = cache.as_ref() {
            return Ok(k.clone());
        }
        let key = Self::load_or_create()?;
        *cache = Some(key.clone());
        Ok(key)
    }
}

/// AES-256-GCM [`Protector`] over a [`KeySource`].
pub struct AeadProtector<S> {
    source: S,
    rng: SystemRandom,
}

/// The production Linux history protector.
pub type SecretServiceProtector = AeadProtector<SecretServiceKeySource>;

impl SecretServiceProtector {
    /// Key from the Secret Service; requires the keyring-core default store to be installed
    /// (it is read lazily, on the first protect/unprotect).
    pub fn new() -> Self {
        Self::with_source(SecretServiceKeySource::new())
    }
}

impl Default for SecretServiceProtector {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: KeySource> AeadProtector<S> {
    pub fn with_source(source: S) -> Self {
        Self {
            source,
            rng: SystemRandom::new(),
        }
    }

    fn key(&self) -> Result<LessSafeKey> {
        let k = self.source.history_key()?;
        let unbound =
            UnboundKey::new(&AES_256_GCM, &k.0).map_err(|_| storage("history key: invalid key"))?;
        Ok(LessSafeKey::new(unbound))
    }
}

impl<S> std::fmt::Debug for AeadProtector<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AeadProtector")
            .field("alg", &"AES-256-GCM")
            .field("format", &FORMAT_V1)
            .finish_non_exhaustive()
    }
}

fn aad(version: u8) -> [u8; AAD_PREFIX.len() + 1] {
    let mut a = [0u8; AAD_PREFIX.len() + 1];
    a[..AAD_PREFIX.len()].copy_from_slice(AAD_PREFIX);
    a[AAD_PREFIX.len()] = version;
    a
}

impl<S: KeySource> Protector for AeadProtector<S> {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let key = self.key()?;
        let mut nonce = [0u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce)
            .map_err(|_| storage("history: system RNG failed"))?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + plain.len() + TAG_LEN);
        out.push(FORMAT_V1);
        out.extend_from_slice(&nonce);
        let mut body = plain.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(FORMAT_V1)),
            &mut body,
        )
        .map_err(|_| storage("history: encryption failed"))?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        let (&version, rest) = sealed
            .split_first()
            .ok_or_else(|| storage("history: sealed data is empty"))?;
        if version != FORMAT_V1 {
            return Err(V2cError::Storage(format!(
                "history: unsupported sealed format version {version}"
            )));
        }
        if rest.len() < NONCE_LEN + TAG_LEN {
            return Err(storage("history: sealed data is too short"));
        }
        let (nonce, body) = rest.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("split_at gives NONCE_LEN");
        let key = self.key()?;
        let mut buf = body.to_vec();
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(version)),
                &mut buf,
            )
            .map_err(|_| storage("history: decryption failed (wrong key or corrupted data)"))?;
        Ok(plain.to_vec())
    }
}

fn storage(msg: &'static str) -> V2cError {
    V2cError::Storage(msg.into())
}

/// keyring-core's `Display` never includes secret bytes; we only add a prefix.
fn key_err(e: keyring_core::Error) -> V2cError {
    V2cError::Storage(format!("history key: secret service: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const PLAIN: &[u8] = "hello 你好 secret-text".as_bytes();

    fn protector(byte: u8) -> AeadProtector<MemoryKeySource> {
        AeadProtector::with_source(MemoryKeySource::new(HistoryKey::from_bytes(
            [byte; KEY_LEN],
        )))
    }

    #[test]
    fn round_trip() {
        let p = protector(7);
        let sealed = p.protect(PLAIN).unwrap();
        assert_eq!(p.unprotect(&sealed).unwrap(), PLAIN);
        assert_eq!(sealed.len(), 1 + NONCE_LEN + PLAIN.len() + TAG_LEN);
        assert!(!sealed.windows(PLAIN.len()).any(|w| w == PLAIN));
    }

    #[test]
    fn empty_plaintext_round_trips() {
        let p = protector(1);
        let sealed = p.protect(b"").unwrap();
        assert_eq!(sealed.len(), 1 + NONCE_LEN + TAG_LEN);
        assert_eq!(p.unprotect(&sealed).unwrap(), b"");
    }

    #[test]
    fn empty_and_short_sealed_input_is_rejected() {
        let p = protector(1);
        assert!(p.unprotect(b"").is_err());
        assert!(p.unprotect(&[FORMAT_V1]).is_err());
        assert!(p
            .unprotect(&[FORMAT_V1; 1 + NONCE_LEN + TAG_LEN - 1])
            .is_err());
    }

    #[test]
    fn version_byte_is_first_and_checked() {
        let p = protector(2);
        let mut sealed = p.protect(PLAIN).unwrap();
        assert_eq!(sealed[0], FORMAT_V1);
        sealed[0] = 2;
        let err = p.unprotect(&sealed).unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
        sealed[0] = 0;
        assert!(p.unprotect(&sealed).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let p = protector(3);
        let sealed = p.protect(PLAIN).unwrap();
        for i in 1..sealed.len() {
            let mut t = sealed.clone();
            t[i] ^= 0x01;
            assert!(p.unprotect(&t).is_err(), "flip at byte {i} not detected");
        }
        let mut truncated = sealed.clone();
        truncated.pop();
        assert!(p.unprotect(&truncated).is_err());
        let mut extended = sealed;
        extended.push(0);
        assert!(p.unprotect(&extended).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = protector(4).protect(PLAIN).unwrap();
        let err = protector(5).unprotect(&sealed).unwrap_err().to_string();
        assert!(err.contains("decryption failed"), "{err}");
    }

    #[test]
    fn nonces_are_unique() {
        let p = protector(6);
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let sealed = p.protect(PLAIN).unwrap();
            assert!(
                seen.insert(sealed[1..1 + NONCE_LEN].to_vec()),
                "nonce reused"
            );
        }
    }

    #[test]
    fn no_key_or_plaintext_in_debug_or_errors() {
        let key = HistoryKey::from_bytes([0xAB; KEY_LEN]);
        let dbg = format!("{key:?}");
        assert_eq!(dbg, "HistoryKey(<redacted>)");
        let src = MemoryKeySource::new(key);
        let p = AeadProtector::with_source(src.clone());
        let all = format!("{src:?} {p:?} {:?}", SecretServiceKeySource::new());
        for needle in ["171", "0xab", "AB", "ab, ab"] {
            assert!(!all.contains(needle), "{needle} in {all}");
        }

        let sealed = p.protect(PLAIN).unwrap();
        let mut t = sealed.clone();
        t[5] ^= 1;
        let other = protector(9);
        let errs = [
            p.unprotect(&t).unwrap_err(),
            other.unprotect(&sealed).unwrap_err(),
            p.unprotect(b"").unwrap_err(),
            p.unprotect(&[3, 1, 2]).unwrap_err(),
        ];
        for e in errs {
            let s = format!("{e} {e:?}");
            assert!(!s.contains("secret-text") && !s.contains("hello"), "{s}");
            assert!(
                !s.contains("171") && !s.to_lowercase().contains("ab, ab"),
                "{s}"
            );
        }
    }

    #[test]
    fn works_as_history_protector() {
        use std::sync::Arc;
        use v2c_core::config::HistoryConfig;
        use v2c_core::history::History;
        let h = History::open_in_memory(Arc::new(protector(8)), HistoryConfig::default());
        assert!(h.is_ok());
    }

    /// Without an installed default store, the Secret Service source fails cleanly (no
    /// panic, no key generated).
    #[test]
    fn secret_service_source_without_store_errors() {
        if keyring_core::get_default_store().is_some() {
            return;
        }
        let err = SecretServiceProtector::new().protect(PLAIN).unwrap_err();
        assert!(err.to_string().contains("history key"), "{err}");
    }

    /// WRITES a test item into the real login keyring and deletes it again. Not authorised to
    /// run by default: needs `V2C_SECRET_REAL_WRITE=1` on top of `--ignored`.
    /// `V2C_SECRET_REAL_WRITE=1 cargo test -p v2c-platform -- --ignored secret_real_write`
    #[test]
    #[ignore = "writes to the real keyring; also needs V2C_SECRET_REAL_WRITE=1"]
    fn secret_real_write_round_trip() {
        if std::env::var("V2C_SECRET_REAL_WRITE").as_deref() != Ok("1") {
            println!("skipped: set V2C_SECRET_REAL_WRITE=1 to write a test item");
            return;
        }
        super::super::secret::install_default_store().expect("secret service store");
        let mods = HashMap::from([("label", "v2c test item (safe to delete)")]);
        let entry = keyring_core::Entry::new_with_modifiers("v2c-test", "secret-real-write", &mods)
            .unwrap();
        let key = HistoryKey::generate().unwrap();
        entry.set_secret(&key.0).unwrap();
        let got = entry.get_secret().unwrap();
        entry.delete_credential().unwrap();
        assert_eq!(got, key.0);
        let p =
            AeadProtector::with_source(MemoryKeySource::new(HistoryKey(got.try_into().unwrap())));
        let sealed = p.protect(PLAIN).unwrap();
        assert_eq!(p.unprotect(&sealed).unwrap(), PLAIN);
    }
}
