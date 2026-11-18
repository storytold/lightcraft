//! Fetching the update file: resumable, capped, and checked before it is given a real name.
//!
//! The transfer itself is [`lcc_net::Client::download`] (pure Rust, TLS-verified, `Range` resume
//! from a `.part` file, size cap, SHA-256 check, delete on mismatch). This module only decides
//! *where* it goes, *how big* it may be and *what* it must hash to, and names the file after the
//! release so two versions never overwrite each other's parts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lcc_net::{Client, DownloadOptions};

use crate::check::MAX_DOWNLOAD_BYTES;
use crate::check::Update;
use crate::error::UpdateError;
use crate::signature::UpdateKey;

/// Progress: `(bytes so far, total when known)`.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// What arrived, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downloaded {
    pub path: PathBuf,
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the file as it was written.
    pub sha256: String,
    /// The file name, as the release spelled it.
    pub name: String,
}

/// The default staging folder inside a cache root: `<cache>/updates`.
pub fn stage_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("updates")
}

/// A client for fetching update files: a long stall timeout (an installer is large) and a body cap
/// that [`lcc_net::Client::download`] does not need but nothing else may exceed either way.
pub fn client() -> Result<Client, UpdateError> {
    Client::new(lcc_net::ClientConfig {
        connect_timeout: std::time::Duration::from_secs(15),
        stall_timeout: std::time::Duration::from_secs(60),
        max_body: MAX_DOWNLOAD_BYTES,
        ..lcc_net::ClientConfig::default()
    })
    .map_err(|e| UpdateError::Net(e.to_string()))
}

/// `<name>.part`, the file a transfer resumes from (the net crate's own convention).
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// The file name an asset may be saved under: its last path segment, and never `.`/`..`.
///
/// A feed is remote input: an asset name like `../../.ssh/authorized_keys` must not be able to
/// write outside the staging folder.
pub fn safe_file_name(name: &str) -> Result<String, UpdateError> {
    let trimmed = name.trim();
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    if base.is_empty() || base == "." || base == ".." || base.contains('\0') || base.chars().any(|c| c.is_control()) {
        return Err(UpdateError::Install(format!("the release names its file `{trimmed}`, which is not a usable file name")));
    }
    // a conservative length: most file systems stop at 255 bytes
    if base.len() > 180 {
        return Err(UpdateError::Install("the release's file name is too long to save".into()));
    }
    Ok(base.to_string())
}

/// Download `update`'s file into `dir`, checking it against the release's signed checksum list.
///
/// First the release's `SHA256SUMS.txt` and its signature are fetched and checked against `key`
/// (the build's pinned key, [`UpdateKey::pinned`]); the SHA-256 the verified list records for the
/// file is the one the download must match, and the size must be what the feed states. The feed's
/// own hash is not used. No key ([`UpdateError::NoKey`]), a list that cannot be fetched, a missing
/// or wrong signature, or a file the list does not name stop it before the transfer, and whatever
/// was staged under that name is deleted.
///
/// A file already there that still matches the signed SHA-256 is returned as it is (a second click
/// starts no second transfer); a `.part` from an interrupted one is resumed.
pub fn download(
    update: &Update,
    key: Option<&UpdateKey>,
    client: &Client,
    dir: &Path,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Progress<'_>>,
) -> Result<Downloaded, UpdateError> {
    let name = safe_file_name(&update.asset.name)?;
    let Some(key) = key else { return Err(UpdateError::NoKey) };
    std::fs::create_dir_all(dir).map_err(|e| UpdateError::Io(format!("could not create {}: {e}", dir.display())))?;
    let dest = dir.join(&name);
    // where it must not escape to
    if dest.parent() != Some(dir) {
        return Err(UpdateError::Io(format!("refusing to write outside {}", dir.display())));
    }
    // the hash comes from the signed list, and from nowhere else
    let hash_we_want = match crate::signature::signed_sha256(update, key, cancel.clone()) {
        Ok(sha) => sha,
        Err(e) => {
            if e != UpdateError::Cancelled {
                // nothing the release no longer vouches for stays staged
                remove_staged(&dest);
            }
            return Err(e);
        }
    };
    let size = (update.asset.size > 0).then_some(update.asset.size);
    if let Some(done) = already_verified(&dest, &name, size, Some(&hash_we_want)) {
        return Ok(done);
    }
    let cap = match size {
        Some(bytes) => bytes.max(1),
        None => MAX_DOWNLOAD_BYTES,
    };
    let opts = DownloadOptions { size, sha256: Some(hash_we_want), max_size: cap, headers: Vec::new(), cancel, progress, deadline: None };
    let got = client.download(&update.asset.url, &dest, opts).map_err(|e| match e {
        lcc_net::NetError::Cancelled => UpdateError::Cancelled,
        lcc_net::NetError::Verify(why) => UpdateError::Verify(why),
        lcc_net::NetError::TooLarge(bytes) => UpdateError::Verify(format!("the file is larger than the release says ({bytes} bytes)")),
        lcc_net::NetError::Io(why) => UpdateError::Io(why),
        other => UpdateError::Net(other.to_string()),
    })?;
    // a resume that needed no transfer still has to hash what was already there; the net crate did
    // that, so `got` is the whole file either way
    if let Some(size) = size
        && got.bytes != size
    {
        let _ = std::fs::remove_file(&dest);
        return Err(UpdateError::Verify(format!("the release says {size} bytes, the file has {}", got.bytes)));
    }
    Ok(Downloaded { path: dest, bytes: got.bytes, sha256: got.sha256, name })
}

