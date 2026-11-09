//! Writes each finished output to the encrypted input history (T1.9).
//!
//! The store is opened in `commands::history::init` (Tauri `setup`, before the controller
//! starts) and installed here once; [`crate::sinks::SinkRegistry::with_defaults`] picks it up,
//! so neither the controller nor its `Deps` change.
//!
//! Logs only outcome codes, ids and lengths (AGENTS.md rule 1).

use std::sync::{Arc, OnceLock};

use tracing::{debug, warn};
use v2c_core::history::{now_unix, History, NewEntry};
use v2c_core::pipeline::Output;

use crate::sinks::{OutputContext, OutputSink};

/// Records outputs in the history. Exclusion (`history.exclude_apps`) and the
/// enabled switch are applied by [`History::add_entry`] before anything is encrypted.
pub struct HistorySink {
    store: Arc<History>,
}

impl HistorySink {
    pub fn new(store: Arc<History>) -> Self {
        Self { store }
    }

    /// Writes one output (blocking: SQLite + DPAPI).
    pub fn record(&self, output: &Output, ctx: &OutputContext) {
        let entry = NewEntry {
            created_at: now_unix(),
            mode: output.mode,
            lang: Some(output.lang),
            app: ctx.app_exe.as_deref().unwrap_or(""),
            text: &output.text,
            timings: Some(output.timings),
        };
        match self.store.add_entry(&entry) {
            Ok(outcome) => debug!(
                outcome = outcome.as_str(),
                injected = ctx.injected,
                "history sink"
            ),
            Err(e) => warn!(error = %e, "history write failed"),
        }
    }
}

impl OutputSink for HistorySink {
    fn name(&self) -> &str {
        "history"
    }

    /// On the tokio blocking pool when called from the runtime (the controller), so the
    /// controller task is not held up; synchronously otherwise.
    fn on_output(&self, output: &Output, ctx: &OutputContext) {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                let sink = HistorySink::new(Arc::clone(&self.store));
                let (output, ctx) = (output.clone(), ctx.clone());
                rt.spawn_blocking(move || sink.record(&output, &ctx));
            }
            Err(_) => self.record(output, ctx),
        }
    }
}

static INSTALLED: OnceLock<Arc<HistorySink>> = OnceLock::new();

/// Installs the app-wide history sink (once; later calls are ignored).
pub fn install(sink: Arc<HistorySink>) {
    let _ = INSTALLED.set(sink);
}

