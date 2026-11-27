//! The command line as types: every subcommand's options, parsed and checked by clap before any
//! work starts (RFC-0018 rules 1.4 and 2.1). Help text is these doc comments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use lightcraft_mcp::DEFAULT_ADDR;
use serde_json::{Value, json};

/// Headless LightCraft: a photo library and raw developer on the command line.
#[derive(Debug, Parser)]
#[command(name = "lightcraft-cli", version, propagate_version = true, subcommand_required = true, arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run commands and print one JSON line per command: {"command", "ok", "result" | "error", "ms"}.
    ///
    /// A word without `=` starts the next command; values are JSON when they parse (1, true, [1,2],
    /// {…}), else strings. The exit status is 1 when a command fails.
    #[command(override_usage = "lightcraft-cli run [OPTIONS] COMMAND [key=value | '{json}']… [COMMAND …]…")]
    Run(RunArgs),
    /// Serve MCP (JSON-RPC 2.0 over stdio). Headless by default, with the given files imported.
    Mcp(McpArgs),
    /// Develop one file and export it, with the same encoder as the app's Export dialog.
    ///
    /// The format comes from the extension of OUTPUT (.jpg, .png, .tif, .webp, .avif, .jxl, .psd or .dng).
    Render(RenderArgs),
    /// Run the full app UI headlessly (no window, CPU-rasterized) and write PNGs.
    Snapshot(SnapshotArgs),
    /// Photo Merge: write <first>-HDR.dng, -Pano.dng or -HDR-Pano.dng next to the first file and print the result as JSON.
    Merge(MergeArgs),
    /// Write synthetic merge inputs (a procedural scene: bracketed DNGs or overlapping PNG views).
    SynthMerge(SynthMergeArgs),
    /// Run AI Denoise tiles for LightCraft on other computers (see docs/denoise.md).
    ///
    /// Plain TCP: listen on a VPN/LAN address or firewall the port to it. The shared token is read
    /// from the environment variable named by --token-env, never from the command line.
    DenoiseServe(DenoiseServeArgs),
    /// List every command id with its parameters.
    Commands(ListArgs),
    /// List every develop control id with its range.
    Controls(ListArgs),
    /// Fit a colour profile per camera model from raw files and their embedded camera JPEGs.
    Calibrate(CalibrateArgs),
    /// Time what culling a shoot costs on its own files.
    BenchCull(BenchCullArgs),
}

/// `--connect` takes an optional address, written `--connect=ADDR`; a bare port means that port on
/// the loopback address.
fn address(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Err("an address, such as 127.0.0.1:7980 or a port".into());
    }
    Ok(if text.bytes().all(|b| b.is_ascii_digit()) { format!("127.0.0.1:{text}") } else { text.to_string() })
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Headless, with the procedural demo library.
    #[arg(long, conflicts_with = "connect")]
    pub demo: bool,
    /// Headless: open (or create) a LightCraft library; edits are saved.
    #[arg(long, value_name = "DIR", conflicts_with = "connect")]
    pub library: Option<PathBuf>,
    /// Headless: import a file or folder first (repeatable).
    #[arg(long = "import", value_name = "PATH", conflicts_with = "connect")]
    pub imports: Vec<PathBuf>,
    /// Drive the running app (`lightcraft --control 7980`) instead; written --connect=ADDR (default 127.0.0.1:7980).
    #[arg(long, value_name = "ADDR", num_args = 0..=1, require_equals = true, default_missing_value = DEFAULT_ADDR, value_parser = address)]
    pub connect: Option<String>,
    /// Also run JSON lines {"command": id, "params": {…}} (or {"method": …}) from this file, or `-` for stdin.
    #[arg(long, value_name = "FILE|-")]
    pub script: Option<PathBuf>,
    /// Continue after a failed command.
    #[arg(long)]
    pub keep_going: bool,
    /// Command ids (from `commands`, or control-protocol methods such as `engine.commands`, `ui.inspect`) with their parameters.
    #[arg(value_name = "COMMAND [key=value]", trailing_var_arg = true, required_unless_present = "script")]
    pub words: Vec<String>,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    /// Drive a running app instead; written --connect=ADDR (default 127.0.0.1:7980).
    #[arg(long, value_name = "ADDR", num_args = 0..=1, require_equals = true, default_missing_value = DEFAULT_ADDR, value_parser = address,
        conflicts_with_all = ["headless", "demo", "library", "files"])]
    pub connect: Option<String>,
    /// Stay headless (the default).
    #[arg(long)]
    pub headless: bool,
    /// Headless: start with the procedurally generated demo library.
    #[arg(long)]
    pub demo: bool,
    /// Headless: open (or create) a persistent LightCraft library; edits are saved.
    #[arg(long, value_name = "DIR")]
    pub library: Option<PathBuf>,
    /// List only the helper tools (every command stays reachable through run_command).
    #[arg(long)]
    pub compact: bool,
    /// Files or folders to import.
    #[arg(value_name = "FILES/FOLDERS")]
    pub files: Vec<PathBuf>,
}

