//! Read-only capability probe for the Linux desktop (T3.1, `tasks/plan.md` section 9.2).
//!
//! [`probe`] only reads: environment variables, `/proc/bus/input/devices`, the portal's
//! introspection data and `version` properties, and whether an input device can be opened.
//! It never creates a portal session (that would show a consent dialog) and never reads key
//! events. Everything it finds is returned as the plain data struct [`LinuxCaps`], so the
//! backend choice (`hotkey::select_backend`) is a pure, unit-tested function.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub(crate) const GLOBAL_SHORTCUTS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
pub(crate) const REMOTE_DESKTOP_IFACE: &str = "org.freedesktop.portal.RemoteDesktop";
pub(crate) const CLIPBOARD_IFACE: &str = "org.freedesktop.portal.Clipboard";

/// Linux evdev key codes (`linux/input-event-codes.h`).
pub(crate) const KEY_ESC: u16 = 1;
pub(crate) const KEY_LEFTSHIFT: u16 = 42;
pub(crate) const KEY_RIGHTSHIFT: u16 = 54;
pub(crate) const KEY_RIGHTCTRL: u16 = 97;

/// `XDG_SESSION_TYPE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionType {
    Wayland,
    X11,
    Tty,
    /// Unset or an unrecognised value (kept verbatim).
    Unknown(String),
}

impl SessionType {
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("wayland") => Self::Wayland,
            Some(v) if v.eq_ignore_ascii_case("x11") => Self::X11,
            Some(v) if v.eq_ignore_ascii_case("tty") => Self::Tty,
            Some(v) => Self::Unknown(v.to_string()),
            None => Self::Unknown(String::new()),
        }
    }
}

/// Whether a portal interface is exported by `xdg-desktop-portal`, and its `version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Portal {
    Missing,
    /// `version` is `None` when the interface exists but the property could not be read.
    Present {
        version: Option<u32>,
    },
}

impl Portal {
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

/// Result of trying to open an input device read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAccess {
    Readable,
    /// `EACCES`: the user is not in the `input` group.
    PermissionDenied {
        path: PathBuf,
    },
    /// No `/dev/input/event*` device exists at all.
    NoDevices,
    /// Any other error (kept as text; contains no user data).
    Error(String),
}

/// What evdev offers on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvdevCaps {
    /// Event devices that report Right Ctrl and Esc (i.e. real keyboards).
    pub keyboards: Vec<PathBuf>,
    /// Result of opening the first keyboard, or any event device when there is no keyboard,
    /// so a missing group membership is reported even before a keyboard is plugged in.
    pub access: DeviceAccess,
}

/// Everything the Linux backends need to know, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxCaps {
    pub session: SessionType,
    /// `XDG_CURRENT_DESKTOP`, e.g. `ubuntu:GNOME`.
    pub desktop: Option<String>,
    pub global_shortcuts: Portal,
    pub remote_desktop: Portal,
    pub clipboard: Portal,
    /// Set when the session bus or the portal could not be reached; all portals then read
    /// as [`Portal::Missing`].
    pub portal_error: Option<String>,
    pub evdev: EvdevCaps,
}

/// Probes the running system. Read-only; see the module docs.
pub fn probe() -> LinuxCaps {
    let session = SessionType::from_env_value(std::env::var("XDG_SESSION_TYPE").ok().as_deref());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let (portals, portal_error) = match probe_portals() {
        Ok(p) => (p, None),
        Err(e) => ([Portal::Missing; 3], Some(e)),
    };
    LinuxCaps {
        session,
        desktop,
        global_shortcuts: portals[0],
        remote_desktop: portals[1],
        clipboard: portals[2],
        portal_error,
        evdev: probe_evdev(),
    }
}

