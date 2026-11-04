//! `cargo xtask ai-bench fetch`: download the WIDER FACE validation set and the YuNet model into the benchmark folder,
//! check each file against its pinned SHA-256 and unpack the archives. Needs `curl` and `tar` (or `unzip`), as `cargo xtask corpus` needs `curl`.

use std::path::Path;
use std::process::Command;

use lightcraft_faces::known;

/// The terms the user accepts by passing `--accept-terms`.
const TERMS: &str = "WIDER FACE (http://shuoyang1213.me/WIDERFACE/) is licensed CC BY-NC-ND 4.0 (as its publisher's Hugging Face card states): \
non-commercial research use only, no derivatives. The photos are downloaded to your machine and used only to measure detection accuracy; they \
are never committed or redistributed. Pass --accept-terms to confirm you accept these terms.";

/// A file to download: name, URL and SHA-256.
struct Archive {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const ARCHIVES: [Archive; 2] = [
    Archive {
        name: "WIDER_val.zip",
        url: "https://huggingface.co/datasets/CUHK-CSE/wider_face/resolve/main/data/WIDER_val.zip",
        sha256: "f9efbd09f28c5d2d884be8c0eaef3967158c866a593fc36ab0413e4b2a58a17a",
    },
    Archive {
        name: "eval_tools.zip",
        url: "http://shuoyang1213.me/WIDERFACE/support/eval_script/eval_tools.zip",
        sha256: "1cf49c8243fa8a1632efe7f6aa09fd7a3994ac838c9008bdbc9dd1d547bb234a",
    },
];

/// `curl` `url` to `dest`, unless `dest` already has `sha256`; a download with another hash is deleted and an error.
fn download(url: &str, dest: &Path, sha256: &str) -> Result<(), String> {
    if crate::corpus::file_sha256(dest).is_ok_and(|h| h == sha256) {
        return Ok(());
    }
    let mut curl = Command::new("curl");
    curl.args(["-fsSL", "--create-dirs", "-o"]).arg(dest).arg(url);
    crate::run(curl, &format!("curl {url}"))?;
    let actual = crate::corpus::file_sha256(dest)?;
    if actual != sha256 {
        std::fs::remove_file(dest).map_err(|e| format!("remove {}: {e}", dest.display()))?;
        return Err(format!("{}: sha256 is {actual}, expected {sha256} (file deleted; source {url})", dest.display()));
    }
    Ok(())
}

/// Unpack `zip` into `into` with the system's bsdtar (Windows, macOS), else `unzip`.
fn unpack(zip: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| format!("create {}: {e}", into.display()))?;
    let mut tar = Command::new("tar");
    tar.arg("-xf").arg(zip).arg("-C").arg(into);
    if crate::run(tar, &format!("tar -xf {}", zip.display())).is_ok() {
        return Ok(());
    }
    let mut unzip = Command::new("unzip");
    unzip.args(["-q", "-o"]).arg(zip).arg("-d").arg(into);
    crate::run(unzip, &format!("unzip {}", zip.display()))
}

/// Unpack `archive` (already checked against `sha256`) with `unpack` unless a completion marker for exactly that archive
/// exists. The marker is written, atomically, only after `unpack` succeeds: the unpacked folders can exist half-filled after an
/// interrupted extraction, so they never count as done and the next fetch extracts again over them.
fn unpack_once(set: &Path, archive: &str, sha256: &str, unpack: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let marker = set.join(format!(".{archive}.unpacked"));
    if std::fs::read_to_string(&marker).is_ok_and(|done| done.trim() == sha256) {
        return Ok(());
    }
    unpack()?;
    let pending = set.join(format!(".{archive}.unpacked.tmp"));
    std::fs::write(&pending, sha256).map_err(|e| format!("write {}: {e}", pending.display()))?;
    std::fs::rename(&pending, &marker).map_err(|e| format!("rename {}: {e}", pending.display()))
}

/// Fetch everything `run` needs under `dir`: `wider-face-val/` and `models/`.
pub fn run(dir: &Path, accept_terms: bool) -> Result<(), String> {
    if !accept_terms {
        return Err(TERMS.to_string());
    }
    let set = dir.join(super::SET);
    for archive in ARCHIVES {
        download(archive.url, &set.join(archive.name), archive.sha256)?;
        unpack_once(&set, archive.name, archive.sha256, || unpack(&set.join(archive.name), &set))?;
    }
    let model = known::download(known::YUNET_ID).ok_or("LightCraft offers no download of YuNet")?;
    download(&model.url, &dir.join("models").join(&model.file_name), &model.sha256)?;
    println!("fetched into {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupted_extraction_is_redone_and_a_finished_one_is_not() {
        let set = std::env::temp_dir().join(format!("ai-bench-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&set);
        assert!(std::fs::create_dir_all(&set).is_ok());
        // the extraction dies after creating the folder that used to count as "done"
        let interrupted = unpack_once(&set, "a.zip", "abc", || {
            std::fs::create_dir_all(set.join("WIDER_val").join("images")).map_err(|e| e.to_string())?;
            Err("interrupted".to_string())
        });
        assert!(interrupted.is_err());
        assert!(set.join("WIDER_val").join("images").is_dir());
        let mut runs = 0;
        assert!(
            unpack_once(&set, "a.zip", "abc", || {
                runs += 1;
                Ok(())
            })
            .is_ok()
        );
        assert_eq!(runs, 1, "the next fetch must extract again");
        assert!(
            unpack_once(&set, "a.zip", "abc", || {
                runs += 1;
                Ok(())
            })
            .is_ok()
        );
        assert_eq!(runs, 1, "a finished extraction is not repeated");
        // a different archive (hash) under the same name is extracted again
        assert!(
            unpack_once(&set, "a.zip", "def", || {
                runs += 1;
                Ok(())
            })
            .is_ok()
        );
        assert_eq!(runs, 2);
        let _ = std::fs::remove_dir_all(&set);
    }
}
