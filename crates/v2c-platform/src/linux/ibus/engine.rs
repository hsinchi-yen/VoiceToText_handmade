//! The IBus engine process side: Factory + Engine D-Bus objects over zbus, and the
//! [`Hotkey`] / [`Injector`] implementations that share one [`Core`].
//!
//! Protocol (ibus 1.5.26): the daemon calls `org.freedesktop.IBus.Factory.CreateEngine(s)
//! → o` at `/org/freedesktop/IBus/Factory` on our connection, then talks to the returned
//! object over `org.freedesktop.IBus.Engine` (`ProcessKeyEvent(uuu) → b`, `FocusIn`,
//! `FocusOut`, `Enable`, `Disable`, `Reset`, …) and listens for its `CommitText(v)` /
//! `ForwardKeyEvent(uuu)` signals. Engines are destroyed with
//! `org.freedesktop.IBus.Service.Destroy`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use v2c_core::inject_plan::Action;
use v2c_core::trigger::Event;
use zbus::names::BusName;
use zbus::object_server::ObjectServer;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::address::IbusAddress;
use super::serial::{ibus_text, ComponentDesc, EngineDesc};
use super::state::{ops_for, EngineOp, EngineState};
use crate::linux::hotkey::monotonic_ms;
use crate::{Hotkey, InjectError, Injector, PlatformError};

pub(crate) const IBUS_SERVICE: &str = "org.freedesktop.IBus";
pub(crate) const IBUS_PATH: &str = "/org/freedesktop/IBus";
pub(crate) const IBUS_IFACE: &str = "org.freedesktop.IBus";
pub(crate) const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
pub(crate) const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";
const ENGINE_PATH_PREFIX: &str = "/org/freedesktop/IBus/Engine/";

/// Error text when there is no focused v2c input context (nothing was typed).
pub const NO_FOCUSED_INPUT: &str =
    "IBus: no focused text field is using the v2c input method; nothing was typed";

type Sink = Box<dyn Fn(Event) + Send + 'static>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// State shared by the D-Bus objects, the hotkey and the injector.
#[derive(Default)]
pub(crate) struct Core {
    state: Mutex<EngineState>,
    sink: Mutex<Option<Sink>>,
}

impl Core {
    fn deliver(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        if let Some(sink) = lock(&self.sink).as_ref() {
            for e in events {
                sink(e);
            }
        }
    }

    fn with_state(&self, f: impl FnOnce(&mut EngineState, u64) -> Vec<Event>) {
        let events = f(&mut lock(&self.state), monotonic_ms());
        self.deliver(events);
    }

    pub(crate) fn key(&self, path: &str, keyval: u32, keycode: u32, state: u32) -> bool {
        let listening = lock(&self.sink).is_some();
        let out = lock(&self.state).key(path, listening, keyval, keycode, state, monotonic_ms());
        self.deliver(out.events);
        out.handled
    }

    pub(crate) fn created(&self, path: &str) {
        lock(&self.state).created(path);
    }
    pub(crate) fn destroyed(&self, path: &str) {
        self.with_state(|s, at| s.destroyed(path, at));
    }
    pub(crate) fn focus_in(&self, path: &str) {
        self.with_state(|s, at| s.focus_in(path, at));
    }
    pub(crate) fn focus_out(&self, path: &str) {
        self.with_state(|s, at| s.focus_out(path, at));
    }
    pub(crate) fn enable(&self, path: &str) {
        self.with_state(|s, at| s.enable(path, at));
    }
    pub(crate) fn disable(&self, path: &str) {
        self.with_state(|s, at| s.disable(path, at));
    }

    #[cfg(test)]
    pub(crate) fn any_active(&self) -> bool {
        let s = lock(&self.state);
        (1..=64).any(|i| s.is_active(&format!("{ENGINE_PATH_PREFIX}{i}")))
    }

    fn target(&self) -> Option<String> {
        lock(&self.state).target().map(str::to_string)
    }

    fn set_sink(&self, sink: Option<Sink>) {
        lock(&self.state).reset_keys();
        *lock(&self.sink) = sink;
    }
}

/// Sends engine output; the real implementation emits D-Bus signals, tests record.
pub(crate) trait OpSender: Send + Sync {
    fn send(&self, engine_path: &str, op: &EngineOp) -> Result<(), InjectError>;
}

