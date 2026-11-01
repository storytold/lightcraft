//! `cargo xtask ai-bench fetch`: download a benchmark set and the models it needs into the benchmark folder, check each
//! file against its pinned SHA-256 and unpack the archives. Needs `curl` and `tar` (or `unzip` for a zip), as
//! `cargo xtask corpus` needs `curl`.

use std::path::Path;
use std::process::Command;

use lightcraft_faces::known;

/// A file to download: name, URL, SHA-256 and whether it is an archive to unpack.
struct File {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    archive: bool,
}

/// A benchmark set: its folder name, the terms the user accepts by passing `--accept-terms`, its files and the models
/// (by id in `lightcraft_faces::known`) its benchmark runs.
pub struct Set {
    pub id: &'static str,
    terms: &'static str,
    files: &'static [File],
    pub models: &'static [&'static str],
}

impl Set {
    /// Each file's name and whether it is an archive (unpacked, with a completion marker).
    #[cfg(test)]
    pub fn archives(&self) -> Vec<(&'static str, bool)> {
        self.files.iter().map(|f| (f.name, f.archive)).collect()
    }
}

pub const WIDER: Set = Set {
    id: "wider-face-val",
    terms: "WIDER FACE (http://shuoyang1213.me/WIDERFACE/) is licensed CC BY-NC-ND 4.0 (as its publisher's Hugging Face card states): \
non-commercial research use only, no derivatives. The photos are downloaded to your machine and used only to measure detection accuracy; they \
are never committed or redistributed.",
    files: &[
        File {
            name: "WIDER_val.zip",
            url: "https://huggingface.co/datasets/CUHK-CSE/wider_face/resolve/main/data/WIDER_val.zip",
            sha256: "f9efbd09f28c5d2d884be8c0eaef3967158c866a593fc36ab0413e4b2a58a17a",
            archive: true,
        },
        File {
            name: "eval_tools.zip",
            url: "http://shuoyang1213.me/WIDERFACE/support/eval_script/eval_tools.zip",
            sha256: "1cf49c8243fa8a1632efe7f6aa09fd7a3994ac838c9008bdbc9dd1d547bb234a",
            archive: true,
        },
    ],
    models: &[known::YUNET_ID],
};

/// The publisher's own host (vis-www.cs.umass.edu) no longer resolves, so the Internet Archive's copies of its
/// `lfw.tgz` and `pairs.txt` are used; the SHA-256 sums pin them, and `lfw.tgz` has the md5 the publisher printed.
pub const LFW: Set = Set {
    id: "lfw",
    terms: "LFW (Labeled Faces in the Wild, http://vis-www.cs.umass.edu/lfw/, University of Massachusetts Amherst) is a research benchmark \
of news photos of public figures. Its publisher gives no licence but warns that it is for research on face verification and \
must not be used to conclude that an algorithm is suitable for any commercial purpose. The photos are downloaded to your machine \
and used only to measure recognition accuracy; they are never committed or redistributed.",
    files: &[
        File {
            name: "lfw.tgz",
            url: "https://web.archive.org/web/20200310174752id_/http://vis-www.cs.umass.edu/lfw/lfw.tgz",
            sha256: "055f7d9c632d7370e6fb4afc7468d40f970c34a80d4c6f50ffec63f5a8d536c0",
            archive: true,
        },
        File {
            name: "pairs.txt",
            url: "https://web.archive.org/web/20191202153930id_/http://vis-www.cs.umass.edu/lfw/pairs.txt",
            sha256: "ea42330c62c92989f9d7c03237ed5d591365e89b3e649747777b70e692dc1592",
            archive: false,
        },
    ],
    models: &[known::YUNET_ID, "sface-2021dec", "auraface-v1"],
};

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

/// Unpack `archive` (a zip or a gzipped tar) into `into` with the system's bsdtar (Windows, macOS), else `unzip`.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| format!("create {}: {e}", into.display()))?;
    let mut tar = Command::new("tar");
    tar.arg("-xf").arg(archive).arg("-C").arg(into);
    if crate::run(tar, &format!("tar -xf {}", archive.display())).is_ok() {
        return Ok(());
    }
    let mut unzip = Command::new("unzip");
    unzip.args(["-q", "-o"]).arg(archive).arg("-d").arg(into);
    crate::run(unzip, &format!("unzip {}", archive.display()))
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

/// Fetch everything the benchmarks of `sets` need under `dir`: each set's folder and `models/`.
pub fn run(dir: &Path, accept_terms: bool, sets: &[&Set]) -> Result<(), String> {
    if !accept_terms {
        let terms: Vec<&str> = sets.iter().map(|s| s.terms).collect();
        return Err(format!("{}\n\nPass --accept-terms to confirm you accept these terms.", terms.join("\n\n")));
    }
    for set in sets {
        let folder = dir.join(set.id);
        for file in set.files {
            download(file.url, &folder.join(file.name), file.sha256)?;
            if file.archive {
                unpack_once(&folder, file.name, file.sha256, || unpack(&folder.join(file.name), &folder))?;
            }
        }
        for id in set.models {
            let model = known::download(id).ok_or_else(|| format!("LightCraft offers no download of {id}"))?;
            download(&model.url, &dir.join("models").join(&model.file_name), &model.sha256)?;
        }
    }
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
