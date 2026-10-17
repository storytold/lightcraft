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

/// Download `update`'s file into `dir`, checking it against the feed's size and its SHA-256.
///
/// A file already there that still matches its SHA-256 is returned as it is (a second click starts
/// no second transfer); a `.part` from an interrupted one is resumed.
pub fn download(
    update: &Update,
    client: &Client,
    dir: &Path,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Progress<'_>>,
) -> Result<Downloaded, UpdateError> {
    let name = safe_file_name(&update.asset.name)?;
    std::fs::create_dir_all(dir).map_err(|e| UpdateError::Io(format!("could not create {}: {e}", dir.display())))?;
    let dest = dir.join(&name);
    // where it must not escape to
    if dest.parent() != Some(dir) {
        return Err(UpdateError::Io(format!("refusing to write outside {}", dir.display())));
    }
    let size = (update.asset.size > 0).then_some(update.asset.size);
    if let Some(done) = already_verified(&dest, &name, size, update.sha256.as_deref()) {
        return Ok(done);
    }
    let cap = match size {
        Some(bytes) => bytes.max(1),
        None => MAX_DOWNLOAD_BYTES,
    };
    let hash_we_want = update.sha256.clone();
    let opts = DownloadOptions { size, sha256: hash_we_want.clone(), max_size: cap, headers: Vec::new(), cancel, progress };
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

/// Lower-case hex SHA-256 of a file, in chunks (an update can be hundreds of megabytes).
fn hash_file(path: &Path) -> Result<String, std::io::Error> {
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

    #[test]
    fn a_part_file_sits_beside_its_target() {
        assert_eq!(part_path(Path::new("/tmp/a.msi")), PathBuf::from("/tmp/a.msi.part"));
        assert_eq!(part_path(Path::new("C:/tmp/a.msi")).file_name().and_then(|n| n.to_str()), Some("a.msi.part"));
    }

    #[test]
    fn stage_dir_is_inside_the_cache() {
        assert_eq!(stage_dir(Path::new("/cache")), PathBuf::from("/cache/updates"));
    }

    /// A local server that answers `n` requests with `body` (and a broken one that hangs up).
    fn serve(n: usize, body: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}/f", l.local_addr().expect("addr"));
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut r = BufReader::new(s.try_clone().expect("clone"));
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
                    let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len()).as_bytes());
                }
                let _ = s.write_all(slice);
            }
        });
        base
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
    fn downloads_checks_and_leaves_no_part() {
        let dir = scratch("ok");
        let bytes: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/app.msi"), "app.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let mut seen = 0u64;
        let got = download(&u, &client(), &dir, None, Some(&mut |done, _| seen = done)).expect("download");
        assert_eq!((got.bytes, std::fs::read(&got.path).expect("read")), (bytes.len() as u64, bytes.clone()));
        assert_eq!(got.sha256, sha256_hex(&bytes));
        assert_eq!(seen, bytes.len() as u64, "progress reports the end");
        assert!(!dir.join("app.msi.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_hash_is_thrown_away() {
        let dir = scratch("bad");
        let bytes = vec![7u8; 5_000];
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/app.msi"), "app.msi", bytes.len() as u64, Some("00".repeat(32)));
        let err = download(&u, &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Verify(_)), "{err}");
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0, "neither the file nor its part is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_file_is_not_renamed_into_place() {
        let dir = scratch("short");
        let bytes = vec![1u8; 5_000];
        let url = serve(1, bytes.clone());
        // the release says 9 000 bytes; the server sends 5 000
        let u = update(&format!("{url}/app.msi"), "app.msi", 9_000, None);
        let err = download(&u, &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Verify(_)), "{err}");
        assert!(!dir.join("app.msi").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_server_is_an_error_not_a_panic() {
        let dir = scratch("down");
        let u = update("http://127.0.0.1:1/app.msi", "app.msi", 10, None);
        let err = download(&u, &client(), &dir, None, None).expect_err("must fail");
        assert!(matches!(err, UpdateError::Net(_)), "{err}");
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_call_reuses_the_verified_file() {
        let dir = scratch("again");
        let bytes = vec![3u8; 4_000];
        // one request: the second call finds the verified file already there
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/app.msi"), "app.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let first = download(&u, &client(), &dir, None, None).expect("first");
        // nothing listens now: the verified copy is used as it is
        let again = download(&u, &client(), &dir, None, None).expect("second");
        assert_eq!(first, again);
        assert_eq!(std::fs::read(&again.path).expect("read"), bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_file_that_no_longer_matches_is_fetched_again() {
        let dir = scratch("stale");
        let bytes: Vec<u8> = (0..9_000u32).map(|i| (i % 97) as u8).collect();
        // what an earlier, different build left under the same name
        std::fs::write(dir.join("app.msi"), vec![0u8; bytes.len()]).expect("stale");
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/app.msi"), "app.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let got = download(&u, &client(), &dir, None, None).expect("re-download");
        assert_eq!(std::fs::read(&got.path).expect("read"), bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_transfer_resumes() {
        let dir = scratch("resume");
        let bytes: Vec<u8> = (0..20_000u32).map(|i| (i % 241) as u8).collect();
        let half = bytes.len() / 2;
        // what an interrupted transfer left behind
        std::fs::write(dir.join("app.msi.part"), &bytes[..half]).expect("part");
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/app.msi"), "app.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let got = download(&u, &client(), &dir, None, None).expect("resume");
        assert_eq!(got.bytes, bytes.len() as u64);
        assert_eq!(std::fs::read(&got.path).expect("read"), bytes);
        assert!(!dir.join("app.msi.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_name_stays_in_the_staging_folder() {
        let dir = scratch("hostile");
        let bytes = vec![5u8; 100];
        let url = serve(1, bytes.clone());
        let u = update(&format!("{url}/evil"), "../../escaped.msi", bytes.len() as u64, Some(sha256_hex(&bytes)));
        let got = download(&u, &client(), &dir, None, None).expect("download");
        assert_eq!(got.path.parent(), Some(dir.as_path()));
        assert_eq!(got.name, "escaped.msi");
        assert!(!dir.parent().expect("parent").join("escaped.msi").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
