//! `cargo xtask ai-bench`: measure how well LightCraft's own models do on public benchmarks, so the accuracy figures in the
//! docs can be reproduced. Two benchmarks so far: face detection (YuNet) on the WIDER FACE validation set and face
//! recognition (SFace, AuraFace) on LFW.
//!
//! ```text
//! cargo xtask ai-bench fetch --accept-terms [--set SET] [--dir DIR]
//! cargo xtask ai-bench run face-detection --set wider-face-val [--dir DIR] [--model FILE] [--limit N] [--threads N]
//! cargo xtask ai-bench run face-recognition --set lfw [--dir DIR] [--model FILE] [--limit N] [--threads N]
//! ```

mod fetch;
mod lfw;
mod mat;
mod metrics;
mod wider;

use std::path::{Path, PathBuf};

use lightcraft_faces::runtime::Embedder;
use lightcraft_faces::{known, yunet};

pub const USAGE: &str = "\
  ai-bench fetch --accept-terms [--set wider-face-val|lfw] [--dir DIR]
                  download the benchmark photos and ground truth (WIDER FACE validation: CC BY-NC-ND 4.0; LFW: research
                  use only; both are kept on your machine, never committed) and the models they need into DIR (default
                  corpus/ai-bench), checking pinned SHA-256 sums; without --set both sets
  ai-bench run face-detection --set wider-face-val [--dir DIR] [--model FILE] [--limit N] [--threads N]
                  run YuNet on every validation photo and print the WIDER FACE average precision for the easy,
                  medium and hard settings and the wall time (--limit N: up to N photos spread evenly, for a quick look;
                  --model: another YuNet-architecture .onnx, default DIR/models/face_detection_yunet_2023mar.onnx)
  ai-bench run face-recognition --set lfw [--dir DIR] [--model FILE] [--limit N] [--threads N]
                  detect, align and embed the photos of LFW's 6,000 pairs the way Photo > Detect/Recognize Faces does and
                  print the 10-fold verification accuracy and the wall time for SFace and AuraFace (--limit N: up to N pairs
                  spread evenly; --model: one recognition .onnx LightCraft knows by its SHA-256 instead of those two)
";

/// What a recognition model's makers publish for LFW (percent) and the page that says so.
const PUBLISHED_LFW: [(&str, f64, &str); 2] = [
    (
        "sface-2021dec",
        99.40,
        "https://github.com/opencv/opencv_zoo/blob/25f423d0e04c31a17254620e58febd7386da523b/models/face_recognition_sface/README.md",
    ),
    ("auraface-v1", 99.65, "https://huggingface.co/fal/AuraFace-v1/blob/af6d057c9b0ec4071d4c49c80e3539258798b609/README.md"),
];

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
            "--limit" => match number("--limit")? {
                0 => return Err("--limit must be at least 1 (omit it to use every photo or pair)".to_string()),
                k => a.limit = Some(k),
            },
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
            let sets = match a.set.as_deref() {
                None => vec![&fetch::WIDER, &fetch::LFW],
                Some(id) if id == fetch::WIDER.id => vec![&fetch::WIDER],
                Some(id) if id == fetch::LFW.id => vec![&fetch::LFW],
                Some(other) => return Err(format!("no benchmark set `{other}` (wider-face-val or lfw)")),
            };
            fetch::run(&a.dir, a.accept_terms, &sets)
        }
        ["run", "face-detection", rest @ ..] => {
            let a = parse(root, rest)?;
            match a.set.as_deref() {
                Some(id) if id == fetch::WIDER.id => face_detection(&a),
                other => Err(format!("face-detection needs --set {} (the only set so far), not {}", fetch::WIDER.id, other.unwrap_or("nothing"))),
            }
        }
        ["run", "face-recognition", rest @ ..] => {
            let a = parse(root, rest)?;
            match a.set.as_deref() {
                Some(id) if id == fetch::LFW.id => face_recognition(&a),
                other => Err(format!("face-recognition needs --set {} (the only set so far), not {}", fetch::LFW.id, other.unwrap_or("nothing"))),
            }
        }
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

/// The YuNet detector from `DIR/models` (or `model`), and whether it is the pinned 2023mar file.
fn load_detector(a: &Args, model: Option<&Path>) -> Result<(yunet::Detector, PathBuf, bool), String> {
    let file = known::download(known::YUNET_ID).map(|d| d.file_name).ok_or("no YuNet file name")?;
    let model = model.map_or_else(|| a.dir.join("models").join(file), Path::to_path_buf);
    let bytes = std::fs::read(&model).map_err(|e| format!("{}: {e} (run `cargo xtask ai-bench fetch` or pass --model)", model.display()))?;
    let detector = yunet::Detector::new(&bytes).map_err(|e| format!("{}: {e}", model.display()))?;
    let pinned = crate::corpus::file_sha256(&model).is_ok_and(|h| h == known::YUNET_SHA256);
    Ok((detector, model, pinned))
}

