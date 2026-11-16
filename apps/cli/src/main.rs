//! The command-line tool (`lcc_brand::CLI_BINARY`): the app, headless.
//!
//! ```text
//! <cli> run [--demo | --library DIR | --connect [ADDR]] [--import PATH]… CMD [key=value…]…
//! <cli> sync-folder [--library DIR | --connect [ADDR]] FOLDER
//! <cli> open-url [--library DIR | --connect [ADDR]] URL
//! <cli> mcp [--connect [ADDR]] [--demo] [--compact] [FILES/FOLDERS…]
//! <cli> render <in> -o <out> [--set control=value]… [--settings FILE.json|PRESET-FILE] [--preset ID] [--size N] [--quality Q] [--trim IN,OUT]
//! <cli> snapshot [--library DIR | --demo] [--script FILE.jsonl|-] [-o OUT.png] [--size WxH] [--scale S] [FILES…]
//! <cli> merge hdr|panorama|hdr-panorama|hdr-batch|focus [OPTIONS] FILES…
//! <cli> synth-merge hdr|panorama|focus -o DIR
//! <cli> denoise-serve [--model ID] [--models DIR] [--listen ADDR] [--token-env VAR] [--run-on auto|gpu|cpu]
//! <cli> genfill-mock [--port N] [--key KEY] [--fill patchmatch|mean]
//! <cli> commands [--json]
//! <cli> controls [--json]
//! <cli> calibrate [--max N] [--out DIR] FOLDERS/FILES…
//! <cli> calibrate-lightroom preset|fit|add|list|verify …
//! <cli> calibrate-chart --shot RAW --corners "x,y x,y x,y x,y" [--light L] [--shot …]… [--chart 2014|2005] [--dcp OUT] [--preview DIR]
//! <cli> compare REFERENCE TEST [--size N] [--no-align] [--diff OUT.png] [--json]
//! <cli> vignetting-calibration REFERENCE.raw [--grid N] [--smooth S] [--check B.raw [--force]] [--save MAP.json] [--save-settings PARTIAL.json] [--json]
//! <cli> camdb extract [--contributor NAME] NATIVE.dng…
//! <cli> camdb show MAKE MODEL
//! <cli> bench-cull FOLDER [--files K] [--n N] [--copy M] [--work DIR] [--json FILE]
//! ```
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod alloc_release;
mod bench_cull;
mod calibrate_chart;
mod calibrate_lightroom;
mod catalog_migrate;
mod compare;
mod run_action;
mod sync_folder;
mod vignetting_calibration;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use lcc_engine::Session;
use lcc_mcp::{Backend, DEFAULT_ADDR, Headless, Remote, Server, expand_paths};
use serde_json::{Value, json};

/// `--help` text; `{cli}`, `{bin}`, `{app}` and `{P}` are filled by [`usage`].
const USAGE: &str = "\
{cli}: headless {app} (photo library + raw developer)

