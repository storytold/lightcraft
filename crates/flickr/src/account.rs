//! The Flickr account saved on this computer: the API key and secret the user entered (if any)
//! and the signed-in token, as JSON in the host's per-user config folder (`flickr.json`; never in
//! a library, which may be shared or backed up elsewhere).
//!
//! The file holds secrets, so it is written atomically (a temp file renamed into place) and, on
//! Unix, readable by its owner only. On Windows the per-user `%APPDATA%` folder is already
//! private to the account.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{ApiKey, Token};

/// Largest account file read (it is well under a kilobyte).
const MAX_FILE: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Account {
    /// The user's own API key (empty: the build's, if it has one).
    pub api_key: String,
    pub api_secret: String,
    /// The signed-in account, if any.
    pub token: Option<Token>,
}

impl Account {
    /// The user's own key, when both halves are set.
    pub fn own_key(&self) -> Option<ApiKey> {
        let k = ApiKey { key: self.api_key.trim().to_string(), secret: self.api_secret.trim().to_string() };
        k.is_set().then_some(k)
    }
}

/// Read the account at `path`: a missing file is no account. A file that can't be read or parsed
/// is an error naming it (the caller keeps it and starts signed out, so a bad file never blocks
/// the app).
pub fn load(path: &Path) -> Result<Account, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Account::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if meta.len() > MAX_FILE {
        return Err(format!("{}: larger than {MAX_FILE} bytes; not a Flickr account file", path.display()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Save `account` at `path` (its folder is created), replacing the file atomically.
pub fn save(path: &Path, account: &Account) -> Result<(), String> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).ok_or_else(|| format!("{}: no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = serde_json::to_string_pretty(account).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), crate::oauth::unique()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, json.as_bytes())?;
        owner_only(&tmp)?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(unix)]
fn owner_only(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn owner_only(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-flickr-account-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn round_trips_and_a_missing_file_is_no_account() {
        let d = dir("roundtrip");
        let path = d.join("sub").join("flickr.json");
        assert_eq!(load(&path).unwrap(), Account::default());
        let a = Account {
            api_key: "key".into(),
            api_secret: "secret".into(),
            token: Some(Token { token: "t".into(), secret: "s".into(), user_nsid: "1@N01".into(), username: "me".into(), fullname: "Me".into() }),
        };
        save(&path, &a).unwrap();
        assert_eq!(load(&path).unwrap(), a);
        assert_eq!(a.own_key(), Some(ApiKey { key: "key".into(), secret: "secret".into() }));
        assert_eq!(Account::default().own_key(), None);
        // no temp files left behind
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_damaged_or_huge_file_is_an_error_not_a_panic() {
        let d = dir("damaged");
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("flickr.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).unwrap_err().contains("flickr.json"));
        std::fs::write(&path, vec![b' '; MAX_FILE as usize + 1]).unwrap();
        assert!(load(&path).is_err());
        // fields from a newer version are ignored, missing ones default
        std::fs::write(&path, r#"{"apiKey":"k","future":1}"#).unwrap();
        assert_eq!(load(&path).unwrap().api_key, "k");
        let _ = std::fs::remove_dir_all(&d);
    }
}