/// A develop control and its value, written CONTROL=VALUE.
#[derive(Debug, Clone)]
pub struct ControlValue {
    pub control: String,
    pub value: f64,
}

fn control_value(text: &str) -> Result<ControlValue, String> {
    let (k, v) = text.split_once('=').ok_or_else(|| format!("expected control=value, got `{text}`"))?;
    // `inf` / `NaN` parse as f64 but become JSON null, which the control would silently ignore.
    let value: f64 = v.trim().parse().ok().filter(|n: &f64| n.is_finite()).ok_or_else(|| format!("`{v}` is not a finite number"))?;
    Ok(ControlValue { control: k.trim().to_string(), value })
}

/// An export option, written KEY=VALUE where VALUE is JSON or else a string.
#[derive(Debug, Clone)]
pub struct ExportOption {
    pub key: String,
    pub value: Value,
}

fn export_option(text: &str) -> Result<ExportOption, String> {
    let (k, v) = text.split_once('=').ok_or_else(|| format!("expected key=value, got `{text}`"))?;
    Ok(ExportOption { key: k.trim().to_string(), value: serde_json::from_str(v).unwrap_or_else(|_| json!(v)) })
}

#[derive(Debug, Args)]
pub struct RenderArgs {
    /// The photo to develop.
    #[arg(value_name = "IN")]
    pub input: PathBuf,
    /// The file to write.
    #[arg(short, long, value_name = "OUT")]
    pub output: PathBuf,
    /// Set a develop slider, repeatable (e.g. --set light.exposure=0.5).
    #[arg(long = "set", value_name = "CONTROL=VALUE", value_parser = control_value)]
    pub set: Vec<ControlValue>,
    /// Merge a partial develop-settings JSON file, or apply the one preset of a .lcpreset or XMP preset.
    #[arg(long, value_name = "FILE")]
    pub settings: Option<PathBuf>,
    /// Apply a preset (see `commands` and presets.list).
    #[arg(long, value_name = "ID")]
    pub preset: Option<String>,
    /// Long edge in pixels (default: full size, cropped).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub size: Option<u64>,
    /// JPEG, AVIF and JPEG XL quality.
    #[arg(long, value_name = "Q", default_value_t = 92, value_parser = clap::value_parser!(u8).range(1..=100))]
    pub quality: u8,
    /// Any export option of `app.export`, repeatable (VALUE is JSON or a string), e.g. --opt colorSpace=displayP3 --opt bitDepth=16.
    #[arg(long = "opt", value_name = "KEY=VALUE", value_parser = export_option)]
    pub opts: Vec<ExportOption>,
}

/// A window size in points, written WxH.
#[derive(Debug, Clone, Copy)]
pub struct Size(pub [f32; 2]);

fn size(text: &str) -> Result<Size, String> {
    let (w, h) = text.split_once(['x', 'X']).ok_or("expected WxH, e.g. 1600x1000")?;
    let w = w.trim().parse().map_err(|_| "bad width")?;
    let h = h.trim().parse().map_err(|_| "bad height")?;
    Ok(Size([w, h]))
}

fn finite_scale(text: &str) -> Result<f32, String> {
    let v: f32 = text.parse().map_err(|_| format!("`{text}` is not a number"))?;
    if v.is_finite() { Ok(v) } else { Err("a finite number".into()) }
}

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Use the procedural demo library (the default unless --library or FILES are given).
    #[arg(long, conflicts_with = "library")]
    pub demo: bool,
    /// Open (or create) a LightCraft library.
    #[arg(long, value_name = "DIR")]
    pub library: Option<PathBuf>,
    /// JSON-lines control-protocol requests, one per line (docs/control-protocol.md). Replies go to stdout.
    #[arg(long, value_name = "FILE")]
    pub script: Option<PathBuf>,
    /// PNG path: a final screenshot is written here if the script took none.
    #[arg(short, long, value_name = "OUT.png")]
    pub output: Option<PathBuf>,
    /// Window size in points.
    #[arg(long, value_name = "WxH", default_value = "1600x1000", value_parser = size)]
    pub size: Size,
    /// Pixels per point.
    #[arg(long, value_name = "S", default_value = "1", value_parser = finite_scale)]
    pub scale: f32,
    /// Files or folders to import.
    #[arg(value_name = "FILES/FOLDERS")]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MergeKind {
    Hdr,
    #[value(alias = "pano")]
    Panorama,
    HdrPanorama,
    /// Merge each bracket set among the files (photos taken less than a second apart at different exposures).
    HdrBatch,
}

