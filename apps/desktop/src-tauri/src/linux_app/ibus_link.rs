//! Keeps the v2c IBus engine connected (T3.6, `tasks/plan.md` section 9.2a).
//!
//! [`IbusLink`] owns the engine connection. On connect it starts the engine's hotkey (feeding
//! the controller's event channel), swaps the engine's injector into the controller's
//! [`SwitchInjector`], and stops the portal / evdev fallback hotkey so a key is never seen
//! twice. When ibus-daemon goes away (`ibus restart`, new session) the engine's
//! `on_disconnect` fires: the user is notified, the injector reports the outage (so text is
//! kept, never typed into nowhere), and a background thread re-registers with backoff.
//! [`IbusLink::poke`] retries at once (a `v2c --ibus` launch handed over by the
//! single-instance plugin means the daemon wants the engine now).
//!
//! The D-Bus side is behind [`EngineFactory`] / [`LiveEngine`] so the logic is unit-tested
//! with fakes; [`RealFactory`] wraps `v2c_platform::linux::ibus`. Every call here may block
//! on D-Bus: never call it on the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use tracing::{info, warn};
use v2c_core::trigger::Event;
use v2c_platform::linux::ibus::{v2c_component, IbusAddress, IbusEngine, Registration};
use v2c_platform::{Hotkey, InjectError, PlatformError};

use super::injectors::{ChainInjector, SharedInjector, SwitchInjector, UnsupportedInjector};

/// Error text while the engine is disconnected (no typed text in it).
pub const IBUS_DISCONNECTED: &str = "IBus: the v2c engine is disconnected; nothing was typed";

/// Waits between reconnect attempts; the last one repeats.
pub const RECONNECT_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// Delay before reconnect attempt `attempt` (0-based).
pub fn retry_delay(delays: &[Duration], attempt: usize) -> Duration {
    delays
        .get(attempt)
        .or(delays.last())
        .copied()
        .unwrap_or(Duration::ZERO)
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// A connected engine.
pub trait LiveEngine: Send {
    fn injector(&self) -> SharedInjector;
    fn start_hotkey(&self, sink: EventSink) -> Result<(), PlatformError>;
    /// Calls `callback` once when the daemon connection is lost (possibly right away).
    fn on_disconnect(&self, callback: Box<dyn FnOnce() + Send>);
    fn close(self: Box<Self>);
}

/// Connects (and registers) an engine.
pub trait EngineFactory: Send + Sync {
    fn connect(&self) -> Result<Box<dyn LiveEngine>, PlatformError>;
}

/// What the user should hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEvent {
    Connected,
    Lost,
    Reconnected,
}

pub struct IbusLink {
    factory: Box<dyn EngineFactory>,
    engine: Mutex<Option<Box<dyn LiveEngine>>>,
    injector: Arc<SwitchInjector>,
    /// Portal injector used while IBus is disconnected (`None` when unsupported here).
    fallback_injector: Option<SharedInjector>,
    /// The portal / evdev hotkey, running while IBus is not connected at startup.
    fallback_hotkey: Mutex<Option<Box<dyn Hotkey + Send>>>,
    sink: EventSink,
    on_event: Box<dyn Fn(LinkEvent) + Send + Sync>,
    delays: Vec<Duration>,
    connected: AtomicBool,
    reconnecting: AtomicBool,
    stopped: AtomicBool,
    /// Wakes the reconnect thread early ([`IbusLink::poke`], [`IbusLink::shutdown`]).
    wake: (Mutex<bool>, Condvar),
}