USAGE:
  {cli} run [OPTIONS] COMMAND [key=value | '{json}']… [COMMAND …]…
      Run one or more commands (ids from `commands`, or control-protocol methods such as
      engine.commands / ui.inspect / ui.screenshot) and print one JSON line per command:
      {\"command\", \"ok\", \"result\" | \"error\", \"ms\"}. A word without `=` starts the next
      command; values are JSON when they parse (1, true, [1,2], {…}), else strings. Exit status
      is non-zero when a command fails. Example:
        {cli} run --import ~/Photos/a.jpg develop.set control=light.exposure value=0.7 \\
            develop.auto app.export path=/tmp/a.jpg longEdge=1600
      Options:
        --demo            headless, the procedural demo library
        --library DIR     headless, open (or create) a {app} library; edits are saved
                          (@app: the library the desktop app opens)
        --import PATH     headless, import a file or folder first (repeatable)
        --connect [ADDR]  drive the running app (`{bin} --control 7980`; default 127.0.0.1:7980)
        --script FILE|-   also run JSON lines {\"command\": id, \"params\": {…}} (or {\"method\": …})
        --keep-going      continue after a failed command
        --shared-settings headless without --library: use the shared presets and profiles (the settings
                          folder's Library Store), e.g. `run --shared-settings store.gatherPresets`; quit the app first
  {cli} run-action NAME [OPTIONS] [param=value…] [ids=[…]]
      Play a saved action (actions.list) on the selection, or on `ids`; OPTIONS as for `run`.
  {cli} sync-folder [OPTIONS] FOLDER
      Synchronize a folder of the library now (folder.synchronize: import its new files, relink
      moved ones): in the running app with --connect, else headless with --library DIR.
  {cli} open-url [OPTIONS] URL
      Follow one of {app}'s links, e.g. {scheme}://sync?path=/Photos/Trip (synchronize that
      folder); OPTIONS as for `run`.
  {cli} mcp [OPTIONS] [FILES/FOLDERS…]
      MCP server (JSON-RPC 2.0 over stdio). Headless by default: an in-process session with the
      given files imported. Options:
        --connect [ADDR]  drive a running app instead (`{bin} --control 7980`; default 127.0.0.1:7980)
        --demo            headless: start with the procedurally generated demo library
        --library DIR     headless: open (or create) a persistent {app} library; edits are saved
        --compact         list only the helper tools (every command stays reachable via run_command)
  {cli} render <IN> -o <OUT> [OPTIONS]
      Develop one file and export it (.jpg, .png, .tif, .webp, .avif, .jxl, .psd or .dng by extension) with the
      same encoder as the app's Export dialog; a video clip exports as a movie (.mp4 / .mov). Options:
        --set CONTROL=VALUE  set a develop slider, repeatable (e.g. --set light.exposure=0.5)
        --settings FILE      merge a partial develop-settings JSON file, or apply the one preset of a
                             .lcpreset (preset.export) or XMP preset
        --preset ID          apply a preset (see `commands`/presets.list)
        --size N             long edge in pixels (default: full size, cropped)
        --quality Q          JPEG/AVIF/JPEG XL quality 1..100 (default 92)
        --trim IN,OUT        video clips: export only IN..OUT seconds (OUT empty = to the end)
        --opt KEY=VALUE      any export option of `app.export`, repeatable (VALUE is JSON or a
                             string), e.g. --opt colorSpace=displayP3 --opt bitDepth=16
                             --opt percent=50 --opt shortEdge=1080 --opt ppi=300
                             --opt format=original (copy + XMP sidecar) --opt metadata=none
                             --opt addToPhotos=true --opt photosAlbum=NAME (macOS: then add
                             the file to Apple Photos, into that album)
  {cli} snapshot [OPTIONS] [FILES/FOLDERS…]
      Run the full app UI headlessly (no window, no GPU: CPU-rasterized egui) and write PNGs.
      Options:
        --demo            the procedural demo library (default unless --library or FILES)
        --library DIR     open (or create) a {app} library
        --script FILE     JSON-lines control-protocol requests (docs/control-protocol.md), one
                          per line: {\"method\": \"ui.set\", \"params\": {\"view\": \"detail\"}}.
                          Replies go to stdout. `ui.screenshot` without a path writes -o (then
                          OUT-2.png, OUT-3.png…); `ui.settle {timeoutMs?}` waits for renders.
                          A failed request (\"ok\": false) does not stop the script, but the exit
                          status is non-zero when any request failed. `--script -` reads
                          stdin and answers each line as it arrives (until EOF).
        -o, --output OUT  PNG path (a final screenshot is written here if the script took none)
        --size WxH        window size in points (default 1600x1000)
        --scale S         pixels per point (default 1)
  {cli} merge hdr|panorama|hdr-panorama|hdr-batch|focus [OPTIONS] FILES…
      Photo Merge: writes <first>-HDR.dng / -Pano.dng / -HDR-Pano.dng / -Focus.dng next to the first file and
      prints the result as JSON. hdr-batch merges each bracket set among the files (photos taken
      less than a second apart at different exposures), e.g. a whole folder. Options:
        --deghost none|low|medium|high   --no-align   --bracket N (HDR panorama)
        --projection auto|spherical|cylindrical|perspective   --boundary-warp 0..100
        --auto-crop   --fill-edges   --no-auto-settings
        --preview OUT.png   only render a ≤ 1024 px preview (nothing written next to the files)
      focus takes only --no-align, --no-auto-settings and --preview.
  {cli} synth-merge hdr|panorama|focus -o DIR
      Write synthetic merge inputs (procedural scene; bracketed DNGs, overlapping PNG views or a
      focus bracket as PNGs).
  {cli} genfill-mock [--port N] [--key KEY] [--fill patchmatch|mean]
      A local stand-in for the generative fill providers (OpenAI, Stability AI, Black Forest Labs,
      fal.ai request shapes) on 127.0.0.1, for testing without an account: start {app} with
      {P}_GENFILL_MOCK=http://127.0.0.1:PORT and any key. It fills holes with {app}'s own
      content-aware fill (or a flat mean); nothing leaves this computer. Default port 18510.
  {cli} denoise-serve [OPTIONS]
      Run AI Denoise tiles for {app} on other computers ({P}_DENOISE_URL=<this host>:7990 and
      {P}_DENOISE_TOKEN there; see docs/denoise.md). Serves the installed model selected in Settings,
      on this computer's graphics card when it can run it, else the CPU. Plain TCP: listen on a VPN/LAN
      address (e.g. the Tailscale IP) or firewall the port to it. Options:
        --model ID        an installed model other than the selected one
        --models DIR      the models folder (default: {P}_DENOISE_MODELS, else this user's)
        --listen ADDR     address:port to listen on (default 127.0.0.1:7990)
        --token-env VAR   environment variable holding the shared token, ≥ 16 characters
                          (default {P}_DENOISE_TOKEN)
        --run-on WHERE    auto (default), gpu or cpu
        --max-connections N   concurrent clients (default 8)
  {cli} commands [--json]   list every command id with its parameters
  {cli} controls [--json]   list every develop control id with its range
  {cli} calibrate [--max N] [--out DIR] FOLDERS/FILES…
      Fit a colour profile per camera model from raw files and their embedded camera JPEGs
      (Sony ARW, Nikon NEF, Fujifilm RAF, Canon CR3/CR2, Panasonic RW2, Olympus ORF, Pentax PEF…): up to N files spread over the folders (default 300; 0 = all), pooled per
      model, written as <model>.json to DIR (default: the profiles folder {app} reads,
      <config>/camera-profiles, or ${P}_CAMERA_PROFILES). Raws of a profiled model then
      take their colour from the profile and only their tone from their own JPEG.
  {cli} calibrate-lightroom preset -o FILE.xmp
      Write the baseline Lightroom preset to apply (on virtual copies) before exporting for a fit.
  {cli} calibrate-lightroom fit --raws DIR… --exports DIR… [-o OUTDIR] [--contributor NAME]
          [--lens MODEL] [--dry-run]
      Fit a camera's colour model and looks to Lightroom's exports of your own raws (16-bit TIFF, one export
      folder per Lightroom profile). Writes <Make>_<Model>.json (the file to send: numbers only, no images) and
      one .dcp per look for your own testing. --lens: the fit is for that lens only (Exif lens model, e.g. one
      of a phone's cameras). --dry-run pairs and reports coverage without fitting.
  {cli} calibrate-lightroom add FILE [--repo DIR] [--contributor NAME] [--lens MODEL] [--replace]
      Developers: validate a contributed calibration, write it to crates/camdb/data/calibrations/ and add its
      assets/ATTRIBUTION.md row (DIR: the repository root, default .). A calibration already there for the
      same camera, lens and editor is kept unless --replace.
  {cli} calibrate-lightroom list
      The calibrations built into this binary, with their looks and held-out colour error.
  {cli} calibrate-lightroom verify --raws DIR… --exports DIR… [--report DIR] [--json] [--size N] [--max N]
      Moving from another editor: render your edited raws the way {app} imports them (their XMP sidecars, or the
      settings Lightroom embeds in its exports) and measure each against its edited export (CIEDE2000), per photo
      and per group of controls the edits used. --report writes verify.json and a ΔE00 map per photo.
  {cli} compare REFERENCE TEST [--size N] [--no-align] [--diff OUT.png] [--json]
      How far TEST is from REFERENCE: two renders of the same photo from any editor or camera (JPEG, TIFF, PNG,
      …, read through their embedded colour profiles). Lines them up, then prints CIEDE2000 (mean, median, p95,
      p99) overall, by tone band and by hue, with the lightness, saturation and hue bias. --size: long edge
      compared (default 512); --no-align: compare pixel for pixel; --diff: write a false-colour ΔE00 map.
  {cli} vignetting-calibration REFERENCE.raw [--grid N] [--smooth S] [--check B.raw [--force]] [--save MAP.json] [--save-settings PARTIAL.json] [--json]
      Measure a flat-field reference (a raw shot through a diffuser): its light fall-off in EV at the corners and
      along the radius, its optical centre, and the lens database's vignetting for the same lens, focal length and
      aperture. --check applies the map to a second reference of the same camera, lens, focal length and aperture
      (--force: of another) and reports the fall-off left; --save writes the map as JSON; --save-settings writes
      it as partial develop settings (render --settings, develop.merge) that apply it at amount 100. --grid: cells
      along the long side (default 96); --smooth: smoothing in cells (default 1).
  {cli} calibrate-lightroom probe -o DIR (--like RAW | --make MAKE --model MODEL) [--unique-model NAME] [--base-only]
      Write synthetic probe DNGs for a camera: colour patches spread through camera RGB, and one copy per
      slider-sweep step with that Lightroom setting embedded (applied when Lightroom imports it). Export them all
      from Lightroom as 16-bit ProPhoto TIFFs; probe_base's export can also go into `fit` with your raws.
  {cli} calibrate-lightroom probe-check --probes DIR --exports DIR [--json] [-o FILE]
      Read Lightroom's probe exports back by position: the default render's ΔE00 against {app}'s, and for each
      slider step the {app} value whose change matches Lightroom's best, next to the value the import uses.
  {cli} calibrate-chart --shot RAW --corners 'x,y x,y x,y x,y' [--light L] [--shot …]… [--chart 2014|2005]
          [--dcp OUT.dcp] [--preview DIR] [--report FILE.json] [--contributor NAME]
      Measure a camera's colour from raw shots of an X-Rite ColorChecker Classic (24 patches): one shot in
      daylight, ideally another under tungsten (--light daylight|d65|d50|shade|tungsten|KELVIN, default
      daylight). --corners: the centres of the four corner patches as fractions of the photo's width and height,
      in any order. Prints a camera database entry (TOML) and the colour error per patch before and after;
      --dcp also writes a camera profile to try at once, --preview a picture of where each patch was read.
  {cli} camdb extract [--contributor NAME] NATIVE.dng…
      Print camera database entries (TOML, for data/cameras/<maker>.toml in the camdb crate) from DNGs the
      camera wrote itself. DNGs written by Adobe software or by {app}'s own export are refused.
  {cli} camdb show MAKE MODEL
      Print the camera database entry a camera's raws use, with its provenance.
  {cli} bench-cull FOLDER [--files K] [--n N] [--copy M] [--work DIR] [--json FILE]
      Time what culling a shoot costs on its own files: reading the folder (browse), the first
      screen of grid thumbnails, camera previews at loupe sizes and full size, a loupe render,
      rating, and importing N files in place (default 2000) and copying M (default 200). --files K
      samples K files of a slow folder. Libraries are made under DIR (default: the temp folder);
      nothing is written next to the photos.
  {cli} --version | --help
";

fn usage() -> String {
    USAGE
        .replace("{cli}", CLI)
        .replace("{bin}", lcc_brand::BINARY)
        .replace("{P}", lcc_brand::ENV_PREFIX)
        .replace("{app}", APP)
        .replace("{scheme}", lcc_brand::URL_SCHEME)
}

/// The command name shown in messages and `--help`.
const CLI: &str = lcc_brand::CLI_BINARY;
const APP: &str = lcc_brand::DISPLAY_NAME;
const BIN: &str = lcc_brand::BINARY;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `--library @app`: the library the desktop app opens — the one last opened with Settings → Open
/// Library… (`settings.libraryPath` in its `ui.json`, unless `<PREFIX>_LIBRARY` is set), else the
/// default location — so a headless run (a prep run before culling) fills the same preview cache
/// the app reads.
pub const APP_LIBRARY: &str = "@app";

fn app_library_with(config: Option<std::path::PathBuf>, env_set: bool, default: Option<std::path::PathBuf>) -> Option<String> {
    let saved = config
        .filter(|_| !env_set)
        .and_then(|d| std::fs::read(d.join("ui.json")).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("settings")?.get("libraryPath")?.as_str().map(str::to_string))
        .filter(|p| !p.trim().is_empty());
    saved.or_else(|| default.map(|d| d.to_string_lossy().into_owned()))
}

/// The folder `--library` names: `@app` is the desktop app's library ([`APP_LIBRARY`]).
fn library_arg(dir: &str) -> Result<String, String> {
    if dir != APP_LIBRARY {
        return Ok(dir.to_string());
    }
    let env_set = lcc_brand::env_os("LIBRARY").is_some_and(|v| !v.is_empty());
    let found = app_library_with(lcc_engine::config::config_dir(), env_set, lcc_engine::library::default_dir())
        .ok_or_else(|| "--library @app: no home folder to find the app's library in".to_string())?;
    eprintln!("{CLI}: the app's library is {found}");
    Ok(found)
}

/// Why `--library DIR` can't be opened; for a library open in another program, how to work with
/// that one instead (issue #99).
/// Print the open library's warnings (an unlockable library, damaged settings files) on stderr:
/// stdout carries command results / MCP protocol messages only.
fn library_warnings(session: &mut Session, who: &str) {
    for w in session.take_library_warnings() {
        eprintln!("{who}: warning: {w}");
    }
}

fn library_error(dir: &str, e: lcc_engine::EngineError) -> String {
    match e {
        lcc_engine::EngineError::LibraryInUse(why) => format!(
            "{dir}: {why}\nTo work with the library while the app has it open, start the app with `--control PORT` and use `{CLI} mcp --connect 127.0.0.1:PORT`."
        ),
        e @ lcc_engine::EngineError::LightroomCatalog { .. } => format!(
            "{e}\nOpen a library with `--library DIR` (an empty folder makes a new one) and run `library.importLightroom` with the catalog's path."
        ),
        e => format!("{dir}: {e}"),
    }
}

fn main() -> ExitCode {
    // `--features dhat-heap`: count allocations; the profile is written when `_heap` drops
    // (<PREFIX>_DHAT_FILE, default dhat-heap.json).
    #[cfg(feature = "dhat-heap")]
    let _heap = {
        let file = lcc_brand::env("DHAT_FILE").unwrap_or_else(|| "dhat-heap.json".into());
        lcc_engine::memory::set_heap_stats(|| {
            let s = dhat::HeapStats::get();
            lcc_engine::memory::HeapUsage { current: s.curr_bytes as u64, peak: s.max_bytes as u64 }
        });
        dhat::Profiler::builder().file_name(file).build()
    };
    alloc_release::install();
    // Warnings (a GPU render redone on the CPU, an unknown backend name) on stderr; <PREFIX>_LOG
    // or RUST_LOG picks another level (#168).
    lcc_engine::logging::install("lcc-cli");
    // first start under this name: copy the settings folder of a previous name (never moved)
    if !lcc_brand::env_is_set("NO_PREFS") {
        match lcc_brand::migrate_legacy_settings() {
            Ok(Some(from)) => log::info!("settings copied from {}", from.display()),
            Ok(None) => {}
            Err(e) => log::warn!("could not copy the previous settings folder: {e}"),
        }
    }
    // `std::env::args` panics on an argument that isn't UTF-8: refuse it with a message instead
    let mut args: Vec<String> = Vec::new();
    for (index, arg) in std::env::args_os().skip(1).enumerate() {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(_) => {
                eprintln!("{CLI}: argument {} is not valid UTF-8; use a UTF-8 encoded argument", index + 1);
                return ExitCode::FAILURE;
            }
        }
    }
    let r = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("run-action") => run_action::to_run_args(&args[1..]).and_then(|a| run(&a)),
        Some("sync-folder") => sync_folder::sync_folder_args(&args[1..]).and_then(|a| run(&a)),
        Some("open-url") => sync_folder::open_url_args(&args[1..]).and_then(|a| run(&a)),
        Some("mcp") => mcp(&args[1..]),
        Some("render") => render(&args[1..]),
        Some("snapshot") => snapshot(&args[1..]),
        Some("bench-cull") => bench_cull::run(&args[1..]),
        Some("commands") => commands(&args[1..]),
        Some("merge") => merge(&args[1..]),
        Some("synth-merge") => synth_merge(&args[1..]),
        Some("controls") => controls(&args[1..]),
        Some("denoise-serve") => denoise_serve(&args[1..]),
        Some("genfill-mock") => genfill_mock(&args[1..]),
        Some("calibrate") => calibrate(&args[1..]),
        Some("camdb") => camdb(&args[1..]),
        Some("calibrate-lightroom") => calibrate_lightroom::run(&args[1..]),
        Some("calibrate-chart") => calibrate_chart::run(&args[1..]),
        Some("compare") => compare::run(&args[1..]),
        Some("vignetting-calibration") => vignetting_calibration::run(&args[1..]),
        Some("--version" | "-V" | "version") => {
            println!("{CLI} {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") | None => {
            print!("{}", usage());
            Ok(())
        }
        Some(other) => Err(format!("unknown subcommand `{other}`\n\n{}", usage())),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{CLI}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Raw files below `path` (or `path` itself), skipping hidden and NAS metadata folders.
///
/// Folders go through the engine's bounded walk ([`lcc_engine::walk::files_in_entering`]): it enters a folder reached
/// through a link once at most and stops at a depth and an entry budget. A folder of links pointing back at their
/// parents made the old recursive walk re-read the same folders exponentially often before its depth limit cut it
/// off (upstream #3643).
fn raw_files(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    let is_raw = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["arw", "nef", "nrw", "raf", "cr3", "cr2", "rw2", "orf", "pef", "srw", "erf"].iter().any(|x| e.eq_ignore_ascii_case(x)))
    };
    if path.is_file() {
        if is_raw(path) {
            out.push(path.to_path_buf());
        }
        return;
    }
    // (hidden entries are left out by the walk; `@eaDir` and other NAS metadata folders and files here, folders
    // before they are listed so their entries do not use up the walk's budget)
    let nas_metadata = |p: &Path| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('@'));
    let walked = lcc_engine::walk::files_in_entering(
        path,
        None,
        lcc_engine::walk::Limits::default(),
        |p| is_raw(p) && !nas_metadata(p) && p.is_file(),
        |p| !nas_metadata(p),
    );
    if walked.truncated {
        eprintln!("calibrate: {} has more folders or entries than a walk looks at; the rest is left out", path.display());
    }
    out.extend(walked.files);
}

/// `camdb extract`: entries from camera-native DNGs; `camdb show`: the entry a camera resolves to.
fn camdb(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("extract") => {
            let mut contributor = None;
            let mut files = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--contributor" => contributor = Some(take_value(args, &mut i, "--contributor")?.to_string()),
                    a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
                    f => files.push(f.to_string()),
                }
                i += 1;
            }
            if files.is_empty() {
                return Err("camdb extract needs camera-native DNG files".into());
            }
            let mut entries = Vec::new();
            for f in &files {
                let bytes = std::fs::read(f).map_err(|e| format!("{f}: {e}"))?;
                let mut entry = lcc_camdb::extract::from_dng(&bytes).map_err(|e| format!("{f}: {e}"))?;
                entry.provenance.contributor = contributor.clone();
                entries.push(entry);
            }
            print!("{}", lcc_camdb::to_toml(&entries).map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("show") => {
            let (Some(make), Some(model)) = (args.get(1), args.get(2)) else {
                return Err("camdb show needs MAKE and MODEL".into());
            };
            let entry = lcc_camdb::find(Some(make), model).ok_or_else(|| format!("no camera database entry for {make} {model}"))?;
            print!("{}", lcc_camdb::to_toml(&[entry]).map_err(|e| e.to_string())?);
            Ok(())
        }
        _ => Err("camdb needs `extract` or `show`".into()),
    }
}

fn calibrate(args: &[String]) -> Result<(), String> {
    let mut max = 300usize;
    let mut out: Option<std::path::PathBuf> = None;
    let mut inputs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max" => max = take_value(args, &mut i, "--max")?.parse().map_err(|_| "--max needs a number".to_string())?,
            "--out" => out = Some(take_value(args, &mut i, "--out")?.into()),
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
            f => inputs.push(f.to_string()),
        }
        i += 1;
    }
    if inputs.is_empty() {
        return Err("calibrate needs folders or raw files".into());
    }
    let mut files = Vec::new();
    for input in &inputs {
        raw_files(Path::new(input), &mut files);
    }
    files.sort();
    files.dedup();
    let found = files.len();
    if max > 0 && files.len() > max {
        // spread over all folders (dates, scenes) rather than the first N
        files = (0..max).filter_map(|k| files.get(k * found / max).cloned()).collect();
    }
    eprintln!("calibrate: {} of {found} raw files", files.len());
    let dir = out.or_else(lcc_engine::camera_profiles::dir).ok_or("no profiles folder: pass --out DIR")?;
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 6);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let mut pools: Vec<(usize, lcc_engine::camera_profiles::Pool)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut pools = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(path) = files.get(index) else { break };
                        let mut pool = lcc_engine::camera_profiles::Pool::default();
                        let result = std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| pool.add(&bytes));
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        match result {
                            Ok(Some(model)) => {
                                eprintln!("[{n}/{}] {} ({model})", files.len(), path.display());
                                pools.push((index, pool));
                            }
                            Ok(None) => eprintln!("[{n}/{}] {}: skipped (no usable camera JPEG or colour)", files.len(), path.display()),
                            Err(e) => eprintln!("[{n}/{}] {}: {e}", files.len(), path.display()),
                        }
                    }
                    pools
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).flatten().collect()
    });
    // The fitting holdout and floating-point sums depend on pair order. Keep parallel decoding, but merge
    // in the sorted input order rather than worker admission/completion order.
    pools.sort_by_key(|(index, _)| *index);
    let mut pool = lcc_engine::camera_profiles::Pool::default();
    for (_, p) in pools {
        pool.merge(p);
    }
    let mut written = 0;
    for result in pool.fit(5) {
        match result {
            Ok(profile) => {
                let path = lcc_engine::camera_profiles::save(&profile, &dir)?;
                println!(
                    "{}: {} photos, {} colour pairs, hue/saturation table {}, tone curve {} → {}",
                    profile.model,
                    profile.files,
                    profile.samples,
                    profile.hue_sat.is_some(),
                    profile.tone.is_some(),
                    path.display()
                );
                written += 1;
            }
            Err(e) => eprintln!("calibrate: {e}"),
        }
    }
    for (model, n) in pool.files() {
        if n < 5 {
            eprintln!("calibrate: {model}: only {n} usable photo(s), at least 5 needed");
        }
    }
    if written == 0 {
        return Err("no profile written".into());
    }
    Ok(())
}

