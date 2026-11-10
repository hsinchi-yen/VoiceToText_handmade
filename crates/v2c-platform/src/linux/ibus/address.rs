//! Finding the IBus daemon's private bus, mirroring `ibus_get_address()` /
//! `ibus_get_socket_path()` in ibus 1.5.26 `src/ibusshare.c`:
//!
//! 1. `IBUS_ADDRESS` if set.
//! 2. Otherwise the address file: `IBUS_ADDRESS_FILE` if set, else
//!    `$XDG_CONFIG_HOME/ibus/bus/<machine-id>-<host>-<display>` where, on Wayland,
//!    host = `unix` and display = `$WAYLAND_DISPLAY`; on X11 `DISPLAY=host:N.S` gives
//!    host (empty → `unix`) and `N`. The file holds `IBUS_ADDRESS=` and `IBUS_DAEMON_PID=`
//!    lines; like IBus, the address only counts if that pid is alive.

use std::path::{Path, PathBuf};

use crate::PlatformError;

/// The D-Bus address of a running IBus daemon (a private bus, not the session bus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IbusAddress(String);

/// What [`IbusAddress::discover_with`] reads from the environment, so discovery is testable.
pub(crate) struct DiscoverEnv<'a> {
    pub var: &'a dyn Fn(&str) -> Option<String>,
    pub read: &'a dyn Fn(&Path) -> std::io::Result<String>,
    pub pid_alive: &'a dyn Fn(i32) -> bool,
}

