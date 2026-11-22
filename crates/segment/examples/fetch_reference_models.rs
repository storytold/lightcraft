//! Download the openly licensed models the model-reference test (`tests/model_reference.rs`)
//! runs, from the same pinned locations and with the same size + SHA-256 checks the app's own
//! download uses (`lcc_segment::models`, `lcc_segment::fetch`): SAM 2.1 Hiera-Tiny (Apache-2.0),
//! BiRefNet_lite (MIT), EoMT-S COCO (MIT) and the MediaPipe Selfie Multiclass and Face Landmarker
//! models (Apache-2.0). SAM 3 (SAM License, gated) and YuNet (retired; trained on CC BY-NC-ND
//! data) are deliberately not fetched (docs/ci.md → Model references).
//!
//! `cargo run -p lcc-segment --release --example fetch_reference_models -- <dir>` puts each model in
//! `<dir>/<model folder>`; files already there and verified are kept, so a cached folder costs
//! nothing.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use lcc_segment::fetch::{self, FileSpec, Options};
use lcc_segment::models;

fn main() -> ExitCode {
    let Some(root) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: fetch_reference_models <dir>");
        return ExitCode::FAILURE;
    };
    let birefnet: Vec<String> = fetch::BIREFNET_MIRRORS.iter().map(|s| s.to_string()).collect();
    let mut jobs: Vec<(&str, &[FileSpec<'static>], Vec<String>)> = Vec::new();
    for m in [&models::SAM2, &models::SCENE, &models::PEOPLE, &models::FACE] {
        jobs.push((m.dir_name, m.files, m.default_mirrors.iter().map(|s| s.to_string()).collect()));
    }
    jobs.push(("birefnet-lite", fetch::BIREFNET_FILES, birefnet));
    let cancel = AtomicBool::new(false);
    let mut failed = false;
    for (dir_name, files, mirrors) in jobs {
        let dir = root.join(dir_name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("{}: {e}", dir.display());
            failed = true;
            continue;
        }
        let mut last = 0u64;
        let mut progress = |p: &fetch::Progress| {
            if p.done >= last + (64 << 20) || p.done == p.total {
                last = p.done;
                eprintln!("{dir_name}/{}: {} of {} MB", p.file, p.done >> 20, p.total >> 20);
            }
        };
        match fetch::download(files, &mirrors, &dir, &Options::default(), &cancel, &mut progress) {
            Ok(()) => eprintln!("{dir_name}: ok"),
            Err(e) => {
                eprintln!("{dir_name}: {e}");
                failed = true;
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
