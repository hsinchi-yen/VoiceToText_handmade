//! Encrypted local history of outputs (T1.9, `tasks/plan.md` section 7).
//!
//! Text is encrypted with a platform [`Protector`] (Windows DPAPI, Android Keystore,
//! Linux Secret Service) before it is stored. Audio is never stored.
//!
//! Storage: one SQLite table (`entries`). Only the text column is sensitive and it is stored
//! as an encrypted BLOB. The foreground executable name is stored in plain text: it is needed
//! for display without decrypting, it is already part of the (text-free) logs, and exclusion
//! (`exclude_apps`) is decided before anything is written, so excluded apps never appear.
//! `secure_delete` is on, so deleted ciphertext is overwritten in the file as well.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, warn};

use crate::config::HistoryConfig;
use crate::pipeline::Timings;
use crate::{redact::Redacted, Lang, Mode, Result, V2cError};

/// Encrypts data for the current OS user. Implemented in `v2c-platform`.
pub trait Protector: Send + Sync {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    /// Unix time in seconds.
    pub created_at: i64,
    pub mode: Mode,
    /// Foreground app executable when the text was produced.
    pub app: String,
    pub text: String,
}

impl std::fmt::Debug for HistoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryEntry")
            .field("id", &self.id)
            .field("created_at", &self.created_at)
            .field("mode", &self.mode)
            .field("app", &self.app)
            .field("text", &Redacted(&self.text))
            .finish()
    }
}

/// History store operations (T1.9 provides the SQLite implementation).
pub trait HistoryStore: Send + Sync {
    fn add(&self, mode: Mode, app: &str, text: &str) -> Result<()>;
    fn list(&self, limit: u32) -> Result<Vec<HistoryEntry>>;
    fn delete(&self, id: i64) -> Result<()>;
    fn clear(&self) -> Result<()>;
    /// Deletes entries older than the retention period. Returns how many were removed.
    fn purge_expired(&self, now_unix: i64) -> Result<u64>;
}

/// A new entry with its metadata (stored next to the encrypted text).
#[derive(Clone, PartialEq, Eq)]
pub struct NewEntry<'a> {
    /// Unix time in seconds.
    pub created_at: i64,
    pub mode: Mode,
    pub lang: Option<Lang>,
    /// Foreground executable; empty when unknown.
    pub app: &'a str,
    pub text: &'a str,
    pub timings: Option<Timings>,
}

impl std::fmt::Debug for NewEntry<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewEntry")
            .field("created_at", &self.created_at)
            .field("mode", &self.mode)
            .field("lang", &self.lang)
            .field("app", &self.app)
            .field("text", &Redacted(self.text))
            .field("timings", &self.timings)
            .finish()
    }
}

/// What [`History::add_entry`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Stored under this id.
    Stored(i64),
    /// `history.enabled = false`.
    Disabled,
    /// The foreground app is in `history.exclude_apps`; nothing was encrypted or stored.
    Excluded,
    /// Empty text (e.g. no speech); nothing stored.
    Empty,
}

impl AddOutcome {
    /// Stable code for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            AddOutcome::Stored(_) => "stored",
            AddOutcome::Disabled => "disabled",
            AddOutcome::Excluded => "excluded",
            AddOutcome::Empty => "empty",
        }
    }
}

/// A listed entry with its stored metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecord {
    pub entry: HistoryEntry,
    pub lang: Option<Lang>,
    /// Character count of the text (Unicode scalar values).
    pub chars: u32,
    /// Release-to-text pipeline time, when known.
    pub total_ms: Option<u64>,
}

/// Whether `app` (an executable name or path) matches an entry of `exclude_apps`
/// (case-insensitive, file name only, `.exe` optional on either side).
pub fn is_excluded(app: &str, exclude_apps: &[String]) -> bool {
    fn norm(s: &str) -> String {
        let name = s.rsplit(['\\', '/']).next().unwrap_or(s).trim();
        let lower = name.to_lowercase();
        lower
            .strip_suffix(".exe")
            .map(str::to_owned)
            .unwrap_or(lower)
    }
    let app = norm(app);
    !app.is_empty() && exclude_apps.iter().any(|e| norm(e) == app)
}

const SECONDS_PER_DAY: i64 = 86_400;
const SCHEMA_VERSION: i32 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  INTEGER NOT NULL,
    app         TEXT    NOT NULL,
    mode        TEXT    NOT NULL,
    lang        TEXT,
    chars       INTEGER NOT NULL,
    asr_ms      INTEGER,
    llm_ms      INTEGER,
    total_ms    INTEGER,
    text_sealed BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS entries_created_at ON entries (created_at);