impl IbusAddress {
    /// Wraps a known address (e.g. a private test daemon).
    pub fn new(address: impl Into<String>) -> Self {
        Self(address.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Finds the address of the user's IBus daemon (see the module docs for the rules).
    pub fn discover() -> Result<Self, PlatformError> {
        let var = |k: &str| std::env::var(k).ok();
        let read = |p: &Path| std::fs::read_to_string(p);
        Self::discover_with(&DiscoverEnv {
            var: &var,
            read: &read,
            pid_alive: &pid_alive,
        })
    }

    pub(crate) fn discover_with(env: &DiscoverEnv<'_>) -> Result<Self, PlatformError> {
        if let Some(a) = (env.var)("IBUS_ADDRESS").filter(|a| !a.is_empty()) {
            return Ok(Self(a));
        }
        let path = address_file(env)?;
        let contents = (env.read)(&path).map_err(|e| {
            PlatformError::Ibus(format!(
                "no IBus daemon: cannot read {}: {e}",
                path.display()
            ))
        })?;
        let (address, pid) = parse_address_file(&contents);
        let address = address
            .ok_or_else(|| PlatformError::Ibus(format!("no IBUS_ADDRESS in {}", path.display())))?;
        match pid {
            Some(pid) if (env.pid_alive)(pid) => Ok(Self(address)),
            _ => Err(PlatformError::Ibus(format!(
                "the IBus daemon recorded in {} is not running",
                path.display()
            ))),
        }
    }
}

/// `kill(pid, 0) == 0`, exactly as `ibus_get_address()` checks it.
fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// Path of the address file (`ibus_get_socket_path()`).
pub(crate) fn address_file(env: &DiscoverEnv<'_>) -> Result<PathBuf, PlatformError> {
    if let Some(p) = (env.var)("IBUS_ADDRESS_FILE") {
        return Ok(PathBuf::from(p));
    }
    let machine_id = ["/var/lib/dbus/machine-id", "/etc/machine-id"]
        .iter()
        .find_map(|p| (env.read)(Path::new(p)).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "machine-id".to_string());
    let config_dir = match (env.var)("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => {
            let home = (env.var)("HOME").ok_or_else(|| {
                PlatformError::Ibus("HOME is not set; cannot find the IBus address file".into())
            })?;
            Path::new(&home).join(".config")
        }
    };
    let name = socket_file_name(
        &machine_id,
        (env.var)("WAYLAND_DISPLAY").as_deref(),
        (env.var)("DISPLAY").as_deref(),
    );
    Ok(config_dir.join("ibus").join("bus").join(name))
}

/// `<machine-id>-<host>-<display>` as built by `ibus_get_socket_path()`.
pub(crate) fn socket_file_name(
    machine_id: &str,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> String {
    let (host, number) = match (wayland_display, display) {
        (Some(w), _) => ("unix".to_string(), w.to_string()),
        (None, Some(d)) => {
            let (host, rest) = match d.split_once(':') {
                Some((h, r)) => (h, r),
                None => (d, "0"),
            };
            // Screen number (".S") is dropped.
            let number = rest.split('.').next().unwrap_or("0");
            let host = if host.is_empty() { "unix" } else { host };
            (host.to_string(), number.to_string())
        }
        (None, None) => ("unix".to_string(), "0".to_string()),
    };
    format!("{machine_id}-{host}-{number}")
}

/// Reads `IBUS_ADDRESS=` and `IBUS_DAEMON_PID=` from an address file (comments skipped).
pub(crate) fn parse_address_file(contents: &str) -> (Option<String>, Option<i32>) {
    let mut address = None;
    let mut pid = None;
    for line in contents.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(a) = line.strip_prefix("IBUS_ADDRESS=") {
            address = Some(a.to_string());
        } else if let Some(p) = line.strip_prefix("IBUS_DAEMON_PID=") {
            pid = p.trim().parse().ok();
        }
    }
    (address, pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const FILE: &str = "# This file is created by ibus-daemon, please do not modify it.\n\
        # This file allows processes on the machine to find the\n\
        # ibus session bus with the below address.\n\
        # If the IBUS_ADDRESS environment variable is set, it will\n\
        # be used rather than this file.\n\
        IBUS_ADDRESS=unix:abstract=/home/u/.cache/ibus/dbus-abc,guid=0123\n\
        IBUS_DAEMON_PID=1297\n";

    fn discover(
        vars: &[(&str, &str)],
        files: &[(&str, &str)],
        alive: bool,
    ) -> Result<IbusAddress, PlatformError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let files: HashMap<PathBuf, String> = files
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.to_string()))
            .collect();
        let var = |k: &str| vars.get(k).cloned();
        let read = |p: &Path| {
            files
                .get(p)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        };
        let pid_alive = |_| alive;
        IbusAddress::discover_with(&DiscoverEnv {
            var: &var,
            read: &read,
            pid_alive: &pid_alive,
        })
    }

    #[test]
    fn env_address_wins() {
        let a = discover(&[("IBUS_ADDRESS", "unix:path=/x")], &[], false).unwrap();
        assert_eq!(a.as_str(), "unix:path=/x");
    }

    #[test]
    fn wayland_file_name_uses_unix_and_the_wayland_display() {
        assert_eq!(
            socket_file_name("mid", Some("wayland-0"), Some(":0")),
            "mid-unix-wayland-0"
        );
    }

    #[test]
    fn x11_file_name_splits_host_and_drops_screen() {
        assert_eq!(socket_file_name("mid", None, Some(":1")), "mid-unix-1");
        assert_eq!(socket_file_name("mid", None, Some(":1.0")), "mid-unix-1");
        assert_eq!(socket_file_name("mid", None, Some("box:2.1")), "mid-box-2");
        assert_eq!(socket_file_name("mid", None, None), "mid-unix-0");
    }

    #[test]
    fn reads_the_address_file_for_this_display() {
        let a = discover(
            &[("HOME", "/home/u"), ("WAYLAND_DISPLAY", "wayland-0")],
            &[
                ("/etc/machine-id", "9471dfa5\n"),
                ("/home/u/.config/ibus/bus/9471dfa5-unix-wayland-0", FILE),
            ],
            true,
        )
        .unwrap();
        assert_eq!(
            a.as_str(),
            "unix:abstract=/home/u/.cache/ibus/dbus-abc,guid=0123"
        );
    }

    #[test]
    fn var_lib_machine_id_and_xdg_config_home_take_precedence() {
        let a = discover(
            &[("XDG_CONFIG_HOME", "/cfg"), ("DISPLAY", ":0")],
            &[
                ("/var/lib/dbus/machine-id", "aaa"),
                ("/etc/machine-id", "bbb"),
                ("/cfg/ibus/bus/aaa-unix-0", FILE),
            ],
            true,
        );
        assert!(a.is_ok());
    }

    #[test]
    fn address_file_env_overrides_the_path() {
        let a = discover(
            &[("IBUS_ADDRESS_FILE", "/t/addr")],
            &[("/t/addr", FILE)],
            true,
        );
        assert!(a.is_ok());
    }

    #[test]
    fn dead_daemon_is_an_error() {
        let e = discover(
            &[("IBUS_ADDRESS_FILE", "/t/addr")],
            &[("/t/addr", FILE)],
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("not running"), "{e}");
    }

    #[test]
    fn missing_file_is_an_error() {
        let e = discover(&[("IBUS_ADDRESS_FILE", "/t/none")], &[], true).unwrap_err();
        assert!(matches!(e, PlatformError::Ibus(_)));
    }

    #[test]
    fn parse_skips_comments_and_reads_pid() {
        let (a, p) = parse_address_file(FILE);
        assert!(a.unwrap().starts_with("unix:abstract="));
        assert_eq!(p, Some(1297));
        assert_eq!(parse_address_file("#IBUS_ADDRESS=x\n"), (None, None));
    }
}