impl IbusLink {
    pub fn new(
        factory: Box<dyn EngineFactory>,
        injector: Arc<SwitchInjector>,
        fallback_injector: Option<SharedInjector>,
        sink: EventSink,
        on_event: impl Fn(LinkEvent) + Send + Sync + 'static,
        delays: Vec<Duration>,
    ) -> Arc<Self> {
        Arc::new(Self {
            factory,
            engine: Mutex::new(None),
            injector,
            fallback_injector,
            fallback_hotkey: Mutex::new(None),
            sink,
            on_event: Box::new(on_event),
            delays,
            connected: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// The injector used while the engine is not connected.
    fn offline_injector(&self) -> SharedInjector {
        let unavailable: SharedInjector = Arc::new(UnsupportedInjector(InjectError::Os(
            IBUS_DISCONNECTED.into(),
        )));
        match &self.fallback_injector {
            Some(f) => Arc::new(ChainInjector::new(unavailable, Some(Arc::clone(f)))),
            None => unavailable,
        }
    }

    /// Hands over the fallback hotkey; it is stopped once IBus connects, or right away if
    /// IBus already connected (a reconnect or poke can win the race with startup).
    /// `connected` is only set to `true` while holding the same lock, so the two cannot
    /// both stay live.
    pub fn set_fallback_hotkey(&self, mut hotkey: Box<dyn Hotkey + Send>) {
        let mut slot = lock(&self.fallback_hotkey);
        if self.is_connected() || self.stopped.load(Ordering::SeqCst) {
            drop(slot);
            hotkey.stop();
            info!("fallback hotkey stopped (IBus engine already connected, or shutting down)");
        } else {
            *slot = Some(hotkey);
        }
    }

    #[cfg(test)]
    pub fn has_fallback_hotkey(&self) -> bool {
        lock(&self.fallback_hotkey).is_some()
    }

    /// Connects once. On success the engine's hotkey and injector are live.
    pub fn connect(self: &Arc<Self>) -> Result<(), PlatformError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(PlatformError::Ibus("stopped".into()));
        }
        let engine = self.factory.connect()?;
        engine.start_hotkey(Arc::clone(&self.sink))?;
        // Connected: IBus only. NO_FOCUSED_INPUT must reach the caller (plan 9.2a); the
        // portal injector is only used while disconnected (`offline_injector`).
        self.injector.set(engine.injector());
        let fallback_hotkey = {
            let mut slot = lock(&self.fallback_hotkey);
            self.connected.store(true, Ordering::SeqCst);
            slot.take()
        };
        if let Some(mut hk) = fallback_hotkey {
            hk.stop();
            info!("fallback hotkey stopped (IBus engine connected)");
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        engine.on_disconnect(Box::new(move || {
            if let Some(link) = weak.upgrade() {
                link.on_lost();
            }
        }));
        if let Some(old) = lock(&self.engine).replace(engine) {
            old.close();
        }
        Ok(())
    }

    fn on_lost(self: &Arc<Self>) {
        self.connected.store(false, Ordering::SeqCst);
        self.injector.set(self.offline_injector());
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        warn!("IBus connection lost; reconnecting");
        (self.on_event)(LinkEvent::Lost);
        self.start_reconnect();
    }

    /// Starts the reconnect thread unless one is running.
    pub fn start_reconnect(self: &Arc<Self>) {
        if self.reconnecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let link = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("v2c-ibus-reconnect".into())
            .spawn(move || link.reconnect_loop());
        if spawned.is_err() {
            self.reconnecting.store(false, Ordering::SeqCst);
            warn!("could not start the IBus reconnect thread");
        }
    }

    fn reconnect_loop(self: Arc<Self>) {
        let mut attempt = 0;
        while !self.stopped.load(Ordering::SeqCst) {
            self.wait(retry_delay(&self.delays, attempt));
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            match self.connect() {
                Ok(()) => {
                    info!(attempt, "IBus engine reconnected");
                    (self.on_event)(LinkEvent::Reconnected);
                    break;
                }
                Err(e) => {
                    info!(attempt, error = %e, "IBus reconnect failed");
                    attempt += 1;
                }
            }
        }
        self.reconnecting.store(false, Ordering::SeqCst);
    }

    /// Sleeps `d` or until poked.
    fn wait(&self, d: Duration) {
        let (m, cv) = &self.wake;
        let mut poked = lock(m);
        if !*poked {
            poked = cv
                .wait_timeout(poked, d)
                .map(|(g, _)| g)
                .unwrap_or_else(|p| p.into_inner().0);
        }
        *poked = false;
    }

    /// Reconnects as soon as possible if not connected (e.g. ibus-daemon just ran
    /// `v2c --ibus`). Blocking when no reconnect thread is running; call off the UI thread.
    pub fn poke(self: &Arc<Self>) {
        if self.is_connected() || self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if self.reconnecting.load(Ordering::SeqCst) {
            let (m, cv) = &self.wake;
            *lock(m) = true;
            cv.notify_all();
            return;
        }
        match self.connect() {
            Ok(()) => (self.on_event)(LinkEvent::Connected),
            Err(e) => {
                info!(error = %e, "IBus connect on request failed");
                self.start_reconnect();
            }
        }
    }

    /// Stops reconnecting, closes the engine and the fallback hotkey.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        {
            let (m, cv) = &self.wake;
            *lock(m) = true;
            cv.notify_all();
        }
        if let Some(e) = lock(&self.engine).take() {
            e.close();
        }
        if let Some(mut hk) = lock(&self.fallback_hotkey).take() {
            hk.stop();
        }
        self.connected.store(false, Ordering::SeqCst);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The real engine over `v2c_platform::linux::ibus`.
pub struct RealFactory {
    pub registration: Registration,
}

struct RealEngine(IbusEngine);

impl LiveEngine for RealEngine {
    fn injector(&self) -> SharedInjector {
        Arc::new(self.0.injector())
    }

    fn start_hotkey(&self, sink: EventSink) -> Result<(), PlatformError> {
        self.0.hotkey().start(Box::new(move |ev| sink(ev)))
    }

    fn on_disconnect(&self, callback: Box<dyn FnOnce() + Send>) {
        self.0.on_disconnect(callback);
    }

    fn close(self: Box<Self>) {
        if let Err(e) = self.0.close() {
            info!(error = %e, "IBus close");
        }
    }
}

impl EngineFactory for RealFactory {
    fn connect(&self) -> Result<Box<dyn LiveEngine>, PlatformError> {
        let address = IbusAddress::discover()?;
        let engine = IbusEngine::start(&address, &v2c_component(), self.registration)?;
        info!(registration = ?self.registration, "IBus engine registered");
        Ok(Box::new(RealEngine(engine)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::mpsc;
    use std::time::Instant;

    use v2c_core::inject_plan::Action;
    use v2c_core::trigger::Key;
    use v2c_platform::Injector;

    use super::*;

    type Disconnect = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

    /// Hands out fake engines; `results` decides whether each connect succeeds.
    struct FakeFactory {
        results: Mutex<VecDeque<bool>>,
        connects: Arc<Mutex<usize>>,
        /// The latest engine's disconnect callback (call it to simulate a daemon restart).
        disconnect: Disconnect,
        sinks: Arc<Mutex<Vec<EventSink>>>,
        closed: Arc<Mutex<usize>>,
    }

    struct FakeEngine {
        disconnect: Disconnect,
        sinks: Arc<Mutex<Vec<EventSink>>>,
        closed: Arc<Mutex<usize>>,
    }

    struct OkInjector;
    impl Injector for OkInjector {
        fn run(&self, _: &[Action]) -> Result<(), InjectError> {
            Ok(())
        }
    }

    impl LiveEngine for FakeEngine {
        fn injector(&self) -> SharedInjector {
            Arc::new(OkInjector)
        }
        fn start_hotkey(&self, sink: EventSink) -> Result<(), PlatformError> {
            self.sinks.lock().unwrap().push(sink);
            Ok(())
        }
        fn on_disconnect(&self, callback: Box<dyn FnOnce() + Send>) {
            *self.disconnect.lock().unwrap() = Some(callback);
        }
        fn close(self: Box<Self>) {
            *self.closed.lock().unwrap() += 1;
        }
    }

    impl EngineFactory for FakeFactory {
        fn connect(&self) -> Result<Box<dyn LiveEngine>, PlatformError> {
            *self.connects.lock().unwrap() += 1;
            if self.results.lock().unwrap().pop_front().unwrap_or(false) {
                Ok(Box::new(FakeEngine {
                    disconnect: Arc::clone(&self.disconnect),
                    sinks: Arc::clone(&self.sinks),
                    closed: Arc::clone(&self.closed),
                }))
            } else {
                Err(PlatformError::Ibus("no daemon".into()))
            }
        }
    }

    struct FakeHotkey(Arc<Mutex<bool>>);
    impl Hotkey for FakeHotkey {
        fn start(&mut self, _: Box<dyn Fn(Event) + Send + 'static>) -> Result<(), PlatformError> {
            Ok(())
        }
        fn stop(&mut self) {
            *self.0.lock().unwrap() = true;
        }
    }

    struct Harness {
        link: Arc<IbusLink>,
        injector: Arc<SwitchInjector>,
        connects: Arc<Mutex<usize>>,
        disconnect: Disconnect,
        sinks: Arc<Mutex<Vec<EventSink>>>,
        closed: Arc<Mutex<usize>>,
        events: mpsc::Receiver<LinkEvent>,
        hotkey_events: mpsc::Receiver<Event>,
    }

    fn harness(results: &[bool], fallback: Option<SharedInjector>) -> Harness {
        let connects = Arc::new(Mutex::new(0));
        let disconnect: Disconnect = Arc::new(Mutex::new(None));
        let sinks = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(0));
        let factory = FakeFactory {
            results: Mutex::new(results.iter().copied().collect()),
            connects: Arc::clone(&connects),
            disconnect: Arc::clone(&disconnect),
            sinks: Arc::clone(&sinks),
            closed: Arc::clone(&closed),
        };
        let injector = Arc::new(SwitchInjector::new(Arc::new(UnsupportedInjector(
            InjectError::Unsupported("start".into()),
        ))));
        let (ev_tx, events) = mpsc::channel();
        let ev_tx = Mutex::new(ev_tx);
        let (hk_tx, hotkey_events) = mpsc::channel();
        let hk_tx = Mutex::new(hk_tx);
        let link = IbusLink::new(
            Box::new(factory),
            Arc::clone(&injector),
            fallback,
            Arc::new(move |e| {
                let _ = hk_tx.lock().unwrap().send(e);
            }),
            move |e| {
                let _ = ev_tx.lock().unwrap().send(e);
            },
            vec![Duration::from_millis(5)],
        );
        Harness {
            link,
            injector,
            connects,
            disconnect,
            sinks,
            closed,
            events,
            hotkey_events,
        }
    }

    fn text() -> Vec<Action> {
        vec![Action::Text("x".into())]
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn retry_delays() {
        let d = [Duration::from_secs(1), Duration::from_secs(5)];
        assert_eq!(retry_delay(&d, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(&d, 1), Duration::from_secs(5));
        assert_eq!(retry_delay(&d, 9), Duration::from_secs(5));
        assert_eq!(retry_delay(&[], 3), Duration::ZERO);
        assert_eq!(retry_delay(&RECONNECT_DELAYS, 100), Duration::from_secs(30));
    }

    #[test]
    fn connect_wires_hotkey_and_injector_and_stops_fallback_hotkey() {
        let h = harness(&[true], None);
        let stopped = Arc::new(Mutex::new(false));
        h.link
            .set_fallback_hotkey(Box::new(FakeHotkey(Arc::clone(&stopped))));
        assert!(h.injector.run(&text()).is_err());
        h.link.connect().unwrap();
        assert!(h.link.is_connected());
        assert_eq!(h.injector.run(&text()), Ok(()));
        assert!(*stopped.lock().unwrap(), "fallback hotkey must stop");
        assert!(!h.link.has_fallback_hotkey());
        // The engine's key events reach the controller's sink.
        (h.sinks.lock().unwrap()[0])(Event::KeyDown {
            key: Key::Trigger,
            at_ms: 1,
        });
        assert!(matches!(
            h.hotkey_events.try_recv(),
            Ok(Event::KeyDown {
                key: Key::Trigger,
                ..
            })
        ));
    }

    #[test]
    fn connected_before_hand_over_stops_the_fallback() {
        // A reconnect / poke connected before the init thread handed over its hotkey.
        let h = harness(&[true], None);
        h.link.connect().unwrap();
        let stopped = Arc::new(Mutex::new(false));
        h.link
            .set_fallback_hotkey(Box::new(FakeHotkey(Arc::clone(&stopped))));
        assert!(
            *stopped.lock().unwrap(),
            "fallback must not run next to IBus"
        );
        assert!(!h.link.has_fallback_hotkey());
        h.link.shutdown();
    }

    #[test]
    fn failed_connect_keeps_fallback() {
        let h = harness(&[false], None);
        let stopped = Arc::new(Mutex::new(false));
        h.link
            .set_fallback_hotkey(Box::new(FakeHotkey(Arc::clone(&stopped))));
        assert!(h.link.connect().is_err());
        assert!(!h.link.is_connected());
        assert!(!*stopped.lock().unwrap());
        assert!(h.link.has_fallback_hotkey());
    }

    #[test]
    fn startup_retry_connects_later_and_stops_the_fallback_hotkey() {
        // ibus-daemon not up at login: two failures, then it is.
        let h = harness(&[false, false, true], None);
        let stopped = Arc::new(Mutex::new(false));
        h.link
            .set_fallback_hotkey(Box::new(FakeHotkey(Arc::clone(&stopped))));
        assert!(h.link.connect().is_err());
        h.link.start_reconnect();
        assert_eq!(
            h.events.recv_timeout(Duration::from_secs(5)),
            Ok(LinkEvent::Reconnected)
        );
        assert!(h.link.is_connected());
        assert!(*stopped.lock().unwrap());
        assert_eq!(h.injector.run(&text()), Ok(()));
        h.link.shutdown();
    }

    #[test]
    fn disconnect_notifies_fails_injection_and_reconnects() {
        // First connect ok, then two failed reconnects, then success.
        let h = harness(&[true, false, false, true], None);
        h.link.connect().unwrap();
        let cb = h.disconnect.lock().unwrap().take().unwrap();
        cb(); // ibus-daemon restarted
        assert_eq!(
            h.events.recv_timeout(Duration::from_secs(5)),
            Ok(LinkEvent::Lost)
        );
        assert_eq!(
            h.events.recv_timeout(Duration::from_secs(5)),
            Ok(LinkEvent::Reconnected)
        );
        assert!(h.link.is_connected());
        assert_eq!(*h.connects.lock().unwrap(), 4);
        assert_eq!(h.injector.run(&text()), Ok(()));
        assert_eq!(h.sinks.lock().unwrap().len(), 2, "hotkey re-started");
        // The dead engine was replaced (closed) by the new one.
        assert_eq!(*h.closed.lock().unwrap(), 1);
        h.link.shutdown();
        assert_eq!(*h.closed.lock().unwrap(), 2);
    }

    #[test]
    fn while_disconnected_injection_fails_or_uses_fallback() {
        // Never reconnects.
        let h = harness(&[true], None);
        h.link.connect().unwrap();
        (h.disconnect.lock().unwrap().take().unwrap())();
        let err = h.injector.run(&text()).unwrap_err();
        assert_eq!(err, InjectError::Os(IBUS_DISCONNECTED.into()));
        h.link.shutdown();

        let h = harness(&[true], Some(Arc::new(OkInjector)));
        h.link.connect().unwrap();
        (h.disconnect.lock().unwrap().take().unwrap())();
        assert_eq!(h.injector.run(&text()), Ok(()), "portal fallback");
        h.link.shutdown();
    }

    #[test]
    fn poke_connects_when_offline_and_is_a_no_op_when_connected() {
        let h = harness(&[false, true], None);
        assert!(h.link.connect().is_err());
        h.link.poke();
        assert_eq!(
            h.events.recv_timeout(Duration::from_secs(5)),
            Ok(LinkEvent::Connected)
        );
        assert!(h.link.is_connected());
        h.link.poke();
        assert_eq!(*h.connects.lock().unwrap(), 2);
        h.link.shutdown();
    }

    #[test]
    fn shutdown_stops_the_reconnect_loop() {
        let h = harness(&[true], None);
        h.link.connect().unwrap();
        (h.disconnect.lock().unwrap().take().unwrap())();
        wait_until(|| *h.connects.lock().unwrap() >= 2);
        h.link.shutdown();
        wait_until(|| !h.link.reconnecting.load(Ordering::SeqCst));
        let n = *h.connects.lock().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(*h.connects.lock().unwrap(), n, "no attempts after shutdown");
        assert!(h.link.connect().is_err());
    }
}