";

/// SQLite-backed, encrypted [`HistoryStore`].
pub struct History {
    conn: Mutex<Connection>,
    protector: Arc<dyn Protector>,
    config: HistoryConfig,
}

impl History {
    /// Opens (or creates) the database file, e.g. `%APPDATA%\v2c\history.db`.
    pub fn open(path: &Path, protector: Arc<dyn Protector>, config: HistoryConfig) -> Result<Self> {
        Self::with_connection(Connection::open(path).map_err(db)?, protector, config)
    }

    /// In-memory database (tests).
    pub fn open_in_memory(protector: Arc<dyn Protector>, config: HistoryConfig) -> Result<Self> {
        Self::with_connection(Connection::open_in_memory().map_err(db)?, protector, config)
    }

    fn with_connection(
        conn: Connection,
        protector: Arc<dyn Protector>,
        config: HistoryConfig,
    ) -> Result<Self> {
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(db)?;
        conn.execute_batch(SCHEMA).map_err(db)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(db)?;
        Ok(Self {
            conn: Mutex::new(conn),
            protector,
            config,
        })
    }

    pub fn config(&self) -> &HistoryConfig {
        &self.config
    }

    /// Stores an entry unless history is disabled, the app is excluded, or the text is empty.
    /// The exclusion check happens before the text is encrypted. Keeps the newest
    /// `max_entries` entries.
    pub fn add_entry(&self, e: &NewEntry<'_>) -> Result<AddOutcome> {
        if !self.config.enabled {
            return Ok(AddOutcome::Disabled);
        }
        if is_excluded(e.app, &self.config.exclude_apps) {
            return Ok(AddOutcome::Excluded);
        }
        if e.text.trim().is_empty() {
            return Ok(AddOutcome::Empty);
        }
        let sealed = self.protector.protect(e.text.as_bytes())?;
        let chars = e.text.chars().count() as i64;
        let t = e.timings;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO entries
                 (created_at, app, mode, lang, chars, asr_ms, llm_ms, total_ms, text_sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.created_at,
                e.app,
                mode_code(e.mode),
                e.lang.map(lang_code),
                chars,
                t.map(|t| t.asr_ms as i64),
                t.and_then(|t| t.llm_ms).map(|v| v as i64),
                t.map(|t| t.total_ms as i64),
                sealed,
            ],
        )
        .map_err(db)?;
        let id = conn.last_insert_rowid();
        let trimmed = conn
            .execute(
                "DELETE FROM entries WHERE id NOT IN
                     (SELECT id FROM entries ORDER BY created_at DESC, id DESC LIMIT ?1)",
                params![i64::from(self.config.max_entries)],
            )
            .map_err(db)?;
        debug!(id, chars, trimmed, "history entry stored");
        Ok(AddOutcome::Stored(id))
    }

    /// Newest first, at most `limit` (and at most `max_entries`), decrypted. Entries that
    /// cannot be decrypted (e.g. a database copied from another Windows user) are skipped.
    pub fn list_records(&self, limit: u32) -> Result<Vec<HistoryRecord>> {
        let limit = limit.min(self.config.max_entries);
        let rows: Vec<RawRow> = {
            let conn = self.lock()?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, created_at, app, mode, lang, chars, total_ms, text_sealed
                     FROM entries ORDER BY created_at DESC, id DESC LIMIT ?1",
                )
                .map_err(db)?;
            let mapped = stmt
                .query_map(params![i64::from(limit)], RawRow::from_row)
                .map_err(db)?;
            mapped.collect::<rusqlite::Result<_>>().map_err(db)?
        };
        let mut out = Vec::with_capacity(rows.len());
        let mut unreadable = 0u32;
        for row in rows {
            match self.open_row(row) {
                Some(r) => out.push(r),
                None => unreadable += 1,
            }
        }
        if unreadable > 0 {
            warn!(unreadable, "history entries could not be decrypted");
        }
        Ok(out)
    }

    /// One entry by id, decrypted. `None` if it does not exist or cannot be decrypted.
    pub fn get(&self, id: i64) -> Result<Option<HistoryRecord>> {
        let row = {
            let conn = self.lock()?;
            conn.query_row(
                "SELECT id, created_at, app, mode, lang, chars, total_ms, text_sealed
                 FROM entries WHERE id = ?1",
                params![id],
                RawRow::from_row,
            )
            .optional()
            .map_err(db)?
        };
        Ok(row.and_then(|r| self.open_row(r)))
    }

    /// Number of stored entries.
    pub fn count(&self) -> Result<u64> {
        let conn = self.lock()?;
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .map_err(db)?;
        Ok(n as u64)
    }

    fn open_row(&self, row: RawRow) -> Option<HistoryRecord> {
        let plain = match self.protector.unprotect(&row.sealed) {
            Ok(p) => p,
            Err(e) => {
                debug!(id = row.id, error = %e, "history entry decrypt failed");
                return None;
            }
        };
        let text = String::from_utf8(plain).ok()?;
        Some(HistoryRecord {
            entry: HistoryEntry {
                id: row.id,
                created_at: row.created_at,
                mode: parse_mode(&row.mode)?,
                app: row.app,
                text,
            },
            lang: row.lang.as_deref().and_then(parse_lang),
            chars: row.chars.clamp(0, i64::from(u32::MAX)) as u32,
            total_ms: row.total_ms.map(|v| v.max(0) as u64),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| V2cError::Storage("history lock poisoned".into()))
    }
}