fn mcp(args: &[String]) -> Result<(), String> {
    let mut connect: Option<String> = None;
    let mut demo = false;
    let mut library: Option<String> = None;
    let mut compact = false;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--connect" => {
                // Optional address argument. A bare port (all digits) is taken as a port on the
                // loopback default — matches the help's `--connect [ADDR]` hint alongside the
                // `--control 7980` example. A non-numeric, non-`--`-prefixed token (e.g. a
                // command id like `ui.inspect`) leaves the address at its default so the
                // command is still consumed next. (#1178)
                match args.get(i + 1).filter(|a| a.contains(':') || a.chars().all(|c| c.is_ascii_digit())) {
                    Some(a) => {
                        connect = Some(if a.contains(':') { a.clone() } else { format!("127.0.0.1:{a}") });
                        i += 1;
                    }
                    None => connect = Some(DEFAULT_ADDR.to_string()),
                }
            }
            a if a.starts_with("--connect=") => connect = Some(a["--connect=".len()..].to_string()),
            "--headless" => connect = None,
            "--demo" => demo = true,
            "--compact" => compact = true,
            "--library" => library = Some(library_arg(take_value(args, &mut i, "--library")?)?),
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    let backend: Box<dyn Backend> = match connect {
        Some(addr) => {
            if !files.is_empty() || demo || library.is_some() {
                return Err("FILES, --demo and --library apply to headless mode only (import through the `import` tool instead)".into());
            }
            match Remote::connect(&addr) {
                Ok(r) => {
                    eprintln!("{CLI} mcp: connected to {APP} at {addr}");
                    Box::new(r)
                }
                Err(e) => {
                    eprintln!("{CLI} mcp: {APP} is not reachable at {addr} yet ({e}); will retry on each call");
                    Box::new(Remote::lazy(&addr))
                }
            }
        }
        None => {
            let mut h = match &library {
                Some(dir) => {
                    let mut h = Headless::default();
                    // the presets and profiles every library shares (the settings folder's Library Store)
                    h.session.use_default_app_store();
                    catalog_migrate::migrate_with_progress(dir)?;
                    let r = h.session.open_library(dir, demo).map_err(|e| library_error(dir, e))?;
                    eprintln!("{CLI} mcp: opened library {dir} ({r:?})");
                    library_warnings(&mut h.session, &format!("{CLI} mcp"));
                    h
                }
                None if demo => Headless::demo(),
                None => Headless::default(),
            };
            if !files.is_empty() {
                let paths = expand_paths(&files);
                let r = h.session.execute("library.import", &json!({"paths": paths})).map_err(|e| e.to_string())?;
                eprintln!("{CLI} mcp: imported {} photo(s)", r["imported"].as_array().map_or(0, Vec::len));
            }
            Box::new(h)
        }
    };
    eprintln!("{CLI} mcp: serving MCP on stdio ({})", backend.describe());
    let mut server = Server::new(backend).with_command_tools(!compact);
    server.serve_stdio(std::io::stdout()).map_err(|e| e.to_string())
}

