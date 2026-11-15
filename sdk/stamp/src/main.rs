//! `plugin-stamp`: copies a plug-in's manifest into its `.wasm` as a custom section, or checks one
//! (the library's docs, and `docs/plugins.md` → Stamping your plug-in).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use plugin_stamp::{Limits, SECTION};

const USAGE: &str = "\
usage: plugin-stamp <plugin.wasm> [-o <out.wasm>]   stamp (in place without -o)
       plugin-stamp --check <plugin.wasm>          exit 0 only if the section matches lcc_manifest

Runs the plug-in's lcc_manifest once in a sandbox and writes its answer into a
photo-plugin.manifest custom section, replacing an earlier one.";

enum Mode {
    Stamp { input: PathBuf, output: Option<PathBuf> },
    Check(PathBuf),
    Help,
    Version,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Mode, String> {
    let mut input = None;
    let mut output = None;
    let mut check = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Mode::Help),
            Some("-V" | "--version") => return Ok(Mode::Version),
            Some("--check") => check = true,
            Some("-o" | "--output") => {
                let path = args.next().ok_or("-o needs a file name")?;
                if output.replace(PathBuf::from(path)).is_some() {
                    return Err("-o given twice".into());
                }
            }
            Some(s) if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => {
                if input.replace(PathBuf::from(arg)).is_some() {
                    return Err("one plug-in file at a time".into());
                }
            }
        }
    }
    let input = input.ok_or("no plug-in file given")?;
    match (check, output) {
        (true, Some(_)) => Err("--check writes nothing; drop -o".into()),
        (true, None) => Ok(Mode::Check(input)),
        (false, output) => Ok(Mode::Stamp { input, output }),
    }
}

/// The error with the file it is about in front; I/O errors name their file already.
fn named(path: &std::path::Path, e: plugin_stamp::Error) -> String {
    match e {
        plugin_stamp::Error::Io(m) => m,
        e => format!("{}: {e}", path.display()),
    }
}

fn main() -> ExitCode {
    let mode = match parse(std::env::args_os().skip(1)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("plugin-stamp: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let limits = Limits::default();
    let result = match mode {
        Mode::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Mode::Version => {
            println!("plugin-stamp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Mode::Stamp { input, output } => plugin_stamp::stamp_file(&input, output.as_deref(), &limits)
            .map(|m| {
                let out = output.as_deref().unwrap_or(&input);
                format!("{}: wrote a {SECTION} section ({} bytes)", out.display(), m.len())
            })
            .map_err(|e| named(&input, e)),
        Mode::Check(path) => plugin_stamp::check_file(&path, &limits)
            .map(|_| format!("{}: the {SECTION} section matches lcc_manifest", path.display()))
            .map_err(|e| named(&path, e)),
    };
    match result {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("plugin-stamp: {e}");
            ExitCode::FAILURE
        }
    }
}