fn face_detection(a: &Args) -> Result<(), String> {
    let (detector, model, pinned) = load_detector(a, a.model.as_deref())?;
    let side = detector.input_side();
    println!(
        "YuNet {} ({}), input {side} px, {} thread(s)",
        model.display(),
        if pinned { "the pinned 2023mar model" } else { "NOT the pinned 2023mar model" },
        a.threads
    );
    let r = wider::run(&a.dir.join(fetch::WIDER.id), &detector, a.limit, a.threads)?;
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

/// The recognition models to measure: `--model`'s file, else the downloaded SFace and AuraFace that are in `DIR/models`.
fn recognisers(a: &Args) -> Result<Vec<(PathBuf, lightcraft_faces::ModelManifest)>, String> {
    let known_model = |path: &Path| {
        let sha = crate::corpus::file_sha256(path)?;
        known::lookup(&sha)
            .filter(|m| m.role == lightcraft_faces::Role::Embedder)
            .map(|m| (path.to_path_buf(), m))
            .ok_or_else(|| format!("{}: not a recognition model LightCraft knows by its SHA-256 ({sha})", path.display()))
    };
    if let Some(model) = &a.model {
        return Ok(vec![known_model(model)?]);
    }
    let mut found = Vec::new();
    for (id, _, _) in PUBLISHED_LFW {
        let file = known::download(id).map(|d| d.file_name).ok_or_else(|| format!("no download of {id}"))?;
        let path = a.dir.join("models").join(file);
        if path.is_file() {
            found.push(known_model(&path)?);
        } else {
            println!("skipping {id}: {} is not there (run `cargo xtask ai-bench fetch --set lfw`)", path.display());
        }
    }
    if found.is_empty() {
        return Err(format!("no recognition model in {} (run `cargo xtask ai-bench fetch --set lfw`)", a.dir.join("models").display()));
    }
    Ok(found)
}

fn face_recognition(a: &Args) -> Result<(), String> {
    let (detector, detector_file, pinned) = load_detector(a, None)?;
    println!(
        "device: CPU (LightCraft's pure-Rust engine, one thread per photo), {} worker thread(s)\nYuNet {} ({}), input {} px, default options",
        a.threads,
        detector_file.display(),
        if pinned { "the pinned 2023mar model" } else { "NOT the pinned 2023mar model" },
        detector.input_side()
    );
    for (path, manifest) in recognisers(a)? {
        let embedder = Embedder::load(&path, &manifest).map_err(|e| format!("{}: {e}", path.display()))?;
        let r = lfw::run(&a.dir.join(fetch::LFW.id), &detector, &embedder, a.limit, a.threads)?;
        println!("{} ({}), LFW view 2, {} pairs of {} photos", manifest.name, path.display(), r.pairs, r.photos);
        println!("  10-fold accuracy {:.2} % (standard deviation {:.2})", r.accuracy.0, r.accuracy.1);
        if let Some((_, published, page)) = PUBLISHED_LFW.iter().find(|(id, _, _)| *id == manifest.id) {
            println!("  published: {published:.2} % ({page})");
        }
        if r.fallbacks > 0 {
            println!("  {} photos had no face found, or none that could be aligned: the centre was cut out", r.fallbacks);
        }
        if r.failed > 0 || r.failed_pairs > 0 {
            println!(
                "  {} photos could not be read or embedded: {} of the {} pairs have no score and count as wrong",
                r.failed, r.failed_pairs, r.pairs
            );
        }
        println!("  wall time {:.1} s ({:.0} ms per photo)", r.seconds, r.seconds * 1000.0 / r.photos.max(1) as f64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_of_zero_is_refused() {
        let e = parse(Path::new("."), &["--limit", "0"]).err().unwrap_or_default();
        assert!(e.contains("--limit") && e.contains("at least 1"), "{e:?}");
        assert!(parse(Path::new("."), &["--limit", "1"]).is_ok());
    }

    #[test]
    fn every_archive_of_every_set_is_unpacked_through_a_completion_marker() {
        // lfw.tgz and both WIDER zips are archives; pairs.txt is the only plain file
        let archives: Vec<(&str, bool)> = [&fetch::WIDER, &fetch::LFW].iter().flat_map(|s| s.archives()).collect();
        assert_eq!(archives, vec![("WIDER_val.zip", true), ("eval_tools.zip", true), ("lfw.tgz", true), ("pairs.txt", false)]);
    }
}
