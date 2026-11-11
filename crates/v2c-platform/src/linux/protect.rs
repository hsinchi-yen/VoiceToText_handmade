//! Linux [`Protector`] for the input history (T3.6): AES-256-GCM with a key kept in the
//! Secret Service (`tasks/plan.md` section 7). Counterpart of `windows::dpapi`.
//!
//! ## The key
//! - 256 random bits, stored as a keyring-core entry (service [`KEY_SERVICE`], user
//!   [`KEY_USER`]) in the default store, i.e. the Secret Service installed by
//!   [`super::secret::install_default_store`]. Cached in memory after the first read.
//! - **An existing key is never overwritten.** A new key is only created when the store
//!   reports "no entry" *and* the source's [`KeyPolicy`] is [`KeyPolicy::CreateIfMissing`]
//!   *and* the item is still missing after the default collection was unlocked (some Secret
//!   Service implementations hide items of locked collections from searches). The desktop
//!   uses [`KeyPolicy::MustExist`] whenever the history database already holds rows.
//! - After creating, the key is read back and the stored value is used, so two processes
//!   racing on first use converge on whichever write the store kept. (The keyring store
//!   creates items with `replace = true`; there is no create-if-absent call.)
//! - Any other failure (locked keyring, prompt dismissed, bus error) fails the call and is
//!   remembered for [`FAILURE_BACKOFF`], so a dismissed unlock prompt is not re-shown for
//!   every history write.
//!
//! **Blocking:** fetching the key can show the desktop's unlock prompt and wait for the user.
//! Call `protect` / `unprotect` off the UI thread (the desktop already runs history work on
//! blocking tasks). Concurrent callers in one process wait for the first fetch.
//!
//! ## Sealed format, version 1
//!
//! ```text
//! [0x01][12-byte random nonce][ciphertext || 16-byte GCM tag]
//! ```
//!
//! The AAD is `"v2c-history"` plus the version byte, so a blob cannot be relabelled to a
//! different version. Random 96-bit nonces are fine for the volume of a local history.
//!
//! Keys and plaintext never appear in `Debug` output or in errors. The key and fetched key
//! bytes are wiped on drop (`zeroize`); ring's expanded AES key schedule is not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use v2c_core::history::Protector;
use v2c_core::{Result, V2cError};
use zeroize::{Zeroize, Zeroizing};

/// Format version written by [`AeadProtector::protect`].
pub const FORMAT_V1: u8 = 1;
/// keyring-core entry "service" for the history key. Distinct from the API-key service
/// (`v2c`), so no `CloudService::id` can collide with it.
pub const KEY_SERVICE: &str = "v2c-history";
/// keyring-core entry "user" for the history key.
pub const KEY_USER: &str = "history-key";
/// After a failed key fetch, further calls fail fast for this long.
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(10);
/// Label shown in the keyring UI (Seahorse / KWallet).
const KEY_LABEL: &str = "v2c history encryption key";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const AAD_PREFIX: &[u8] = b"v2c-history";

/// A 256-bit history key. `Debug` is redacted; no `Display`, no `PartialEq`; wiped on drop.
#[derive(Clone)]
pub struct HistoryKey([u8; KEY_LEN]);

impl HistoryKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// A fresh key from the OS RNG.
    pub fn generate() -> Result<Self> {
        let mut k = Self([0u8; KEY_LEN]);
        SystemRandom::new()
            .fill(&mut k.0)
            .map_err(|_| storage("history key: system RNG failed"))?;
        Ok(k)
    }

    /// Parses stored key bytes; anything but 32 bytes is an error (never "repaired").
    fn from_stored(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| storage("history key: stored key has the wrong length"))?;
        Ok(Self(arr))
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for HistoryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for HistoryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryKey(<redacted>)")
    }
}

/// Where the history key comes from.
pub trait KeySource: Send + Sync {
    /// Returns the key, creating it on first use if the source's policy allows that.
    fn history_key(&self) -> Result<HistoryKey>;
}