impl HistoryStore for History {
    fn add(&self, mode: Mode, app: &str, text: &str) -> Result<()> {
        self.add_entry(&NewEntry {
            created_at: now_unix(),
            mode,
            lang: None,
            app,
            text,
            timings: None,
        })
        .map(|_| ())
    }

    fn list(&self, limit: u32) -> Result<Vec<HistoryEntry>> {
        Ok(self
            .list_records(limit)?
            .into_iter()
            .map(|r| r.entry)
            .collect())
    }

    fn delete(&self, id: i64) -> Result<()> {
        let n = self
            .lock()?
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
            .map_err(db)?;
        debug!(id, deleted = n, "history entry deleted");
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        let n = self
            .lock()?
            .execute("DELETE FROM entries", [])
            .map_err(db)?;
        debug!(deleted = n, "history cleared");
        Ok(())
    }

    fn purge_expired(&self, now_unix: i64) -> Result<u64> {
        let cutoff =
            now_unix.saturating_sub(i64::from(self.config.retention_days) * SECONDS_PER_DAY);
        let n = self
            .lock()?
            .execute("DELETE FROM entries WHERE created_at < ?1", params![cutoff])
            .map_err(db)?;
        Ok(n as u64)
    }
}

/// Current Unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct RawRow {
    id: i64,
    created_at: i64,
    app: String,
    mode: String,
    lang: Option<String>,
    chars: i64,
    total_ms: Option<i64>,
    sealed: Vec<u8>,
}

impl RawRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            created_at: r.get(1)?,
            app: r.get(2)?,
            mode: r.get(3)?,
            lang: r.get(4)?,
            chars: r.get(5)?,
            total_ms: r.get(6)?,
            sealed: r.get(7)?,
        })
    }
}

/// SQLite errors carry SQL and codes, never parameter values (user text is always bound).
fn db(e: rusqlite::Error) -> V2cError {
    V2cError::Storage(e.to_string())
}

fn mode_code(m: Mode) -> &'static str {
    match m {
        Mode::Dictate => "dictate",
        Mode::TranslateZhToEn => "translate_zh_en",
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "dictate" => Some(Mode::Dictate),
        "translate_zh_en" => Some(Mode::TranslateZhToEn),
        _ => None,
    }
}

fn lang_code(l: Lang) -> &'static str {
    match l {
        Lang::Zh => "zh",
        Lang::En => "en",
        Lang::Mixed => "mixed",
    }
}