/// Reads `[GlobalShortcuts, RemoteDesktop, Clipboard]` from the portal: one `Introspect`
/// call, then `Properties.Get(version)` for each interface that exists.
fn probe_portals() -> Result<[Portal; 3], String> {
    use zbus::blocking::{Connection, Proxy};

    let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
    let introspectable = Proxy::new(
        &conn,
        PORTAL_DEST,
        PORTAL_PATH,
        "org.freedesktop.DBus.Introspectable",
    )
    .map_err(|e| format!("portal proxy: {e}"))?;
    let xml: String = introspectable
        .call("Introspect", &())
        .map_err(|e| format!("portal introspect: {e}"))?;
    let interfaces = parse_interfaces(&xml);

    let read = |iface: &str| -> Portal {
        if !interfaces.iter().any(|i| i == iface) {
            return Portal::Missing;
        }
        let version = Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, iface)
            .and_then(|p| p.get_property::<u32>("version"))
            .ok();
        Portal::Present { version }
    };
    Ok([
        read(GLOBAL_SHORTCUTS_IFACE),
        read(REMOTE_DESKTOP_IFACE),
        read(CLIPBOARD_IFACE),
    ])
}

/// Extracts the `name` of every `<interface>` element of a D-Bus introspection document.
pub(crate) fn parse_interfaces(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<interface") {
        rest = &rest[start + "<interface".len()..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        if let Some(name) = attr(tag, "name") {
            out.push(name.to_string());
        }
        rest = &rest[tag_end..];
    }
    out
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let after = rest[at + name.len()..].trim_start();
        // Require a word boundary before the attribute name.
        let boundary = at == 0 || rest.as_bytes()[at - 1].is_ascii_whitespace();
        if boundary {
            if let Some(value) = after.strip_prefix('=') {
                let value = value.trim_start();
                let quote = value.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let body = &value[1..];
                    return body.find(quote).map(|end| &body[..end]);
                }
            }
        }
        rest = &rest[at + name.len()..];
    }
}

fn probe_evdev() -> EvdevCaps {
    let keyboards = std::fs::read_to_string("/proc/bus/input/devices")
        .map(|s| find_keyboards(&s))
        .unwrap_or_default();
    let target = keyboards.first().cloned().or_else(first_event_device);
    let access = match target {
        None => DeviceAccess::NoDevices,
        // Opening (and immediately closing) a device reads no events.
        Some(path) => match File::open(&path) {
            Ok(_) => DeviceAccess::Readable,
            Err(e) => access_error(&path, &e),
        },
    };
    EvdevCaps { keyboards, access }
}

fn first_event_device() -> Option<PathBuf> {
    let mut devices: Vec<PathBuf> = std::fs::read_dir("/dev/input")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect();
    devices.sort();
    devices.into_iter().next()
}