impl<T: KeySource + ?Sized> KeySource for Arc<T> {
    fn history_key(&self) -> Result<HistoryKey> {
        (**self).history_key()
    }
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

/// Whether a missing key may be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPolicy {
    /// First use: create the key if the store has none.
    CreateIfMissing,
    /// Encrypted data already exists: a missing key is an error, never a new key.
    MustExist,
}

/// Raw storage for the key bytes (the Secret Service, or memory in tests).
pub trait KeyVault: Send + Sync {
    /// `Ok(None)` when the store has no such item.
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    /// Writes the key (creates the item).
    fn store(&self, key: &[u8]) -> Result<()>;
    /// Makes sure the collection new items go into is unlocked (may prompt the user), so a
    /// following [`KeyVault::load`] also sees items a locked collection was hiding.
    fn prepare_create(&self) -> Result<()>;
}

struct SourceState {
    policy: KeyPolicy,
    key: Option<HistoryKey>,
    failed_at: Option<Instant>,
}

/// [`KeySource`] over a [`KeyVault`], with the create / never-overwrite rules above.
pub struct StoredKeySource<V> {
    vault: V,
    state: Mutex<SourceState>,
}

/// The production key source: the Secret Service.
pub type SecretServiceKeySource = StoredKeySource<SecretServiceVault>;

impl SecretServiceKeySource {
    pub fn new(policy: KeyPolicy) -> Self {
        Self::with_vault(SecretServiceVault, policy)
    }
}

impl<V: KeyVault> StoredKeySource<V> {
    pub fn with_vault(vault: V, policy: KeyPolicy) -> Self {
        Self {
            vault,
            state: Mutex::new(SourceState {
                policy,
                key: None,
                failed_at: None,
            }),
        }
    }

    /// Changes the policy (e.g. after checking whether the history has rows). Clears a
    /// remembered failure.
    pub fn set_policy(&self, policy: KeyPolicy) {
        if let Ok(mut st) = self.state.lock() {
            st.policy = policy;
            st.failed_at = None;
        }
    }

    pub fn policy(&self) -> Option<KeyPolicy> {
        self.state.lock().ok().map(|s| s.policy)
    }

    fn fetch(&self, policy: KeyPolicy) -> Result<HistoryKey> {
        if let Some(bytes) = self.vault.load()? {
            return HistoryKey::from_stored(&bytes);
        }
        if policy == KeyPolicy::MustExist {
            return Err(storage(
                "history key: not found, but the history already holds encrypted entries; \
                 not creating a new key",
            ));
        }
        // Locked collections may hide the item: unlock, then look again before creating.
        self.vault.prepare_create()?;
        if let Some(bytes) = self.vault.load()? {
            return HistoryKey::from_stored(&bytes);
        }
        let fresh = HistoryKey::generate()?;
        self.vault.store(fresh.as_bytes())?;
        drop(fresh);
        // Use what the store kept (another process may have written at the same time).
        match self.vault.load()? {
            Some(bytes) => HistoryKey::from_stored(&bytes),
            None => Err(storage("history key: not found right after creating it")),
        }
    }
}

impl<V: KeyVault> KeySource for StoredKeySource<V> {
    fn history_key(&self) -> Result<HistoryKey> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| storage("history key: state lock poisoned"))?;
        if let Some(k) = st.key.as_ref() {
            return Ok(k.clone());
        }
        if let Some(at) = st.failed_at {
            if at.elapsed() < FAILURE_BACKOFF {
                return Err(storage(
                    "history key: unavailable (recent failure, retrying shortly)",
                ));
            }
        }
        match self.fetch(st.policy) {
            Ok(k) => {
                st.key = Some(k.clone());
                st.failed_at = None;
                Ok(k)
            }
            Err(e) => {
                st.failed_at = Some(Instant::now());
                Err(e)
            }
        }
    }
}

impl<V> std::fmt::Debug for StoredKeySource<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (policy, cached) = self
            .state
            .lock()
            .map(|s| (Some(s.policy), s.key.is_some()))
            .unwrap_or((None, false));
        f.debug_struct("StoredKeySource")
            .field("policy", &policy)
            .field("cached", &cached)
            .finish()
    }
}

