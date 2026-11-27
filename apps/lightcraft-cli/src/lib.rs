//! `lightcraft-cli`: headless LightCraft.
//!
//! ```text
//! lightcraft-cli run [--demo | --library DIR | --connect [ADDR]] [--import PATH]… CMD [key=value…]…
//! lightcraft-cli mcp [--connect [ADDR]] [--demo] [--compact] [FILES/FOLDERS…]
//! lightcraft-cli render <in> -o <out> [--set control=value]… [--settings FILE.json|FILE.lcpreset] [--preset ID] [--size N] [--quality Q]
//! lightcraft-cli snapshot [--library DIR | --demo] [--script FILE.jsonl] [-o OUT.png] [--size WxH] [--scale S] [FILES…]
//! lightcraft-cli merge hdr|panorama|hdr-panorama|hdr-batch [OPTIONS] FILES…
//! lightcraft-cli synth-merge hdr|panorama -o DIR
//! lightcraft-cli denoise-serve [--model ID] [--models DIR] [--listen ADDR] [--token-env VAR] [--run-on auto|gpu|cpu]
//! lightcraft-cli commands [--json]
//! lightcraft-cli controls [--json]
//! lightcraft-cli calibrate [--max N] [--out DIR] FOLDERS/FILES…
//! lightcraft-cli bench-cull FOLDER [--files K] [--n N] [--copy M] [--work DIR] [--json FILE]
//! ```
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[macro_use]
mod io;
pub mod alloc_release;
mod args;
mod bench_cull;
#[cfg(test)]
mod cli_tests;
mod error;

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, ValueEnum};

use args::{
    CalibrateArgs, Cli, Command, ControlValue, DenoiseServeArgs, ExportOption, ListArgs, McpArgs, MergeArgs, RenderArgs, RunArgs, Size, SnapshotArgs,
    SynthKind, SynthMergeArgs,
};
use error::CliError;
use io::Io;
use lightcraft_engine::Session;
use lightcraft_mcp::{Backend, Headless, Remote, Server, expand_paths};
use serde_json::{Value, json};

/// Why `--library DIR` can't be opened; for a library open in another program, how to work with
/// that one instead (issue #99).
/// Print the open library's warnings (an unlockable library, damaged settings files) on stderr:
/// stdout carries command results / MCP protocol messages only.
fn library_warnings(io: &mut Io, session: &mut Session, who: &str) {
    for w in session.take_library_warnings() {
        errln!(io, "{who}: warning: {w}");
    }
}

fn library_error(dir: &str, e: lightcraft_engine::EngineError) -> String {
    match e {
        lightcraft_engine::EngineError::LibraryInUse(why) => format!(
            "{dir}: {why}\nTo work with the library while the app has it open, start the app with `--control PORT` and use `lightcraft-cli mcp --connect 127.0.0.1:PORT`."
        ),
        e @ lightcraft_engine::EngineError::LightroomCatalog { .. } => format!(
            "{e}\nOpen a LightCraft library with `--library DIR` (an empty folder makes a new one) and run `library.importLightroom` with the catalog's path."
        ),
        e => format!("{dir}: {e}"),
    }
}

/// The desktop GUI's Launcher protocol is not a headless option: a CLI invoked with it (a shell
/// completing `lightcraft` to `lightcraft-cli`, say) must say so rather than import the profile path
/// as a folder. Checked before the arguments are otherwise read.
fn launch_profile_refusal(args: impl Iterator<Item = String>) -> Option<String> {
    for arg in args {
        // (`--` ends the options: a file after it may be named like one of these.)
        if arg == "--" {
            break;
        }
        if arg == "--launch-profile" || arg == "--profile-root" || arg.starts_with("--launch-profile=") || arg.starts_with("--profile-root=") {
            return Some(format!("lightcraft-cli: `{arg}` belongs to the LightCraft desktop app, not the headless CLI"));
        }
    }
    None
}

/// The clap command, for the man page and shell completions.
pub fn command() -> clap::Command {
    Cli::command()
}

