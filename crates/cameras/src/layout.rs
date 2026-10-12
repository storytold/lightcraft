//! Which of a camera's files to list, for backends that hand over every file with its folders at
//! once (macOS: ImageCaptureCore's `mediaFiles`, each with its chain of parent folders). Kept apart
//! from the framework calls so it is tested on every platform.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::MAX_DEPTH;

/// The files to import, each with its folder path (`/`-joined): those under a `DCIM` folder (the
/// DCF layout cameras and phones use), with the path from that folder down (whatever is above it,
/// such as a storage, is left out), or every file when none is under one. `folders` lists a file's
/// folders from the top down. Files nested more than [`MAX_DEPTH`] folders below where the path
/// starts, and files without a name, are skipped.
pub fn pick<N>(files: Vec<(N, String, Vec<String>)>) -> Vec<(N, String, String)> {
    let dcim_at = |folders: &[String]| folders.iter().position(|f| f.eq_ignore_ascii_case("DCIM"));
    let any_dcim = files.iter().any(|(_, _, folders)| dcim_at(folders).is_some());
    files
        .into_iter()
        .filter_map(|(file, name, folders)| {
            if name.is_empty() {
                return None;
            }
            let from = match dcim_at(&folders) {
                Some(i) => i,
                None if any_dcim => return None,
                None => 0,
            };
            let path = folders.get(from..).unwrap_or_default();
            (path.len() <= MAX_DEPTH).then(|| (file, name, path.join("/")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, folders: &[&str]) -> ((), String, Vec<String>) {
        ((), name.to_string(), folders.iter().map(|s| s.to_string()).collect())
    }

    fn paths(files: Vec<((), String, Vec<String>)>) -> Vec<String> {
        pick(files).into_iter().map(|(_, n, d)| format!("{d}|{n}")).collect()
    }

    #[test]
    fn files_under_dcim_keep_the_path_from_dcim_down() {
        // what an M11-P reports, plus a storage level some cameras put above DCIM
        let files = vec![
            f("L1007951.DNG", &["DCIM", "100LEICA"]),
            f("L1000002.JPG", &["SD Card", "DCIM", "101LEICA"]),
            f("x.dat", &["MISC"]),
            f("top.jpg", &[]),
        ];
        assert_eq!(paths(files), ["DCIM/100LEICA|L1007951.DNG", "DCIM/101LEICA|L1000002.JPG"]);
    }

    #[test]
    fn dcim_in_any_case() {
        assert_eq!(paths(vec![f("IMG_1.CR3", &["dcim", "100CANON"])]), ["dcim/100CANON|IMG_1.CR3"]);
    }

    #[test]
    fn without_dcim_every_file_is_listed() {
        let files = vec![f("a.jpg", &["Pictures"]), f("b.heic", &[]), f("c.jpg", &["Phone", "Camera"])];
        assert_eq!(paths(files), ["Pictures|a.jpg", "|b.heic", "Phone/Camera|c.jpg"]);
    }

    #[test]
    fn deep_folders_and_nameless_files_are_skipped() {
        let deep: Vec<String> = std::iter::once("DCIM".to_string()).chain((0..MAX_DEPTH).map(|i| format!("d{i}"))).collect();
        let deep: Vec<&str> = deep.iter().map(String::as_str).collect();
        let files = vec![f("deep.jpg", &deep), f("", &["DCIM", "100"]), f("ok.jpg", &["DCIM", "100"])];
        assert_eq!(paths(files), ["DCIM/100|ok.jpg"]);
        // exactly MAX_DEPTH folders is still fine
        let edge: Vec<&str> = deep.get(..MAX_DEPTH).unwrap().to_vec();
        assert_eq!(paths(vec![f("edge.jpg", &edge)]).len(), 1);
    }

    #[test]
    fn an_empty_camera_lists_nothing() {
        assert!(paths(Vec::new()).is_empty());
    }
}
