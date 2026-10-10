//! The freedesktop Secret Service API (GNOME Keyring, KWallet, KeePassXC) over the D-Bus session
//! bus, with `zbus`'s blocking API.
//!
//! Sources: the freedesktop.org Secret Service API specification (0.2). Own implementation.
//!
//! Items carry the attributes `application` (the app id from `dac_brand`), `service` and
//! `account`; lookups match all three. The session uses the `plain` algorithm: the secret travels
//! over the local session bus to the keyring daemon, as with most clients. A locked collection
//! that needs an unlock prompt is reported as [`CredError::Locked`].

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::{CredError, Key, Secret, SecretStore};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const SESSION: &str = "org.freedesktop.Secret.Session";

/// The Secret struct `(oayays)`: session, parameters, value, content type.
type WireSecret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

fn unavailable(e: zbus::Error) -> CredError {
    CredError::Unavailable(e.to_string())
}

pub(crate) struct SecretService {
    conn: Connection,
}

/// An open `plain` session, closed on drop.
struct Session<'a> {
    conn: &'a Connection,
    path: OwnedObjectPath,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        if let Ok(p) = Proxy::new(self.conn, DEST, self.path.as_ref(), SESSION) {
            let _ = p.call_method("Close", &());
        }
    }
}

fn is_root(p: &ObjectPath<'_>) -> bool {
    p.as_str() == "/"
}

impl SecretService {
    pub(crate) fn connect() -> Result<SecretService, CredError> {
        let conn = Connection::session().map_err(unavailable)?;
        let svc = SecretService { conn };
        // fail now, not at first use, when nothing provides the service
        svc.service()?.call::<_, _, OwnedObjectPath>("ReadAlias", &"default").map_err(unavailable)?;
        Ok(svc)
    }

    fn service(&self) -> Result<Proxy<'_>, CredError> {
        Proxy::new(&self.conn, DEST, SERVICE_PATH, SERVICE).map_err(unavailable)
    }

    fn proxy<'a>(&'a self, path: &'a OwnedObjectPath, iface: &'static str) -> Result<Proxy<'a>, CredError> {
        Proxy::new(&self.conn, DEST, path.as_ref(), iface).map_err(unavailable)
    }

    fn session(&self) -> Result<Session<'_>, CredError> {
        let (_, path): (OwnedValue, OwnedObjectPath) = self.service()?.call("OpenSession", &("plain", Value::from(""))).map_err(unavailable)?;
        Ok(Session { conn: &self.conn, path })
    }

    fn attributes(key: &Key) -> HashMap<String, String> {
        HashMap::from([
            ("application".to_string(), dac_brand::APP_ID.to_string()),
            ("service".to_string(), key.service.clone()),
            ("account".to_string(), key.account.clone()),
        ])
    }

    /// Matching items, unlocked.
    fn find(&self, key: &Key) -> Result<Vec<OwnedObjectPath>, CredError> {
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
            self.service()?.call("SearchItems", &Self::attributes(key)).map_err(unavailable)?;
        if locked.is_empty() {
            return Ok(unlocked);
        }
        let (mut now, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self.service()?.call("Unlock", &locked).map_err(unavailable)?;
        if !is_root(&prompt) && now.is_empty() {
            return Err(CredError::Locked);
        }
        now.extend(unlocked);
        Ok(now)
    }
}

impl SecretStore for SecretService {
    fn name(&self) -> &'static str {
        "Secret Service"
    }

    fn get(&self, key: &Key) -> Result<Option<Secret>, CredError> {
        let Some(item) = self.find(key)?.into_iter().next() else { return Ok(None) };
        let session = self.session()?;
        let (_, _, value, _): WireSecret = self.proxy(&item, ITEM)?.call("GetSecret", &session.path).map_err(unavailable)?;
        let value = zeroize::Zeroizing::new(value);
        let text = std::str::from_utf8(&value).map_err(|_| CredError::Corrupt("the stored secret is not text".into()))?;
        Ok(Some(Secret::new(text)))
    }

    fn set(&self, key: &Key, secret: &Secret) -> Result<(), CredError> {
        let collection: OwnedObjectPath = self.service()?.call("ReadAlias", &"default").map_err(unavailable)?;
        if is_root(&collection) {
            return Err(CredError::Unavailable("the keyring has no default collection".into()));
        }
        let session = self.session()?;
        let label = format!("{} – {} ({})", dac_brand::DISPLAY_NAME, key.service, key.account);
        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
        props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(Self::attributes(key)));
        let wire: WireSecret = (session.path.clone(), Vec::new(), secret.expose().as_bytes().to_vec(), "text/plain; charset=utf8".into());
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) =
            self.proxy(&collection, COLLECTION)?.call("CreateItem", &(props, wire, true)).map_err(unavailable)?;
        if is_root(&item) && !is_root(&prompt) {
            return Err(CredError::Locked);
        }
        log::info!("credentials: stored a secret for {} in the Secret Service", key.service);
        Ok(())
    }

    fn delete(&self, key: &Key) -> Result<bool, CredError> {
        let items = self.find(key)?;
        for item in &items {
            let prompt: OwnedObjectPath = self.proxy(item, ITEM)?.call("Delete", &()).map_err(unavailable)?;
            if !is_root(&prompt) {
                return Err(CredError::Locked);
            }
        }
        if !items.is_empty() {
            log::info!("credentials: removed a secret for {} from the Secret Service", key.service);
        }
        Ok(!items.is_empty())
    }
}