/// Run the CLI with these arguments (the first is the program name) and return the exit code:
/// 0 on success, 2 for a usage error, 1 for any other failure.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if let Some(refusal) = launch_profile_refusal(args.iter().skip(1).map(|a| a.to_string_lossy().into_owned())) {
        let _ = writeln!(err, "{refusal}");
        return 2;
    }
    let cli = match Cli::try_parse_from(attach_connect_addresses(args)) {
        Ok(cli) => cli,
        Err(e) => {
            let shown = e.render().to_string();
            let _ = if e.use_stderr() { write!(err, "{shown}") } else { write!(out, "{shown}") };
            return e.exit_code();
        }
    };
    let mut io = Io { out, err };
    match dispatch(&mut io, cli.command) {
        Ok(()) => 0,
        Err(e) => {
            match &e {
                CliError::Usage(_) | CliError::NotUnicode(_) => errln!(io, "error: {e}"),
                CliError::StepsFailed(_) | CliError::Failed(_) => errln!(io, "lightcraft-cli: {e}"),
            }
            e.exit_code()
        }
    }
}

fn dispatch(io: &mut Io, command: Command) -> Result<(), CliError> {
    match command {
        Command::Run(a) => run_commands(io, a),
        Command::Mcp(a) => mcp(io, a),
        Command::Render(a) => render(io, a),
        Command::Snapshot(a) => snapshot(io, a),
        Command::Merge(a) => merge(io, a),
        Command::SynthMerge(a) => synth_merge(io, a),
        Command::DenoiseServe(a) => denoise_serve(io, a),
        Command::Commands(a) => commands(io, a),
        Command::Controls(a) => controls(io, a),
        Command::Calibrate(a) => calibrate(io, a),
        Command::BenchCull(a) => bench_cull::run(io, a),
    }
}

/// `--connect ADDR` and `--connect PORT` keep meaning `--connect=ADDR` (#1178): a word after a bare
/// `--connect` that is an address (it has a `:`) or a port (all digits) belongs to the flag, any other word (a
/// command id such as `ui.inspect`) is not touched.
fn attach_connect_addresses(args: Vec<OsString>) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::with_capacity(args.len());
    let mut rest = args.into_iter().peekable();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            out.push(arg);
            out.extend(rest);
            break;
        }
        let address = arg == "--connect"
            && rest
                .peek()
                .and_then(|next| next.to_str())
                .is_some_and(|next| !next.contains('=') && (next.contains(':') || next.bytes().all(|b| b.is_ascii_digit())) && !next.is_empty());
        if address && let Some(next) = rest.next() {
            let mut joined = OsString::from("--connect=");
            joined.push(next);
            out.push(joined);
        } else {
            out.push(arg);
        }
    }
    out
}

/// An argument as text, for the engine calls that take strings.
fn text(path: &Path) -> Result<String, CliError> {
    path.to_str().map(str::to_string).ok_or_else(|| CliError::NotUnicode(path.to_path_buf()))
}

fn texts(paths: &[PathBuf]) -> Result<Vec<String>, CliError> {
    paths.iter().map(|p| text(p)).collect()
}

/// Raw files below `path` (or `path` itself), skipping hidden and NAS metadata folders.
///
/// Folders go through the engine's bounded walk ([`lightcraft_engine::walk::files_in`]): it enters
/// a folder reached through a link once at most and stops at a depth and an entry budget. A
/// folder of links pointing back at their parents made the old recursive walk re-read the same
/// folders exponentially often before its depth limit cut it off (CWE-400).
fn raw_files(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    let is_raw = |p: &Path| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["arw", "nef", "nrw", "raf", "crw"].iter().any(|x| e.eq_ignore_ascii_case(x)))
    };
    if path.is_file() {
        if is_raw(path) {
            out.push(path.to_path_buf());
        }
        return;
    }
    // (hidden entries are left out by the walk; `@eaDir` and other NAS metadata folders and files
    // here, folders before they are listed so their entries do not use up the walk's budget)
    let nas_metadata = |p: &Path| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('@'));
    let walked = lightcraft_engine::walk::files_in_entering(
        path,
        None,
        lightcraft_engine::walk::Limits::default(),
        |p| is_raw(p) && !nas_metadata(p) && p.is_file(),
        |p| !nas_metadata(p),
    );
    if walked.truncated {
        eprintln!("calibrate: {} has more folders or entries than a walk looks at; the rest is left out", path.display());
    }
    out.extend(walked.files);
}

