//! Personal vocabulary (names, product terms), stored as a UTF-8 text file with one entry per
//! line (desktop: `%APPDATA%\v2c\vocab.txt`).
//!
//! File format: each line is trimmed; blank lines and lines starting with `#` are skipped; runs
//! of whitespace inside an entry collapse to one space; duplicates (ignoring case) keep the first
//! spelling. At most [`MAX_ENTRIES`] entries of at most [`MAX_ENTRY_CHARS`] characters are kept.
//!
//! Privacy: entries are user text. `Debug` shows only the count, and errors never carry entries.

use std::io::Write;
use std::path::Path;

use crate::{Result, V2cError};

/// Maximum number of entries kept. The prompts only use the first ones that fit, so more
/// entries would only cost prompt size.
pub const MAX_ENTRIES: usize = 500;
/// Maximum length of one entry, in characters (Unicode scalar values).
pub const MAX_ENTRY_CHARS: usize = 40;

/// One word or phrase per entry, in the user's preferred spelling.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Vocab {
    pub words: Vec<String>,
}

impl std::fmt::Debug for Vocab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vocab")
            .field("len", &self.words.len())
            .finish()
    }
}

/// Result of checking editor text against the limits (see [`check`]).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct VocabCheck {
    /// The entries that would be kept (same rules as [`Vocab::parse`]).
    pub vocab: Vocab,
    /// 1-based line numbers of entries longer than [`MAX_ENTRY_CHARS`].
    pub too_long_lines: Vec<usize>,
    /// Number of distinct valid entries beyond [`MAX_ENTRIES`].
    pub over_limit: usize,
}

impl VocabCheck {
    /// True when nothing would be dropped because of the limits.
    pub fn is_ok(&self) -> bool {
        self.too_long_lines.is_empty() && self.over_limit == 0
    }
}

impl std::fmt::Debug for VocabCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VocabCheck")
            .field("len", &self.vocab.words.len())
            .field("too_long_lines", &self.too_long_lines)
            .field("over_limit", &self.over_limit)
            .finish()
    }
}

