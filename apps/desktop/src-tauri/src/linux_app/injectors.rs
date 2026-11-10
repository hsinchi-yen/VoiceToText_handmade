//! Injector plumbing for Linux (pure, unit-tested with fakes).
//!
//! - [`ChainInjector`]: the IBus injector first; if it typed nothing (e.g.
//!   `NO_FOCUSED_INPUT`), the portal clipboard injector when one exists.
//! - [`SwitchInjector`]: the controller's injector; the IBus link swaps what is inside it
//!   when the engine reconnects, without restarting the controller.

use std::sync::{Arc, RwLock};

use v2c_core::inject_plan::Action;
use v2c_platform::{InjectError, Injector};

pub type SharedInjector = Arc<dyn Injector + Send + Sync>;

/// Used when no injection backend works: every run fails with the stored reason, so the
/// controller reports it and the kept-text sink keeps the text.
pub struct UnsupportedInjector(pub InjectError);

impl Injector for UnsupportedInjector {
    fn run(&self, _: &[Action]) -> Result<(), InjectError> {
        Err(self.0.clone())
    }
}

/// `primary`, then `fallback` if the primary typed nothing.
pub struct ChainInjector {
    primary: SharedInjector,
    fallback: Option<SharedInjector>,
}

impl ChainInjector {
    pub fn new(primary: SharedInjector, fallback: Option<SharedInjector>) -> Self {
        Self { primary, fallback }
    }
}

impl Injector for ChainInjector {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        match self.primary.run(actions) {
            Ok(()) => Ok(()),
            // Part of the text is already in the field: running the rest through another
            // backend would duplicate it. The kept-text sink keeps the whole text.
            Err(e @ InjectError::Partial { .. }) => Err(e),
            Err(e) => match &self.fallback {
                Some(f) => f.run(actions),
                None => Err(e),
            },
        }
    }
}

/// An injector whose implementation can be replaced at runtime.
pub struct SwitchInjector {
    current: RwLock<SharedInjector>,
}

impl SwitchInjector {
    pub fn new(initial: SharedInjector) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    pub fn set(&self, injector: SharedInjector) {
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = injector;
    }

    fn get(&self) -> SharedInjector {
        Arc::clone(&self.current.read().unwrap_or_else(|p| p.into_inner()))
    }
}

impl Injector for SwitchInjector {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        // Clone out so a swap during a (blocking) run does not wait for it.
        self.get().run(actions)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Fake {
        result: Result<(), InjectError>,
        runs: Mutex<usize>,
    }

    impl Fake {
        fn new(result: Result<(), InjectError>) -> Arc<Self> {
            Arc::new(Self {
                result,
                runs: Mutex::new(0),
            })
        }
        fn runs(&self) -> usize {
            *self.runs.lock().unwrap()
        }
    }

    impl Injector for Fake {
        fn run(&self, _: &[Action]) -> Result<(), InjectError> {
            *self.runs.lock().unwrap() += 1;
            self.result.clone()
        }
    }

    fn actions() -> Vec<Action> {
        vec![Action::Text("x".into())]
    }

    #[test]
    fn primary_success_skips_fallback() {
        let (p, f) = (Fake::new(Ok(())), Fake::new(Ok(())));
        let c = ChainInjector::new(p.clone(), Some(f.clone()));
        assert_eq!(c.run(&actions()), Ok(()));
        assert_eq!((p.runs(), f.runs()), (1, 0));
    }

    #[test]
    fn no_focused_input_falls_back() {
        let no_input = InjectError::Os(v2c_platform::linux::ibus::NO_FOCUSED_INPUT.into());
        let (p, f) = (Fake::new(Err(no_input.clone())), Fake::new(Ok(())));
        let c = ChainInjector::new(p.clone(), Some(f.clone()));
        assert_eq!(c.run(&actions()), Ok(()));
        assert_eq!((p.runs(), f.runs()), (1, 1));

        // Fallback fails too: its error is reported.
        let denied = Fake::new(Err(InjectError::PermissionDenied));
        let c = ChainInjector::new(Fake::new(Err(no_input.clone())), Some(denied));
        assert_eq!(c.run(&actions()), Err(InjectError::PermissionDenied));

        // No fallback: the primary's error.
        let c = ChainInjector::new(Fake::new(Err(no_input.clone())), None);
        assert_eq!(c.run(&actions()), Err(no_input));
    }

    #[test]
    fn partial_typing_is_not_repeated_by_the_fallback() {
        let partial = InjectError::Partial { sent: 1, total: 3 };
        let (p, f) = (Fake::new(Err(partial.clone())), Fake::new(Ok(())));
        let c = ChainInjector::new(p, Some(f.clone()));
        assert_eq!(c.run(&actions()), Err(partial));
        assert_eq!(f.runs(), 0);
    }

    #[test]
    fn switch_replaces_the_backend() {
        let s = SwitchInjector::new(Arc::new(UnsupportedInjector(InjectError::Unsupported(
            "none".into(),
        ))));
        assert!(matches!(
            s.run(&actions()),
            Err(InjectError::Unsupported(_))
        ));
        let ok = Fake::new(Ok(()));
        s.set(ok.clone());
        assert_eq!(s.run(&actions()), Ok(()));
        assert_eq!(ok.runs(), 1);
    }
}
