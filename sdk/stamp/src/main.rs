//! `plugin-stamp`: copies a plug-in's manifest into its `.wasm` as a custom section, or checks one;
//! makes an author key and signs a stamped plug-in with it (the library's docs, and
//! `docs/plugins.md` → Stamping your plug-in, Signing your plug-in).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use plugin_stamp::{Limits, SECTION, SIGNATURE_SECTION, Signature};

const USAGE: &str = "\
usage: plugin-stamp <plugin.wasm> [-o <out.wasm>]   stamp (in place without -o)
       plugin-stamp --check <plugin.wasm>          exit 0 only if the section matches lcc_manifest
                                                   and the file isn't signed, or validly signed
       plugin-stamp keygen <secret.key>            make an author key (never overwrites a file)
       plugin-stamp sign --key <secret.key> <plugin.wasm> [-o <out.wasm>]
                                                   sign a stamped plug-in (in place without -o)

Runs the plug-in's lcc_manifest once in a sandbox and writes its answer into a
photo-plugin.manifest custom section, replacing an earlier one. Stamping a signed
file drops its signature: sign it again after. A file named keygen or sign is
given as ./keygen or ./sign.";

enum Mode {
    Stamp { input: PathBuf, output: Option<PathBuf> },
    Check(PathBuf),
    Keygen(PathBuf),
    Sign { key: PathBuf, input: PathBuf, output: Option<PathBuf> },
    Help,
    Version,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Mode, String> {
    let mut args = args.into_iter().peekable();
    match args.peek().and_then(|a| a.to_str()) {
        Some("keygen") => {
            args.next();
            let out = args.next().ok_or("keygen needs the file to write the secret key to")?;
            if let Some(more) = args.next() {
                return Err(format!("keygen takes one file; unexpected {}", more.to_string_lossy()));
            }
            return Ok(Mode::Keygen(PathBuf::from(out)));
        }
        Some("sign") => {
            args.next();
            return parse_sign(args);
        }
        _ => {}
    }
    let mut input = None;
    let mut output = None;
    let mut check = false;
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

fn parse_sign(args: impl Iterator<Item = OsString>) -> Result<Mode, String> {
    let (mut key, mut input, mut output) = (None, None, None);
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--key") => {
                let path = args.next().ok_or("--key needs the secret key file")?;
                if key.replace(PathBuf::from(path)).is_some() {
                    return Err("--key given twice".into());
                }
            }
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
    let key = key.ok_or("sign needs --key <secret.key>")?;
    let input = input.ok_or("no plug-in file given")?;
    Ok(Mode::Sign { key, input, output })
}

/// The secret key in the file `path`.
fn read_key(path: &Path) -> Result<[u8; 32], String> {
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?);
    plugin_stamp::secret_key_from_text(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The signature line `--check` prints, and whether it passes.
fn signature_line(path: &Path) -> Result<(String, bool), String> {
    Ok(match plugin_stamp::signature_file(path).map_err(|e| named(path, e))? {
        Signature::None => (format!("{}: not signed", path.display()), true),
        Signature::Valid { key } => (format!("{}: signed by {}", path.display(), plugin_stamp::fingerprint(&key)), true),
        Signature::Invalid(why) => (format!("{}: the {SIGNATURE_SECTION} section is invalid: {why}", path.display()), false),
    })
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
        Mode::Stamp { input, output } => {
            let signed = matches!(plugin_stamp::signature_file(&input), Ok(Signature::Valid { .. } | Signature::Invalid(_)));
            plugin_stamp::stamp_file(&input, output.as_deref(), &limits)
                .map(|m| {
                    let out = output.as_deref().unwrap_or(&input);
                    let mut line = format!("{}: wrote a {SECTION} section ({} bytes)", out.display(), m.len());
                    if signed {
                        line.push_str(&format!("\n{}: its signature was dropped (stamping changes the signed bytes): sign it again", out.display()));
                    }
                    line
                })
                .map_err(|e| named(&input, e))
        }
        Mode::Check(path) => plugin_stamp::check_file(&path, &limits)
            .map_err(|e| named(&path, e))
            .and_then(|_| signature_line(&path))
            .and_then(|(signature, ok)| {
                let line = format!("{}: the {SECTION} section matches lcc_manifest\n{signature}", path.display());
                if ok { Ok(line) } else { Err(line) }
            }),
        Mode::Keygen(out) => plugin_stamp::new_key_file(&out).map_err(|e| e.to_string()).map(|public| {
            format!(
                "{}: wrote a new secret key; its public key's fingerprint is {}\nKeep the file out of your repository, and back it up: a plug-in signed with a lost key can only be updated by hand",
                out.display(),
                plugin_stamp::fingerprint(&public)
            )
        }),
        Mode::Sign { key, input, output } => read_key(&key).and_then(|secret| {
            plugin_stamp::sign_file(&input, output.as_deref(), &secret, &limits)
                .map(|public| {
                    let out = output.as_deref().unwrap_or(&input);
                    format!("{}: signed by {}", out.display(), plugin_stamp::fingerprint(&public))
                })
                .map_err(|e| named(&input, e))
        }),
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
