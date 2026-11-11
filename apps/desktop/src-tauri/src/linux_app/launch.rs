//! How this process was started and which IBus registration it uses (pure, unit-tested).
//!
//! - `v2c --ibus` is the `<exec>` of the installed component (`tools/ibus/v2c.xml`):
//!   ibus-daemon runs it when the v2c engine is needed and no process owns the component
//!   name yet. That process becomes the normal app, started hidden (like autostart), and
//!   claims the name ([`Registration::Launched`]).
//! - If v2c is already running, the single-instance plugin hands the `--ibus` launch over
//!   to it and the new process exits; the running app then (re)connects to IBus instead of
//!   showing its window ([`is_ibus_launch`]). So there is only ever one engine and one
//!   hotkey consumer.
//! - A user-launched app claims the component name itself when the component XML is
//!   installed. Without it, IBus is left alone (portal / evdev fallback) unless a developer
//!   asks for runtime registration ([`Registration::Dynamic`], `V2C_IBUS=dynamic`): with
//!   ibus 1.5.26 removing a dynamic component can unset the focused field's global engine
//!   (T3.5), so it is never done implicitly.
//! - `V2C_IBUS` overrides the choice: `off` (no IBus; portal / evdev fallback only, also
//!   for headless test runs), `launched`, `dynamic`.

use std::path::{Path, PathBuf};

use v2c_platform::linux::ibus::Registration;

/// Command-line flag in the component's `<exec>`.
pub const IBUS_ARG: &str = "--ibus";
/// Environment override, see the module docs.
pub const IBUS_ENV: &str = "V2C_IBUS";
/// ibus-daemon's component search path override (colon-separated directories).
const COMPONENT_PATH_ENV: &str = "IBUS_COMPONENT_PATH";
const DEFAULT_COMPONENT_DIR: &str = "/usr/share/ibus/component";
/// File name of the installed component (`tools/ibus/v2c.xml`, installed by the .deb).
const COMPONENT_FILE: &str = "v2c.xml";

/// What the app does with IBus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IbusMode {
    /// Do not touch IBus; use the portal / evdev hotkey and the portal injector.
    Off,
    /// Claim the component name if the component XML is installed, otherwise no IBus.
    Auto,
    /// Always claim the component name (started by ibus-daemon, or forced).
    Launched,
    /// Always register the component at runtime (development).
    Dynamic,
}

/// Whether the arguments (including `argv[0]`) contain [`IBUS_ARG`].
pub fn is_ibus_launch<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().skip(1).any(|a| a.as_ref() == IBUS_ARG)
}

/// The mode from the arguments and the value of [`IBUS_ENV`]. The environment wins, so a
/// developer (or a headless test run) can switch IBus off even for an `--ibus` launch.
/// Unknown values are ignored (the second element is `true` then, so the caller can warn).
pub fn ibus_mode<S: AsRef<str>>(args: &[S], env: Option<&str>) -> (IbusMode, bool) {
    let from_args = if is_ibus_launch(args) {
        IbusMode::Launched
    } else {
        IbusMode::Auto
    };
    match env.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("auto") => (from_args, false),
        Some("off" | "0" | "false" | "no") => (IbusMode::Off, false),
        Some("launched") => (IbusMode::Launched, false),
        Some("dynamic") => (IbusMode::Dynamic, false),
        Some(_) => (from_args, true),
    }
}

/// The registration to use at startup, or `None` to start without IBus.
/// `component_installed`: the component XML is on ibus-daemon's search path.
pub fn registration(mode: IbusMode, component_installed: bool) -> Option<Registration> {
    match mode {
        IbusMode::Off => None,
        IbusMode::Launched => Some(Registration::Launched),
        IbusMode::Dynamic => Some(Registration::Dynamic),
        IbusMode::Auto if component_installed => Some(Registration::Launched),
        IbusMode::Auto => None,
    }
}

/// Candidate paths of the installed component XML: each directory of
/// `IBUS_COMPONENT_PATH` if set (ibus-daemon then ignores the default), else
/// `/usr/share/ibus/component`.
pub fn component_xml_paths(component_path_env: Option<&str>) -> Vec<PathBuf> {
    let dirs: Vec<&str> = match component_path_env {
        Some(v) if !v.trim().is_empty() => v.split(':').filter(|d| !d.is_empty()).collect(),
        _ => vec![DEFAULT_COMPONENT_DIR],
    };
    dirs.into_iter()
        .map(|d| Path::new(d).join(COMPONENT_FILE))
        .collect()
}

/// Whether the v2c component XML is installed (reads the file system).
pub fn component_installed() -> bool {
    let env = std::env::var(COMPONENT_PATH_ENV).ok();
    component_xml_paths(env.as_deref())
        .iter()
        .any(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibus_flag_is_detected_after_argv0_only() {
        assert!(is_ibus_launch(&["/usr/bin/v2c", "--ibus"]));
        assert!(is_ibus_launch(&["v2c", "--autostart", "--ibus"]));
        assert!(!is_ibus_launch(&["v2c"]));
        assert!(!is_ibus_launch(&["--ibus"]), "argv[0] is not an argument");
        assert!(!is_ibus_launch(&["v2c", "--ibus=1", "-ibus", "--IBUS"]));
        assert!(!is_ibus_launch::<&str>(&[]));
    }

    #[test]
    fn mode_from_args_and_env() {
        let ibus = ["v2c", "--ibus"];
        let plain = ["v2c"];
        assert_eq!(ibus_mode(&ibus, None), (IbusMode::Launched, false));
        assert_eq!(ibus_mode(&plain, None), (IbusMode::Auto, false));
        assert_eq!(ibus_mode(&plain, Some("")), (IbusMode::Auto, false));
        assert_eq!(ibus_mode(&plain, Some("auto")), (IbusMode::Auto, false));
        assert_eq!(ibus_mode(&ibus, Some("auto")), (IbusMode::Launched, false));
        for off in ["off", "OFF", " 0 ", "false", "no"] {
            assert_eq!(ibus_mode(&ibus, Some(off)), (IbusMode::Off, false), "{off}");
        }
        assert_eq!(
            ibus_mode(&plain, Some("dynamic")),
            (IbusMode::Dynamic, false)
        );
        assert_eq!(
            ibus_mode(&plain, Some("Launched")),
            (IbusMode::Launched, false)
        );
        assert_eq!(ibus_mode(&plain, Some("bogus")), (IbusMode::Auto, true));
        assert_eq!(ibus_mode(&ibus, Some("bogus")), (IbusMode::Launched, true));
    }

    #[test]
    fn registration_choice() {
        use IbusMode::*;
        assert_eq!(registration(Off, true), None);
        assert_eq!(registration(Launched, false), Some(Registration::Launched));
        assert_eq!(registration(Dynamic, true), Some(Registration::Dynamic));
        assert_eq!(registration(Auto, true), Some(Registration::Launched));
        // Without the installed component nothing is registered on the user's daemon.
        assert_eq!(registration(Auto, false), None);
    }

    #[test]
    fn component_paths() {
        assert_eq!(
            component_xml_paths(None),
            vec![PathBuf::from("/usr/share/ibus/component/v2c.xml")]
        );
        assert_eq!(component_xml_paths(Some("  ")), component_xml_paths(None));
        assert_eq!(
            component_xml_paths(Some("/a:/b/c::")),
            vec![PathBuf::from("/a/v2c.xml"), PathBuf::from("/b/c/v2c.xml")]
        );
    }
}
