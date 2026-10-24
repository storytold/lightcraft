#![cfg(target_os = "android")]

//! Startup self test: prove the photo pipeline works on the device, through the engine's own code
//! paths, and say so in logcat.
//!
//! LightCraft's product is photos, not video, so what is worth proving is the *whole still pipeline*
//! the app runs all day:
//!
//! 1. `lightcraft-scenes` generates the procedural demo library and the catalog holds it (the same
//!    `Session::with_demo()` the desktop's `--memory` mode uses);
//! 2. the engine's export path renders one of those photos — decode/scene → develop pipeline →
//!    JPEG encode (`lightcraft_engine::export::export_photo`), exactly what File ▸ Export runs;
//! 3. the encoded bytes are decoded *again* with the app's own decoder (`lightcraft_codecs::decode`)
//!    and the two sizes are compared, which checks the encoder and the decoder against each other
//!    rather than just counting bytes;
//! 4. the bytes are written to the app's own storage and read back (a phone's sandbox is the one
//!    thing the desktop never exercises), and the scratch file is removed again.
//!
//! Every failure is reported as text: this runs on a background thread and never panics (upstream
//! rule: never crash).

use std::path::Path;
use std::time::Instant;

use lightcraft_engine::Session;
use lightcraft_engine::export::{ExportOptions, Resize, export_photo};

/// The long edge the self test renders at: big enough to exercise the resampling, small enough to
/// finish in a second or two on a phone.
const EDGE: u32 = 1024;

/// Run the self test and describe what happened (one logcat line).
pub fn run(scratch_dir: &Path) -> String {
    let t0 = Instant::now();
    // 1. the procedural demo library, through the engine's own session
    let mut session = Session::with_demo().with_fs().with_system_clock();
    let commands = session.commands().len();
    let photos = session.catalog.len();
    let first = session.catalog.photos().next().map(|p| (p.id, p.width, p.height, p.file_name.clone()));
    let Some((id, width, height, name)) = first else {
        return "自检: 引擎起来了，但示例库是空的（lightcraft-scenes 没有照片）".to_string();
    };

    // 2. render + encode through the engine's export path (JPEG, 1024 px long edge)
    let opts = ExportOptions { resize: Some(Resize::long_edge(EDGE)), quality: 80, ..Default::default() };
    let exported = match export_photo(&mut session, id, &opts, 1) {
        Ok(e) => e,
        Err(e) => return format!("自检: 命令 {commands} 个 ／ 示例库 {photos} 张 ／ 渲染 {name} 失败: {e}"),
    };
    let is_jpeg = exported.bytes.starts_with(&[0xff, 0xd8]);

    // 3. decode the encoded bytes again with the app's own decoder
    let readback = match lightcraft_codecs::decode(&exported.bytes, lightcraft_codecs::DecodeOptions::default()) {
        Ok(d) => format!(
            "回读 {}×{}（{:?}，{} 位）{}",
            d.width,
            d.height,
            d.format,
            d.bit_depth,
            if (d.width as usize, d.height as usize) == (exported.width, exported.height) { "，尺寸一致" } else { "，尺寸不一致！" }
        ),
        Err(e) => format!("回读失败: {e}"),
    };

    // 4. the app's own storage: write the exported bytes, read them back, remove the scratch file
    let file = scratch_dir.join("selftest-export.jpg");
    let storage = match std::fs::write(&file, &exported.bytes).and_then(|()| std::fs::read(&file)) {
        Ok(bytes) => {
            let same = bytes.len() == exported.bytes.len();
            let _ = std::fs::remove_file(&file);
            if same {
                format!("写入并读回 {} 字节", bytes.len())
            } else {
                format!("读回 {} 字节，写的是 {} 字节！", bytes.len(), exported.bytes.len())
            }
        }
        Err(e) => format!("写入 {} 失败: {e}", file.display()),
    };

    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    format!(
        "自检: 命令 {commands} 个 | 示例库 {photos} 张，首张 {name} {width}×{height} | 渲染导出 {}×{} JPEG{} {} 字节 | {readback} | 存储 {storage} | {ms:.0} ms",
        exported.width,
        exported.height,
        if is_jpeg { "" } else { "（头部不是 JPEG！）" },
        exported.bytes.len(),
    )
}
