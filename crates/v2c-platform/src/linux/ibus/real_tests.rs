//! Real IBus tests (`#[ignore]`, names contain `ibus_real`):
//!
//! `cargo test -p v2c-platform -- --ignored ibus_real --test-threads=1`
//!
//! - `ibus_real_engine_round_trip` starts a **private** ibus-daemon (the installed 1.5.26
//!   binary) on its own abstract socket, with panel/config/emoji disabled, no registry
//!   cache, `IBUS_ADDRESS_FILE` / XDG dirs under `tests/tmp/`, and the session bus made
//!   unreachable — so the user's daemon, GNOME input sources and focused apps are never
//!   touched. It registers a temporary `v2c-test` engine (`RegisterComponent`), acts as a
//!   client (`CreateInputContext` → `SetCapabilities(FOCUS)` → `FocusIn` →
//!   `SetEngine("v2c-test")`), sends synthetic key events, checks the commit / forwarded
//!   Return, then cleans up and checks the engine is gone.
//!
//!   Why not the user's daemon: in 1.5.26, when a dynamically registered component goes
//!   away, `_component_destroy_cb` → `bus_ibus_impl_check_global_engine` only looks for the
//!   current global engine (here `xkb:us::eng`, a registry engine) in the *dynamic* list,
//!   does not find it, and may call `bus_ibus_impl_set_global_engine (ibus, NULL)`, which
//!   detaches the engine from whatever text field the user has focused.
//! - `ibus_real_user_daemon_read_only` only discovers and connects to the user's daemon and
//!   reads two properties.
//!
//! Only synthetic strings are used.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use v2c_core::config::{HotkeyConfig, NewlineKey};
use v2c_core::inject_plan::Action;
use v2c_core::trigger::{Command as TriggerCommand, Event, Key, Trigger, TriggerConfig};
use v2c_core::Mode;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::address::parse_address_file;
use super::engine::{IBUS_IFACE, IBUS_PATH, IBUS_SERVICE};
use super::keyevent::{
    IBUS_CONTROL_MASK, IBUS_RELEASE_MASK, IBUS_SHIFT_MASK, KEYCODE_ENTER, KEY_CONTROL_R, KEY_RETURN,
};
use super::serial::text_of;
use super::{ComponentDesc, EngineDesc, IbusAddress, IbusEngine, Registration};
use crate::{Hotkey, InjectError, Injector};

const IC_IFACE: &str = "org.freedesktop.IBus.InputContext";
const SERVICE_IFACE: &str = "org.freedesktop.IBus.Service";
const IBUS_CAP_FOCUS: u32 = 1 << 3;
const KC_RCTRL: u32 = 97;
const TEST_ENGINE: &str = "v2c-test";

fn tmp_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/tmp")
}

/// A private ibus-daemon, killed on drop.
struct PrivateDaemon {
    child: Child,
    dir: PathBuf,
    address: IbusAddress,
}

