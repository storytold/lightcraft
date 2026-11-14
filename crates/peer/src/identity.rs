//! This device's identity (a random id, a name people see, an Ed25519 key) and the devices it is
//! paired with, kept in one JSON file (`<settings>/sync/peers.json`).
//!
//! The private key sits in that file, readable only by the user on Unix. It only proves which
//! device is talking; it guards no photo data at rest.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The longest device name kept (characters).
pub const MAX_NAME: usize = 64;

/// This device.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    /// Random, 32 hex digits; never changes.
    pub device_id: String,
    /// What other devices show.
    pub name: String,
    /// The Ed25519 secret key, base64.
    secret: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("device_id", &self.device_id).field("name", &self.name).finish_non_exhaustive()
    }
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Io(format!("no random source: {e}")))?;
    Ok(b)
}

/// A random id: 32 hex digits.
pub fn random_id() -> Result<String> {
    Ok(hex(&random::<16>()?))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A device name cut to [`MAX_NAME`] characters, without control characters; `fallback` if empty.
pub fn clean_name(name: &str, fallback: &str) -> String {
    let s: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let s = s.trim();
    if s.is_empty() { fallback.to_string() } else { s.to_string() }
}

impl Identity {
    pub fn generate(name: &str) -> Result<Identity> {
        let id: [u8; 16] = random()?;
        let secret: [u8; 32] = random()?;
        Ok(Identity { device_id: hex(&id), name: clean_name(name, "Device"), secret: B64.encode(secret) })
    }

    fn signing_key(&self) -> Result<SigningKey> {
        let bytes = B64.decode(&self.secret).map_err(|_| Error::Corrupt("the device key".into()))?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::Corrupt("the device key".into()))?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    /// The public key, base64 (what beacons and paired devices carry).
    pub fn public_key(&self) -> Result<String> {
        Ok(B64.encode(self.signing_key()?.verifying_key().to_bytes()))
    }

    pub(crate) fn sign(&self, msg: &[u8]) -> Result<[u8; 64]> {
        Ok(self.signing_key()?.sign(msg).to_bytes())
    }
}

/// Checks `sig` over `msg` by the device whose public key (base64) is `key`.
pub(crate) fn verify(key: &str, msg: &[u8], sig: &[u8]) -> bool {
    let Ok(bytes) = B64.decode(key) else { return false };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&bytes) else { return false };
    let Ok(sig) = Signature::from_slice(sig) else { return false };
    vk.verify(msg, &sig).is_ok()
}

/// A device this one is paired with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub device_id: String,
    pub name: String,
    /// Its Ed25519 public key, base64: a connection claiming this device must prove it.
    pub public_key: String,
    /// When it was paired (Unix seconds).
    pub paired_at: u64,
}

/// What `peers.json` holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub identity: Identity,
    #[serde(default)]
    pub peers: Vec<Peer>,
}

/// The largest `peers.json` read (a damaged or hostile file is refused, not loaded).
const MAX_FILE: u64 = 1 << 20;
/// The most paired devices kept when merging with what another copy of the app wrote.
const MAX_PEERS: usize = 1024;

/// `path` with `suffix` appended to its file name (`peers.json` → `peers.json.corrupt`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_else(|| "peers.json".into());
    name.push(suffix);
    path.with_file_name(name)
}

/// Holds `<path>.lock` while this copy of the app reads, merges and writes the store, so two
/// copies don't interleave. `None` (no lock; the save still goes ahead) where the file system has
/// no locks or another copy holds it for seconds.
fn lock_store(path: &Path) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(sibling(path, ".lock")).ok()?;
    for _ in 0..40 {
        match f.try_lock() {
            Ok(()) => return Some(f),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(std::fs::TryLockError::Error(_)) => return None,
        }
    }
    None
}

/// The comparison key of a device name: case and surrounding blanks don't make two names differ.
fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// `base` (cleaned as [`clean_name`] does), or, when a device in `taken` already has that name,
/// `base` with " 2", " 3"… appended: the first that is free. The result never exceeds
/// [`MAX_NAME`] characters (the base is cut, at a character boundary, to make room). Names are
/// compared without regard to case or surrounding blanks.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let base = clean_name(base, "Device");
    let taken: std::collections::HashSet<String> = taken.iter().map(|t| name_key(t)).collect();
    if !taken.contains(&name_key(&base)) {
        return base;
    }
    // `taken.len() + 1` different candidates: at least one of them is free
    for n in 2..=taken.len().saturating_add(2) {
        let suffix = format!(" {n}");
        let keep = MAX_NAME.saturating_sub(suffix.chars().count());
        let stem: String = base.chars().take(keep).collect();
        let candidate = format!("{}{suffix}", stem.trim_end());
        if !taken.contains(&name_key(&candidate)) {
            return candidate;
        }
    }
    base
}