fn calibrate(io: &mut Io, args: CalibrateArgs) -> Result<(), CliError> {
    let CalibrateArgs { max, out, inputs } = args;
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
    errln!(io, "calibrate: {} of {found} raw files", files.len());
    let dir = out.or_else(lightcraft_engine::camera_profiles::dir).ok_or("no profiles folder: pass --out DIR")?;
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 6);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let pools: Vec<lightcraft_engine::camera_profiles::Pool> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut pool = lightcraft_engine::camera_profiles::Pool::default();
                    while let Some(path) = files.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        let result = std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| pool.add(&bytes));
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        match result {
                            Ok(Some(added)) if added.colour => eprintln!("[{n}/{}] {} ({})", files.len(), path.display(), added.model),
                            Ok(Some(added)) => {
                                eprintln!("[{n}/{}] {} ({}, tone only: too dark or colourless)", files.len(), path.display(), added.model)
                            }
                            Ok(None) => eprintln!("[{n}/{}] {}: skipped (no usable or a black-and-white camera JPEG)", files.len(), path.display()),
                            Err(e) => eprintln!("[{n}/{}] {}: {e}", files.len(), path.display()),
                        }
                    }
                    pool
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut pool = lightcraft_engine::camera_profiles::Pool::default();
    for p in pools {
        pool.merge(p);
    }
    let mut written = 0;
    for result in pool.fit(5) {
        match result {
            Ok(profile) => {
                let path = lightcraft_engine::camera_profiles::save(&profile, &dir)?;
                outln!(
                    io,
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
            Err(e) => errln!(io, "calibrate: {e}"),
        }
    }
    for (model, n) in pool.files() {
        if n < 5 {
            errln!(io, "calibrate: {model}: only {n} usable photo(s), at least 5 needed");
        }
    }
    if written == 0 {
        return Err("no profile written".into());
    }
    Ok(())
}

fn mcp(io: &mut Io, args: McpArgs) -> Result<(), CliError> {
    let McpArgs { connect, headless: _, demo, library, compact, files } = args;
    let files = texts(&files)?;
    let library = library.as_deref().map(text).transpose()?;
    let backend: Box<dyn Backend> = match connect {
        Some(addr) => match Remote::connect(&addr) {
            Ok(r) => {
                errln!(io, "lightcraft-cli mcp: connected to LightCraft at {addr}");
                Box::new(r)
            }
            Err(e) => {
                errln!(io, "lightcraft-cli mcp: LightCraft is not reachable at {addr} yet ({e}); will retry on each call");
                Box::new(Remote::lazy(&addr))
            }
        },
        None => {
            let mut h = match &library {
                Some(dir) => {
                    let mut h = Headless::default();
                    let r = h.session.open_library(dir, demo).map_err(|e| library_error(dir, e))?;
                    errln!(io, "lightcraft-cli mcp: opened library {dir} ({r:?})");
                    library_warnings(io, &mut h.session, "lightcraft-cli mcp");
                    h
                }
                None if demo => Headless::demo(),
                None => Headless::default(),
            };
            if !files.is_empty() {
                let paths = expand_paths(&files);
                let r = h.session.execute("library.import", &json!({"paths": paths})).map_err(|e| e.to_string())?;
                errln!(io, "lightcraft-cli mcp: imported {} photo(s)", r["imported"].as_array().map_or(0, Vec::len));
            }
            Box::new(h)
        }
    };
    errln!(io, "lightcraft-cli mcp: serving MCP on stdio ({})", backend.describe());
    let mut server = Server::new(backend).with_command_tools(!compact);
    server.serve_stdio(std::io::stdout()).map_err(|e| CliError::Failed(e.to_string()))
}

fn merge(io: &mut Io, args: MergeArgs) -> Result<(), CliError> {
    let cmd = args.kind.command();
    let mut p = serde_json::Map::new();
    if let Some(deghost) = args.deghost {
        p.insert("deghost".into(), json!(deghost.to_possible_value().map(|v| v.get_name().to_string())));
    }
    if args.no_align {
        p.insert("align".into(), json!(false));
    }
    if let Some(bracket) = args.bracket {
        p.insert("bracket".into(), json!(bracket));
    }
    if let Some(projection) = args.projection {
        p.insert("projection".into(), json!(projection.to_possible_value().map(|v| v.get_name().to_string())));
    }
    if let Some(warp) = args.boundary_warp {
        p.insert("boundaryWarp".into(), json!(warp));
    }
    if args.auto_crop {
        p.insert("autoCrop".into(), json!(true));
    }
    if args.fill_edges {
        p.insert("fillEdges".into(), json!(true));
    }
    if args.no_auto_settings {
        p.insert("autoSettings".into(), json!(false));
    }
    if let Some(preview) = &args.preview {
        p.insert("preview".into(), json!(true));
        p.insert("previewPath".into(), json!(text(preview)?));
        p.insert("showOverlay".into(), json!(true));
    }
    let files = texts(&args.files)?;
    let mut s = file_session();
    let paths = expand_paths(&files);
    let r = s.execute("library.import", &json!({"paths": paths})).map_err(|e| e.to_string())?;
    let mut ids: Vec<u64> = r["imported"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    // keep the command line's order
    ids.sort_by_key(|id| {
        paths.iter().position(|f| {
            s.catalog
                .photo(lightcraft_engine::catalog::PhotoId(*id))
                .is_some_and(|ph| matches!(&ph.source, lightcraft_engine::catalog::Source::File { path } if path == f))
        })
    });
    p.insert("ids".into(), json!(ids));
    let t0 = std::time::Instant::now();
    let mut out = s.execute(cmd, &Value::Object(p)).map_err(|e| e.to_string())?;
    out["seconds"] = json!(t0.elapsed().as_secs_f64());
    outln!(io, "{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(())
}

fn synth_merge(io: &mut Io, args: SynthMergeArgs) -> Result<(), CliError> {
    let dir = args.output;
    let kind = args.kind;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    match kind {
        SynthKind::Hdr => {
            for (k, b) in lightcraft_merge::synth::bracket_dngs(1800, 1200, &[-2.0, 0.0, 2.0]).map_err(|e| e.to_string())?.into_iter().enumerate() {
                let p = Path::new(&dir).join(format!("bracket-{k}.dng"));
                std::fs::write(&p, b).map_err(|e| e.to_string())?;
                written.push(p);
            }
        }
        SynthKind::Panorama => {
            for (k, v) in lightcraft_merge::synth::pano_views(1200, 900, 1000.0, &[-50.0, -25.0, 0.0, 25.0, 50.0])
                .map_err(|e| e.to_string())?
                .into_iter()
                .enumerate()
            {
                let p = Path::new(&dir).join(format!("view-{k}.png"));
                let img = v.to_srgb8();
                let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default())
                    .map_err(|e| e.to_string())?;
                std::fs::write(&p, png).map_err(|e| e.to_string())?;
                written.push(p);
            }
        }
    }
    for p in written {
        outln!(io, "{}", p.display());
    }
    Ok(())
}

/// One step of `lightcraft-cli run`: a command id (or control-protocol method) and its params.
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

fn run_commands(io: &mut Io, args: RunArgs) -> Result<(), CliError> {
    let RunArgs { demo, library, imports, connect, script, keep_going, words } = args;
    let imports = texts(&imports)?;
    let library = library.as_deref().map(text).transpose()?;
    let script = script.as_deref().map(text).transpose()?;
    let mut steps = parse_steps(&words)?;
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
            let params = match v.get("params") {
                None | Some(Value::Null) => json!({}),
                Some(Value::Object(params)) => Value::Object(params.clone()),
                Some(_) => return Err(format!("{path}:{}: `params` must be a JSON object", n + 1).into()),
            };
            steps.push(Step { id: id.to_string(), params });
        }
    }
    if steps.is_empty() {
        return Err(CliError::Usage("run: no command given (see `lightcraft-cli commands`)".into()));
    }
    let mut backend: Box<dyn Backend> = match connect {
        Some(addr) => Box::new(
            Remote::connect(&addr)
                .map_err(|e| format!("LightCraft is not reachable at {addr} ({e}); start it with `lightcraft --control {}`", connect_port(&addr)))?,
        ),
        None => {
            let mut h = match &library {
                Some(dir) => {
                    let mut h = Headless::default();
                    h.session.open_library(dir, demo).map_err(|e| library_error(dir, e))?;
                    library_warnings(io, &mut h.session, "lightcraft-cli");
                    h
                }
                None if demo => Headless::demo(),
                None => Headless::default(),
            };
            if !imports.is_empty() {
                import_and_select(&mut h.session, &imports)?;
            }
            Box::new(h)
        }
    };
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
        writeln!(io.out, "{line}").map_err(|e| e.to_string())?;
        if failed > 0 && !keep_going {
            break;
        }
    }
    drop(backend); // a library session is flushed here
    if failed > 0 { Err(CliError::StepsFailed(failed)) } else { Ok(()) }
}