impl PrivateDaemon {
    fn start() -> Self {
        let dir = tmp_root().join(format!("ibus-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "config", "cache", "data"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let addr_file = dir.join("address");
        let log = std::fs::File::create(dir.join("daemon.log")).unwrap();
        let socket = format!("unix:abstract=v2c-ibus-test-{}", std::process::id());
        let child = Command::new("ibus-daemon")
            .args([
                "--single",
                "--panel=disable",
                "--emoji-extension=disable",
                "--config=disable",
                "--cache=none",
                "--timeout=5000",
                "--verbose",
            ])
            .arg(format!("--address={socket}"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("IBUS_ADDRESS_FILE", &addr_file)
            // No session bus: the private daemon cannot claim or disturb anything there.
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/v2c-ibus-test",
            )
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("ibus-daemon not found (IBus is required for ibus_real tests)");
        let pid = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(10);
        let address = loop {
            if let Ok(s) = std::fs::read_to_string(&addr_file) {
                if let (Some(a), Some(p)) = parse_address_file(&s) {
                    assert_eq!(p, pid, "address file written by another daemon?");
                    break IbusAddress::new(a);
                }
            }
            assert!(
                Instant::now() < deadline,
                "private ibus-daemon did not write {}",
                addr_file.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        eprintln!("private ibus-daemon pid {pid} at {}", address.as_str());
        Self {
            child,
            dir,
            address,
        }
    }
}

impl Drop for PrivateDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn connect(address: &IbusAddress) -> Connection {
    zbus::blocking::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .unwrap()
}

fn ibus_proxy(conn: &Connection) -> Proxy<'_> {
    Proxy::new(conn, IBUS_SERVICE, IBUS_PATH, IBUS_IFACE).unwrap()
}

fn get_property(conn: &Connection, name: &str) -> OwnedValue {
    let p = Proxy::new(
        conn,
        IBUS_SERVICE,
        IBUS_PATH,
        "org.freedesktop.DBus.Properties",
    )
    .unwrap();
    let (v,): (OwnedValue,) = p
        .call_method("Get", &(IBUS_IFACE, name))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    v
}

/// Names of the dynamically registered engines (`ActiveEngines` = register_engine_list).
fn active_engine_names(conn: &Connection) -> Vec<String> {
    fn unwrap(v: &Value<'_>) -> Value<'static> {
        match v {
            Value::Value(inner) => unwrap(inner),
            v => v.try_to_owned().unwrap().into(),
        }
    }
    let v = get_property(conn, "ActiveEngines");
    let Value::Array(a) = unwrap(&v) else {
        panic!("ActiveEngines is not an array")
    };
    a.inner()
        .iter()
        .filter_map(|e| match unwrap(e) {
            Value::Structure(s) => match &s.fields()[2] {
                Value::Str(n) => Some(n.as_str().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Signals the input context sent to the test client.
#[derive(Debug, PartialEq)]
enum IcSignal {
    Commit(String),
    Forward(u32, u32, u32),
}

fn test_component() -> ComponentDesc {
    ComponentDesc {
        name: "org.freedesktop.IBus.V2cTest".into(),
        description: "v2c test engine".into(),
        version: "0".into(),
        license: "MIT".into(),
        author: "v2c tests".into(),
        engines: vec![EngineDesc {
            name: TEST_ENGINE.into(),
            longname: "v2c test".into(),
            language: "zh_TW".into(),
            layout: "us".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
#[ignore = "starts a private ibus-daemon; run with --ignored ibus_real"]
fn ibus_real_engine_round_trip() {
    let daemon = PrivateDaemon::start();

    // --- engine side ---
    let engine = IbusEngine::start(&daemon.address, &test_component(), Registration::Dynamic)
        .expect("RegisterComponent");
    let mut hotkey = engine.hotkey();
    let injector = engine.injector();
    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    hotkey
        .start(Box::new(move |e| {
            let _ = ev_tx.send(e);
        }))
        .unwrap();

    // --- client side (acts like a text field) ---
    let client = connect(&daemon.address);
    let registered = active_engine_names(&client);
    eprintln!("ActiveEngines after RegisterComponent: {registered:?}");
    assert!(
        registered.contains(&TEST_ENGINE.to_string()),
        "v2c-test not registered"
    );
    let (ic,): (OwnedObjectPath,) = ibus_proxy(&client)
        .call_method("CreateInputContext", &("v2c-test-client",))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    eprintln!("input context {}", ic.as_str());
    let ic_proxy = Proxy::new(&client, IBUS_SERVICE, ic.as_str(), IC_IFACE).unwrap();

    let (sig_tx, sig_rx) = mpsc::channel::<IcSignal>();
    // ibus-daemon sends input-context signals with sender `org.freedesktop.IBus` (a
    // well-known name; `bus_input_context_send_signal`). zbus 5.19 panics when it rebuilds
    // such a header (`Message::header()`: the sender must be a unique name), so this test
    // client only looks at the message type and body signature. The engine side never
    // receives such messages.
    let iter = MessageIterator::from(&client);
    std::thread::spawn(move || {
        for msg in iter {
            let Ok(msg) = msg else { break };
            if msg.message_type() != zbus::message::Type::Signal {
                continue;
            }
            let body = msg.body();
            let sig = match body.signature().to_string().trim_matches(['(', ')']) {
                "v" => match body.deserialize::<(OwnedValue,)>() {
                    Ok((v,)) => match text_of(&v) {
                        Some(t) => IcSignal::Commit(t),
                        None => continue,
                    },
                    Err(_) => continue,
                },
                "uuu" => {
                    let (a, b, c): (u32, u32, u32) = body.deserialize().unwrap();
                    IcSignal::Forward(a, b, c)
                }
                _ => continue,
            };
            if sig_tx.send(sig).is_err() {
                break;
            }
        }
    });

    ic_proxy
        .call_method("SetCapabilities", &(IBUS_CAP_FOCUS,))
        .unwrap();
    ic_proxy.call_method("FocusIn", &()).unwrap();
    ic_proxy
        .call_method("SetEngine", &(TEST_ENGINE,))
        .expect("SetEngine(v2c-test)");
    wait_until("engine FocusIn + Enable", || engine.engine_active());
    assert!(!engine.has_focused_input(), "no key from the field yet");
    eprintln!("engine is enabled and focused");

    let key = |keyval: u32, keycode: u32, state: u32| -> bool {
        let (handled,): (bool,) = ic_proxy
            .call_method("ProcessKeyEvent", &(keyval, keycode, state))
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        handled
    };
    let next_event = || ev_rx.recv_timeout(Duration::from_secs(5)).expect("event");
    let kind = |e: Event| match e {
        Event::KeyDown { key, .. } => (true, key),
        Event::KeyUp { key, .. } => (false, key),
        Event::Tick { .. } => unreachable!(),
    };

    // Hold Right Ctrl ~0.5 s through the real daemon → trigger hold-to-talk.
    let mut trigger = Trigger::new(TriggerConfig::from(&HotkeyConfig::default()));
    let mut cmds = Vec::new();
    assert!(
        key(KEY_CONTROL_R, KC_RCTRL, 0),
        "Control_R press not swallowed"
    );
    let down = next_event();
    assert_eq!(kind(down), (true, Key::Trigger));
    cmds.extend(trigger.on_event(down));
    std::thread::sleep(Duration::from_millis(500));
    assert!(key(KEY_CONTROL_R, KC_RCTRL, IBUS_CONTROL_MASK), "repeat");
    assert!(key(
        KEY_CONTROL_R,
        KC_RCTRL,
        IBUS_CONTROL_MASK | IBUS_RELEASE_MASK
    ));
    let up = next_event();
    assert_eq!(kind(up), (false, Key::Trigger));
    cmds.extend(trigger.on_event(up));
    assert_eq!(
        cmds,
        vec![
            TriggerCommand::StartRecording(Mode::Dictate),
            TriggerCommand::StopAndProcess
        ]
    );
    eprintln!("Right Ctrl hold via ibus-daemon -> {cmds:?}");

    // Ordinary typing passes through.
    assert!(!key(0x61, 30, 0), "'a' must pass through");
    assert!(!key(0x61, 30, IBUS_RELEASE_MASK));
    assert_eq!(kind(next_event()), (true, Key::Other));
    assert_eq!(kind(next_event()), (false, Key::Other));
    eprintln!("'a' passed through (handled=false)");

    // Output: commit text + newline keys.
    let text = "測試中文：你好，世界 v2c";
    injector
        .run(&[
            Action::Text(text.into()),
            Action::NewLine(NewlineKey::Enter),
            Action::Paste("第二行".into()),
            Action::NewLine(NewlineKey::ShiftEnter),
        ])
        .unwrap();
    let mut got = Vec::new();
    while got.len() < 6 {
        got.push(
            sig_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("input context signal"),
        );
    }
    let ret = |state| IcSignal::Forward(KEY_RETURN, KEYCODE_ENTER, state);
    assert_eq!(
        got,
        vec![
            IcSignal::Commit(text.into()),
            ret(0),
            ret(IBUS_RELEASE_MASK),
            IcSignal::Commit("第二行".into()),
            ret(IBUS_SHIFT_MASK),
            ret(IBUS_SHIFT_MASK | IBUS_RELEASE_MASK),
        ]
    );
    eprintln!(
        "client received {} signals: 2 CommitText (chars {}, {}), 4 ForwardKeyEvent",
        got.len(),
        text.chars().count(),
        3
    );

    // --- cleanup ---
    ic_proxy.call_method("FocusOut", &()).unwrap();
    wait_until("engine FocusOut", || !engine.has_focused_input());
    // Global-engine mode: the daemon moves the engine to its fake context and enables it
    // again there; output must still be refused.
    wait_until("engine re-focused on the fake context", || {
        engine.engine_active()
    });
    eprintln!("after FocusOut the daemon re-focused the engine on its fake context");
    let err = injector.run(&[Action::Text("x".into())]).unwrap_err();
    assert!(matches!(err, InjectError::Os(ref m) if m == super::NO_FOCUSED_INPUT));
    eprintln!("after FocusOut injector refuses: {err}");
    Proxy::new(&client, IBUS_SERVICE, ic.as_str(), SERVICE_IFACE)
        .unwrap()
        .call_method("Destroy", &())
        .unwrap();
    hotkey.stop();
    engine.close().unwrap();
    wait_until("v2c-test to be unregistered", || {
        !active_engine_names(&client).contains(&TEST_ENGINE.to_string())
    });
    eprintln!(
        "ActiveEngines after close: {:?}",
        active_engine_names(&client)
    );
    // SetEngine on a fresh context must now fail.
    let (ic2,): (OwnedObjectPath,) = ibus_proxy(&client)
        .call_method("CreateInputContext", &("v2c-test-client-2",))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let ic2_proxy = Proxy::new(&client, IBUS_SERVICE, ic2.as_str(), IC_IFACE).unwrap();
    ic2_proxy
        .call_method("SetCapabilities", &(IBUS_CAP_FOCUS,))
        .unwrap();
    ic2_proxy.call_method("FocusIn", &()).unwrap();
    let e = ic2_proxy
        .call_method("SetEngine", &(TEST_ENGINE,))
        .unwrap_err();
    eprintln!("after close, SetEngine(v2c-test) -> {e}");
    assert!(e.to_string().contains("Can not find engine"), "{e}");
    let _ = Proxy::new(&client, IBUS_SERVICE, ic2.as_str(), SERVICE_IFACE)
        .unwrap()
        .call_method("Destroy", &());
    drop(client);
    drop(daemon);
}

#[test]
#[ignore = "starts a private ibus-daemon; run with --ignored ibus_real"]
fn ibus_real_daemon_exit_is_detected() {
    let daemon = PrivateDaemon::start();
    let engine = IbusEngine::start(&daemon.address, &test_component(), Registration::Dynamic)
        .expect("RegisterComponent");
    assert!(engine.is_connected());
    let (tx, rx) = mpsc::channel();
    engine.on_disconnect(move || {
        let _ = tx.send(());
    });
    drop(daemon); // kills the private daemon
    rx.recv_timeout(Duration::from_secs(5))
        .expect("on_disconnect callback");
    assert!(!engine.is_connected());
    eprintln!("private daemon killed -> is_connected() = false, callback fired");

    // A deliberate close does not fire the callback.
    let daemon = PrivateDaemon::start();
    let engine = IbusEngine::start(&daemon.address, &test_component(), Registration::Dynamic)
        .expect("RegisterComponent");
    let (tx, rx) = mpsc::channel::<()>();
    engine.on_disconnect(move || {
        let _ = tx.send(());
    });
    engine.close().unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    eprintln!("close() -> no disconnect callback");
    drop(daemon);
}

#[test]
#[ignore = "connects to the user's ibus-daemon (read only); run with --ignored ibus_real"]
fn ibus_real_user_daemon_read_only() {
    let address = IbusAddress::discover().expect("discover the user's IBus daemon");
    eprintln!(
        "discovered address kind: {}",
        address.as_str().split('=').next().unwrap_or("")
    );
    let conn = connect(&address);
    let names = active_engine_names(&conn);
    eprintln!(
        "user daemon: {} dynamically registered engine(s)",
        names.len()
    );
    assert!(
        !names.iter().any(|n| n == TEST_ENGINE),
        "a v2c-test engine is registered on the user's daemon"
    );
    let global = get_property(&conn, "GlobalEngine");
    let name = match &*global {
        Value::Value(v) => match v.as_ref() {
            Value::Structure(s) => format!("{:?}", s.fields().get(2)),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    };
    eprintln!("user daemon global engine: {name}");
}