/// Delete `dest` and its `.part`: a file that may not be installed is not kept.
fn remove_staged(dest: &Path) {
    for path in [dest.to_path_buf(), part_path(dest)] {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("update: {}: {e}", path.display());
        }
    }
}

/// A file already staged from an earlier download that still checks out, if there is one.
///
/// The size and the hash are re-read from disk: the folder is on the user's machine, and trusting
/// a file because of its name would be trusting whatever wrote it last.
fn already_verified(dest: &Path, name: &str, size: Option<u64>, sha256: Option<&str>) -> Option<Downloaded> {
    let meta = std::fs::metadata(dest).ok()?;
    if !meta.is_file() {
        return None;
    }
    let bytes = meta.len();
    if let Some(want) = size
        && bytes != want
    {
        return None;
    }
    let sha256 = sha256?;
    let got = hash_file(dest).ok()?;
    if !got.eq_ignore_ascii_case(sha256) {
        // whatever it is, it is not what we are about to install
        let _ = std::fs::remove_file(dest);
        return None;
    }
    Some(Downloaded { path: dest.to_path_buf(), bytes, sha256: got, name: name.to_string() })
}

/// Hash a staged file again right before it is installed, against the SHA-256 the download step
/// checked it with: the cache folder is on the user's machine, and a file swapped there after the
/// check must not reach the installer. A mismatch (or a file that can't be read) deletes the file.
pub fn recheck(staged: &Downloaded) -> Result<(), UpdateError> {
    let got = match hash_file(&staged.path) {
        Ok(got) => got,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpdateError::Signature(format!("{} is no longer in the download folder; download it again", staged.name)));
        }
        Err(e) => {
            remove_staged(&staged.path);
            return Err(UpdateError::Signature(format!("{} could not be read again before installing ({e}); it was deleted", staged.name)));
        }
    };
    if staged.sha256.is_empty() || !got.eq_ignore_ascii_case(&staged.sha256) {
        remove_staged(&staged.path);
        return Err(UpdateError::Signature(format!(
            "{} changed after it was downloaded and checked; it was deleted, download it again",
            staged.name
        )));
    }
    Ok(())
}