struct DbusSender {
    conn: zbus::blocking::Connection,
}

impl OpSender for DbusSender {
    fn send(&self, engine_path: &str, op: &EngineOp) -> Result<(), InjectError> {
        let r = match op {
            EngineOp::Commit(text) => self.conn.emit_signal(
                None::<BusName<'_>>,
                engine_path,
                ENGINE_IFACE,
                "CommitText",
                &(ibus_text(text),),
            ),
            EngineOp::Key {
                keyval,
                keycode,
                state,
            } => self.conn.emit_signal(
                None::<BusName<'_>>,
                engine_path,
                ENGINE_IFACE,
                "ForwardKeyEvent",
                &(*keyval, *keycode, *state),
            ),
        };
        // zbus errors describe the transport, never the message body.
        r.map_err(|e| InjectError::Os(format!("IBus signal: {e}")))
    }
}

// ---------------------------------------------------------------------------------------
// D-Bus objects
// ---------------------------------------------------------------------------------------

struct FactoryObj {
    core: Arc<Core>,
    engine_names: Vec<String>,
    next_id: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl FactoryObj {
    async fn create_engine(
        &self,
        name: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        if !self.engine_names.contains(&name) {
            return Err(zbus::fdo::Error::Failed(format!(
                "this component has no engine named {name}"
            )));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let path = format!("{ENGINE_PATH_PREFIX}{id}");
        self.core.created(&path);
        let engine = EngineObj {
            core: self.core.clone(),
            path: path.clone(),
            content_type: (0, 0),
        };
        let service = ServiceObj {
            core: self.core.clone(),
            path: path.clone(),
        };
        server.at(path.as_str(), engine).await?;
        server.at(path.as_str(), service).await?;
        OwnedObjectPath::try_from(path).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

struct EngineObj {
    core: Arc<Core>,
    path: String,
    content_type: (u32, u32),
}

/// Every method of the 1.5.26 engine introspection is implemented so the daemon never gets
/// `UnknownMethod`; the ones v2c does not use are no-ops. Arguments that may carry user
/// text (`SetSurroundingText`) are ignored, never inspected or logged.
#[zbus::interface(name = "org.freedesktop.IBus.Engine")]
impl EngineObj {
    fn process_key_event(&self, keyval: u32, keycode: u32, state: u32) -> bool {
        self.core.key(&self.path, keyval, keycode, state)
    }
    fn set_cursor_location(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}
    fn process_hand_writing_event(&self, _coordinates: Vec<f64>) {}
    fn cancel_hand_writing(&self, _n_strokes: u32) {}
    fn set_capabilities(&self, _caps: u32) {}
    fn property_activate(&self, _name: String, _state: u32) {}
    fn property_show(&self, _name: String) {}
    fn property_hide(&self, _name: String) {}
    fn candidate_clicked(&self, _index: u32, _button: u32, _state: u32) {}
    fn focus_in(&self) {
        self.core.focus_in(&self.path);
    }
    fn focus_out(&self) {
        self.core.focus_out(&self.path);
    }
    fn reset(&self) {}
    fn enable(&self) {
        self.core.enable(&self.path);
    }
    fn disable(&self) {
        self.core.disable(&self.path);
    }
    fn page_up(&self) {}
    fn page_down(&self) {}
    fn cursor_up(&self) {}
    fn cursor_down(&self) {}
    fn set_surrounding_text(&self, _text: OwnedValue, _cursor_pos: u32, _anchor_pos: u32) {}
    fn panel_extension_received(&self, _event: OwnedValue) {}
    fn panel_extension_register_keys(&self, _data: OwnedValue) {}

    /// `(purpose, hints)`; write-only in IBus, readable here because zbus needs a getter.
    #[zbus(property)]
    fn content_type(&self) -> (u32, u32) {
        self.content_type
    }
    #[zbus(property)]
    fn set_content_type(&mut self, value: (u32, u32)) {
        self.content_type = value;
    }
}

struct ServiceObj {
    core: Arc<Core>,
    path: String,
}

#[zbus::interface(name = "org.freedesktop.IBus.Service")]
impl ServiceObj {
    async fn destroy(&self, #[zbus(connection)] conn: &zbus::Connection) {
        self.core.destroyed(&self.path);
        // Unexport after this call has been answered.
        let executor = conn.executor().clone();
        let conn = conn.clone();
        let path = self.path.clone();
        executor
            .spawn(
                async move {
                    let server = conn.object_server();
                    let _ = server.remove::<EngineObj, _>(path.as_str()).await;
                    let _ = server.remove::<ServiceObj, _>(path.as_str()).await;
                },
                "v2c-ibus-destroy",
            )
            .detach();
    }
}

// ---------------------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------------------

/// How the daemon learns about the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    /// ibus-daemon started this process from the installed component XML
    /// (`/usr/share/ibus/component/v2c.xml`); claim the component's bus name.
    Launched,
    /// Register the component at runtime (`RegisterComponent`); it disappears when this
    /// connection closes. For development and tests.
    Dynamic,
}

/// The v2c component as installed by `tools/ibus/v2c.xml`.
pub fn v2c_component() -> ComponentDesc {
    ComponentDesc {
        name: "org.freedesktop.IBus.V2c".into(),
        description: "v2c voice input".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        license: "MIT".into(),
        author: "v2c".into(),
        homepage: String::new(),
        exec: "/usr/bin/v2c --ibus".into(),
        textdomain: "v2c".into(),
        engines: vec![EngineDesc {
            name: "v2c".into(),
            longname: "v2c 語音輸入".into(),
            description: "按住右 Ctrl 說話，放開後輸入文字".into(),
            language: "zh_TW".into(),
            license: "MIT".into(),
            author: "v2c".into(),
            icon: String::new(),
            layout: "us".into(),
            rank: 0,
            symbol: "語".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }],
    }
}

/// A running engine process: one connection to the IBus bus that serves the factory and
/// its engines. Hand out [`IbusEngine::hotkey`] and [`IbusEngine::injector`].
pub struct IbusEngine {
    conn: zbus::blocking::Connection,
    core: Arc<Core>,
}

impl IbusEngine {
    /// Connects to the daemon at `address`, exports the factory and registers `component`.
    pub fn start(
        address: &IbusAddress,
        component: &ComponentDesc,
        registration: Registration,
    ) -> Result<Self, PlatformError> {
        let err =
            |what: &'static str| move |e: zbus::Error| PlatformError::Ibus(format!("{what}: {e}"));
        let core = Arc::new(Core::default());
        let factory = FactoryObj {
            core: core.clone(),
            engine_names: component.engines.iter().map(|e| e.name.clone()).collect(),
            next_id: AtomicU32::new(0),
        };
        let conn = zbus::blocking::connection::Builder::address(address.as_str())
            .map_err(err("address"))?
            .serve_at(FACTORY_PATH, factory)
            .map_err(err("factory"))?
            .build()
            .map_err(err("connect"))?;
        match registration {
            Registration::Dynamic => {
                let ibus = zbus::blocking::Proxy::new(&conn, IBUS_SERVICE, IBUS_PATH, IBUS_IFACE)
                    .map_err(err("IBus proxy"))?;
                ibus.call_method("RegisterComponent", &(component.to_value(),))
                    .map_err(err("RegisterComponent"))?;
            }
            Registration::Launched => {
                conn.request_name(component.name.as_str())
                    .map_err(err("RequestName"))?;
            }
        }
        Ok(Self { conn, core })
    }

    /// Hotkey events from key presses in text fields where the v2c engine is active.
    pub fn hotkey(&self) -> IbusHotkey {
        IbusHotkey {
            core: self.core.clone(),
        }
    }

    /// Commits text to the focused text field where the v2c engine is active.
    pub fn injector(&self) -> IbusInjector {
        IbusInjector {
            core: self.core.clone(),
            sender: Arc::new(DbusSender {
                conn: self.conn.clone(),
            }),
        }
    }

    /// Whether output would currently reach a text field: the v2c engine is enabled and
    /// focused there and a key arrived from it since it got focus (see [`IbusInjector`]).
    pub fn has_focused_input(&self) -> bool {
        self.core.target().is_some()
    }

    /// Some engine object is enabled and focused (possibly on IBus' fake context).
    #[cfg(test)]
    pub(crate) fn engine_active(&self) -> bool {
        self.core.any_active()
    }

    /// Closes the connection; a dynamically registered component disappears with it.
    pub fn close(self) -> Result<(), PlatformError> {
        self.conn
            .close()
            .map_err(|e| PlatformError::Ibus(format!("close: {e}")))
    }
}

/// [`Hotkey`] fed by the engine's `ProcessKeyEvent`. Only sees keys while a text field
/// with the v2c engine has focus; while stopped, every key passes through untouched.
pub struct IbusHotkey {
    core: Arc<Core>,
}

impl Hotkey for IbusHotkey {
    fn start(&mut self, sink: Box<dyn Fn(Event) + Send + 'static>) -> Result<(), PlatformError> {
        self.core.set_sink(Some(sink));
        Ok(())
    }

    fn stop(&mut self) {
        self.core.set_sink(None);
    }
}

/// [`Injector`] that commits through the engine (`CommitText` / `ForwardKeyEvent`).
///
/// Output only goes to the text field the last key event came from, and only while it
/// still has focus; otherwise [`Injector::run`] fails with [`NO_FOCUSED_INPUT`] and sends
/// nothing. (IBus moves the engine to an internal fake context when no text field has
/// focus and silently drops commits there; key events tell real fields apart.)
pub struct IbusInjector {
    core: Arc<Core>,
    sender: Arc<dyn OpSender>,
}

impl Injector for IbusInjector {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        let ops = ops_for(actions);
        if ops.is_empty() {
            return Ok(());
        }
        let target = self
            .core
            .target()
            .ok_or_else(|| InjectError::Os(NO_FOCUSED_INPUT.into()))?;
        for (i, op) in ops.iter().enumerate() {
            if let Err(e) = self.sender.send(&target, op) {
                return Err(if i == 0 {
                    e
                } else {
                    InjectError::Partial {
                        sent: i,
                        total: ops.len(),
                    }
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::keyevent::{IBUS_RELEASE_MASK, KEY_CONTROL_R};
    use super::*;
    use v2c_core::config::{HotkeyConfig, NewlineKey};
    use v2c_core::trigger::{Command, Key, Trigger, TriggerConfig};
    use v2c_core::Mode;

    const E: &str = "/org/freedesktop/IBus/Engine/1";

    #[derive(Default)]
    struct Recorder {
        sent: Mutex<Vec<(String, EngineOp)>>,
        fail_at: Option<usize>,
    }

    impl OpSender for Recorder {
        fn send(&self, path: &str, op: &EngineOp) -> Result<(), InjectError> {
            let mut sent = lock(&self.sent);
            if Some(sent.len()) == self.fail_at {
                return Err(InjectError::Os("IBus signal: broken pipe".into()));
            }
            sent.push((path.to_string(), op.clone()));
            Ok(())
        }
    }

    fn active_core() -> Arc<Core> {
        let core = Arc::new(Core::default());
        core.created(E);
        core.focus_in(E);
        core.enable(E);
        core.key(E, 0x61, 30, 0); // the user typed in the field
        core
    }

    fn injector(core: &Arc<Core>, rec: Arc<Recorder>) -> IbusInjector {
        IbusInjector {
            core: core.clone(),
            sender: rec,
        }
    }

    #[test]
    fn injector_sends_ops_to_the_focused_engine() {
        let core = active_core();
        let rec = Arc::new(Recorder::default());
        injector(&core, rec.clone())
            .run(&[
                Action::Text("測試".into()),
                Action::NewLine(NewlineKey::ShiftEnter),
            ])
            .unwrap();
        let sent = lock(&rec.sent).clone();
        assert_eq!(sent.len(), 3);
        assert!(sent.iter().all(|(p, _)| p == E));
        assert_eq!(sent[0].1, EngineOp::Commit("測試".into()));
        assert_eq!(
            sent[2].1,
            EngineOp::Key {
                keyval: 0xff0d,
                keycode: 28,
                state: 1 | IBUS_RELEASE_MASK
            }
        );
    }

    #[test]
    fn injector_errors_without_a_focused_engine_and_sends_nothing() {
        let core = active_core();
        core.focus_out(E);
        let rec = Arc::new(Recorder::default());
        let err = injector(&core, rec.clone())
            .run(&[Action::Text("不應送出".into())])
            .unwrap_err();
        assert_eq!(err, InjectError::Os(NO_FOCUSED_INPUT.into()));
        assert!(!err.to_string().contains("不應送出"));
        assert!(lock(&rec.sent).is_empty());
        // Empty plans are fine even without focus.
        assert!(injector(&core, rec).run(&[]).is_ok());
    }

    #[test]
    fn injector_reports_partial_failure() {
        let core = active_core();
        let rec = Arc::new(Recorder {
            fail_at: Some(1),
            ..Default::default()
        });
        let err = injector(&core, rec)
            .run(&[
                Action::Text("一".into()),
                Action::NewLine(NewlineKey::Enter),
            ])
            .unwrap_err();
        assert_eq!(err, InjectError::Partial { sent: 1, total: 3 });
    }

    #[test]
    fn hotkey_delivers_only_while_started_and_engine_active() {
        let core = Arc::new(Core::default());
        core.created(E);
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut hk = IbusHotkey { core: core.clone() };
        let ev = events.clone();
        hk.start(Box::new(move |e| lock(&ev).push(e))).unwrap();

        // Not focused/enabled: passes through, no events.
        assert!(!core.key(E, KEY_CONTROL_R, 97, 0));
        core.focus_in(E);
        core.enable(E);
        assert!(core.key(E, KEY_CONTROL_R, 97, 0));
        assert!(core.key(E, KEY_CONTROL_R, 97, IBUS_RELEASE_MASK));
        assert!(!core.key(E, 0x61, 30, 0));
        hk.stop();
        assert!(!core.key(E, KEY_CONTROL_R, 97, 0), "stopped: pass through");

        let keys: Vec<_> = lock(&events)
            .iter()
            .map(|e| match e {
                Event::KeyDown { key, .. } => (true, *key),
                Event::KeyUp { key, .. } => (false, *key),
                Event::Tick { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (true, Key::Trigger),
                (false, Key::Trigger),
                (true, Key::Other)
            ]
        );
    }

    #[test]
    fn hotkey_events_drive_the_trigger_and_injector_types_the_result() {
        // Engine + trigger + injector wired together like the desktop controller does.
        let core = active_core();
        let trigger = Arc::new(Mutex::new(Trigger::new(TriggerConfig::from(
            &HotkeyConfig::default(),
        ))));
        let cmds = Arc::new(Mutex::new(Vec::new()));
        let mut hk = IbusHotkey { core: core.clone() };
        let (t, c) = (trigger.clone(), cmds.clone());
        hk.start(Box::new(move |e| {
            // Re-stamp with synthetic times so the hold threshold is met deterministically.
            let e = match e {
                Event::KeyDown { key, .. } => Event::KeyDown { key, at_ms: 1000 },
                Event::KeyUp { key, .. } => Event::KeyUp { key, at_ms: 3000 },
                t => t,
            };
            lock(&c).extend(lock(&t).on_event(e));
        }))
        .unwrap();
        core.key(E, KEY_CONTROL_R, 97, 0);
        core.key(E, KEY_CONTROL_R, 97, IBUS_RELEASE_MASK);
        assert_eq!(
            *lock(&cmds),
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
        let rec = Arc::new(Recorder::default());
        injector(&core, rec.clone())
            .run(&[Action::Text("辨識結果".into())])
            .unwrap();
        assert_eq!(lock(&rec.sent).len(), 1);
    }

    #[test]
    fn component_matches_the_packaged_xml() {
        let xml = include_str!("../../../../../tools/ibus/v2c.xml");
        let c = v2c_component();
        for needle in [
            format!("<name>{}</name>", c.name),
            format!("<exec>{}</exec>", c.exec),
            format!("<name>{}</name>", c.engines[0].name),
            format!("<language>{}</language>", c.engines[0].language),
            format!("<layout>{}</layout>", c.engines[0].layout),
            format!("<symbol>{}</symbol>", c.engines[0].symbol),
        ] {
            assert!(xml.contains(&needle), "v2c.xml lacks {needle}");
        }
    }

    #[test]
    fn errors_never_contain_text() {
        let core = active_core();
        let rec = Arc::new(Recorder {
            fail_at: Some(0),
            ..Default::default()
        });
        let err = injector(&core, rec)
            .run(&[Action::Paste("機密句子".into())])
            .unwrap_err();
        assert!(!format!("{err} {err:?}").contains("機密"));
    }
}
