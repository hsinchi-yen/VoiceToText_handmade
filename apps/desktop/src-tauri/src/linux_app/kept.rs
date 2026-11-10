//! Text that could not be typed is never dropped (T3.6).
//!
//! On Linux the IBus injector refuses to type when no text field uses the v2c input method
//! (`NO_FOCUSED_INPUT`), and the portal fallback may be unsupported or declined. Then
//! [`KeepTextSink`] makes sure the text survives: it is written to the encrypted input
//! history when that is possible, otherwise kept in memory ([`UnsentStore`], never on
//! disk, never logged) and listed at the top of the 紀錄 page with a negative id. Either
//! way the user is told where it is ([`Kept`] → `NotifyStatus::TextKept`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tracing::{info, warn};
use v2c_core::history::now_unix;
use v2c_core::pipeline::Output;
use v2c_core::redact::Redacted;
use v2c_core::Mode;

use crate::history_sink::HistorySink;
use crate::sinks::{OutputContext, OutputSink};

/// Unsent texts kept in memory (oldest dropped first).
pub const MAX_UNSENT: usize = 20;

/// Where a text that was not typed went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// In the encrypted history.
    History,
    /// In memory only (history disabled or unavailable); listed on the 紀錄 page until the
    /// app exits.
    Memory,
}

/// One unsent text.
#[derive(Clone)]
pub struct Unsent {
    /// Negative, so it never collides with a history row id.
    pub id: i64,
    pub created_at: i64,
    pub mode: Mode,
    pub text: String,
}

impl std::fmt::Debug for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unsent")
            .field("id", &self.id)
            .field("created_at", &self.created_at)
            .field("mode", &self.mode)
            .field("text", &Redacted(&self.text))
            .finish()
    }
}

#[derive(Default)]
struct Inner {
    items: VecDeque<Unsent>,
    last_id: i64,
}

/// In-memory list of unsent texts, newest first.
#[derive(Default)]
pub struct UnsentStore {
    inner: Mutex<Inner>,
}

/// Whether a 紀錄 page id refers to an [`UnsentStore`] item.
pub fn is_unsent_id(id: i64) -> bool {
    id < 0
}

impl UnsentStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keeps `text`; returns its id.
    pub fn push(&self, mode: Mode, text: &str, created_at: i64) -> i64 {
        let mut inner = self.lock();
        inner.last_id -= 1;
        let id = inner.last_id;
        inner.items.push_front(Unsent {
            id,
            created_at,
            mode,
            text: text.to_owned(),
        });
        inner.items.truncate(MAX_UNSENT);
        id
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Unsent> {
        self.lock().items.iter().cloned().collect()
    }

    pub fn text(&self, id: i64) -> Option<String> {
        self.lock()
            .items
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.text.clone())
    }

    pub fn remove(&self, id: i64) -> bool {
        let mut inner = self.lock();
        let before = inner.items.len();
        inner.items.retain(|u| u.id != id);
        inner.items.len() != before
    }

    pub fn clear(&self) {
        self.lock().items.clear();
    }

    pub fn len(&self) -> usize {
        self.lock().items.len()
    }
}

/// Tauri-managed handle to the app's [`UnsentStore`] (read by the history commands).
pub struct UnsentState(pub Arc<UnsentStore>);

type OnKept = Box<dyn Fn(Kept) + Send + Sync>;

/// Replaces the plain history sink on Linux: successful outputs go to the history as usual;
/// failed ones are kept (history or memory) and reported through `on_kept`.
pub struct KeepTextSink {
    history: Option<Arc<HistorySink>>,
    unsent: Arc<UnsentStore>,
    on_kept: Arc<OnKept>,
}

impl KeepTextSink {
    pub fn new(
        history: Option<Arc<HistorySink>>,
        unsent: Arc<UnsentStore>,
        on_kept: impl Fn(Kept) + Send + Sync + 'static,
    ) -> Self {
        Self {
            history,
            unsent,
            on_kept: Arc::new(Box::new(on_kept)),
        }
    }

