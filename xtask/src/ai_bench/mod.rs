//! `cargo xtask ai-bench`: measure how well LightCraft's own models do on public benchmarks, so the accuracy figures in the
//! docs can be reproduced. For now one benchmark: face detection (YuNet) on the WIDER FACE validation set.
//!
//! ```text
//! cargo xtask ai-bench fetch --accept-terms [--dir DIR]
//! cargo xtask ai-bench run face-detection --set wider-face-val [--dir DIR] [--model FILE] [--limit N] [--threads N]
//! ```

mod fetch;
mod mat;
mod metrics;
mod wider;

use std::path::{Path, PathBuf};

use lightcraft_faces::{known, yunet};

/// The only benchmark set so far.
const SET: &str = "wider-face-val";

pub const USAGE: &str = "\
  ai-bench fetch --accept-terms [--dir DIR]
                  download the WIDER FACE validation photos (CC BY-NC-ND 4.0, research use only) and its evaluation
                  toolkit's ground truth, and the YuNet model, into DIR (default corpus/ai-bench), checking pinned
                  SHA-256 sums; nothing is committed
  ai-bench run face-detection --set wider-face-val [--dir DIR] [--model FILE] [--limit N] [--threads N]
                  run YuNet on every validation photo and print the WIDER FACE average precision for the easy,
                  medium and hard settings and the wall time (--limit N: N photos spread evenly, for a quick look;
                  --model: another YuNet-architecture .onnx, default DIR/models/face_detection_yunet_2023mar.onnx)
";

struct Args {
    dir: PathBuf,
    model: Option<PathBuf>,
    limit: Option<usize>,
    threads: usize,
    accept_terms: bool,
    set: Option<String>,
}

fn parse(root: &Path, args: &[&str]) -> Result<Args, String> {
    let mut a = Args {
        dir: root.join("corpus").join("ai-bench"),
        model: None,
        limit: None,
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        accept_terms: false,
        set: None,
    };
    let mut it = args.iter();
    while let Some(&flag) = it.next() {
        if flag == "--accept-terms" {
            a.accept_terms = true;
            continue;
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let number = |what: &str| value.parse::<usize>().map_err(|_| format!("{what} must be a whole number, not {value}"));
        match flag {
            "--dir" => a.dir = PathBuf::from(value),
            "--model" => a.model = Some(PathBuf::from(value)),
            "--set" => a.set = Some((*value).to_string()),
            "--limit" => a.limit = Some(number("--limit")?),
            "--threads" => a.threads = number("--threads")?,
            _ => return Err(format!("unknown option `{flag}`\n\n{USAGE}")),
        }
    }
    Ok(a)
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    match args {
        ["fetch", rest @ ..] => {
            let a = parse(root, rest)?;
            fetch::run(&a.dir, a.accept_terms)
        }
        ["run", "face-detection", rest @ ..] => {
            let a = parse(root, rest)?;
            match a.set.as_deref() {
                Some(SET) => face_detection(&a),
                other => Err(format!("face-detection needs --set {SET} (the only set so far), not {}", other.unwrap_or("nothing"))),
            }
        }
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

fn face_detection(a: &Args) -> Result<(), String> {
    let file = known::download(known::YUNET_ID).map(|d| d.file_name).ok_or("no YuNet file name")?;
    let model = a.model.clone().unwrap_or_else(|| a.dir.join("models").join(file));
    let bytes = std::fs::read(&model).map_err(|e| format!("{}: {e} (run `cargo xtask ai-bench fetch` or pass --model)", model.display()))?;
    let detector = yunet::Detector::new(&bytes).map_err(|e| format!("{}: {e}", model.display()))?;
    let pinned = crate::corpus::file_sha256(&model).is_ok_and(|h| h == known::YUNET_SHA256);
    let side = detector.input_side();
    println!(
        "YuNet {} ({}), input {side} px, {} thread(s)",
        model.display(),
        if pinned { "the pinned 2023mar model" } else { "NOT the pinned 2023mar model" },
        a.threads
    );
    let r = wider::run(&a.dir.join(SET), &detector, a.limit, a.threads)?;
    println!(
        "WIDER FACE validation, {} photos{}",
        r.photos,
        if r.failed > 0 { format!(" ({} failed, scored as no detections)", r.failed) } else { String::new() }
    );
    for (setting, ap) in wider::SETTINGS.iter().zip(r.ap) {
        println!("  AP {setting:<6} {ap:.1}");
    }
    println!("  wall time {:.1} s ({:.0} ms per photo)", r.seconds, r.seconds * 1000.0 / r.photos.max(1) as f64);
    Ok(())
}
