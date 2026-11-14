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
    // `load` refuses a larger file: saving one would sign the user out at the next start
    if json.len() as u64 > MAX_FILE {
        return Err(format!("{}: the account is larger than {MAX_FILE} bytes; not saved", path.display()));
    }
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), crate::oauth::unique()));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        // owner-only from the moment it exists: the secrets are never readable by others, even briefly
        let mut f = create_owner_only(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    // the rename lives in the folder: sync it too, so a power cut after `Ok` keeps the new file
    sync_dir(dir);
    Ok(())
}

/// Flush a folder's entries to disk (best effort; Windows can't open a folder as a file, and its
/// renames are already durable).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// A new file at `p` (never an existing one), on Unix readable and writable by its owner only.
#[cfg(unix)]
fn create_owner_only(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)
}

#[cfg(not(unix))]
fn create_owner_only(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(p)
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
    fn the_temp_file_is_owner_only_before_anything_is_written() {
        let d = dir("tempmode");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(".flickr.json.tmp");
        let f = create_owner_only(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(f.metadata().unwrap().permissions().mode() & 0o777, 0o600, "private while still empty");
        }
        drop(f);
        assert!(create_owner_only(&p).is_err(), "never reuses a file that is already there");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_account_too_large_to_load_is_not_saved() {
        let d = dir("toolarge");
        let path = d.join("flickr.json");
        let small = Account { api_key: "k".into(), api_secret: "s".into(), token: None };
        save(&path, &small).unwrap();
        let huge = Account { api_key: "k".repeat(MAX_FILE as usize), ..small.clone() };
        assert!(save(&path, &huge).unwrap_err().contains("larger than"));
        assert_eq!(load(&path).unwrap(), small, "the saved account is kept");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temp file left behind");
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
