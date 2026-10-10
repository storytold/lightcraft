//! The connected Immich accounts, as kept in the app's settings (`connections.json`). Never holds
//! the API key: that lives in the secret store under [`secret_key`].

use serde::{Deserialize, Serialize};

use crate::extlib::PathMap;
use crate::types::ServerVersion;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Account {
    /// `<server URL>#<user id>`: unique, and what remote links are written under.
    pub id: String,
    /// Server address (`https://host[:port]`).
    pub url: String,
    pub user_id: String,
    pub user_name: String,
    pub email: String,
    /// Version at the last check.
    pub version: Option<ServerVersion>,
    /// The key's permissions at the last check (`None`: the key can't read them).
    pub permissions: Option<Vec<String>>,
    /// A certificate fingerprint the user confirmed (self-signed servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
    /// External library folder mapping (IMM-EXTLIB).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path_maps: Vec<PathMap>,
    /// The newest `updatedAt` the link pass has seen (incremental listing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_until: Option<String>,
}

impl Account {
    pub fn make_id(url: &str, user_id: &str) -> String {
        format!("{url}#{user_id}")
    }
}

/// The key an account's API key is stored under.
#[cfg(not(target_arch = "wasm32"))]
pub fn secret_key(account_id: &str) -> dac_credentials::Key {
    dac_credentials::Key::new(crate::link::SERVICE, account_id)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Accounts {
    pub immich: Vec<Account>,
}

impl Accounts {
    pub fn get(&self, id: &str) -> Option<&Account> {
        self.immich.iter().find(|a| a.id == id)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Account> {
        self.immich.iter_mut().find(|a| a.id == id)
    }
    /// Add or replace (by id), keeping what the old entry learned (mappings, link progress).
    pub fn upsert(&mut self, mut a: Account) {
        if let Some(old) = self.get_mut(&a.id) {
            if a.path_maps.is_empty() {
                a.path_maps = std::mem::take(&mut old.path_maps);
            }
            if a.linked_until.is_none() {
                a.linked_until = old.linked_until.take();
            }
            *old = a;
        } else {
            self.immich.push(a);
        }
    }
    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.immich.len();
        self.immich.retain(|a| a.id != id);
        n != self.immich.len()
    }

    /// Read from `path`; a missing file is empty, a corrupt one an error the caller shows.
    pub fn load(path: &std::path::Path) -> Result<Accounts, String> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Accounts::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Write atomically (temp file + rename).
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}