impl Store {
    /// What the file at `path` holds; `Ok(None)` when there is no file. A file that can't be used
    /// is [`Error::Corrupt`]; one that can't be read is [`Error::Io`].
    fn read(path: &Path) -> Result<Option<Store>> {
        use std::io::Read as _;
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
        };
        let mut bytes = Vec::new();
        f.take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(Error::Corrupt(format!("{} is too large", path.display())));
        }
        let store: Store = serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
        store.identity.signing_key()?;
        Ok(Some(store))
    }

    fn create(path: &Path, name: &str) -> Result<Store> {
        let store = Store { identity: Identity::generate(name)?, peers: Vec::new() };
        store.save(path)?;
        Ok(store)
    }

    /// The store at `path`; a new identity (named `name`) when there is no file yet. A file that
    /// can't be used is an error and stays where it is ([`Store::load_or_recover`] moves it aside).
    pub fn load_or_create(path: &Path, name: &str) -> Result<Store> {
        match Self::read(path)? {
            Some(store) => Ok(store),
            None => Self::create(path, name),
        }
    }

    /// As [`Store::load_or_create`], but a file that can't be used doesn't end sync for good: it
    /// is moved aside (`peers.json.corrupt`, replacing an older one) and a new identity is made.
    /// The second value is then the path of the file moved aside: the pairings in it are lost
    /// (every device has to be paired again), which the caller should tell the person.
    pub fn load_or_recover(path: &Path, name: &str) -> Result<(Store, Option<PathBuf>)> {
        match Self::read(path) {
            Ok(Some(store)) => Ok((store, None)),
            Ok(None) => Ok((Self::create(path, name)?, None)),
            Err(Error::Corrupt(why)) => {
                let aside = sibling(path, ".corrupt");
                let _ = std::fs::remove_file(&aside);
                std::fs::rename(path, &aside).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
                log::warn!("peer: {why}; moved it to {} and started fresh (pairings lost)", aside.display());
                Ok((Self::create(path, name)?, Some(aside)))
            }
            Err(e) => Err(e),
        }
    }

    /// Writes the store atomically (a temporary file of its own in the same folder, then rename),
    /// private to the user on Unix.
    pub fn save(&self, path: &Path) -> Result<()> {
        let io = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
        let json = serde_json::to_vec_pretty(self).map_err(|e| Error::Io(e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        // a name of its own: two copies of the app saving at once don't write the same file
        let unique = random::<8>().map(|b| hex(&b)).unwrap_or_default();
        let tmp = sibling(path, &format!(".{}-{unique}.tmp", std::process::id()));
        let written = (|| {
            use std::io::Write as _;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.map_err(io)
    }

    /// Saves without undoing what another copy of the app saved since this one loaded: the file
    /// is read again and the paired devices only it knows are kept (the union of both lists; of
    /// two entries for one device the later pairing wins), except `removed`, the device being
    /// unpaired now. A file of another identity, or one that can't be read, is replaced.
    /// A lock file (`peers.json.lock`) keeps two copies from doing this at the same moment.
    pub fn save_merged(&mut self, path: &Path, removed: Option<&str>) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        }
        let _lock = lock_store(path);
        if let Ok(Some(disk)) = Self::read(path)
            && disk.identity.device_id == self.identity.device_id
        {
            for theirs in disk.peers {
                if removed == Some(theirs.device_id.as_str()) {
                    continue;
                }
                let room = self.peers.len() < MAX_PEERS;
                match self.peers.iter_mut().find(|p| p.device_id == theirs.device_id) {
                    Some(mine) if theirs.paired_at > mine.paired_at => *mine = theirs,
                    Some(_) => {}
                    None if room => self.peers.push(theirs),
                    None => {}
                }
            }
        }
        self.save(path)
    }

    /// Whether this device's name is also the name of a paired device or of one of `others`
    /// (the devices found on the network), compared as [`unique_name`] compares.
    pub fn name_collides(&self, others: &[String]) -> bool {
        let mine = name_key(&self.identity.name);
        self.peers.iter().any(|p| name_key(&p.name) == mine) || others.iter().any(|n| name_key(n) == mine)
    }

    /// Stores a paired device's new name; `true` when it changed. Only for a name the device sent
    /// over the authenticated channel, never one from an advert.
    pub fn rename_peer(&mut self, device_id: &str, name: &str) -> bool {
        match self.peers.iter_mut().find(|p| p.device_id == device_id) {
            Some(p) if p.name != name => {
                p.name = name.to_string();
                true
            }
            _ => false,
        }
    }

    pub fn peer(&self, device_id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.device_id == device_id)
    }

    /// Adds or refreshes a paired device.
    pub fn add_peer(&mut self, peer: Peer) {
        self.peers.retain(|p| p.device_id != peer.device_id);
        self.peers.push(peer);
    }

    pub fn remove_peer(&mut self, device_id: &str) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| p.device_id != device_id);
        self.peers.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_check_out_and_forgeries_dont() {
        let a = Identity::generate("A").unwrap();
        let b = Identity::generate("B").unwrap();
        let sig = a.sign(b"hello").unwrap();
        assert!(verify(&a.public_key().unwrap(), b"hello", &sig));
        assert!(!verify(&b.public_key().unwrap(), b"hello", &sig));
        assert!(!verify(&a.public_key().unwrap(), b"hellO", &sig));
        assert!(!verify("not base64!", b"hello", &sig));
        assert!(!verify(&a.public_key().unwrap(), b"hello", &sig[..10]));
    }

    #[test]
    fn the_store_round_trips_and_keeps_the_identity() {
        let dir = std::env::temp_dir().join(format!("lcc-peer-store-{}", std::process::id()));
        let path = dir.join("peers.json");
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Store::load_or_create(&path, "Tablet").unwrap();
        s.add_peer(Peer { device_id: "x".into(), name: "Desk".into(), public_key: s.identity.public_key().unwrap(), paired_at: 1 });
        s.save(&path).unwrap();
        let again = Store::load_or_create(&path, "Other").unwrap();
        assert_eq!(again.identity.device_id, s.identity.device_id);
        assert_eq!(again.peers, s.peers);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_store_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("lcc-peer-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, bytes) in
            [&b"{"[..], b"[]", b"{\"identity\":{\"deviceId\":\"a\",\"name\":\"b\",\"secret\":\"AAAA\"}}", b"\xff\xfe"].iter().enumerate()
        {
            let path = dir.join(format!("p{i}.json"));
            std::fs::write(&path, bytes).unwrap();
            assert!(Store::load_or_create(&path, "x").is_err(), "{i}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lcc-peer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn peer(id: &str, name: &str, paired_at: u64) -> Peer {
        Peer { device_id: id.into(), name: name.into(), public_key: "k".into(), paired_at }
    }

    /// A store that can't be used is moved aside and replaced, whatever is in it; one that is
    /// fine, or missing, is left alone.
    #[test]
    fn a_damaged_store_is_moved_aside_and_sync_starts_fresh() {
        let dir = temp_dir("recover");
        let big = vec![b' '; (MAX_FILE + 10) as usize];
        let cases: [&[u8]; 7] =
            [b"", b"{", b"[]", b"null", b"{\"identity\":{\"deviceId\":\"a\",\"name\":\"b\",\"secret\":\"AAAA\"}}", b"\xff\xfe\x00", &big];
        for (i, bytes) in cases.iter().enumerate() {
            let path = dir.join(format!("p{i}.json"));
            std::fs::write(&path, bytes).unwrap();
            let (store, lost) = Store::load_or_recover(&path, "Fresh").unwrap();
            let aside = lost.unwrap_or_else(|| panic!("case {i} wasn't reported"));
            assert_eq!(aside, dir.join(format!("p{i}.json.corrupt")));
            assert_eq!(std::fs::read(&aside).unwrap(), *bytes, "{i}");
            assert_eq!(store.identity.name, "Fresh");
            assert!(store.peers.is_empty());
            // the new file is good: loading again reports nothing and keeps the identity
            let (again, lost) = Store::load_or_recover(&path, "Other").unwrap();
            assert!(lost.is_none());
            assert_eq!(again.identity.device_id, store.identity.device_id);
        }
        let (_, lost) = Store::load_or_recover(&dir.join("new.json"), "x").unwrap();
        assert!(lost.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two copies of the app share one file: a save keeps what the other copy paired meanwhile.
    #[test]
    fn a_save_keeps_the_pairings_another_copy_wrote() {
        let dir = temp_dir("merge");
        let path = dir.join("peers.json");
        let mut one = Store::load_or_create(&path, "Desk").unwrap();
        let mut two = Store::load_or_create(&path, "Desk").unwrap();
        one.add_peer(peer("aa", "Tablet", 10));
        one.save_merged(&path, None).unwrap();
        two.add_peer(peer("bb", "Phone", 20));
        two.save_merged(&path, None).unwrap();
        let on_disk = Store::load_or_create(&path, "x").unwrap();
        assert!(on_disk.peer("aa").is_some() && on_disk.peer("bb").is_some());
        assert!(two.peer("aa").is_some(), "the saving copy learns the other's pairing");

        // of two entries for one device the later pairing wins, whichever copy holds it
        one.add_peer(peer("bb", "Phone (old)", 5));
        one.save_merged(&path, None).unwrap();
        assert_eq!(one.peer("bb").unwrap().name, "Phone");

        // unpairing removes the device although the file still lists it
        assert!(one.remove_peer("aa"));
        one.save_merged(&path, Some("aa")).unwrap();
        let on_disk = Store::load_or_create(&path, "x").unwrap();
        assert!(on_disk.peer("aa").is_none() && on_disk.peer("bb").is_some());

        // another identity's file, or junk, is not merged (and is no error)
        let mut stranger = Store { identity: Identity::generate("S").unwrap(), peers: vec![peer("cc", "C", 1)] };
        stranger.save_merged(&path, None).unwrap();
        assert!(stranger.peer("bb").is_none());
        std::fs::write(&path, b"{ not json").unwrap();
        one.save_merged(&path, None).unwrap();
        assert_eq!(Store::load_or_create(&path, "x").unwrap().identity.device_id, one.identity.device_id);
        // no temporary files are left behind
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")), "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taken_name_gets_a_number_and_stays_within_the_limit() {
        let taken = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(unique_name("Desk", &[]), "Desk");
        assert_eq!(unique_name("Desk", &taken(&["Tablet"])), "Desk");
        assert_eq!(unique_name("Desk", &taken(&["desk "])), "Desk 2");
        assert_eq!(unique_name("Desk", &taken(&["Desk", "Desk 2", "DESK 3"])), "Desk 4");
        assert_eq!(unique_name("", &taken(&["Device"])), "Device 2");
        // a full-length name of multi-byte characters is cut at a character boundary
        let long = "ä".repeat(MAX_NAME);
        let got = unique_name(&long, std::slice::from_ref(&long));
        assert_eq!(got.chars().count(), MAX_NAME);
        assert!(got.ends_with(" 2") && got.starts_with('ä'));
        // every name taken up to a large number: still ends, still unique, still within the limit
        let mut many: Vec<String> = (2..500).map(|n| format!("Desk {n}")).collect();
        many.push("Desk".into());
        let got = unique_name("Desk", &many);
        assert_eq!(got, "Desk 500");
        // the same with full-length names, where the number's width changes how much is cut
        let mut wide = vec![long.clone()];
        for _ in 0..120 {
            let next = unique_name(&long, &wide);
            assert!(next.chars().count() <= MAX_NAME && !wide.contains(&next), "{next}");
            wide.push(next);
        }
    }

    #[test]
    fn a_name_shared_with_another_device_is_reported() {
        let mut s = Store { identity: Identity::generate("Desk").unwrap(), peers: vec![peer("aa", "Tablet", 1)] };
        assert!(!s.name_collides(&[]));
        assert!(s.name_collides(&["  desk".to_string()]));
        s.peers.push(peer("bb", "DESK", 2));
        assert!(s.name_collides(&[]));
        assert!(s.rename_peer("bb", "Phone"));
        assert!(!s.rename_peer("bb", "Phone"));
        assert!(!s.rename_peer("zz", "Phone"));
        assert!(!s.name_collides(&[]));
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  \u{7}Tab\nlet ", "D"), "Tablet");
        assert_eq!(clean_name("", "D"), "D");
        assert_eq!(clean_name(&"x".repeat(200), "D").chars().count(), MAX_NAME);
    }
}