fn merge(args: &[String]) -> Result<(), String> {
    let cmd = match args.first().map(String::as_str) {
        Some("hdr") => "merge.hdr",
        Some("panorama" | "pano") => "merge.panorama",
        Some("hdr-panorama") => "merge.hdrPanorama",
        Some("hdr-batch") => "merge.hdrBatch",
        Some("focus" | "focus-stack") => "merge.focusStack",
        _ => return Err("merge: expected hdr, panorama, hdr-panorama, hdr-batch or focus".into()),
    };
    let mut p = serde_json::Map::new();
    let mut files = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--deghost" => {
                p.insert("deghost".into(), json!(take_value(args, &mut i, "--deghost")?));
            }
            "--no-align" => {
                p.insert("align".into(), json!(false));
            }
            "--bracket" => {
                p.insert("bracket".into(), json!(take_value(args, &mut i, "--bracket")?.parse::<u64>().map_err(|e| e.to_string())?));
            }
            "--projection" => {
                p.insert("projection".into(), json!(take_value(args, &mut i, "--projection")?));
            }
            "--boundary-warp" => {
                p.insert("boundaryWarp".into(), json!(take_value(args, &mut i, "--boundary-warp")?.parse::<f64>().map_err(|e| e.to_string())?));
            }
            "--auto-crop" => {
                p.insert("autoCrop".into(), json!(true));
            }
            "--fill-edges" => {
                p.insert("fillEdges".into(), json!(true));
            }
            "--no-auto-settings" => {
                p.insert("autoSettings".into(), json!(false));
            }
            "--preview" => {
                p.insert("preview".into(), json!(true));
                p.insert("previewPath".into(), json!(take_value(args, &mut i, "--preview")?));
                p.insert("showOverlay".into(), json!(true));
            }
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    let mut s = Session::new().with_fs().with_default_denoise_models().with_default_genfill().with_system_clock().with_default_connections();
    let paths = expand_paths(&files);
    let r = s.execute("library.import", &json!({"paths": paths})).map_err(|e| e.to_string())?;
    let mut ids: Vec<u64> = r["imported"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    // keep the command line's order
    ids.sort_by_key(|id| {
        paths.iter().position(|f| {
            s.catalog
                .photo(lcc_engine::catalog::PhotoId(*id))
                .is_some_and(|ph| matches!(&ph.source, lcc_engine::catalog::Source::File { path } if path == f))
        })
    });
    p.insert("ids".into(), json!(ids));
    let t0 = std::time::Instant::now();
    let mut out = s.execute(cmd, &Value::Object(p)).map_err(|e| e.to_string())?;
    out["seconds"] = json!(t0.elapsed().as_secs_f64());
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(())
}

fn synth_merge(args: &[String]) -> Result<(), String> {
    let kind = args.first().map(String::as_str).unwrap_or("");
    let mut dir = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => dir = Some(take_value(args, &mut i, "-o")?.to_string()),
            a => return Err(format!("unknown option `{a}`")),
        }
        i += 1;
    }
    let dir = dir.ok_or("synth-merge needs -o DIR")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    match kind {
        "hdr" => {
            for (k, b) in lcc_merge::synth::bracket_dngs(1800, 1200, &[-2.0, 0.0, 2.0]).map_err(|e| e.to_string())?.into_iter().enumerate() {
                let p = Path::new(&dir).join(format!("bracket-{k}.dng"));
                std::fs::write(&p, b).map_err(|e| e.to_string())?;
                written.push(p);
            }
        }
        "panorama" | "pano" => {
            for (k, v) in
                lcc_merge::synth::pano_views(1200, 900, 1000.0, &[-50.0, -25.0, 0.0, 25.0, 50.0]).map_err(|e| e.to_string())?.into_iter().enumerate()
            {
                let p = Path::new(&dir).join(format!("view-{k}.png"));
                let img = v.to_srgb8();
                let png =
                    lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).map_err(|e| e.to_string())?;
                std::fs::write(&p, png).map_err(|e| e.to_string())?;
                written.push(p);
            }
        }
        "focus" | "focus-stack" => {
            let (_, frames) = lcc_merge::synth::focus_bracket(1800, 1200, 8).map_err(|e| e.to_string())?;
            for (k, v) in frames.into_iter().enumerate() {
                let p = Path::new(&dir).join(format!("focus-{k}.png"));
                let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&v.to_srgb8()), &lcc_codecs::EncodeMeta::default())
                    .map_err(|e| e.to_string())?;
                std::fs::write(&p, png).map_err(|e| e.to_string())?;
                written.push(p);
            }
        }
        _ => return Err("synth-merge: expected hdr, panorama or focus".into()),
    }
    for p in written {
        println!("{}", p.display());
    }
    Ok(())
}