/// The history key item in the Secret Service, via the keyring-core default store.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretServiceVault;

impl SecretServiceVault {
    fn entry() -> Result<keyring_core::Entry> {
        let mods = HashMap::from([("label", KEY_LABEL)]);
        keyring_core::Entry::new_with_modifiers(KEY_SERVICE, KEY_USER, &mods).map_err(key_err)
    }
}

impl KeyVault for SecretServiceVault {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match Self::entry()?.get_secret() {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(key_err(e)),
        }
    }

    fn store(&self, key: &[u8]) -> Result<()> {
        Self::entry()?.set_secret(key).map_err(key_err)
    }

    fn prepare_create(&self) -> Result<()> {
        use secret_service::blocking::SecretService;
        use secret_service::EncryptionType;

        let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
        let result = match ss.get_default_collection() {
            Ok(c) => {
                if c.is_locked().map_err(ss_err)? {
                    c.unlock().map_err(ss_err)?;
                }
                Ok(())
            }
            // No default collection yet: nothing can be hidden; the store creates one.
            Err(secret_service::Error::NoResult) => Ok(()),
            Err(e) => Err(ss_err(e)),
        };
        result
    }
}

/// AES-256-GCM [`Protector`] over a [`KeySource`].
pub struct AeadProtector<S> {
    source: S,
    rng: SystemRandom,
}

/// The production Linux history protector.
pub type SecretServiceProtector = AeadProtector<Arc<SecretServiceKeySource>>;

impl<S: KeySource> AeadProtector<S> {
    pub fn with_source(source: S) -> Self {
        Self {
            source,
            rng: SystemRandom::new(),
        }
    }

    fn key(&self) -> Result<LessSafeKey> {
        let k = self.source.history_key()?;
        let unbound = UnboundKey::new(&AES_256_GCM, k.as_bytes())
            .map_err(|_| storage("history key: invalid key"))?;
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

const HEADER_LEN: usize = 1 + NONCE_LEN;

impl<S: KeySource> Protector for AeadProtector<S> {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let key = self.key()?;
        let mut nonce = [0u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce)
            .map_err(|_| storage("history: system RNG failed"))?;
        // Encrypted in place in the output buffer: no extra plaintext copy.
        let mut out = Zeroizing::new(Vec::with_capacity(HEADER_LEN + plain.len() + TAG_LEN));
        out.push(FORMAT_V1);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plain);
        let tag = key
            .seal_in_place_separate_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(FORMAT_V1)),
                &mut out[HEADER_LEN..],
            )
            .map_err(|_| storage("history: encryption failed"))?;
        out.extend_from_slice(tag.as_ref());
        Ok(std::mem::take(&mut *out))
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
        // Decrypted in place; wiped if anything fails, truncated (not copied) on success.
        let mut buf = Zeroizing::new(body.to_vec());
        let n = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(version)),
                &mut buf,
            )
            .map_err(|_| storage("history: decryption failed (wrong key or corrupted data)"))?
            .len();
        // Wipe the tag bytes left after the plaintext, then hand the buffer over.
        buf[n..].zeroize();
        buf.truncate(n);
        Ok(std::mem::take(&mut *buf))
    }
}

fn storage(msg: &'static str) -> V2cError {
    V2cError::Storage(msg.into())
}

/// keyring-core's `Display` never includes secret bytes; we only add a prefix.
fn key_err(e: keyring_core::Error) -> V2cError {
    V2cError::Storage(format!("history key: secret service: {e}"))
}