/// One entry from one line: trimmed, inner whitespace collapsed; `None` for blanks and comments.
fn entry(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    Some(line.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Applies the file rules to `text` and reports what the limits dropped.
pub fn check(text: &str) -> VocabCheck {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = VocabCheck::default();
    let mut seen: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(word) = entry(line) else { continue };
        if word.chars().count() > MAX_ENTRY_CHARS {
            out.too_long_lines.push(i + 1);
            continue;
        }
        let key = word.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if out.vocab.words.len() < MAX_ENTRIES {
            out.vocab.words.push(word);
        } else {
            out.over_limit += 1;
        }
    }
    out
}

impl Vocab {
    /// Parses file or editor text; entries over the limits are dropped (see [`check`]).
    pub fn parse(text: &str) -> Self {
        check(text).vocab
    }

    /// Builds a vocabulary from a list (e.g. `config.vocabulary`), applying the same rules.
    pub fn from_words<S: AsRef<str>>(words: &[S]) -> Self {
        let text = words
            .iter()
            .map(|w| w.as_ref().replace(['\r', '\n'], " "))
            .collect::<Vec<_>>()
            .join("\n");
        Self::parse(&text)
    }

    /// The entries, in file order.
    pub fn words(&self) -> &[String] {
        &self.words
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// File text: one entry per line, with a trailing newline (empty string for no entries).
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for w in &self.words {
            s.push_str(w);
            s.push('\n');
        }
        s
    }

    /// Reads `path`. A missing file is an empty vocabulary; invalid UTF-8 is an error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(V2cError::Storage(format!(
                "reading vocabulary {}: {e}",
                path.display()
            ))),
        }
    }

    /// Writes `path` atomically: a temporary file next to it is written, flushed to disk, and
    /// renamed over the old file, so a crash never leaves a half-written vocabulary.
    /// Creates the parent directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let err = |what: &str, e: std::io::Error| {
            V2cError::Storage(format!("{what} vocabulary {}: {e}", path.display()))
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| err("creating directory for", e))?;
        }
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_text().as_bytes())?;
            f.sync_all()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&tmp);
            return Err(err("writing", e));
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            err("replacing", e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        Vocab::parse(text).words
    }

    #[test]
    fn parse_trims_and_skips_blanks_and_comments() {
        let text = "\u{feff}  台積電  \n\n# a comment\n   # indented comment\nLM   Studio\r\nv2c\n";
        assert_eq!(words(text), vec!["台積電", "LM Studio", "v2c"]);
        assert!(words("").is_empty());
        assert!(words("\n  \n# only comments\n").is_empty());
    }

    #[test]
    fn parse_dedupes_case_insensitively_keeping_first_spelling() {
        assert_eq!(
            words("Typeless\ntypeless\nTYPELESS\n台積電\n台積電\nBreeze"),
            vec!["Typeless", "台積電", "Breeze"]
        );
    }

    #[test]
    fn check_reports_too_long_lines() {
        let long = "長".repeat(MAX_ENTRY_CHARS + 1);
        let ok = "長".repeat(MAX_ENTRY_CHARS);
        let c = check(&format!("a\n{long}\n{ok}\n"));
        assert_eq!(c.vocab.words, vec!["a".to_string(), ok]);
        assert_eq!(c.too_long_lines, vec![2]);
        assert_eq!(c.over_limit, 0);
        assert!(!c.is_ok());
    }

    #[test]
    fn check_caps_entry_count() {
        let text: String = (0..MAX_ENTRIES + 3).map(|i| format!("w{i}\n")).collect();
        let c = check(&text);
        assert_eq!(c.vocab.len(), MAX_ENTRIES);
        assert_eq!(c.over_limit, 3);
        assert_eq!(c.vocab.words[0], "w0");
        assert!(!c.is_ok());
        // Duplicates do not count towards the limit.
        let dupes: String = (0..MAX_ENTRIES + 3).map(|_| "same\n").collect();
        assert!(check(&dupes).is_ok());
    }

    #[test]
    fn from_words_applies_the_same_rules() {
        let v = Vocab::from_words(&["  A ", "a", "", "foo\nbar", "#hashtag"]);
        assert_eq!(v.words, vec!["A", "foo bar"]);
    }

    #[test]
    fn to_text_round_trips() {
        let v = Vocab::parse("台積電\nLM Studio\n");
        assert_eq!(v.to_text(), "台積電\nLM Studio\n");
        assert_eq!(Vocab::parse(&v.to_text()), v);
        assert_eq!(Vocab::default().to_text(), "");
    }

    #[test]
    fn debug_shows_only_the_count() {
        let v = Vocab::parse("秘密專案\nSecretName");
        let s = format!("{v:?} {:?}", check("秘密專案"));
        assert!(!s.contains("秘密") && !s.contains("Secret"), "{s}");
        assert!(s.contains("len: 2"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("v2c-vocab-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sub").join("vocab.txt");

        assert_eq!(
            Vocab::load(&path).unwrap(),
            Vocab::default(),
            "missing file"
        );

        let v = Vocab::parse("台積電\nLM Studio\n");
        v.save(&path).unwrap();
        assert_eq!(Vocab::load(&path).unwrap(), v);

        // Overwrite an existing file; no temporary file is left behind.
        let v2 = Vocab::parse("Breeze");
        v2.save(&path).unwrap();
        assert_eq!(Vocab::load(&path).unwrap(), v2);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("vocab.txt")]);

        // Invalid UTF-8 is an error, and the error does not echo the file contents.
        std::fs::write(&path, [0xff, 0xfe, b'x']).unwrap();
        assert!(matches!(Vocab::load(&path), Err(V2cError::Storage(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