/// One step of `<cli> run`: a command id (or control-protocol method) and its params.
#[derive(Debug, PartialEq)]
struct Step {
    id: String,
    params: Value,
}

/// Parse `CMD [key=value | {json}]… CMD …` into steps. A token without `=` that doesn't start
/// with `{` begins a new command; values are JSON when they parse, else strings.
fn parse_steps(tokens: &[String]) -> Result<Vec<Step>, String> {
    let mut steps: Vec<Step> = Vec::new();
    for t in tokens {
        if t.starts_with('{') {
            let v: Value = serde_json::from_str(t).map_err(|e| format!("bad JSON params `{t}`: {e}"))?;
            let step = steps.last_mut().ok_or_else(|| format!("params `{t}` before any command"))?;
            match (step.params.as_object_mut(), v) {
                (Some(o), Value::Object(m)) => o.extend(m),
                _ => return Err(format!("params `{t}` must be a JSON object")),
            }
        } else if let Some((k, v)) = t.split_once('=') {
            let step = steps.last_mut().ok_or_else(|| format!("param `{t}` before any command"))?;
            let v = serde_json::from_str(v).unwrap_or_else(|_| json!(v));
            step.params[k.trim()] = v;
        } else {
            steps.push(Step { id: t.clone(), params: json!({}) });
        }
    }
    Ok(steps)
}

/// Run one step: control-protocol methods (`ui.*`, `engine.*`) directly, everything else as a
/// command through `engine.execute`.
fn run_step(b: &mut dyn Backend, s: &Step) -> Result<Value, String> {
    let direct = s.id.starts_with("ui.") || s.id.starts_with("engine.") || s.id == "app.quit";
    if direct { b.call(&s.id, s.params.clone()) } else { b.call("engine.execute", json!({"command": s.id, "params": s.params})) }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut connect: Option<String> = None;
    let mut demo = false;
    let mut library: Option<String> = None;
    let mut imports = Vec::new();
    let mut script: Option<String> = None;
    let mut keep_going = false;
    let mut shared_settings = false;
    let mut i = 0;
    while i < args.len() && args[i].starts_with("--") {
        match args[i].as_str() {
            "--" => {
                i += 1;
                break;
            }
            "--connect" => match args.get(i + 1).filter(|a| !a.contains('=') && (a.contains(':') || a.chars().all(|c| c.is_ascii_digit()))) {
                Some(a) => {
                    connect = Some(if a.contains(':') { a.clone() } else { format!("127.0.0.1:{a}") });
                    i += 1;
                }
                None => connect = Some(DEFAULT_ADDR.to_string()),
            },
            a if a.starts_with("--connect=") => connect = Some(a["--connect=".len()..].to_string()),
            "--demo" => demo = true,
            "--library" => library = Some(library_arg(take_value(args, &mut i, "--library")?)?),
            "--import" => imports.push(take_value(args, &mut i, "--import")?.to_string()),
            "--script" => script = Some(take_value(args, &mut i, "--script")?.to_string()),
            "--keep-going" => keep_going = true,
            "--shared-settings" => shared_settings = true,
            a => return Err(format!("unknown option `{a}`")),
        }
        i += 1;
    }
    let mut steps = parse_steps(&args[i..])?;
    if let Some(path) = &script {
        let text = if path == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
            s
        } else {
            std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?
        };
        // Windows PowerShell 5.1 writes a UTF-8 BOM; it is not part of the first JSON line.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
            let v: Value = serde_json::from_str(line).map_err(|e| format!("{path}:{}: {e}", n + 1))?;
            // `{"command": id, "params": …}` or a control request `{"method": m, "params": …}`
            let id = v
                .get("command")
                .or(v.get("method"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{path}:{}: needs `command` or `method`", n + 1))?;
            // a `params` that isn't an object is a mistake in the script: refuse it before any command runs
            let params = match v.get("params") {
                None | Some(Value::Null) => json!({}),
                Some(Value::Object(params)) => Value::Object(params.clone()),
                Some(_) => return Err(format!("{path}:{}: `params` must be a JSON object", n + 1)),
            };
            steps.push(Step { id: id.to_string(), params });
        }
    }
    if steps.is_empty() {
        return Err(format!("run: no command given (see `{CLI} commands`)"));
    }
    let mut backend: Box<dyn Backend> = match connect {
        Some(addr) => {
            if demo || library.is_some() || !imports.is_empty() || shared_settings {
                return Err("--demo, --library, --shared-settings and --import apply to headless mode only".into());
            }
            Box::new(Remote::connect(&addr).map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => format!("{APP} at {addr} refused the connection: {e}"),
                _ => format!("{APP} is not reachable at {addr} ({e}); start it with `{BIN} --control {}`", connect_port(&addr)),
            })?)
        }
        None => {
            let mut h = match &library {
                Some(dir) => {
                    let mut h = Headless::default();
                    h.session.use_default_app_store();
                    catalog_migrate::migrate_with_progress(dir)?;
                    h.session.open_library(dir, demo).map_err(|e| library_error(dir, e))?;
                    library_warnings(&mut h.session, "lcc-cli");
                    h
                }
                None if demo => Headless::demo(),
                None if shared_settings => {
                    // the shared presets and profiles without a library (`store.*`, presets)
                    let mut h = Headless::default();
                    h.session.use_default_app_store();
                    library_warnings(&mut h.session, "lcc-cli");
                    h
                }
                None => Headless::default(),
            };
            if !imports.is_empty() {
                let paths = expand_paths(&imports);
                let r = h.session.execute("library.import", &json!({"paths": paths})).map_err(|e| e.to_string())?;
                // what follows acts on the imported photos (already-known files are reported as duplicates)
                let mut ids: Vec<Value> = r["imported"].as_array().cloned().unwrap_or_default();
                ids.extend(r["duplicates"].as_array().into_iter().flatten().filter_map(|d| d.get("existing").filter(|v| v.is_u64()).cloned()));
                if let Some(first) = ids.first().cloned() {
                    h.session.execute("library.select", &json!({"ids": ids, "active": first})).map_err(|e| e.to_string())?;
                }
            }
            // the user at a shell may name the program Edit In starts; `mcp` may not
            h.trusted = true;
            Box::new(h)
        }
    };
    let mut out = std::io::stdout().lock();
    let mut failed = 0;
    for s in &steps {
        let t = std::time::Instant::now();
        let line = match run_step(backend.as_mut(), s) {
            Ok(r) => json!({"command": s.id, "ok": true, "result": r, "ms": (t.elapsed().as_secs_f64() * 1e4).round() / 10.0}),
            Err(e) => {
                failed += 1;
                json!({"command": s.id, "ok": false, "error": e})
            }
        };
        writeln!(out, "{line}").map_err(|e| e.to_string())?;
        if failed > 0 && !keep_going {
            break;
        }
    }
    drop(backend); // a library session is flushed here
    if failed > 0 { Err(format!("{failed} command(s) failed")) } else { Ok(()) }
}

fn take_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i).map(String::as_str).ok_or_else(|| format!("{flag} needs a value"))
}

/// The port an `--connect` address names, for the recovery hint. The whole address when it has
/// no port, so a hint is never built from a guess.
fn connect_port(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(_, port)| port)
}

/// What `render --settings FILE` applies: a partial develop-settings object, or the one preset of
/// a preset file (`.lcpreset` from `preset.export`, XMP), applied as the app applies it. Merging a
/// `.lcpreset` as settings dropped its wrapper and applied nothing (#1730).
enum SettingsFile {
    Partial(Value),
    Preset(lcc_engine::develop::Preset),
}