/// The installed history sink, if history is available and enabled.
pub fn installed() -> Option<Arc<dyn OutputSink>> {
    INSTALLED
        .get()
        .map(|s| Arc::clone(s) as Arc<dyn OutputSink>)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use v2c_core::config::HistoryConfig;
    use v2c_core::history::{HistoryStore, Protector};
    use v2c_core::pipeline::Timings;
    use v2c_core::{Lang, Mode, V2cError};

    use super::*;

    /// Test sentence; must never appear in logs.
    const SENTENCE: &str = "下週三要繳房租兩萬五千元給房東 Bob";

    struct XorProtector;

    impl Protector for XorProtector {
        fn protect(&self, plain: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(plain.iter().map(|b| b ^ 0xA5).collect())
        }
        fn unprotect(&self, sealed: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(sealed.iter().map(|b| b ^ 0xA5).collect())
        }
    }

    struct FailingProtector;

    impl Protector for FailingProtector {
        fn protect(&self, _: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Err(V2cError::Storage("protect failed (code 5)".into()))
        }
        fn unprotect(&self, _: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Err(V2cError::Storage("unprotect failed (code 5)".into()))
        }
    }

    fn output(text: &str) -> Output {
        Output {
            text: text.into(),
            mode: Mode::Dictate,
            lang: Lang::Zh,
            timings: Timings {
                asr_ms: 320,
                postprocess_ms: 1,
                llm_ms: None,
                total_ms: 330,
            },
            warnings: vec![],
        }
    }

    fn ctx(app: &str) -> OutputContext {
        OutputContext {
            app_exe: Some(app.into()),
            injected: true,
        }
    }

    fn store(p: Arc<dyn Protector>) -> Arc<History> {
        Arc::new(History::open_in_memory(p, HistoryConfig::default()).unwrap())
    }

    #[test]
    fn sink_records_outputs_but_not_excluded_apps() {
        let store = store(Arc::new(XorProtector));
        let sink = HistorySink::new(Arc::clone(&store));
        sink.on_output(&output(SENTENCE), &ctx("notepad.exe"));
        sink.on_output(&output("hunter2 password"), &ctx("KeePassXC.exe"));
        sink.on_output(
            &output("unknown app"),
            &OutputContext {
                app_exe: None,
                injected: false,
            },
        );
        let list = store.list_records(10).unwrap();
        let texts: Vec<_> = list.iter().map(|r| r.entry.text.as_str()).collect();
        assert_eq!(texts, ["unknown app", SENTENCE]);
        assert_eq!(list[1].entry.app, "notepad.exe");
        assert_eq!(list[1].lang, Some(Lang::Zh));
        assert_eq!(list[1].total_ms, Some(330));
        assert_eq!(sink.name(), "history");
    }

    #[tokio::test]
    async fn inside_the_runtime_the_write_runs_on_the_blocking_pool() {
        let store = store(Arc::new(XorProtector));
        HistorySink::new(Arc::clone(&store)).on_output(&output(SENTENCE), &ctx("notepad.exe"));
        for _ in 0..200 {
            if store.count().unwrap() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("entry was not written");
    }

    #[test]
    fn logs_never_contain_the_stored_sentence() {
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
        // Thread-scoped: `controller::tests` owns the global subscriber in this binary.
        let subscriber = crate::logging::subscriber(
            move || writer.clone(),
            tracing_subscriber::EnvFilter::new("trace"),
        );
        tracing::subscriber::with_default(subscriber, || {
            let good = store(Arc::new(XorProtector));
            let sink = HistorySink::new(Arc::clone(&good));
            sink.on_output(&output(SENTENCE), &ctx("notepad.exe"));
            sink.on_output(&output(SENTENCE), &ctx("1Password.exe"));
            let items = crate::commands::history::list_items(&good).unwrap();
            assert_eq!(items[0].text, SENTENCE);
            let id = items[0].id;
            assert_eq!(
                crate::commands::history::entry_text(&good, id).unwrap(),
                SENTENCE
            );
            good.delete(id).unwrap();
            good.clear().unwrap();

            // Failure paths log errors; they must not carry the text either.
            let bad = store(Arc::new(FailingProtector));
            HistorySink::new(bad).on_output(&output(SENTENCE), &ctx("notepad.exe"));
        });

        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("history entry stored"),
            "core log missing:\n{log}"
        );
        assert!(
            log.contains("outcome=\"excluded\""),
            "sink log missing:\n{log}"
        );
        assert!(
            log.contains("history write failed"),
            "error log missing:\n{log}"
        );
        assert!(!log.contains(SENTENCE), "sentence leaked:\n{log}");
        for word in ["房租", "房東", "兩萬五千", "Bob"] {
            assert!(!log.contains(word), "`{word}` leaked:\n{log}");
        }
    }

    /// Real DPAPI + a real file: the database must not contain the sentence.
    #[cfg(windows)]
    #[test]
    fn dpapi_history_file_has_no_plaintext() {
        use v2c_platform::windows::dpapi::DpapiProtector;

        let dir = std::env::temp_dir().join(format!("v2c-hist-dpapi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        {
            let store = Arc::new(
                History::open(
                    &path,
                    Arc::new(DpapiProtector::new()),
                    HistoryConfig::default(),
                )
                .unwrap(),
            );
            HistorySink::new(Arc::clone(&store)).on_output(&output(SENTENCE), &ctx("notepad.exe"));
            assert_eq!(store.list_records(1).unwrap()[0].entry.text, SENTENCE);
        }
        let raw = std::fs::read(&path).unwrap();
        let has = |n: &[u8]| raw.windows(n.len()).any(|w| w == n);
        assert!(!has(SENTENCE.as_bytes()));
        assert!(!has("房租".as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
