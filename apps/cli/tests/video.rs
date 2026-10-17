//! M14 end to end: `render` a synthetic H.264 clip to a graded movie (.mp4 / .mov, trimmed) and
//! re-decode the result with lcc-video.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use lcc_video::test_util::{mean, write_clip};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("app-cli-video-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn render(args: &[&str]) -> (bool, String) {
    let o = Command::new(BIN).arg("render").args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn mean_at(path: &Path, t: f64) -> f64 {
    mean(&lcc_video::FrameReader::open(path).unwrap().frame_at(t, None).unwrap().image)
}

#[test]
fn render_a_clip_to_graded_mp4_and_mov() {
    let d = dir("formats");
    let clip = d.join("clip.mp4");
    write_clip(&clip, 64, 48, 30, |_| 90, &[]);
    let base = mean_at(&clip, 0.5);
    for ext in ["mp4", "mov"] {
        let out = d.join(format!("out.{ext}"));
        let (ok, err) = render(&[clip.to_str().unwrap(), "-o", out.to_str().unwrap(), "--set", "light.exposure=1"]);
        assert!(ok, "{ext}: {err}");
        let info = lcc_video::probe(&out).unwrap();
        assert!((info.duration - 1.0).abs() < 0.1, "{ext}: {info:?}");
        assert_eq!((info.width, info.height), (64, 48), "{ext}");
        let n = lcc_video::FrameReader::open(&out).unwrap().frame_times().len();
        assert!((29..=31).contains(&n), "{ext}: {n} frames");
        let graded = mean_at(&out, 0.5);
        assert!(graded > base * 1.5, "{ext}: exposure +1 brightens {base} → {graded}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn render_trims_a_clip() {
    let d = dir("trim");
    let clip = d.join("clip.mp4");
    // 2 s: dark first second, bright second second
    write_clip(&clip, 64, 48, 60, |k| if k < 30 { 40 } else { 200 }, &[]);
    let out = d.join("trim.mp4");
    let (ok, err) = render(&[clip.to_str().unwrap(), "-o", out.to_str().unwrap(), "--trim", "1,1.5"]);
    assert!(ok, "{err}");
    let info = lcc_video::probe(&out).unwrap();
    assert!((info.duration - 0.5).abs() < 0.1, "{info:?}");
    assert!(mean_at(&out, 0.0) > mean_at(&clip, 0.5) * 2.0, "the trimmed export starts in the bright second");
    // open-ended trim and a bad one
    let out2 = d.join("tail.mp4");
    let (ok, err) = render(&[clip.to_str().unwrap(), "-o", out2.to_str().unwrap(), "--trim", "1.5,"]);
    assert!(ok, "{err}");
    assert!((lcc_video::probe(&out2).unwrap().duration - 0.5).abs() < 0.1);
    let (ok, err) = render(&[clip.to_str().unwrap(), "-o", d.join("bad.mp4").to_str().unwrap(), "--trim", "soon,1"]);
    assert!(!ok && err.contains("--trim"), "{err}");
    let (ok, err) = render(&[clip.to_str().unwrap(), "-o", d.join("bad.mp4").to_str().unwrap(), "--trim", "1.5,1"]);
    assert!(!ok && err.contains("--trim"), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}