fn read_settings(p: &str) -> Result<SettingsFile, String> {
    use lcc_engine::presets::{LCPRESET_EXT, parse_preset_file};
    let bytes = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
    let ext = Path::new(p).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    // Windows text editors and PowerShell can put a UTF-8 BOM before the settings object.
    let json = serde_json::from_slice::<Value>(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes));
    // a settings object never has a top-level `presets` or `settings` key; a preset file always does
    let preset_file =
        ext == LCPRESET_EXT || ext == "xmp" || json.as_ref().is_ok_and(|v| v.is_array() || v.get("presets").is_some() || v.get("settings").is_some());
    if preset_file {
        let mut list = parse_preset_file(p, &bytes).map_err(|e| format!("{p}: {e}"))?;
        return match (list.pop(), list.len()) {
            (Some(preset), 0) => Ok(SettingsFile::Preset(preset)),
            (None, _) => Err(format!("{p}: no presets in this file")),
            (Some(_), n) => Err(format!("{p}: {} presets in this file; --settings applies one, export it on its own", n + 1)),
        };
    }
    match json.map_err(|e| format!("{p}: {e}"))? {
        v @ Value::Object(_) => Ok(SettingsFile::Partial(v)),
        _ => Err(format!("{p}: not a develop-settings object or a preset file")),
    }
}

/// Why `library.import` (its result `r`) brought in nothing from `input`.
fn not_imported(input: &str, r: &Value) -> String {
    if r["still_copying"].as_array().is_some_and(|a| !a.is_empty()) {
        return format!("{input}: still being copied, try again shortly");
    }
    format!("{input}: not a readable photo")
}

fn render(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut values = serde_json::Map::new();
    let mut settings = None;
    let mut preset = None;
    let mut size: Option<u64> = None;
    let mut quality = 92u8;
    let mut opts = serde_json::Map::new();
    let mut trim: Option<Value> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trim" => {
                let v = take_value(args, &mut i, "--trim")?;
                let (a, b) = v.split_once(',').ok_or_else(|| format!("--trim expects IN,OUT seconds (OUT may be empty = end), got `{v}`"))?;
                let num =
                    |x: &str| x.trim().parse::<f64>().ok().filter(|t| t.is_finite()).ok_or_else(|| format!("--trim: `{x}` is not a time in seconds"));
                let mut t = json!({"in": num(a)?});
                if !b.trim().is_empty() {
                    t["out"] = json!(num(b)?);
                }
                trim = Some(t);
            }
            "-o" | "--output" => output = Some(take_value(args, &mut i, "-o")?.to_string()),
            "--set" => {
                let kv = take_value(args, &mut i, "--set")?;
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("--set expects control=value, got `{kv}`"))?;
                let v: f64 =
                    v.trim().parse().ok().filter(|n: &f64| n.is_finite()).ok_or_else(|| format!("--set {k}: `{v}` is not a finite number"))?;
                values.insert(k.trim().to_string(), json!(v));
            }
            "--settings" => settings = Some(read_settings(take_value(args, &mut i, "--settings")?)?),
            "--preset" => preset = Some(take_value(args, &mut i, "--preset")?.to_string()),
            "--size" => size = Some(take_value(args, &mut i, "--size")?.parse().map_err(|_| "--size expects a number")?),
            "--quality" => quality = take_value(args, &mut i, "--quality")?.parse().map_err(|_| "--quality expects 1..100")?,
            "--opt" => {
                let kv = take_value(args, &mut i, "--opt")?;
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("--opt expects key=value, got `{kv}`"))?;
                opts.insert(k.trim().to_string(), serde_json::from_str(v).unwrap_or_else(|_| json!(v)));
            }
            a if a.starts_with('-') => return Err(format!("unknown option `{a}`")),
            f if input.is_none() => input = Some(f.to_string()),
            f => return Err(format!("unexpected argument `{f}`")),
        }
        i += 1;
    }
    let input = input.ok_or("render: missing input file")?;
    let output = output.ok_or("render: missing -o OUTPUT")?;
    let mut s = Session::new().with_fs().with_default_denoise_models().with_default_genfill().with_system_clock().with_default_connections();
    let abs = expand_paths(std::slice::from_ref(&input));
    let r = s.execute("library.import", &json!({"paths": abs})).map_err(|e| e.to_string())?;
    let id = r["imported"][0].as_u64().ok_or_else(|| not_imported(&input, &r))?;
    let run = |s: &mut Session, cmd: &str, p: Value| s.execute(cmd, &p).map(|_| ()).map_err(|e| e.to_string());
    run(&mut s, "library.select", json!({"ids": [id], "active": id}))?;
    if let Some(p) = preset {
        run(&mut s, "preset.apply", json!({"id": p}))?;
    }
    match settings {
        Some(SettingsFile::Partial(st)) => run(&mut s, "develop.merge", json!({"settings": st}))?,
        Some(SettingsFile::Preset(mut p)) => {
            p.id = "cli.settings".into();
            s.presets.push(p);
            run(&mut s, "preset.apply", json!({"id": "cli.settings"}))?;
        }
        None => {}
    }
    if !values.is_empty() {
        run(&mut s, "develop.set", json!({"values": values}))?;
    }
    if let Some(t) = trim {
        run(&mut s, "video.setTrim", t).map_err(|e| format!("--trim: {e}"))?;
    }
    use lcc_engine::export::{ExportFormat, ExportOptions, export_photo};
    let mut p = json!({"quality": quality});
    if let Some(n) = size {
        p["longEdge"] = json!(n);
    }
    for (k, v) in opts {
        p[k] = v;
    }
    let video = if p.get("format").is_none() {
        let ext = Path::new(&output).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        let mut video = None;
        match (ExportFormat::parse(&ext), lcc_engine::export_video::VideoFormat::parse(&ext)) {
            // given before validating, so `bitDepth` is checked against the format written (issue #1952)
            (Some(_), _) => p["format"] = json!(ext),
            // a movie (.mp4 / .mov): a video clip exports as one; the format stays a rendered one
            (None, Some(v)) if p.get("videoFormat").is_none() => video = Some(v),
            (None, Some(_)) => {}
            (None, None) => return Err(format!("{output}: unknown extension (use .jpg .png .tif .webp .avif .jxl .psd .dng .mp4 .mov or --opt format=…)")),
        }
        video
    } else {
        None
    };
    let mut o = ExportOptions::from_params(&p).map_err(|e| e.to_string())?;
    if let Some(v) = video {
        o.video_format = v;
    }
    // Add to Apple Photos: refused before anything is written when it can't be done
    let after = s.after_export(&o)?;
    let e = export_photo(&mut s, lcc_engine::catalog::PhotoId(id), &o, 1)?;
    // never over the input (or its sidecar), however it is spelled: `render IMG.jpg -o IMG.jpg`
    let sidecars: Vec<(String, &Vec<u8>)> =
        e.sidecars.iter().map(|(ext, bytes)| (Path::new(&output).with_extension(ext).to_string_lossy().to_string(), bytes)).collect();
    let guard = s.original_guard();
    for p in std::iter::once(&output).chain(sidecars.iter().map(|(p, _)| p)) {
        guard.check(Path::new(p)).map_err(|err| format!("render: {err}"))?;
    }
    e.save(&output)?;
    for (sc, bytes) in &sidecars {
        lcc_engine::export::write_file(sc, bytes)?;
        eprintln!("{CLI}: wrote {sc}");
    }
    let limit =
        e.limit_quality.filter(|q| *q < o.quality).zip(o.limit_kb).map(|(q, kb)| format!(", JPEG quality {q} to fit {kb} KB")).unwrap_or_default();
    eprintln!("{CLI}: wrote {output} ({}×{}{limit})", e.width, e.height);
    if let Some(photos) = after.run(&[json!({"path": output})], false) {
        if let Some(err) = photos.get("error").and_then(Value::as_str) {
            return Err(format!("render: {output} was written, but {err}"));
        }
        eprintln!("{CLI}: added to Apple Photos: {photos}");
    }
    Ok(())
}

