//! `quality-scan [OPTIONS] [FOLDER]` (issue #1313): the Image Quality Scanner from a shell, the
//! same as `run [OPTIONS] photo.qualityScan '{…}'` — every option here becomes one of its params,
//! so the scan, the cutoffs, the weights and the summary are the engine's, not a second copy.
//!
//! The scan runs where `run` runs it: in the running app with `--connect` (its activity stack
//! shows it, and the scores land in the library it has open), else headless in `--library`'s
//! catalog, and with `--import` it reads files in first. A whole catalog takes a while, so it is
//! run to the end here (`wait`), which is what a script wants; the app runs it in the background.

use serde_json::{Map, Value, json};

const USAGE: &str = "quality-scan [--demo] [--library DIR] [--import PATH]… [--connect [ADDR]] [--scope selection|view|folder|catalog] [--accepted N] [--rejected N] [--apply-flags] [--rescan] [--weights a,b,c,d] [--ids 1,2,3] [FOLDER]";

/// The `run` arguments that scan quality with these options.
pub fn to_run_args(args: &[String]) -> Result<Vec<String>, String> {
    let mut opts: Vec<String> = Vec::new();
    let mut params = Map::new();
    let mut folder: Option<String> = None;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        // the value of an option, as the next word
        macro_rules! value {
            () => {{
                i += 1;
                args.get(i).cloned().ok_or_else(|| format!("{a} needs a value"))?
            }};
        }
        match a.as_str() {
            // (the run options `run` itself takes, passed through)
            "--demo" => opts.push(a.clone()),
            "--library" | "--import" | "--script" => {
                opts.push(a.clone());
                opts.push(value!());
            }
            "--connect" => {
                // a bare port is an address; a word is the next option's start
                let bare = args
                    .get(i + 1)
                    .is_some_and(|n| !n.is_empty() && !n.contains('=') && (n.contains(':') || n.chars().all(|c| c.is_ascii_digit())));
                opts.push(a.clone());
                if bare {
                    opts.push(value!());
                }
            }
            "--scope" => {
                let v = value!();
                if !matches!(v.as_str(), "selection" | "view" | "folder" | "catalog") {
                    return Err(format!("--scope is selection, view, folder or catalog, not {v:?}"));
                }
                params.insert("scope".into(), json!(v));
            }
            "--accepted" | "--rejected" => {
                let v = value!();
                let n: f64 = v.parse().map_err(|_| format!("{a} takes a number from 0 to 100, not {v:?}"))?;
                params.insert(a.trim_start_matches("--").into(), json!(n));
            }
            "--apply-flags" => {
                params.insert("applyFlags".into(), json!(true));
            }
            "--rescan" => {
                params.insert("rescan".into(), json!(true));
            }
            "--weights" => {
                let v = value!();
                let parts: Vec<&str> = v.split(',').map(str::trim).collect();
                let [blur, noise, exposure, compression] = parts.as_slice() else {
                    return Err(format!("--weights takes four numbers, blur,noise,exposure,compression, not {v:?}"));
                };
                let num = |s: &str| -> Result<f64, String> { s.parse::<f64>().map_err(|_| format!("--weights takes numbers, not {s:?}")) };
                params.insert(
                    "weights".into(),
                    json!({
                        "blur": num(blur)?,
                        "noise": num(noise)?,
                        "exposure": num(exposure)?,
                        "compression": num(compression)?,
                    }),
                );
            }
            "--ids" => {
                let v = value!();
                let ids: Vec<u64> = v
                    .split(',')
                    .map(|s| s.trim().parse::<u64>().map_err(|_| format!("--ids takes photo ids, not {s:?}")))
                    .collect::<Result<_, _>>()?;
                params.insert("ids".into(), json!(ids));
            }
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`\n{USAGE}")),
            a => {
                if folder.is_some() {
                    return Err(format!("unexpected `{a}` (one folder, at most)\n{USAGE}"));
                }
                folder = Some(a.to_string());
            }
        }
        i += 1;
    }
    // a folder on the command line is what `--scope folder` scans (a scan of the whole catalog
    // otherwise, so a bare `quality-scan --library DIR` scores everything)
    if let Some(folder) = folder {
        params.insert("scope".into(), json!("folder"));
        params.insert("path".into(), json!(folder));
    } else if !params.contains_key("scope") {
        params.insert("scope".into(), json!("catalog"));
    }
    // headless, the caller wants the answer; in the app, the activity stack shows it
    let connect = opts.iter().any(|o| o == "--connect");
    if !connect {
        params.insert("wait".into(), json!(true));
    }
    opts.push("photo.qualityScan".into());
    opts.push(Value::Object(params).to_string());
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn params(r: &[String]) -> Value {
        serde_json::from_str(r.last().unwrap()).unwrap()
    }

    #[test]
    fn a_bare_scan_of_a_library_scores_the_whole_catalog() {
        let r = to_run_args(&s(&["--library", "/l"])).unwrap();
        assert_eq!(&r[..3], &s(&["--library", "/l", "photo.qualityScan"])[..]);
        let p = params(&r);
        assert_eq!(p["scope"], "catalog");
        assert_eq!(p["wait"], true, "headless runs it to the end");
    }

    #[test]
    fn a_folder_argument_is_the_folder_scope() {
        let p = params(&to_run_args(&s(&["/photos/trip", "--rescan"])).unwrap());
        assert_eq!(p["scope"], "folder");
        assert_eq!(p["path"], "/photos/trip");
        assert_eq!(p["rescan"], true);
    }

    #[test]
    fn cutoffs_weights_and_flags_are_the_commands_params() {
        let p = params(&to_run_args(&s(&["--accepted", "80", "--rejected", "30", "--apply-flags", "--weights", "2,1,1,0.5"])).unwrap());
        assert_eq!(p["accepted"], 80.0);
        assert_eq!(p["rejected"], 30.0);
        assert_eq!(p["applyFlags"], true);
        assert_eq!(p["weights"], json!({"blur": 2.0, "noise": 1.0, "exposure": 1.0, "compression": 0.5}));
    }

    #[test]
    fn the_app_runs_it_in_the_background() {
        let p = params(&to_run_args(&s(&["--connect", "7980"])).unwrap());
        assert!(p.get("wait").is_none(), "a remote scan is the app's activity");
        // without an address, the next word is not taken as one
        let r = to_run_args(&s(&["--connect"])).unwrap();
        assert_eq!(&r[..2], &s(&["--connect", "photo.qualityScan"])[..]);
    }

    #[test]
    fn what_it_cannot_use_it_refuses_with_the_usage() {
        for bad in [s(&["--scope", "somewhere"]), s(&["--accepted", "high"]), s(&["--weights", "1,2,3"]), s(&["a", "b"]), s(&["--nope"])] {
            assert!(to_run_args(&bad).is_err(), "{bad:?}");
        }
        assert!(to_run_args(&s(&["--library"])).is_err(), "an option without its value");
        assert!(to_run_args(&s(&["--ids", "1,x"])).is_err());
    }
}