fn import_and_select(session: &mut Session, imports: &[String]) -> Result<(), String> {
    let selection = session.selection.clone();
    let paths = expand_paths(imports);
    let result = (|| {
        let r = session.execute("library.import", &json!({"paths": paths})).map_err(|e| format!("import failed: {e}; no commands were run"))?;
        let ids = import_targets(&r)?;
        let first = ids.first().cloned().ok_or("import found no photos to select; no commands were run")?;
        session.execute("library.select", &json!({"ids": ids, "active": first})).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if result.is_err() {
        session.selection = selection;
    }
    result
}

fn import_targets(report: &Value) -> Result<Vec<Value>, String> {
    if let Some(failed) = report["failed"].as_array().filter(|failed| !failed.is_empty()) {
        return Err(format!("import failed for {} item(s): {}; no commands were run", failed.len(), json!(failed)));
    }
    let mut ids: Vec<Value> = report["imported"].as_array().cloned().unwrap_or_default();
    ids.extend(report["duplicates"].as_array().into_iter().flatten().filter_map(|d| d.get("existing").filter(|v| v.is_u64()).cloned()));
    ids.extend(report["restored"].as_array().into_iter().flatten().filter(|id| id.is_u64()).cloned());
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| id.as_u64().is_none_or(|id| seen.insert(id)));
    if ids.is_empty() { Err("import found no photos to select; no commands were run".into()) } else { Ok(ids) }
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
    Preset(lightcraft_engine::develop::Preset),
}

fn read_settings(p: &str) -> Result<SettingsFile, String> {
    use lightcraft_engine::presets::{LCPRESET_EXT, parse_preset_file};
    let bytes = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
    let ext = Path::new(p).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let json = serde_json::from_slice::<Value>(&bytes);
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

/// The session `render` and `merge` develop files in: with the lens database the desktop app downloaded, when it is
/// there (never fetched from here: scripts and tests stay offline).
fn file_session() -> Session {
    Session::new().with_fs().with_default_denoise_models().with_system_clock().with_default_lens_database()
}

fn render(io: &mut Io, args: RenderArgs) -> Result<(), CliError> {
    let RenderArgs { input, output, set, settings, preset, size, quality, opts: export_opts } = args;
    let input = text(&input)?;
    let output = text(&output)?;
    let mut values = serde_json::Map::new();
    for ControlValue { control, value } in set {
        values.insert(control, json!(value));
    }
    let mut opts = serde_json::Map::new();
    for ExportOption { key, value } in export_opts {
        opts.insert(key, value);
    }
    let settings = settings.map(|p| read_settings(&text(&p)?).map_err(CliError::from)).transpose()?;
    let mut s = file_session();
    let abs = expand_paths(std::slice::from_ref(&input));
    let r = s.execute("library.import", &json!({"paths": abs})).map_err(|e| e.to_string())?;
    let id = r["imported"][0].as_u64().ok_or_else(|| format!("{input}: not a readable photo"))?;
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
    use lightcraft_engine::export::{ExportFormat, ExportOptions, export_photo};
    let mut p = json!({"quality": quality});
    if let Some(n) = size {
        p["longEdge"] = json!(n);
    }
    for (k, v) in opts {
        p[k] = v;
    }
    if p.get("format").is_none() {
        let ext = Path::new(&output).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        ExportFormat::parse(&ext).ok_or_else(|| format!("{output}: unknown extension (use .jpg .png .tif .webp .avif .dng or --opt format=…)"))?;
        // given before validating, so `bitDepth` is checked against the format written (issue #1952)
        p["format"] = json!(ext);
    }
    let o = ExportOptions::from_params(&p).map_err(|e| e.to_string())?;
    // Add to Apple Photos: refused before anything is written when it can't be done
    let after = s.after_export(&o)?;
    let e = export_photo(&mut s, lightcraft_engine::catalog::PhotoId(id), &o, 1)?;
    // never over the input (or its sidecar), however it is spelled: `render IMG.jpg -o IMG.jpg`
    let sidecars: Vec<(String, &Vec<u8>)> =
        e.sidecars.iter().map(|(ext, bytes)| (Path::new(&output).with_extension(ext).to_string_lossy().to_string(), bytes)).collect();
    let guard = s.original_guard();
    for p in std::iter::once(&output).chain(sidecars.iter().map(|(p, _)| p)) {
        guard.check(Path::new(p)).map_err(|err| format!("render: {err}"))?;
    }
    lightcraft_engine::export::write_file(&output, &e.bytes)?;
    for (sc, bytes) in &sidecars {
        lightcraft_engine::export::write_file(sc, bytes)?;
        errln!(io, "lightcraft-cli: wrote {sc}");
    }
    errln!(io, "lightcraft-cli: wrote {output} ({}×{})", e.width, e.height);
    if let Some(photos) = after.run(&[json!({"path": output})], false) {
        if let Some(err) = photos.get("error").and_then(Value::as_str) {
            return Err(format!("render: {output} was written, but {err}").into());
        }
        errln!(io, "lightcraft-cli: added to Apple Photos: {photos}");
    }
    Ok(())
}

fn snapshot(io: &mut Io, args: SnapshotArgs) -> Result<(), CliError> {
    use lightcraft_ui_egui::headless::Headless;
    use std::time::{Duration, Instant};
    let SnapshotArgs { demo: _, library, script, output, size: Size(size), scale, files } = args;
    let library = library.as_deref().map(text).transpose()?;
    let script = script.as_deref().map(text).transpose()?;
    let output = output.as_deref().map(text).transpose()?;
    let files = texts(&files)?;
    if script.is_none() && output.is_none() {
        return Err(CliError::Usage("snapshot: give -o OUT.png and/or --script FILE".into()));
    }
    lightcraft_ui_egui::headless::viewport_pixels(size, scale).map_err(|e| CliError::Usage(format!("snapshot: bad --size/--scale: {e}")))?;
    let t0 = Instant::now();
    let mut session = match &library {
        Some(dir) => {
            let mut s = Session::new()
                .with_fs()
                .with_default_denoise_models()
                .with_system_clock()
                .with_default_face_models()
                .with_default_lens_database()
                .with_default_slide_templates();
            s.open_library(dir, false).map_err(|e| library_error(dir, e))?;
            s
        }
        None if files.is_empty() => Session::with_demo()
            .with_fs()
            .with_default_denoise_models()
            .with_system_clock()
            .with_default_face_models()
            .with_default_lens_database()
            .with_default_slide_templates(),
        None => Session::new()
            .with_fs()
            .with_default_denoise_models()
            .with_system_clock()
            .with_default_face_models()
            .with_default_lens_database()
            .with_default_slide_templates(),
    };
    // Apply compute policy before imports or the first scripted query can discover an adapter.
    session.execute("app.gpu", &json!({"enabled": false})).map_err(|e| e.to_string())?;
    if !files.is_empty() {
        let ti = Instant::now();
        let r = session.execute("library.import", &json!({"paths": expand_paths(&files)})).map_err(|e| e.to_string())?;
        let n = r["imported"].as_array().map_or(0, Vec::len);
        errln!(io, "lightcraft-cli snapshot: imported {n} files in {:.0} ms", ti.elapsed().as_secs_f64() * 1e3);
    }
    let services = lightcraft_ui_egui::Services {
        write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)),
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        ..Default::default()
    };
    let mut app = lightcraft_ui_egui::LightcraftApp::new(session, services);
    // Snapshot sessions promise no GPU: disable photo compute as well as the compositor
    // before the first frame can start background adapter discovery.
    // This UI state is session-local: the CLI neither loads nor saves desktop ui.json.
    app.ui.settings.gpu = false;
    // as the desktop app opens it: an empty library on the grid, where Import is offered
    lightcraft_ui_egui::panels::empty_library::open_on_empty_library(&mut app);
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
        let text = std::fs::read_to_string(script).map_err(|e| format!("{script}: {e}"))?;
        // Windows PowerShell 5.1 writes a UTF-8 BOM; it is not part of the first JSON line.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        for (n, line) in text.lines().enumerate() {
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
                                    return Err(format!("{script}:{}: ui.screenshot: `params` must be a JSON object", n + 1).into());
                                }
                            }
                            None => return Err(format!("{script}:{}: ui.screenshot needs a `path` (or give -o)", n + 1).into()),
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
            writeln!(io.out, "{reply}").map_err(|e| e.to_string())?;
            if method == "ui.screenshot" {
                errln!(
                    io,
                    "lightcraft-cli snapshot: {} ({:.0} ms)",
                    reply["result"]["path"].as_str().unwrap_or("?"),
                    ts.elapsed().as_secs_f64() * 1000.0
                );
            }
            if h.quit_requested() {
                break;
            }
        }
    }
    if !wrote_output && let Some(path) = next_path(&mut shots) {
        let r = h.request("ui.screenshot", json!({"path": path}), timeout);
        if r["ok"] != true {
            return Err(format!("screenshot failed: {}", r["error"]).into());
        }
        errln!(io, "lightcraft-cli snapshot: wrote {path} ({}×{})", r["result"]["width"], r["result"]["height"]);
    }
    // a background export started by the script finishes before we exit (its files would be cut off)
    if h.app.export.is_some() {
        errln!(io, "lightcraft-cli snapshot: waiting for the background export");
        let te = std::time::Instant::now();
        while h.app.export.is_some() && te.elapsed() < std::time::Duration::from_secs(3600) {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    errln!(io, "lightcraft-cli snapshot: done in {:.2} s ({} frames)", t0.elapsed().as_secs_f64(), h.frames());
    if failed > 0 { Err(format!("{failed} scripted request(s) failed").into()) } else { Ok(()) }
}

fn commands(io: &mut Io, args: ListArgs) -> Result<(), CliError> {
    let mut h = Headless::demo();
    let mut cmds = h.call("engine.commands", json!({}))?;
    // Listed by id so each family (`photo.*`, `mask.*`, …) reads together; the registry keeps its own order.
    if let Some(list) = cmds.as_array_mut() {
        list.sort_by(|a, b| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")));
    }
    if args.json {
        writeln!(io.out, "{}", serde_json::to_string_pretty(&cmds).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(());
    }
    for c in cmds.as_array().into_iter().flatten() {
        let sc = c["shortcut"].as_str().map(|s| format!("  [{s}]")).unwrap_or_default();
        writeln!(
            io.out,
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

fn controls(io: &mut Io, args: ListArgs) -> Result<(), CliError> {
    let mut s = Session::new();
    let v = s.execute("develop.controls", &json!({})).map_err(|e| e.to_string())?;
    if args.json {
        writeln!(io.out, "{}", serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(());
    }
    for c in v.as_array().into_iter().flatten() {
        writeln!(
            io.out,
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

/// `denoise-serve`: the remote half of AI Denoise (`lightcraft_denoise::remote`).
#[cfg(feature = "denoise")]
fn denoise_serve(io: &mut Io, args: DenoiseServeArgs) -> Result<(), CliError> {
    use lightcraft_denoise::remote::{DEFAULT_PORT, ServerOptions, serve};
    use lightcraft_engine::denoise::RunOn;

    use crate::args::RunOnArg;
    let DenoiseServeArgs { model, models: dir, listen, token_env, run_on, max_connections } = args;
    let listen = listen.unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}"));
    let run_on = match run_on {
        RunOnArg::Auto => RunOn::Auto,
        RunOnArg::Gpu => RunOn::Gpu,
        RunOnArg::Cpu => RunOn::Cpu,
    };
    let token = std::env::var(&token_env).map_err(|_| format!("denoise-serve: set the shared token in ${token_env}"))?;
    let s = lightcraft_engine::denoise::serving(dir.as_deref().map(Path::new), model.as_deref(), run_on)?;
    let listener = std::net::TcpListener::bind(&listen).map_err(|e| format!("denoise-serve: {listen}: {e}"))?;
    errln!(io, "lightcraft-cli denoise-serve: {} {} ({}) on {listen} via {}", s.model.id, s.model.version, s.model.sha256, s.device);
    if listen.starts_with("0.0.0.0") || listen.starts_with("[::]") {
        errln!(io, "lightcraft-cli denoise-serve: listening on every interface; firewall the port to your VPN/LAN (the protocol is not encrypted)");
    }
    let opts = ServerOptions { token, max_connections, ..Default::default() };
    serve(listener, s.model, s.tile, s.device, s.run, opts, std::sync::Arc::new(|m: &str| eprintln!("lightcraft-cli denoise-serve: {m}")))
        .map_err(CliError::from)
}

#[cfg(not(feature = "denoise"))]
fn denoise_serve(_: &mut Io, _: DenoiseServeArgs) -> Result<(), CliError> {
    Err("denoise-serve: this build has no denoise support (build with the `denoise` feature)".into())
}

#[cfg(all(test, unix))]
mod raw_files_tests {
    use super::raw_files;

    /// Links back to their own folders are entered once, not once per path that reaches them: a
    /// handful of such links made the old walk list the same folders exponentially often.
    #[test]
    fn folder_links_back_up_are_walked_once() {
        let dir = std::env::temp_dir().join(format!("lightcraft-cli-raw-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("a");
        std::fs::create_dir_all(a.join("@eaDir")).unwrap();
        std::fs::write(a.join("one.NEF"), b"").unwrap();
        std::fs::write(a.join("@eaDir").join("thumb.nef"), b"").unwrap();
        std::fs::write(a.join("notes.txt"), b"").unwrap();
        for i in 0..8 {
            std::os::unix::fs::symlink(&dir, a.join(format!("up{i}"))).unwrap();
        }
        // a named pipe with a raw extension: not a file, so never opened (a read would block)
        assert!(std::process::Command::new("mkfifo").arg(a.join("pipe.nef")).status().unwrap().success());
        let started = std::time::Instant::now();
        let mut files = Vec::new();
        raw_files(&dir, &mut files);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "walk took {:?}", started.elapsed());
        assert_eq!(files, vec![a.join("one.NEF")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod launch_profile_tests {
    use super::launch_profile_refusal;

    fn refusal(args: &[&str]) -> Option<String> {
        launch_profile_refusal(args.iter().map(|a| (*a).to_string()))
    }

    #[test]
    fn the_desktop_launch_protocol_is_refused_before_it_can_be_read_as_a_folder() {
        for args in [
            vec!["--launch-profile", "nightly-v1", "--profile-root", "/p", "--"],
            vec!["run", "--profile-root=/p"],
            vec!["--launch-profile=nightly-v1"],
        ] {
            assert!(refusal(&args).is_some(), "{args:?}");
        }
        assert!(refusal(&["run", "--library", "a", "--", "/p"]).is_none(), "a file named like an option after -- is a file");
        assert!(refusal(&["run", "--demo"]).is_none());
    }
}

#[cfg(test)]
mod tests {
    /// `render` and `merge` read the lens database the desktop app downloaded, as the desktop does (the same raw
    /// gets the same profile everywhere), but never fetch it.
    #[test]
    fn render_reads_the_downloaded_lens_database() {
        let s = super::file_session();
        assert_eq!(lightcraft_engine::lens_profiles::host_dir(), lightcraft_engine::lensfun_download::default_dir());
        assert!(!s.lens_auto_download, "selecting it is not permission to download it");
    }
}

#[cfg(test)]
mod import_target_tests {
    use super::{import_and_select, import_targets, json};

    #[test]
    fn empty_reports_and_duplicates_without_ids_have_no_target() {
        assert!(import_targets(&json!({})).is_err());
        assert!(import_targets(&json!({"imported": [], "duplicates": [{"existing": null}]})).is_err());
    }

    #[test]
    fn restored_only_and_mixed_reports_keep_target_order() {
        assert_eq!(import_targets(&json!({"restored": [9]})).unwrap(), vec![json!(9)]);
        assert_eq!(
            import_targets(&json!({"imported": [1], "duplicates": [{"existing": null}, {"existing": 2}], "restored": [3]})).unwrap(),
            vec![json!(1), json!(2), json!(3)]
        );
    }

    #[test]
    fn repeated_targets_are_selected_once_in_first_occurrence_order() {
        let report = json!({"imported": [5, 7], "duplicates": [{"existing": 5}, {"existing": 8}, {"existing": 8}, {"existing": 7}, {"existing": null}], "restored": [8, 9, 5]});
        assert_eq!(import_targets(&report).unwrap(), vec![json!(5), json!(7), json!(8), json!(9)]);
    }

    #[test]
    fn failed_items_reject_even_when_targets_exist() {
        let error = import_targets(&json!({"imported": [1], "failed": [["broken.png", "invalid"]]})).unwrap_err();
        assert!(error.contains("1") && error.contains("broken.png") && error.contains("no commands were run"), "{error}");
    }

    #[test]
    fn failed_partial_import_restores_empty_selection() {
        let dir = std::env::temp_dir().join(format!("lightcraft-import-selection-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("valid.png");
        let broken = dir.join("broken.png");
        let img = lightcraft_raster::Rgba8::from_fn(2, 2, |_, _| [20, 40, 60, 255]);
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap();
        std::fs::write(&input, png).unwrap();
        std::fs::write(&broken, b"not a PNG").unwrap();
        let mut headless = lightcraft_mcp::Headless::default();
        let session = &mut headless.session;
        let selection = session.selection.clone();
        assert!(selection.ids.is_empty() && selection.active.is_none());
        assert!(import_and_select(session, &[input.to_string_lossy().into_owned(), broken.to_string_lossy().into_owned()]).is_err());
        assert_eq!(session.selection, selection);
        let _ = std::fs::remove_dir_all(dir);
    }
}