impl MergeKind {
    /// The engine command that does the merge.
    pub fn command(self) -> &'static str {
        match self {
            Self::Hdr => "merge.hdr",
            Self::Panorama => "merge.panorama",
            Self::HdrPanorama => "merge.hdrPanorama",
            Self::HdrBatch => "merge.hdrBatch",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Deghost {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Projection {
    Auto,
    Spherical,
    Cylindrical,
    Perspective,
}

fn warp(text: &str) -> Result<f64, String> {
    let v: f64 = text.parse().map_err(|_| format!("`{text}` is not a number"))?;
    if (0.0..=100.0).contains(&v) { Ok(v) } else { Err("a number from 0 to 100".into()) }
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// What to merge.
    #[arg(value_enum)]
    pub kind: MergeKind,
    /// Ghost removal strength.
    #[arg(long, value_enum)]
    pub deghost: Option<Deghost>,
    /// Do not align the frames first.
    #[arg(long)]
    pub no_align: bool,
    /// Bracket size (HDR panorama).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub bracket: Option<u64>,
    /// Panorama projection.
    #[arg(long, value_enum)]
    pub projection: Option<Projection>,
    /// Boundary warp, 0 to 100.
    #[arg(long, value_name = "0..100", value_parser = warp)]
    pub boundary_warp: Option<f64>,
    /// Crop the result to the largest clean rectangle.
    #[arg(long)]
    pub auto_crop: bool,
    /// Fill the empty edges.
    #[arg(long)]
    pub fill_edges: bool,
    /// Leave the merge settings as they are instead of choosing them.
    #[arg(long)]
    pub no_auto_settings: bool,
    /// Only render a preview of at most 1024 px to this PNG (nothing is written next to the files).
    #[arg(long, value_name = "OUT.png")]
    pub preview: Option<PathBuf>,
    /// The photos to merge.
    #[arg(value_name = "FILES", required = true)]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SynthKind {
    Hdr,
    #[value(alias = "pano")]
    Panorama,
}

#[derive(Debug, Args)]
pub struct SynthMergeArgs {
    /// Which synthetic inputs to write.
    #[arg(value_enum)]
    pub kind: SynthKind,
    /// The folder to write them in.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunOnArg {
    Auto,
    Gpu,
    Cpu,
}

fn positive(text: &str) -> Result<usize, String> {
    match text.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("`{text}` is not a number from 1")),
    }
}

#[derive(Debug, Args)]
pub struct DenoiseServeArgs {
    /// An installed model other than the selected one.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// The models folder (default: LIGHTCRAFT_DENOISE_MODELS, else this user's).
    #[arg(long, value_name = "DIR")]
    pub models: Option<PathBuf>,
    /// Address:port to listen on (default 127.0.0.1:7990).
    #[arg(long, value_name = "ADDR")]
    pub listen: Option<String>,
    /// The environment variable holding the shared token, at least 16 characters.
    #[arg(long, value_name = "VAR", default_value = "LIGHTCRAFT_DENOISE_TOKEN")]
    pub token_env: String,
    /// Where to run the model.
    #[arg(long, value_enum, default_value = "auto")]
    pub run_on: RunOnArg,
    /// Concurrent clients.
    #[arg(long, value_name = "N", default_value_t = 8, value_parser = positive)]
    pub max_connections: usize,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CalibrateArgs {
    /// Most files to use, spread over the folders; 0 means all.
    #[arg(long, value_name = "N", default_value_t = 300)]
    pub max: usize,
    /// Where to write <model>.json (default: the folder LightCraft reads, or $LIGHTCRAFT_CAMERA_PROFILES).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Folders or raw files.
    #[arg(value_name = "FOLDERS/FILES", required = true)]
    pub inputs: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct BenchCullArgs {
    /// The folder of photos to cull.
    #[arg(value_name = "FOLDER")]
    pub folder: PathBuf,
    /// Read only the first K files by name (a sample of a slow network folder).
    #[arg(long, value_name = "K")]
    pub files: Option<usize>,
    /// Import this many files in place.
    #[arg(long, value_name = "N", default_value_t = 2000)]
    pub n: usize,
    /// Import and copy this many files.
    #[arg(long, value_name = "M", default_value_t = 200)]
    pub copy: usize,
    /// Where libraries are made, in a new folder (default: the temp folder).
    #[arg(long, value_name = "DIR")]
    pub work: Option<PathBuf>,
    /// Write the result to this file as JSON.
    #[arg(long, value_name = "FILE")]
    pub json: Option<PathBuf>,
}
