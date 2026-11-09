//! Output sinks: things notified after each finished utterance (e.g. the input history, T1.9).
//! Register a sink in [`SinkRegistry`]; the controller does not need to change.

use std::sync::Arc;

use v2c_core::pipeline::Output;

/// Context about where an output went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputContext {
    /// Foreground executable at injection time, e.g. `notepad.exe`.
    pub app_exe: Option<String>,
    /// Whether the text was injected successfully. Sinks still get failed outputs so the
    /// user can recover the text (e.g. re-send it from the history).
    pub injected: bool,
}

pub trait OutputSink: Send + Sync {
    /// Short stable name for logs.
    fn name(&self) -> &str;

    /// Called once per output, after injection. Must not log `output.text`.
    fn on_output(&self, output: &Output, ctx: &OutputContext);
}

/// The registered sinks, called in registration order.
#[derive(Default, Clone)]
pub struct SinkRegistry {
    sinks: Vec<Arc<dyn OutputSink>>,
}

impl SinkRegistry {
    /// The registry used by the app. Empty for now; T1.9 registers the history here.
    pub fn with_defaults() -> Self {
        Self::default()
    }

    #[cfg_attr(not(test), allow(dead_code))] // first production caller: T1.9
    pub fn register(&mut self, sink: Arc<dyn OutputSink>) {
        self.sinks.push(sink);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.sinks.len()
    }

    pub fn notify(&self, output: &Output, ctx: &OutputContext) {
        for sink in &self.sinks {
            sink.on_output(output, ctx);
            tracing::debug!(sink = sink.name(), "output sink notified");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use v2c_core::{pipeline::Timings, Lang, Mode};

    use super::*;

    #[derive(Default)]
    struct Recording(Mutex<Vec<(String, OutputContext)>>);

    impl OutputSink for Recording {
        fn name(&self) -> &str {
            "recording"
        }
        fn on_output(&self, output: &Output, ctx: &OutputContext) {
            self.0
                .lock()
                .unwrap()
                .push((output.text.clone(), ctx.clone()));
        }
    }

    #[test]
    fn default_registry_is_empty() {
        assert_eq!(SinkRegistry::with_defaults().len(), 0);
    }

    #[test]
    fn notifies_every_registered_sink() {
        let a = Arc::new(Recording::default());
        let b = Arc::new(Recording::default());
        let mut reg = SinkRegistry::default();
        reg.register(a.clone());
        reg.register(b.clone());
        let out = Output {
            text: "hi".into(),
            mode: Mode::Dictate,
            lang: Lang::En,
            timings: Timings::default(),
            warnings: vec![],
        };
        let ctx = OutputContext {
            app_exe: Some("notepad.exe".into()),
            injected: true,
        };
        reg.notify(&out, &ctx);
        assert_eq!(a.0.lock().unwrap().len(), 1);
        assert_eq!(b.0.lock().unwrap()[0], ("hi".to_string(), ctx));
        assert_eq!(a.name(), "recording");
    }
}