fn snapshot(args: &[String]) -> Result<(), String> {
    use lcc_ui_egui::headless::Headless;
    use std::time::{Duration, Instant};
    let mut library: Option<String> = None;
    let mut script: Option<String> = None;
    let mut output: Option<String> = None;
    let mut size = [1600.0f32, 1000.0];
    let mut scale = 1.0f32;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--demo" => {}
            "--library" => library = Some(library_arg(take_value(args, &mut i, "--library")?)?),
            "--script" => script = Some(take_value(args, &mut i, "--script")?.to_string()),
            "-o" | "--output" => output = Some(take_value(args, &mut i, "-o")?.to_string()),
            "--size" => {
                let v = take_value(args, &mut i, "--size")?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size expects WxH, e.g. 1600x1000")?;
                size = [w.trim().parse().map_err(|_| "--size: bad width")?, h.trim().parse().map_err(|_| "--size: bad height")?];
            }
            "--scale" => scale = take_value(args, &mut i, "--scale")?.parse().map_err(|_| "--scale expects a number")?,
            a if a.starts_with('-') => return Err(format!("unknown option `{a}`")),
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    if script.is_none() && output.is_none() {
        return Err("snapshot: give -o OUT.png and/or --script FILE".into());
    }
    lcc_ui_egui::headless::viewport_pixels(size, scale).map_err(|e| format!("snapshot: bad --size/--scale: {e}"))?;
    let t0 = Instant::now();
    let mut session = match &library {
        Some(dir) => {
            let mut s = Session::new()
                .with_fs()
                .with_default_denoise_models()
                .with_default_genfill()
                .with_system_clock()
                .with_default_connections()
                .with_default_face_models()
                .with_default_lens_profiles()
                .with_default_sam3_model()
                .with_default_slide_templates()
                .with_default_app_store();
            catalog_migrate::migrate_with_progress(dir)?;
            s.open_library(dir, false).map_err(|e| library_error(dir, e))?;
            s
        }
        None if files.is_empty() => Session::with_demo()
            .with_fs()
            .with_default_denoise_models()
            .with_default_genfill()
            .with_system_clock()
            .with_default_connections()
            .with_default_face_models()
            .with_default_lens_profiles()
            .with_default_sam3_model()
            .with_default_slide_templates(),
        None => Session::new()
            .with_fs()
            .with_default_denoise_models()
            .with_default_genfill()
            .with_system_clock()
            .with_default_connections()
            .with_default_face_models()
            .with_default_lens_profiles()
            .with_default_sam3_model()
            .with_default_slide_templates(),
    };
    // Apply compute policy before imports or the first scripted query can discover an adapter.
    session.execute("app.gpu", &json!({"enabled": false})).map_err(|e| e.to_string())?;
    if !files.is_empty() {
        let ti = Instant::now();
        let r = session.execute("library.import", &json!({"paths": expand_paths(&files)})).map_err(|e| e.to_string())?;
        let n = r["imported"].as_array().map_or(0, Vec::len);
        eprintln!("{CLI} snapshot: imported {n} files in {:.0} ms", ti.elapsed().as_secs_f64() * 1e3);
    }
    let services = lcc_ui_egui::Services {
        write_shared: Some(std::sync::Arc::new(lcc_engine::export::write_file)),
        write: Some(Box::new(lcc_engine::export::write_file)),
        png: Some(Box::new(|img: &lcc_raster::Rgba8| {
            lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        ..Default::default()
    };
    let mut app = lcc_ui_egui::LccApp::new(session, services);
    // Snapshot sessions promise no GPU: disable photo compute as well as the compositor
    // before the first frame can start background adapter discovery.
    // This UI state is session-local: the CLI neither loads nor saves desktop ui.json.
    app.ui.settings.gpu = false;
    // as the desktop app opens it: an empty library on the grid, where Import is offered
    lcc_ui_egui::panels::empty_library::open_on_empty_library(&mut app);
    let mut h = Headless::new(app, size, scale);
    let timeout = Duration::from_secs(60);
    let mut shots = 0usize;
    let next_path = |shots: &mut usize| -> Option<String> {
        let out = output.as_deref()?;
        *shots += 1;
        if *shots == 1 {
            return Some(out.to_string());
        }
        let p = Path::new(out);
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "png".into());
        Some(p.with_file_name(format!("{stem}-{shots}.{ext}")).to_string_lossy().to_string())
    };
    let mut wrote_output = false;
    // Issue #166: a scripted request that fails still gets its reply printed and the run carries
    // on (like `run --keep-going`), but the exit status reports it — CI judges by exit status.
    let mut failed = 0usize;
    if let Some(script) = &script {
        // `--script -` reads stdin a line at a time and answers each request as it arrives (flushed),
        // so a driver can hold a conversation with a real headless session (the SIL harness,
        // docs/testing-sil.md); EOF ends the run like the end of a script file.
        let lines: Box<dyn Iterator<Item = std::io::Result<String>>> = if script == "-" {
            Box::new(std::io::stdin().lines())
        } else {
            let text = std::fs::read_to_string(script).map_err(|e| format!("{script}: {e}"))?;
            // Windows PowerShell 5.1 writes a UTF-8 BOM; it is not part of the first JSON line.
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            Box::new(text.lines().map(|l| Ok(l.to_string())).collect::<Vec<_>>().into_iter())
        };
        let mut out = std::io::stdout().lock();
        for (n, line) in lines.enumerate() {
            let line = line.map_err(|e| format!("{script}: {e}"))?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let msg: Value = serde_json::from_str(line).map_err(|e| format!("{script}:{}: {e}", n + 1))?;
            let id = msg.get("id").cloned().unwrap_or(json!(n + 1));
            let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
            let mut params = msg.get("params").cloned().unwrap_or(json!({}));
            let ts = Instant::now();
            let mut reply = match method.as_str() {
                "ui.settle" => {
                    let ms = params.get("timeoutMs").and_then(Value::as_u64).unwrap_or(20_000);
                    json!({"ok": true, "result": {"settled": h.settle(Duration::from_millis(ms))}})
                }
                "ui.screenshot" => {
                    if params.get("path").and_then(Value::as_str).is_none() {
                        match next_path(&mut shots) {
                            Some(p) => {
                                wrote_output = true;
                                // `params` is untrusted script JSON: `params["path"] = …` panics on
                                // an array or a string. Only an object (or null, like an absent
                                // `params`) may take the path.
                                if params.is_null() {
                                    params = json!({});
                                }
                                if let Some(o) = params.as_object_mut() {
                                    o.insert("path".into(), json!(p));
                                } else {
                                    return Err(format!("{script}:{}: ui.screenshot: `params` must be a JSON object", n + 1));
                                }
                            }
                            None => return Err(format!("{script}:{}: ui.screenshot needs a `path` (or give -o)", n + 1)),
                        }
                    }
                    h.request(&method, params, timeout)
                }
                _ => h.request(&method, params, timeout),
            };
            if reply["ok"] != true {
                failed += 1;
            }
            if let Some(o) = reply.as_object_mut() {
                o.insert("id".into(), id);
                // wall time of the request (incl. the frames it ran), and since the start
                o.insert("ms".into(), json!((ts.elapsed().as_secs_f64() * 1e4).round() / 10.0));
                o.insert("t".into(), json!((t0.elapsed().as_secs_f64() * 1e4).round() / 10.0));
            }
            writeln!(out, "{reply}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            if method == "ui.screenshot" {
                eprintln!("{CLI} snapshot: {} ({:.0} ms)", reply["result"]["path"].as_str().unwrap_or("?"), ts.elapsed().as_secs_f64() * 1000.0);
            }
            if h.quit_requested() {
                break;
            }
        }
    }
    if !wrote_output && let Some(path) = next_path(&mut shots) {
        let r = h.request("ui.screenshot", json!({"path": path}), timeout);
        if r["ok"] != true {
            return Err(format!("screenshot failed: {}", r["error"]));
        }
        eprintln!("{CLI} snapshot: wrote {path} ({}×{})", r["result"]["width"], r["result"]["height"]);
    }
    // a background export started by the script finishes before we exit (its files would be cut off)
    if h.app.export.is_some() {
        eprintln!("{CLI} snapshot: waiting for the background export");
        let te = std::time::Instant::now();
        while h.app.export.is_some() && te.elapsed() < std::time::Duration::from_secs(3600) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    eprintln!("{CLI} snapshot: done in {:.2} s ({} frames)", t0.elapsed().as_secs_f64(), h.frames());
    if failed > 0 { Err(format!("{failed} scripted request(s) failed")) } else { Ok(()) }
}

fn commands(args: &[String]) -> Result<(), String> {
    if let Some(arg) = args.iter().find(|arg| arg.as_str() != "--json") {
        return Err(format!("commands: unexpected argument `{arg}` (expected only --json)"));
    }
    let mut h = Headless::demo();
    let mut cmds = h.call("engine.commands", json!({}))?;
    // Listed by id so each family (`photo.*`, `mask.*`, …) reads together; the registry keeps its own order.
    if let Some(list) = cmds.as_array_mut() {
        list.sort_by(|a, b| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")));
    }
    let mut out = std::io::stdout().lock();
    if args.iter().any(|a| a == "--json") {
        writeln!(out, "{}", serde_json::to_string_pretty(&cmds).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(());
    }
    for c in cmds.as_array().into_iter().flatten() {
        let sc = c["shortcut"].as_str().map(|s| format!("  [{s}]")).unwrap_or_default();
        writeln!(
            out,
            "{:<28} {}{sc}\n{:<28} params: {}",
            c["id"].as_str().unwrap_or(""),
            c["label"].as_str().unwrap_or(""),
            "",
            c["params"].as_str().unwrap_or("")
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn controls(args: &[String]) -> Result<(), String> {
    if let Some(arg) = args.iter().find(|arg| arg.as_str() != "--json") {
        return Err(format!("controls: unexpected argument `{arg}` (expected only --json)"));
    }
    let mut s = Session::new();
    let v = s.execute("develop.controls", &json!({})).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout().lock();
    if args.iter().any(|a| a == "--json") {
        writeln!(out, "{}", serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(());
    }
    for c in v.as_array().into_iter().flatten() {
        writeln!(
            out,
            "{:<28} {:<22} {} .. {} (default {})",
            c["id"].as_str().unwrap_or(""),
            c["label"].as_str().unwrap_or(""),
            c["min"],
            c["max"],
            c["default"]
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `genfill-mock`: the generative fill providers' request shapes on 127.0.0.1, answered with
/// the app's own content-aware fill (`lcc_genfill::cloud::mock`).
fn genfill_mock(args: &[String]) -> Result<(), String> {
    let (mut port, mut key, mut fill) = (18510u16, None::<String>, "patchmatch".to_string());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => port = it.next().and_then(|v| v.parse().ok()).ok_or("genfill-mock: --port N")?,
            "--key" => key = Some(it.next().ok_or("genfill-mock: --key KEY")?.clone()),
            "--fill" => fill = it.next().ok_or("genfill-mock: --fill patchmatch|mean")?.clone(),
            other => return Err(format!("genfill-mock: unknown option `{other}`")),
        }
    }
    let filler: lcc_genfill::cloud::mock::Filler = match fill.as_str() {
        "mean" => lcc_genfill::cloud::mock::mean_filler(),
        "patchmatch" => std::sync::Arc::new(|w, h, rgb: &[u8], mask: &[u8], var| {
            let img: Vec<f32> = rgb.iter().map(|v| f32::from(*v) / 255.0).collect();
            let hole: Vec<bool> = mask.iter().map(|m| *m >= 128).collect();
            let p = lcc_inpaint::CompleteParams { seed: u64::from(var) + 1, ..Default::default() };
            let out = lcc_inpaint::complete(w, h, 3, &img, &hole, &p).unwrap_or_else(|| lcc_inpaint::membrane_fill(w, h, 3, &img, &hole));
            out.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
        }),
        other => return Err(format!("genfill-mock: --fill patchmatch|mean, not `{other}`")),
    };
    let server =
        lcc_genfill::cloud::mock::MockServer::start(port, key.as_deref(), filler).map_err(|e| format!("genfill-mock: 127.0.0.1:{port}: {e}"))?;
    eprintln!(
        "{} genfill-mock: listening on {} ({fill} fill{}); set {}={} for {}",
        lcc_brand::CLI_BINARY,
        server.base,
        if key.is_some() { ", key required" } else { "" },
        lcc_brand::env_var("GENFILL_MOCK"),
        server.base,
        lcc_brand::DISPLAY_NAME
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// `denoise-serve`: the remote half of AI Denoise (`lcc_denoise::remote`).
#[cfg(feature = "denoise")]
fn denoise_serve(args: &[String]) -> Result<(), String> {
    use lcc_denoise::remote::{DEFAULT_PORT, ServerOptions, serve};
    use lcc_engine::denoise::RunOn;
    let mut model: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut listen = format!("127.0.0.1:{DEFAULT_PORT}");
    let mut token_env = lcc_brand::env_var("DENOISE_TOKEN");
    let mut run_on = RunOn::Auto;
    let mut max_connections = 8usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => model = Some(take_value(args, &mut i, "--model")?.to_string()),
            "--models" => dir = Some(take_value(args, &mut i, "--models")?.to_string()),
            "--listen" => listen = take_value(args, &mut i, "--listen")?.to_string(),
            "--token-env" => token_env = take_value(args, &mut i, "--token-env")?.to_string(),
            "--run-on" => {
                let v = take_value(args, &mut i, "--run-on")?;
                run_on = RunOn::parse(v).ok_or_else(|| format!("--run-on expects auto, gpu or cpu, not `{v}`"))?;
            }
            "--max-connections" => {
                max_connections = take_value(args, &mut i, "--max-connections")?.parse().map_err(|_| "--max-connections expects a number")?
            }
            other => return Err(format!("denoise-serve: unknown option `{other}`")),
        }
        i += 1;
    }
    let token = std::env::var(&token_env).map_err(|_| format!("denoise-serve: set the shared token in ${token_env}"))?;
    let s = lcc_engine::denoise::serving(dir.as_deref().map(Path::new), model.as_deref(), run_on)?;
    let listener = std::net::TcpListener::bind(&listen).map_err(|e| format!("denoise-serve: {listen}: {e}"))?;
    eprintln!("{CLI} denoise-serve: {} {} ({}) on {listen} via {}", s.model.id, s.model.version, s.model.sha256, s.device);
    if listen.starts_with("0.0.0.0") || listen.starts_with("[::]") {
        eprintln!("{CLI} denoise-serve: listening on every interface; firewall the port to your VPN/LAN (the protocol is not encrypted)");
    }
    let opts = ServerOptions { token, max_connections, ..Default::default() };
    serve(listener, s.model, s.tile, s.device, s.run, opts, std::sync::Arc::new(|m: &str| eprintln!("{CLI} denoise-serve: {m}")))
}

#[cfg(not(feature = "denoise"))]
fn denoise_serve(_: &[String]) -> Result<(), String> {
    Err("denoise-serve: this build has no denoise support (build with the `denoise` feature)".into())
}

#[cfg(test)]
mod tests {
    /// Settings saved with a UTF-8 BOM keep the same values as an ordinary JSON file.
    #[test]
    fn render_settings_accepts_a_utf8_bom() {
        use std::io::{Seek, Write};

        let path = std::env::temp_dir().join(format!("cli-settings-bom-{}.json", std::process::id()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        let settings = serde_json::json!({"light": {"exposure": 0.7}, "profile": {"id": "\u{feff}kept"}});
        for prefix in ["", "\u{feff}"] {
            file.set_len(0).unwrap();
            file.rewind().unwrap();
            write!(file, "{prefix}{settings}").unwrap();
            let super::SettingsFile::Partial(actual) = super::read_settings(path.to_str().unwrap()).unwrap() else {
                panic!("partial settings were read as a preset");
            };
            assert_eq!(actual, settings);
        }
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    use super::*;

    // `--library @app` finds the library the app opens: its saved Settings → Open Library…
    // choice, unless the environment names one, else the default; anything else is passed through
    #[test]
    fn library_at_app_reads_the_apps_saved_library() {
        let d = std::env::temp_dir().join(format!("cli-app-library-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let default = Some(std::path::PathBuf::from("/home/me/Pictures/Library"));
        let def = default.clone().map(|p| p.to_string_lossy().into_owned());
        assert_eq!(app_library_with(Some(d.clone()), false, default.clone()), def, "no settings yet");
        std::fs::write(d.join("ui.json"), br#"{"settings": {"libraryPath": "/Volumes/Photos/Lib"}}"#).unwrap();
        assert_eq!(app_library_with(Some(d.clone()), false, default.clone()).as_deref(), Some("/Volumes/Photos/Lib"));
        assert_eq!(app_library_with(Some(d.clone()), true, default.clone()), def, "the environment wins, as in the app");
        std::fs::write(d.join("ui.json"), br#"{"settings": {"libraryPath": ""}}"#).unwrap();
        assert_eq!(app_library_with(Some(d.clone()), false, default.clone()), def);
        std::fs::write(d.join("ui.json"), b"{not json").unwrap();
        assert_eq!(app_library_with(Some(d.clone()), false, default.clone()), def);
        assert_eq!(app_library_with(None, false, None), None);
        assert_eq!(library_arg("/some/dir").unwrap(), "/some/dir");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(all(test, unix))]
mod raw_files_tests {
    use super::raw_files;

    /// Links back to their own folders are entered once, not once per path that reaches them: a
    /// handful of such links made the old walk list the same folders exponentially often.
    #[test]
    fn folder_links_back_up_are_walked_once() {
        let dir = std::env::temp_dir().join(format!("cli-raw-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("a");
        std::fs::create_dir_all(a.join("@eaDir")).unwrap();
        std::fs::write(a.join("one.NEF"), b"").unwrap();
        std::fs::write(a.join("@eaDir").join("thumb.nef"), b"").unwrap();
        std::fs::write(a.join("notes.txt"), b"").unwrap();
        for i in 0..8 {
            std::os::unix::fs::symlink(&dir, a.join(format!("up{i}"))).unwrap();
        }
        let started = std::time::Instant::now();
        let mut files = Vec::new();
        raw_files(&dir, &mut files);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "walk took {:?}", started.elapsed());
        assert_eq!(files, vec![a.join("one.NEF")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