/// Lower-case hex SHA-256 of a file, in chunks (an update can be hundreds of megabytes).
pub fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{Asset, Kind, Os, Target};
    use crate::version::Version;
    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn update(url: &str, name: &str, size: u64, sha: Option<String>) -> Update {
        Update {
            current: Version::parse("1.0.0").expect("v"),
            version: Version::parse("2.0.0").expect("v"),
            title: "App 2.0.0".into(),
            tag: "v2.0.0".into(),
            asset: Asset { name: name.into(), url: url.into(), size, sha256: sha.clone(), kind: Kind::Msi },
            sha256: sha,
            published: None,
            notes: String::new(),
            action: Target { os: Os::Linux, arch: "x86_64" }.describe(),
        }
    }

    /// A throwaway signing key for these tests (never a release key) and its public half.
    fn test_key(seed: u8) -> (SigningKey, UpdateKey) {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let text = base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes());
        (signing, UpdateKey::parse(&text).expect("a key"))
    }

    fn sign(key: &SigningKey, list: &[u8]) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.encode(key.sign(list).to_bytes()).into_bytes()
    }

    #[test]
    fn names_are_sanitised() {
        assert_eq!(safe_file_name("app-1.2.3-windows-x64.msi").as_deref(), Ok("app-1.2.3-windows-x64.msi"));
        assert_eq!(safe_file_name("  app.msi  ").as_deref(), Ok("app.msi"));
        assert_eq!(safe_file_name("dist/app.msi").as_deref(), Ok("app.msi"));
        assert_eq!(safe_file_name("..\\..\\.ssh\\authorized_keys").as_deref(), Ok("authorized_keys"), "only the last segment survives");
        for bad in ["", "   ", ".", "..", "a/b/..", "a\0b"] {
            assert!(safe_file_name(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(safe_file_name(&"x".repeat(200)).is_err(), "an absurdly long name is refused");
    }

    /// What the local release serves: the file, its `SHA256SUMS.txt` and the list's signature
    /// (`None`: a 404, as on a release that was never signed).
    struct Served {
        file: Vec<u8>,
        sums: Option<Vec<u8>>,
        sig: Option<Vec<u8>>,
    }

    /// A release whose list names `name` with the file's real hash, signed by `key`.
    fn signed(file: Vec<u8>, name: &str, key: &SigningKey) -> Served {
        let sums = format!("{}  {name}\n", sha256_hex(&file)).into_bytes();
        let sig = sign(key, &sums);
        Served { file, sums: Some(sums), sig: Some(sig) }
    }

    /// A local release folder: `<base>/SHA256SUMS.txt`, `<base>/SHA256SUMS.txt.sig`, and the file
    /// at any other path (with `Range` resume). Returns the folder's URL and how many times the file
    /// itself was asked for.
    fn serve(served: Served) -> (String, Arc<AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}/v2.0.0", l.local_addr().expect("addr"));
        let file_hits = Arc::new(AtomicUsize::new(0));
        let hits = file_hits.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(mut s) = stream else { return };
                let mut r = BufReader::new(s.try_clone().expect("clone"));
                let mut request = String::new();
                let _ = r.read_line(&mut request);
                let mut range = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if line.to_ascii_lowercase().starts_with("range:") {
                        range = line[6..].trim().to_string();
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let small = if path.ends_with("/SHA256SUMS.txt") {
                    Some(served.sums.as_deref())
                } else if path.ends_with("/SHA256SUMS.txt.sig") {
                    Some(served.sig.as_deref())
                } else {
                    None
                };
                match small {
                    Some(Some(body)) => {
                        let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                        let _ = s.write_all(body);
                    }
                    Some(None) => {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    None => {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let body = &served.file;
                        let start: usize = range.strip_prefix("bytes=").and_then(|r| r.split('-').next()).and_then(|s| s.parse().ok()).unwrap_or(0);
                        let slice = body.get(start..).unwrap_or_default();
                        if start > 0 {
                            let _ = s.write_all(
                                format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n",
                                    slice.len(),
                                    body.len().saturating_sub(1),
                                    body.len()
                                )
                                .as_bytes(),
                            );
                        } else {
                            let _ =
                                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len()).as_bytes());
                        }
                        let _ = s.write_all(slice);
                    }
                }
            }
        });
        (base, file_hits)
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lcc-updater-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        d
    }

    fn client() -> Client {
        Client::new(lcc_net::ClientConfig { stall_timeout: std::time::Duration::from_secs(5), ..Default::default() }).expect("client")
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn a_staged_file_swapped_before_install_is_refused_and_deleted() {
        let dir = scratch("recheck");
        let path = dir.join("app.msi");
        std::fs::write(&path, b"the checked installer").unwrap();
        let staged = Downloaded { path: path.clone(), bytes: 21, sha256: sha256_hex(b"the checked installer"), name: "app.msi".into() };
        assert_eq!(recheck(&staged), Ok(()), "the file it was checked as passes");
        assert!(path.exists(), "and stays");
        std::fs::write(&path, b"something else entirely").unwrap();
        let err = recheck(&staged).expect_err("a swapped file is refused");
        assert!(matches!(err, UpdateError::Signature(_)) && err.to_string().contains("changed after"), "{err}");
        assert!(!path.exists(), "the swapped file is deleted");
        let err = recheck(&staged).expect_err("a missing file is refused");
        assert!(err.to_string().contains("no longer in the download folder"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloads_checks_and_leaves_no_part() {
        let dir = scratch("ok");
        let (signing, key) = test_key(1);
        let bytes: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let (url, _) = serve(signed(bytes.clone(), "app-2.0.0-windows-x64.msi", &signing));
        // the feed states no hash: the signed list is where it comes from
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", bytes.len() as u64, None);
        let mut seen = 0u64;
        let got = download(&u, Some(&key), &client(), &dir, None, Some(&mut |done, _| seen = done)).expect("download");
        assert_eq!((got.bytes, std::fs::read(&got.path).expect("read")), (bytes.len() as u64, bytes.clone()));
        assert_eq!(got.sha256, sha256_hex(&bytes));
        assert_eq!(seen, bytes.len() as u64, "progress reports the end");
        assert!(!dir.join("app-2.0.0-windows-x64.msi.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Someone who can change the release swaps the file and gives the feed its hash: the signed
    /// list still names the genuine file, so the swapped one is thrown away.
    #[test]
    fn a_tampered_file_is_thrown_away_whatever_the_feed_says() {
        let dir = scratch("bad");
        let (signing, key) = test_key(1);
        let genuine = vec![7u8; 5_000];
        let mut served = signed(genuine, "app-2.0.0-windows-x64.msi", &signing);
        let tampered = vec![8u8; 5_000];
        served.file = tampered.clone();
        let (url, _) = serve(served);
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", tampered.len() as u64, Some(sha256_hex(&tampered)));
        let err = download(&u, Some(&key), &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Verify(_)), "{err}");
        assert!(err.to_string().contains("did not check out"), "the message names the check: {err}");
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0, "neither the file nor its part is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each way the list can fail to vouch for the file stops the update before the transfer, with
    /// a message naming why, and deletes what an earlier download staged under that name.
    #[test]
    fn a_list_that_does_not_vouch_for_the_file_stops_the_update() {
        let (signing, key) = test_key(1);
        let (other, _) = test_key(2);
        let file = vec![9u8; 3_000];
        let genuine = signed(file.clone(), "app-2.0.0-windows-x64.msi", &signing);
        let sums = genuine.sums.clone().expect("sums");
        let mut changed = sums.clone();
        // the list says another hash (the attacker's file), the signature is the genuine one
        changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
        let cases: Vec<(&str, Served, &str)> = vec![
            ("a tampered list", Served { file: file.clone(), sums: Some(changed), sig: genuine.sig.clone() }, "not signed by this app's update key"),
            (
                "another key",
                Served { file: file.clone(), sums: Some(sums.clone()), sig: Some(sign(&other, &sums)) },
                "not signed by this app's update key",
            ),
            ("no signature", Served { file: file.clone(), sums: Some(sums.clone()), sig: None }, "SHA256SUMS.txt.sig is missing"),
            ("no list", Served { file: file.clone(), sums: None, sig: genuine.sig.clone() }, "checksum list (SHA256SUMS.txt) could not be read"),
            (
                "a list without the file",
                {
                    let other_list = format!("{}  app-other.msi\n", sha256_hex(&file)).into_bytes();
                    let sig = sign(&signing, &other_list);
                    Served { file: file.clone(), sums: Some(other_list), sig: Some(sig) }
                },
                "does not list app-2.0.0-windows-x64.msi",
            ),
            (
                "a signature that is not one",
                Served { file: file.clone(), sums: Some(sums.clone()), sig: Some(b"<html>".to_vec()) },
                "is not a signature",
            ),
        ];
        for (what, served, says) in cases {
            let dir = scratch("unvouched");
            // what an earlier download left: it must not stay staged
            std::fs::write(dir.join("app-2.0.0-windows-x64.msi"), &file).expect("staged");
            std::fs::write(dir.join("app-2.0.0-windows-x64.msi.part"), &file[..100]).expect("part");
            let (url, file_hits) = serve(served);
            let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", file.len() as u64, Some(sha256_hex(&file)));
            let err = download(&u, Some(&key), &client(), &dir, None, None).expect_err(what);
            assert!(matches!(err, UpdateError::Signature(_)), "{what}: {err}");
            assert!(err.to_string().contains(says), "{what}: the message names the cause: {err}");
            assert!(err.to_string().contains("not installed"), "{what}: {err}");
            assert_eq!(file_hits.load(Ordering::SeqCst), 0, "{what}: the file itself is not fetched");
            assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0, "{what}: nothing stays staged");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// AC-8: an older release's genuine signed list and file, served under a newer tag, are not
    /// installed as the newer version: the file must carry the offered version in its name, as a
    /// whole `-<version>-<os>-` token (a prefix of another version doesn't count).
    #[test]
    fn an_older_releases_signed_files_are_not_offered_as_a_newer_one() {
        let (signing, key) = test_key(1);
        let file = vec![6u8; 2_000];
        let cases = [
            ("1.1.0", "app-1.0.0-windows-x64.msi", "an older release under a new tag"),
            ("1.1.0", "app-1.1.0-rc.1-windows-x64.msi", "its release candidate"),
            ("1.1.0", "app-1.1.01-windows-x64.msi", "a version the offer is a prefix of"),
            ("1.1.0", "app-11.1.0-windows-x64.msi", "a version the offer is a suffix of"),
            ("1.2.0-rc.1", "app-1.2.0-windows-x64.msi", "the release offered as its candidate"),
            ("1.1.0", "app-windows-x64.msi", "a name with no version"),
        ];
        for (offered, name, what) in cases {
            let dir = scratch("replay");
            std::fs::write(dir.join(name), &file).expect("staged");
            let (url, file_hits) = serve(signed(file.clone(), name, &signing));
            let mut u = update(&format!("{url}/{name}"), name, file.len() as u64, None);
            u.version = Version::parse(offered).expect("v");
            u.tag = format!("v{offered}");
            let err = download(&u, Some(&key), &client(), &dir, None, None).expect_err(what);
            assert!(matches!(err, UpdateError::Signature(_)), "{what}: {err}");
            assert!(err.to_string().contains(&format!("is not version {offered}")), "{what}: the message names the cause: {err}");
            assert_eq!(file_hits.load(Ordering::SeqCst), 0, "{what}: the file itself is not fetched");
            assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0, "{what}: nothing stays staged");
            let _ = std::fs::remove_dir_all(&dir);
        }
        // the matching names still install, prereleases and every platform's file included
        for (offered, name) in [
            ("1.2.0-rc.1", "app-1.2.0-rc.1-windows-x64.msi"),
            ("1.1.0", "lightweight-app-1.1.0-macos-universal.dmg"),
            ("1.1.0", "app-1.1.0-linux-x86_64.AppImage"),
            ("1.1.0", "app-1.1.0-freebsd-x86_64.tar.gz"),
        ] {
            let dir = scratch("replay-ok");
            let (url, _) = serve(signed(file.clone(), name, &signing));
            let mut u = update(&format!("{url}/{name}"), name, file.len() as u64, None);
            u.version = Version::parse(offered).expect("v");
            u.tag = format!("v{offered}");
            let got = download(&u, Some(&key), &client(), &dir, None, None).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(got.name, name);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_list_that_cannot_be_reached_stops_the_update() {
        let dir = scratch("nolist");
        let (_, key) = test_key(1);
        let u = update("http://127.0.0.1:1/v2.0.0/app-2.0.0-windows-x64.msi", "app-2.0.0-windows-x64.msi", 10, Some("ab".repeat(32)));
        let err = download(&u, Some(&key), &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Signature(_)), "{err}");
        assert!(err.to_string().contains("SHA256SUMS.txt"), "{err}");
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No key, no download: the build cannot tell a genuine release from a swapped one.
    #[test]
    fn without_a_key_nothing_is_downloaded() {
        let dir = scratch("nokey");
        let (signing, _) = test_key(1);
        let bytes = vec![4u8; 1_000];
        let (url, file_hits) = serve(signed(bytes.clone(), "app-2.0.0-windows-x64.msi", &signing));
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let err = download(&u, None, &client(), &dir, None, None).expect_err("no key");
        assert_eq!(err, UpdateError::NoKey);
        assert!(err.to_string().starts_with("this build can't verify updates"), "{err}");
        assert_eq!(file_hits.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_file_is_not_renamed_into_place() {
        let dir = scratch("short");
        let (signing, key) = test_key(1);
        let bytes = vec![1u8; 5_000];
        let (url, _) = serve(signed(bytes, "app-2.0.0-windows-x64.msi", &signing));
        // the release says 9 000 bytes; the server sends 5 000
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", 9_000, None);
        let err = download(&u, Some(&key), &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Verify(_)), "{err}");
        assert!(!dir.join("app-2.0.0-windows-x64.msi").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_call_reuses_the_verified_file() {
        let dir = scratch("again");
        let (signing, key) = test_key(1);
        let bytes = vec![3u8; 4_000];
        let (url, file_hits) = serve(signed(bytes.clone(), "app-2.0.0-windows-x64.msi", &signing));
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", bytes.len() as u64, None);
        let first = download(&u, Some(&key), &client(), &dir, None, None).expect("first");
        // the list is read again, the file is not: the verified copy is used as it is
        let again = download(&u, Some(&key), &client(), &dir, None, None).expect("second");
        assert_eq!(first, again);
        assert_eq!(file_hits.load(Ordering::SeqCst), 1, "one transfer");
        assert_eq!(std::fs::read(&again.path).expect("read"), bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_file_that_no_longer_matches_is_fetched_again() {
        let dir = scratch("stale");
        let (signing, key) = test_key(1);
        let bytes: Vec<u8> = (0..9_000u32).map(|i| (i % 97) as u8).collect();
        // what an earlier, different build left under the same name
        std::fs::write(dir.join("app-2.0.0-windows-x64.msi"), vec![0u8; bytes.len()]).expect("stale");
        let (url, _) = serve(signed(bytes.clone(), "app-2.0.0-windows-x64.msi", &signing));
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", bytes.len() as u64, None);
        let got = download(&u, Some(&key), &client(), &dir, None, None).expect("re-download");
        assert_eq!(std::fs::read(&got.path).expect("read"), bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_transfer_resumes() {
        let dir = scratch("resume");
        let (signing, key) = test_key(1);
        let bytes: Vec<u8> = (0..20_000u32).map(|i| (i % 241) as u8).collect();
        let half = bytes.len() / 2;
        // what an interrupted transfer left behind
        std::fs::write(dir.join("app-2.0.0-windows-x64.msi.part"), &bytes[..half]).expect("part");
        let (url, _) = serve(signed(bytes.clone(), "app-2.0.0-windows-x64.msi", &signing));
        let u = update(&format!("{url}/app-2.0.0-windows-x64.msi"), "app-2.0.0-windows-x64.msi", bytes.len() as u64, None);
        let got = download(&u, Some(&key), &client(), &dir, None, None).expect("resume");
        assert_eq!(got.bytes, bytes.len() as u64);
        assert_eq!(std::fs::read(&got.path).expect("read"), bytes);
        assert!(!dir.join("app-2.0.0-windows-x64.msi.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_name_stays_in_the_staging_folder() {
        let dir = scratch("hostile");
        let (signing, key) = test_key(1);
        let bytes = vec![5u8; 100];
        let name = "escaped-2.0.0-windows-x64.msi";
        let (url, _) = serve(signed(bytes.clone(), name, &signing));
        let u = update(&format!("{url}/evil"), &format!("../../{name}"), bytes.len() as u64, None);
        let got = download(&u, Some(&key), &client(), &dir, None, None).expect("download");
        assert_eq!(got.path.parent(), Some(dir.as_path()));
        assert_eq!(got.name, name);
        assert!(!dir.parent().expect("parent").join(name).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