fn parse_lang(s: &str) -> Option<Lang> {
    match s {
        "zh" => Some(Lang::Zh),
        "en" => Some(Lang::En),
        "mixed" => Some(Lang::Mixed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const SENTENCE: &str = "明天下午三點跟 Alice 開會討論預算";

    /// Reversible fake: XOR with a key plus a header, and counts calls.
    #[derive(Default)]
    struct FakeProtector {
        protects: AtomicUsize,
    }

    const KEY: u8 = 0x5A;
    const HEADER: &[u8] = b"FAKE1";

    impl Protector for FakeProtector {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
            self.protects.fetch_add(1, Ordering::Relaxed);
            let mut out = HEADER.to_vec();
            out.extend(plain.iter().map(|b| b ^ KEY));
            Ok(out)
        }
        fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            let body = sealed
                .strip_prefix(HEADER)
                .ok_or_else(|| V2cError::Storage("bad header".into()))?;
            Ok(body.iter().map(|b| b ^ KEY).collect())
        }
    }

    fn store_with(cfg: HistoryConfig) -> (History, Arc<FakeProtector>) {
        let p = Arc::new(FakeProtector::default());
        (History::open_in_memory(p.clone(), cfg).unwrap(), p)
    }

    fn store() -> (History, Arc<FakeProtector>) {
        store_with(HistoryConfig::default())
    }

    fn entry<'a>(at: i64, app: &'a str, text: &'a str) -> NewEntry<'a> {
        NewEntry {
            created_at: at,
            mode: Mode::Dictate,
            lang: Some(Lang::Mixed),
            app,
            text,
            timings: Some(Timings {
                asr_ms: 300,
                postprocess_ms: 2,
                llm_ms: Some(400),
                total_ms: 710,
            }),
        }
    }

    #[test]
    fn add_then_list_round_trips_with_metadata() {
        let (h, _) = store();
        let id = match h.add_entry(&entry(1_000, "notepad.exe", SENTENCE)).unwrap() {
            AddOutcome::Stored(id) => id,
            other => panic!("not stored: {other:?}"),
        };
        let list = h.list_records(200).unwrap();
        assert_eq!(list.len(), 1);
        let r = &list[0];
        assert_eq!(r.entry.id, id);
        assert_eq!(r.entry.text, SENTENCE);
        assert_eq!(r.entry.app, "notepad.exe");
        assert_eq!(r.entry.mode, Mode::Dictate);
        assert_eq!(r.entry.created_at, 1_000);
        assert_eq!(r.lang, Some(Lang::Mixed));
        assert_eq!(r.chars, SENTENCE.chars().count() as u32);
        assert_eq!(r.total_ms, Some(710));
        assert_eq!(h.get(id).unwrap().as_ref(), Some(r));
        assert_eq!(h.get(id + 100).unwrap(), None);
    }

    #[test]
    fn trait_add_and_list_work() {
        let (h, _) = store();
        h.add(Mode::TranslateZhToEn, "LINE.exe", "see you tomorrow")
            .unwrap();
        let list = HistoryStore::list(&h, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].mode, Mode::TranslateZhToEn);
        assert_eq!(list[0].text, "see you tomorrow");
        assert!(list[0].created_at > 1_600_000_000);
    }

    #[test]
    fn text_is_stored_encrypted() {
        let (h, p) = store();
        h.add_entry(&entry(1_000, "notepad.exe", SENTENCE)).unwrap();
        assert_eq!(p.protects.load(Ordering::Relaxed), 1);
        let conn = h.lock().unwrap();
        let sealed: Vec<u8> = conn
            .query_row("SELECT text_sealed FROM entries", [], |r| r.get(0))
            .unwrap();
        assert!(sealed.starts_with(HEADER));
        assert!(!contains(&sealed, SENTENCE.as_bytes()));
    }

    #[test]
    fn database_file_has_no_plaintext() {
        let dir = std::env::temp_dir().join(format!("v2c-history-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        let _ = std::fs::remove_file(&path);
        {
            let p = Arc::new(FakeProtector::default());
            let h = History::open(&path, p, HistoryConfig::default()).unwrap();
            h.add_entry(&entry(1_000, "notepad.exe", SENTENCE)).unwrap();
            h.add_entry(&entry(1_001, "notepad.exe", "second secret line"))
                .unwrap();
            HistoryStore::delete(&h, 2).unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.is_empty());
        assert!(!contains(&raw, SENTENCE.as_bytes()));
        assert!(!contains(&raw, b"second secret line"));
        assert!(!contains(&raw, "預算".as_bytes()));
        // Reopening reads the same entries back.
        let h = History::open(
            &path,
            Arc::new(FakeProtector::default()),
            HistoryConfig::default(),
        )
        .unwrap();
        assert_eq!(h.list_records(10).unwrap()[0].entry.text, SENTENCE);
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excluded_apps_are_never_encrypted_or_stored() {
        let (h, p) = store();
        for app in [
            "1Password.exe",
            "bitwarden.EXE",
            r"C:\Program Files\KeePassXC\KeePassXC.exe",
            "keepass",
        ] {
            assert_eq!(
                h.add_entry(&entry(1_000, app, SENTENCE)).unwrap(),
                AddOutcome::Excluded,
                "{app}"
            );
        }
        assert_eq!(p.protects.load(Ordering::Relaxed), 0);
        assert_eq!(h.count().unwrap(), 0);
        assert!(matches!(
            h.add_entry(&entry(1_000, "notepad.exe", SENTENCE)).unwrap(),
            AddOutcome::Stored(_)
        ));
    }

    #[test]
    fn exclusion_matching() {
        let list = vec!["1Password.exe".to_string(), "MyVault".to_string()];
        assert!(is_excluded("1password.exe", &list));
        assert!(is_excluded("1Password", &list));
        assert!(is_excluded("myvault.exe", &list));
        assert!(!is_excluded("notepad.exe", &list));
        assert!(!is_excluded("", &list));
        assert!(!is_excluded("1Password.exe.bak", &list));
    }

    #[test]
    fn disabled_and_empty_are_not_stored() {
        let (h, p) = store_with(HistoryConfig {
            enabled: false,
            ..HistoryConfig::default()
        });
        assert_eq!(
            h.add_entry(&entry(1, "notepad.exe", SENTENCE)).unwrap(),
            AddOutcome::Disabled
        );
        let (h2, p2) = store();
        assert_eq!(
            h2.add_entry(&entry(1, "notepad.exe", "  \n")).unwrap(),
            AddOutcome::Empty
        );
        assert_eq!(p.protects.load(Ordering::Relaxed), 0);
        assert_eq!(p2.protects.load(Ordering::Relaxed), 0);
        assert_eq!(h.count().unwrap() + h2.count().unwrap(), 0);
    }

    #[test]
    fn purge_removes_entries_past_retention() {
        let (h, _) = store();
        let now = 10_000_000;
        let day = SECONDS_PER_DAY;
        h.add_entry(&entry(now - 8 * day, "a.exe", "old")).unwrap();
        h.add_entry(&entry(now - 7 * day - 1, "a.exe", "just expired"))
            .unwrap();
        h.add_entry(&entry(now - 7 * day, "a.exe", "boundary"))
            .unwrap();
        h.add_entry(&entry(now - 60, "a.exe", "fresh")).unwrap();
        assert_eq!(h.purge_expired(now).unwrap(), 2);
        let texts: Vec<_> = h
            .list_records(10)
            .unwrap()
            .into_iter()
            .map(|r| r.entry.text)
            .collect();
        assert_eq!(texts, ["fresh", "boundary"]);
        assert_eq!(h.purge_expired(now).unwrap(), 0);
    }

    #[test]
    fn keeps_only_the_newest_max_entries() {
        let (h, _) = store_with(HistoryConfig {
            max_entries: 5,
            ..HistoryConfig::default()
        });
        for i in 0..8 {
            h.add_entry(&entry(1_000 + i, "a.exe", &format!("line {i}")))
                .unwrap();
        }
        assert_eq!(h.count().unwrap(), 5);
        let list = h.list_records(200).unwrap();
        assert_eq!(list.len(), 5);
        assert_eq!(list[0].entry.text, "line 7");
        assert_eq!(list[4].entry.text, "line 3");
        assert_eq!(h.list_records(2).unwrap().len(), 2);
    }

    #[test]
    fn default_cap_is_200() {
        let (h, _) = store();
        for i in 0..205 {
            h.add_entry(&entry(i, "a.exe", "x")).unwrap();
        }
        assert_eq!(h.count().unwrap(), 200);
    }

    #[test]
    fn delete_and_clear() {
        let (h, _) = store();
        let ids: Vec<i64> = (0..3)
            .map(|i| match h.add_entry(&entry(i, "a.exe", "t")).unwrap() {
                AddOutcome::Stored(id) => id,
                o => panic!("{o:?}"),
            })
            .collect();
        HistoryStore::delete(&h, ids[1]).unwrap();
        let left: Vec<i64> = h
            .list_records(10)
            .unwrap()
            .iter()
            .map(|r| r.entry.id)
            .collect();
        assert_eq!(left, [ids[2], ids[0]]);
        HistoryStore::delete(&h, 9_999).unwrap(); // missing id is not an error
        h.clear().unwrap();
        assert_eq!(h.count().unwrap(), 0);
    }

    #[test]
    fn undecryptable_entries_are_skipped() {
        let (h, _) = store();
        h.add_entry(&entry(1, "a.exe", "good")).unwrap();
        h.lock()
            .unwrap()
            .execute(
                "INSERT INTO entries (created_at, app, mode, chars, text_sealed)
                 VALUES (2, 'a.exe', 'dictate', 3, x'00')",
                [],
            )
            .unwrap();
        let list = h.list_records(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].entry.text, "good");
    }

    #[test]
    fn debug_output_redacts_text() {
        let e = entry(1, "a.exe", SENTENCE);
        let s = format!("{e:?}");
        assert!(!s.contains(SENTENCE) && !s.contains("預算"), "{s}");
        let (h, _) = store();
        h.add_entry(&e).unwrap();
        let s = format!("{:?}", h.list_records(1).unwrap());
        assert!(!s.contains(SENTENCE) && !s.contains("預算"), "{s}");
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
}