    /// Blocking (SQLite + Secret Service).
    fn keep(
        history: Option<&HistorySink>,
        unsent: &UnsentStore,
        on_kept: &OnKept,
        output: &Output,
        ctx: &OutputContext,
    ) {
        let in_history = history.is_some_and(|h| h.record(output, ctx));
        let kept = if in_history {
            Kept::History
        } else {
            unsent.push(output.mode, &output.text, now_unix());
            Kept::Memory
        };
        info!(
            ?kept,
            chars = output.text.chars().count(),
            "text not typed; kept"
        );
        on_kept(kept);
    }
}

impl OutputSink for KeepTextSink {
    fn name(&self) -> &str {
        "keep-text"
    }

    fn on_output(&self, output: &Output, ctx: &OutputContext) {
        if ctx.injected {
            if let Some(h) = &self.history {
                h.on_output(output, ctx);
            }
            return;
        }
        if output.text.trim().is_empty() {
            return;
        }
        let (history, unsent, on_kept) = (
            self.history.clone(),
            Arc::clone(&self.unsent),
            Arc::clone(&self.on_kept),
        );
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                let (output, ctx) = (output.clone(), ctx.clone());
                rt.spawn_blocking(move || {
                    Self::keep(history.as_deref(), &unsent, &on_kept, &output, &ctx)
                });
            }
            Err(_) => Self::keep(history.as_deref(), &unsent, &on_kept, output, ctx),
        }
    }
}