/// Maps an `open()` error on an input device to [`DeviceAccess`].
pub(crate) fn access_error(path: &Path, e: &io::Error) -> DeviceAccess {
    if e.kind() == io::ErrorKind::PermissionDenied {
        DeviceAccess::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else {
        DeviceAccess::Error(format!("open {}: {e}", path.display()))
    }
}

/// Parses `/proc/bus/input/devices` and returns `/dev/input/eventN` for every device whose
/// key bitmap contains Right Ctrl and Esc. Power buttons and HDMI audio jacks are skipped.
pub(crate) fn find_keyboards(proc_devices: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for block in proc_devices.split("\n\n") {
        let mut event = None;
        let mut keys = None;
        for line in block.lines() {
            if let Some(h) = line.strip_prefix("H: Handlers=") {
                event = h
                    .split_whitespace()
                    .find(|w| w.starts_with("event") && w[5..].parse::<u32>().is_ok())
                    .map(str::to_string);
            } else if let Some(k) = line.strip_prefix("B: KEY=") {
                keys = Some(k.trim().to_string());
            }
        }
        if let (Some(event), Some(keys)) = (event, keys) {
            if has_key(&keys, KEY_RIGHTCTRL) && has_key(&keys, KEY_ESC) {
                out.push(PathBuf::from("/dev/input").join(event));
            }
        }
    }
    out
}

/// Tests bit `code` in a kernel bitmap printed as space-separated hex words of the kernel's
/// `long` size, most significant word first.
fn has_key(bitmap: &str, code: u16) -> bool {
    let bits = usize::BITS as usize;
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let idx = usize::from(code) / bits;
    if idx >= words.len() {
        return false;
    }
    let word = words[words.len() - 1 - idx];
    u64::from_str_radix(word, 16).is_ok_and(|w| w & (1u64 << (usize::from(code) % bits)) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC: &str = "\
I: Bus=0019 Vendor=0000 Product=0003 Version=0000
N: Name=\"Sleep Button\"
H: Handlers=kbd event0
B: EV=3
B: KEY=4000 0 0

I: Bus=0011 Vendor=0001 Product=0001 Version=ab83
N: Name=\"AT Translated Set 2 keyboard\"
H: Handlers=sysrq kbd leds event3
B: EV=120013
B: KEY=402000000 3803078f800d001 feffffdfffefffff fffffffffffffffe

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"HDA Intel PCH HDMI/DP,pcm=3\"
H: Handlers=event5
B: EV=21
";

    #[test]
    fn finds_only_real_keyboards() {
        assert_eq!(
            find_keyboards(PROC),
            vec![PathBuf::from("/dev/input/event3")]
        );
    }

    #[test]
    fn power_buttons_are_not_keyboards() {
        let proc = "H: Handlers=kbd event1\nB: KEY=8000 10000000000000 0\n";
        assert!(find_keyboards(proc).is_empty());
    }

    #[test]
    fn key_bitmap_bits() {
        // Word 0 (last) holds codes 0..63; bit 1 = Esc. Word 1 holds 64..127; 97 = bit 33.
        assert!(has_key("200000000 2", KEY_RIGHTCTRL));
        assert!(has_key("200000000 2", KEY_ESC));
        assert!(!has_key("0 2", KEY_RIGHTCTRL));
        assert!(!has_key("2", KEY_RIGHTCTRL)); // too short
        assert!(!has_key("zz 2", KEY_RIGHTCTRL));
    }

    #[test]
    fn interfaces_from_introspection() {
        let xml = r#"<node>
  <interface name="org.freedesktop.DBus.Properties"><method name="Get"/></interface>
  <interface name='org.freedesktop.portal.RemoteDesktop'>
    <property name="version" type="u" access="read"/>
  </interface>
  <interface  name="org.freedesktop.portal.Settings"/>
</node>"#;
        assert_eq!(
            parse_interfaces(xml),
            vec![
                "org.freedesktop.DBus.Properties",
                "org.freedesktop.portal.RemoteDesktop",
                "org.freedesktop.portal.Settings",
            ]
        );
    }

    #[test]
    fn session_type_from_env() {
        assert_eq!(
            SessionType::from_env_value(Some("wayland")),
            SessionType::Wayland
        );
        assert_eq!(SessionType::from_env_value(Some("x11")), SessionType::X11);
        assert_eq!(
            SessionType::from_env_value(None),
            SessionType::Unknown(String::new())
        );
        assert_eq!(
            SessionType::from_env_value(Some("mir")),
            SessionType::Unknown("mir".into())
        );
    }

    #[test]
    fn eacces_maps_to_permission_denied() {
        let path = Path::new("/dev/input/event3");
        let e = io::Error::from_raw_os_error(13); // EACCES
        assert_eq!(
            access_error(path, &e),
            DeviceAccess::PermissionDenied {
                path: path.to_path_buf()
            }
        );
        let e = io::Error::from_raw_os_error(2); // ENOENT
        assert!(matches!(access_error(path, &e), DeviceAccess::Error(_)));
    }

    /// Real, read-only probe of this machine. Run with `-- --ignored linux_real --nocapture`.
    #[test]
    #[ignore = "reads the real session bus and /dev/input"]
    fn linux_real_probe() {
        let caps = probe();
        println!("{caps:#?}");
        // T3.0 host (Ubuntu 22.04 / GNOME 42): no GlobalShortcuts, RemoteDesktop v1.
        assert_eq!(caps.portal_error, None);
        assert_eq!(caps.global_shortcuts, Portal::Missing);
        assert_eq!(caps.remote_desktop, Portal::Present { version: Some(1) });
        assert_eq!(caps.clipboard, Portal::Missing);
    }

    /// Opens the real input devices; on the T3.0 host the user is not in `input`.
    #[test]
    #[ignore = "opens real /dev/input devices"]
    fn linux_real_evdev_permission_denied() {
        let caps = probe_evdev();
        println!("{caps:#?}");
        assert!(
            matches!(caps.access, DeviceAccess::PermissionDenied { .. }),
            "expected EACCES on /dev/input/event* (user not in the input group)"
        );
    }
}