/// secret-service errors are D-Bus / prompt failures; they carry no secret bytes.
fn ss_err(e: secret_service::Error) -> V2cError {
    V2cError::Storage(format!("history key: secret service: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const PLAIN: &[u8] = "hello 你好 secret-text".as_bytes();

    fn protector(byte: u8) -> AeadProtector<MemoryKeySource> {
        AeadProtector::with_source(MemoryKeySource::new(HistoryKey::from_bytes(
            [byte; KEY_LEN],
        )))
    }

    /// In-memory [`KeyVault`] with knobs for the tricky store behaviours.
    #[derive(Default)]
    struct MemVault {
        item: Mutex<Option<Vec<u8>>>,
        /// Simulates a locked collection hiding the item until `prepare_create`.
        hidden: AtomicBool,
        /// Simulates another process winning the first-use race: `store` keeps this instead.
        race_winner: Option<Vec<u8>>,
        fail_load: AtomicBool,
        loads: AtomicUsize,
        stores: AtomicUsize,
        prepares: AtomicUsize,
    }

    impl MemVault {
        fn with_item(bytes: &[u8]) -> Self {
            let v = Self::default();
            *v.item.lock().unwrap() = Some(bytes.to_vec());
            v
        }
        fn item(&self) -> Option<Vec<u8>> {
            self.item.lock().unwrap().clone()
        }
    }

    impl KeyVault for MemVault {
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if self.fail_load.load(Ordering::SeqCst) {
                return Err(storage("history key: secret service: prompt dismissed"));
            }
            if self.hidden.load(Ordering::SeqCst) {
                return Ok(None);
            }
            Ok(self.item().map(Zeroizing::new))
        }
        fn store(&self, key: &[u8]) -> Result<()> {
            self.stores.fetch_add(1, Ordering::SeqCst);
            let kept = self.race_winner.clone().unwrap_or_else(|| key.to_vec());
            *self.item.lock().unwrap() = Some(kept);
            Ok(())
        }
        fn prepare_create(&self) -> Result<()> {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            self.hidden.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    fn sealed_with(source: &impl KeySource) -> Vec<u8> {
        AeadProtector::with_source(MemoryKeySource::new(source.history_key().unwrap()))
            .protect(PLAIN)
            .unwrap()
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
        let stored = StoredKeySource::with_vault(
            MemVault::with_item(&[0xAB; KEY_LEN]),
            KeyPolicy::MustExist,
        );
        stored.history_key().unwrap();
        let all = format!(
            "{src:?} {p:?} {stored:?} {:?}",
            SecretServiceKeySource::new(KeyPolicy::CreateIfMissing)
        );
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
        use v2c_core::config::HistoryConfig;
        use v2c_core::history::History;
        let h = History::open_in_memory(Arc::new(protector(8)), HistoryConfig::default());
        assert!(h.is_ok());
    }

    #[test]
    fn existing_key_is_used_and_cached() {
        let src = StoredKeySource::with_vault(
            MemVault::with_item(&[5; KEY_LEN]),
            KeyPolicy::CreateIfMissing,
        );
        let a = sealed_with(&src);
        protector(5).unprotect(&a).unwrap();
        src.history_key().unwrap();
        assert_eq!(
            src.vault.loads.load(Ordering::SeqCst),
            1,
            "cached after first read"
        );
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 0);
        assert_eq!(src.vault.prepares.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_key_is_created_once_and_read_back() {
        let src = StoredKeySource::with_vault(MemVault::default(), KeyPolicy::CreateIfMissing);
        let k1 = src.history_key().unwrap();
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 1);
        assert_eq!(src.vault.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(src.vault.item().unwrap(), k1.as_bytes());
        // A fresh source (next process start) reads the same key, creates nothing.
        let again = StoredKeySource::with_vault(
            MemVault::with_item(&src.vault.item().unwrap()),
            KeyPolicy::MustExist,
        );
        assert_eq!(again.history_key().unwrap().as_bytes(), k1.as_bytes());
        assert_eq!(again.vault.stores.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn must_exist_never_creates() {
        let src = StoredKeySource::with_vault(MemVault::default(), KeyPolicy::MustExist);
        let err = src.history_key().unwrap_err().to_string();
        assert!(err.contains("not creating a new key"), "{err}");
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 0);
        assert_eq!(src.vault.prepares.load(Ordering::SeqCst), 0);
        assert!(src.vault.item().is_none());
    }

    #[test]
    fn set_policy_switches_and_clears_backoff() {
        let src = StoredKeySource::with_vault(MemVault::default(), KeyPolicy::MustExist);
        assert!(src.history_key().is_err());
        src.set_policy(KeyPolicy::CreateIfMissing);
        assert_eq!(src.policy(), Some(KeyPolicy::CreateIfMissing));
        assert!(src.history_key().is_ok());
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn hidden_item_is_found_after_unlock_not_overwritten() {
        let vault = MemVault::with_item(&[7; KEY_LEN]);
        vault.hidden.store(true, Ordering::SeqCst);
        let src = StoredKeySource::with_vault(vault, KeyPolicy::CreateIfMissing);
        let k = src.history_key().unwrap();
        assert_eq!(k.as_bytes(), &[7; KEY_LEN]);
        assert_eq!(src.vault.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(
            src.vault.stores.load(Ordering::SeqCst),
            0,
            "existing key overwritten"
        );
        assert_eq!(src.vault.item().unwrap(), vec![7; KEY_LEN]);
    }

    #[test]
    fn converges_on_the_stored_key_after_create() {
        let vault = MemVault {
            race_winner: Some(vec![9; KEY_LEN]),
            ..MemVault::default()
        };
        let src = StoredKeySource::with_vault(vault, KeyPolicy::CreateIfMissing);
        let k = src.history_key().unwrap();
        // The other process's key won; we use it, not the one we generated.
        assert_eq!(k.as_bytes(), &[9; KEY_LEN]);
        let sealed = sealed_with(&src);
        assert_eq!(protector(9).unprotect(&sealed).unwrap(), PLAIN);
    }

    #[test]
    fn wrong_length_stored_key_is_an_error_not_replaced() {
        let src =
            StoredKeySource::with_vault(MemVault::with_item(&[1; 16]), KeyPolicy::CreateIfMissing);
        let err = src.history_key().unwrap_err().to_string();
        assert!(err.contains("wrong length"), "{err}");
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 0);
        assert_eq!(src.vault.item().unwrap(), vec![1; 16]);
    }

    #[test]
    fn load_failure_is_not_treated_as_missing_and_backs_off() {
        let vault = MemVault::default();
        vault.fail_load.store(true, Ordering::SeqCst);
        let src = StoredKeySource::with_vault(vault, KeyPolicy::CreateIfMissing);
        assert!(src.history_key().is_err());
        let err = src.history_key().unwrap_err().to_string();
        assert!(err.contains("retrying shortly"), "{err}");
        assert_eq!(
            src.vault.loads.load(Ordering::SeqCst),
            1,
            "backoff not applied"
        );
        assert_eq!(src.vault.stores.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn arc_source_works_with_protector() {
        let src = Arc::new(StoredKeySource::with_vault(
            MemVault::with_item(&[3; KEY_LEN]),
            KeyPolicy::MustExist,
        ));
        let p = AeadProtector::with_source(Arc::clone(&src));
        let sealed = p.protect(PLAIN).unwrap();
        assert_eq!(protector(3).unprotect(&sealed).unwrap(), PLAIN);
    }

    /// Without an installed default store, the Secret Service source fails cleanly (no
    /// panic, no key generated).
    #[test]
    fn secret_service_source_without_store_errors() {
        if keyring_core::get_default_store().is_some() {
            return;
        }
        let p = AeadProtector::with_source(SecretServiceKeySource::new(KeyPolicy::MustExist));
        let err = p.protect(PLAIN).unwrap_err();
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
        entry.set_secret(key.as_bytes()).unwrap();
        let got = Zeroizing::new(entry.get_secret().unwrap());
        entry.delete_credential().unwrap();
        assert_eq!(got.as_slice(), key.as_bytes());
        let p = AeadProtector::with_source(MemoryKeySource::new(
            HistoryKey::from_stored(&got).unwrap(),
        ));
        let sealed = p.protect(PLAIN).unwrap();
        assert_eq!(p.unprotect(&sealed).unwrap(), PLAIN);
    }
}