/// Logs (without text) when the history could not take a failed output either.
pub fn warn_if_memory(kept: Kept) {
    if kept == Kept::Memory {
        warn!("history unavailable; unsent text kept in memory until exit");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use v2c_core::config::HistoryConfig;
    use v2c_core::history::{History, Protector};
    use v2c_core::pipeline::Timings;
    use v2c_core::{Lang, V2cError};

    use super::*;

    const SENTENCE: &str = "這段沒有輸入成功的秘密句子不能出現在日誌 secret";

    struct Rev;
    impl Protector for Rev {
        fn protect(&self, p: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(p.iter().rev().copied().collect())
        }
        fn unprotect(&self, s: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(s.iter().rev().copied().collect())
        }
    }

    /// The Secret Service key is unavailable (locked keyring, no daemon).
    struct NoKey;
    impl Protector for NoKey {
        fn protect(&self, _: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Err(V2cError::Storage("history key unavailable".into()))
        }
        fn unprotect(&self, _: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Err(V2cError::Storage("history key unavailable".into()))
        }
    }

    fn output(text: &str) -> Output {
        Output {
            text: text.into(),
            mode: Mode::Dictate,
            lang: Lang::Zh,
            timings: Timings::default(),
            warnings: vec![],
        }
    }

    fn ctx(injected: bool) -> OutputContext {
        OutputContext {
            app_exe: None,
            injected,
        }
    }

    fn history(p: Arc<dyn Protector>, enabled: bool) -> (Arc<History>, Arc<HistorySink>) {
        let cfg = HistoryConfig {
            enabled,
            ..HistoryConfig::default()
        };
        let h = Arc::new(History::open_in_memory(p, cfg).unwrap());
        (Arc::clone(&h), Arc::new(HistorySink::new(h)))
    }

    fn sink(
        history: Option<Arc<HistorySink>>,
    ) -> (KeepTextSink, Arc<UnsentStore>, Arc<Mutex<Vec<Kept>>>) {
        let unsent = Arc::new(UnsentStore::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = Arc::clone(&seen);
        let sink = KeepTextSink::new(history, Arc::clone(&unsent), move |k| {
            s2.lock().unwrap().push(k)
        });
        (sink, unsent, seen)
    }

    #[test]
    fn failed_output_goes_to_history_when_possible() {
        let (store, hs) = history(Arc::new(Rev), true);
        let (sink, unsent, seen) = sink(Some(hs));
        sink.on_output(&output(SENTENCE), &ctx(false));
        assert_eq!(*seen.lock().unwrap(), vec![Kept::History]);
        assert_eq!(unsent.len(), 0);
        assert_eq!(store.list_records(5).unwrap()[0].entry.text, SENTENCE);
    }

    #[test]
    fn failed_output_is_kept_in_memory_without_history() {
        for hs in [
            None,
            Some(history(Arc::new(Rev), false).1), // history.enabled = false
            Some(history(Arc::new(NoKey), true).1), // keyring locked / missing
        ] {
            let (sink, unsent, seen) = sink(hs);
            sink.on_output(&output(SENTENCE), &ctx(false));
            assert_eq!(*seen.lock().unwrap(), vec![Kept::Memory]);
            let items = unsent.list();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].text, SENTENCE);
            assert!(is_unsent_id(items[0].id));
        }
    }

    #[test]
    fn typed_output_only_goes_to_history() {
        let (store, hs) = history(Arc::new(Rev), true);
        let (sink, unsent, seen) = sink(Some(hs));
        sink.on_output(&output(SENTENCE), &ctx(true));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(unsent.len(), 0);
        assert_eq!(store.count().unwrap(), 1);

        // Without history a typed output leaves nothing behind.
        let (sink, unsent, seen) = self::sink(None);
        sink.on_output(&output(SENTENCE), &ctx(true));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(unsent.len(), 0);
    }

    #[test]
    fn empty_text_is_not_kept() {
        let (sink, unsent, seen) = sink(None);
        sink.on_output(&output("  "), &ctx(false));
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(unsent.len(), 0);
    }

    #[test]
    fn unsent_store_ids_order_and_limit() {
        let s = UnsentStore::default();
        let a = s.push(Mode::Dictate, "a", 1);
        let b = s.push(Mode::TranslateZhToEn, "b", 2);
        assert!(a < 0 && b < 0 && a != b);
        assert_eq!(s.list()[0].text, "b", "newest first");
        assert_eq!(s.text(a).as_deref(), Some("a"));
        assert!(s.remove(a));
        assert!(!s.remove(a));
        assert_eq!(s.text(a), None);
        for i in 0..(MAX_UNSENT + 5) {
            s.push(Mode::Dictate, &i.to_string(), 3);
        }
        assert_eq!(s.len(), MAX_UNSENT);
        assert_eq!(s.list()[0].text, (MAX_UNSENT + 4).to_string());
        s.clear();
        assert_eq!(s.len(), 0);
        // Ids keep counting down after a clear (no reuse).
        assert!(s.push(Mode::Dictate, "x", 4) < b);
    }

    #[test]
    fn debug_hides_text() {
        let s = UnsentStore::default();
        s.push(Mode::Dictate, SENTENCE, 1);
        let dbg = format!("{:?}", s.list());
        assert!(!dbg.contains("秘密") && !dbg.contains("secret"), "{dbg}");
    }

    #[test]
    fn logs_never_contain_kept_text() {
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = crate::logging::subscriber(
            move || writer.clone(),
            tracing_subscriber::EnvFilter::new("trace"),
        );
        tracing::subscriber::with_default(subscriber, || {
            let (sink, unsent, _) = sink(Some(history(Arc::new(Rev), true).1));
            sink.on_output(&output(SENTENCE), &ctx(false));
            let (sink, _, seen) = self::sink(Some(history(Arc::new(NoKey), true).1));
            sink.on_output(&output(SENTENCE), &ctx(false));
            warn_if_memory(seen.lock().unwrap()[0]);
            tracing::debug!(items = ?unsent.list(), "unsent");
        });
        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("text not typed; kept"), "{log}");
        assert!(log.contains("kept in memory"), "{log}");
        for word in [SENTENCE, "秘密", "secret"] {
            assert!(!log.contains(word), "`{word}` leaked:\n{log}");
        }
    }
}
