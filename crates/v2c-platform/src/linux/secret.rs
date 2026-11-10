//! freedesktop Secret Service (GNOME Keyring / KWallet) as the keyring-core default store (T3.6).
//!
//! Linux counterpart of the Windows Credential Manager store the desktop app installs at
//! startup. API keys then go through `v2c_core::secrets::KeyringSecretStore` unchanged
//! (service `v2c`, user = `CloudService::id`), and the history key ([`super::protect`]) uses
//! the same store.
//!
//! The store is `zbus-secret-service-keyring-store` (pure-Rust D-Bus via zbus, no libdbus;
//! `crypto-rust` = blocking API with a Rust DH session). Items land in the user's default
//! ("login") collection; a locked collection makes the desktop show its unlock prompt.
//!
//! [`probe`] is a read-only status check (is the service on the session bus, which
//! collections exist, are they locked) for diagnostics and the `secret_real` test. It never
//! creates, reads, or deletes items, and never asks to unlock.
//!
//! Nothing here logs or formats secret values.

use std::sync::Arc;

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
use zbus::names::BusName;
use zbus::zvariant::OwnedObjectPath;

use crate::PlatformError;

/// Well-known bus name of the Secret Service.
pub const SECRETS_DEST: &str = "org.freedesktop.secrets";
const SECRETS_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";

/// Connects to the Secret Service and installs it as `keyring_core`'s default store.
///
/// Call once at startup (desktop `main.rs`), like the Windows Credential Manager store. On
/// error, cloud services report a key error; local use is unaffected.
pub fn install_default_store() -> Result<(), PlatformError> {
    let store = zbus_secret_service_keyring_store::Store::new().map_err(keyring_err)?;
    keyring_core::set_default_store(store as Arc<keyring_core::CredentialStore>);
    Ok(())
}

/// Maps a keyring-core error to a [`PlatformError`]. keyring-core errors carry no secret
/// values; the text names the failure kind only.
pub(crate) fn keyring_err(e: keyring_core::Error) -> PlatformError {
    PlatformError::Os(format!("secret service: {e}"))
}

/// One collection (keyring) as reported by the Secret Service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionInfo {
    /// D-Bus object path, e.g. `/org/freedesktop/secrets/collection/login`.
    pub path: String,
    /// User-visible name (`Label`), e.g. `Login`.
    pub label: String,
    pub locked: bool,
    /// The collection the `default` alias points at (where new items go).
    pub is_default: bool,
}

/// Read-only Secret Service status (see [`probe`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecretServiceStatus {
    /// `org.freedesktop.secrets` currently has an owner on the session bus.
    pub running: bool,
    /// The bus can start it on demand (a `.service` activation file exists).
    pub activatable: bool,
    /// Empty when the service is neither running nor activatable.
    pub collections: Vec<CollectionInfo>,
}

impl SecretServiceStatus {
    /// The service exists (running, or startable on demand).
    pub fn available(&self) -> bool {
        self.running || self.activatable
    }

    /// The `default` collection, if the service has one.
    pub fn default_collection(&self) -> Option<&CollectionInfo> {
        self.collections.iter().find(|c| c.is_default)
    }
}

/// Read-only probe of the Secret Service on the session bus.
///
/// Reads `Collections`, `ReadAlias("default")`, and each collection's `Label` / `Locked`.
/// If the service is only activatable, reading its properties starts it (D-Bus activation);
/// nothing is created, unlocked, or written.
pub fn probe() -> Result<SecretServiceStatus, PlatformError> {
    let conn = Connection::session().map_err(bus_err)?;
    let dbus = DBusProxy::new(&conn).map_err(bus_err)?;
    let name = BusName::try_from(SECRETS_DEST).map_err(|e| PlatformError::Os(e.to_string()))?;
    let running = dbus.name_has_owner(name).map_err(fdo_err)?;
    let activatable = dbus
        .list_activatable_names()
        .map_err(fdo_err)?
        .iter()
        .any(|n| n.as_str() == SECRETS_DEST);
    let mut status = SecretServiceStatus {
        running,
        activatable,
        collections: Vec::new(),
    };
    if !status.available() {
        return Ok(status);
    }
    let service = Proxy::new(&conn, SECRETS_DEST, SECRETS_PATH, SERVICE_IFACE).map_err(bus_err)?;
    let paths: Vec<OwnedObjectPath> = service.get_property("Collections").map_err(bus_err)?;
    // `/` means "no such alias".
    let default: Option<OwnedObjectPath> = service
        .call::<_, _, OwnedObjectPath>("ReadAlias", &("default",))
        .ok()
        .filter(|p| p.as_str() != "/");
    for path in paths {
        let coll =
            Proxy::new(&conn, SECRETS_DEST, path.as_str(), COLLECTION_IFACE).map_err(bus_err)?;
        status.collections.push(CollectionInfo {
            label: coll.get_property("Label").unwrap_or_default(),
            locked: coll.get_property("Locked").unwrap_or(true),
            is_default: default.as_ref() == Some(&path),
            path: path.as_str().to_owned(),
        });
    }
    Ok(status)
}

fn bus_err(e: zbus::Error) -> PlatformError {
    PlatformError::Os(format!("secret service D-Bus: {e}"))
}

fn fdo_err(e: zbus::fdo::Error) -> PlatformError {
    PlatformError::Os(format!("secret service D-Bus: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll(path: &str, default: bool) -> CollectionInfo {
        CollectionInfo {
            path: path.into(),
            label: "Login".into(),
            locked: false,
            is_default: default,
        }
    }

    #[test]
    fn status_available_and_default() {
        let none = SecretServiceStatus::default();
        assert!(!none.available());
        assert!(none.default_collection().is_none());

        let s = SecretServiceStatus {
            running: false,
            activatable: true,
            collections: vec![coll("/a", false), coll("/b", true)],
        };
        assert!(s.available());
        assert_eq!(s.default_collection().unwrap().path, "/b");
    }

    /// Read-only: reports whether the Secret Service is on the session bus and its
    /// collections / locked state. Never writes, reads items, or unlocks.
    /// `cargo test -p v2c-platform -- --ignored secret_real --nocapture`
    #[test]
    #[ignore = "talks to the real session bus (read-only)"]
    fn secret_real_probe() {
        match probe() {
            Ok(s) => {
                println!(
                    "secret service: running={} activatable={} collections={}",
                    s.running,
                    s.activatable,
                    s.collections.len()
                );
                for c in &s.collections {
                    println!(
                        "  {} label={:?} locked={} default={}",
                        c.path, c.label, c.locked, c.is_default
                    );
                }
            }
            Err(e) => println!("secret service probe failed: {e}"),
        }
    }

    /// Installs the store (connects and opens a DH session, no item access).
    #[test]
    #[ignore = "talks to the real session bus (read-only)"]
    fn secret_real_install_store() {
        match install_default_store() {
            Ok(()) => println!("keyring-core default store: Secret Service installed"),
            Err(e) => println!("install_default_store failed: {e}"),
        }
    }
}
