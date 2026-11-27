//! `run` called in-process: what the command line does for a person, without starting the binary.
//! (`tests/it` runs the built binary for what needs the whole process.)

use crate::run;

fn go(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(std::iter::once("lightcraft-cli").chain(args.iter().copied()), &mut out, &mut err);
    (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
}

#[test]
fn no_arguments_print_help_on_stderr_and_exit_2() {
    let (code, out, err) = go(&[]);
    assert_eq!(code, 2);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("Usage: lightcraft-cli"), "{err}");
}

#[test]
fn help_and_version_go_to_stdout_with_exit_0() {
    let (code, out, err) = go(&["--help"]);
    assert_eq!((code, err.as_str()), (0, ""));
    for sub in ["run", "mcp", "render", "snapshot", "merge", "synth-merge", "denoise-serve", "commands", "controls", "calibrate", "bench-cull"] {
        assert!(out.contains(sub), "{sub} in {out}");
    }
    let (code, out, _) = go(&["--version"]);
    assert_eq!((code, out), (0, format!("lightcraft-cli {}\n", env!("CARGO_PKG_VERSION"))));
    // `-V` works after a subcommand too, and there is no `version` subcommand.
    assert_eq!(go(&["render", "-V"]).0, 0);
    let (code, _, err) = go(&["version"]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn an_unknown_flag_after_a_subcommand_is_a_usage_error() {
    for args in [
        &["commands", "--bogus"][..],
        &["controls", "--jsno"],
        &["render", "in.jpg", "-o", "out.jpg", "--bogus"],
        &["run", "--bogus", "develop.auto"],
        &["mcp", "--bogus"],
        &["snapshot", "--bogus"],
        &["merge", "hdr", "--bogus", "a.dng"],
        &["synth-merge", "hdr", "-o", "d", "--bogus"],
        &["calibrate", "--bogus", "dir"],
        &["bench-cull", "dir", "--bogus"],
        &["denoise-serve", "--bogus"],
    ] {
        let (code, out, err) = go(args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(err.contains("--bogus") || err.contains("--jsno"), "{args:?}: {err}");
    }
}

#[test]
fn missing_and_bad_values_are_usage_errors_naming_the_flag() {
    for (args, needle) in [
        (&["render", "in.jpg"][..], "--output"),
        (&["render", "in.jpg", "-o"], "--output"),
        (&["render", "in.jpg", "-o", "o.jpg", "--set", "nonsense"], "--set"),
        (&["render", "in.jpg", "-o", "o.jpg", "--set", "x=inf"], "finite"),
        (&["render", "in.jpg", "-o", "o.jpg", "--quality", "0"], "--quality"),
        (&["render", "in.jpg", "-o", "o.jpg", "--quality", "101"], "--quality"),
        (&["render", "in.jpg", "-o", "o.jpg", "--size", "x"], "--size"),
        (&["render", "a.jpg", "b.jpg", "-o", "o.jpg"], "b.jpg"),
        (&["snapshot", "--size", "100"], "--size"),
        (&["snapshot", "--scale", "nan"], "--scale"),
        (&["merge", "sideways", "a.dng"], "sideways"),
        (&["merge", "hdr"], "FILES"),
        (&["merge", "hdr", "--deghost", "extreme", "a.dng"], "--deghost"),
        (&["merge", "panorama", "--boundary-warp", "101", "a.dng"], "--boundary-warp"),
        (&["synth-merge", "hdr"], "--output"),
        (&["synth-merge", "video", "-o", "d"], "video"),
        (&["calibrate"], "FOLDERS"),
        (&["calibrate", "--max", "many", "dir"], "--max"),
        (&["bench-cull"], "FOLDER"),
        (&["bench-cull", "a", "b"], "b"),
        (&["denoise-serve", "--run-on", "tpu"], "--run-on"),
        (&["denoise-serve", "--max-connections", "0"], "--max-connections"),
        (&["run"], "COMMAND"),
        (&["run", "--connect=", "ui.inspect"], "--connect"),
    ] {
        let (code, out, err) = go(args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
        assert!(err.contains(needle), "{args:?}: `{needle}` not in {err}");
    }
}

#[test]
fn a_flag_given_twice_is_refused_where_the_value_would_be_lost() {
    for args in [
        &["render", "i.jpg", "-o", "a.jpg", "-o", "b.jpg"][..],
        &["run", "--script", "a", "--script", "b"],
        &["snapshot", "-o", "a.png", "-o", "b.png"],
    ] {
        let (code, _, err) = go(args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("cannot be used multiple times"), "{args:?}: {err}");
    }
}

#[test]
fn flags_that_cannot_be_combined_are_a_usage_error() {
    for args in [
        &["run", "--demo", "--connect=7980", "ui.inspect"][..],
        &["run", "--library", "d", "--connect", "7980", "ui.inspect"],
        &["run", "--import", "a.jpg", "--connect=1", "ui.inspect"],
        &["mcp", "--connect=7980", "--demo"],
        &["mcp", "--connect=7980", "photo.jpg"],
        &["snapshot", "--demo", "--library", "d"],
    ] {
        let (code, _, err) = go(args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("cannot be used with"), "{args:?}: {err}");
    }
}

#[test]
fn the_launch_profile_of_the_desktop_app_is_refused_with_exit_2() {
    let (code, out, err) = go(&["--launch-profile=x", "commands"]);
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("belongs to the LightCraft desktop app"), "{err}");
}

#[test]
fn commands_and_controls_list_without_a_library() {
    let (code, out, err) = go(&["commands"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("params:"), "{out}");
    let (code, out, _) = go(&["commands", "--json"]);
    assert_eq!(code, 0);
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok_and(|v| v.is_array()), "{out}");
    let (code, out, _) = go(&["controls"]);
    assert_eq!(code, 0);
    assert!(out.contains("light.exposure"), "{out}");
    let (code, out, _) = go(&["controls", "--json"]);
    assert_eq!(code, 0);
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok_and(|v| v.is_array()), "{out}");
}

#[test]
fn run_prints_one_json_line_per_command_and_exits_1_when_one_fails() {
    let (code, out, err) = go(&["run", "--demo", "engine.commands"]);
    assert_eq!(code, 0, "{err}");
    let line: serde_json::Value = serde_json::from_str(out.lines().next().unwrap_or("")).unwrap_or_default();
    assert_eq!(line["ok"], true, "{out}");
    let (code, out, err) = go(&["run", "--demo", "no.such.command"]);
    assert_eq!(code, 1, "{err}");
    assert!(out.contains("\"ok\":false"), "{out}");
    assert!(err.contains("1 command(s) failed"), "{err}");
}

#[test]
fn an_address_after_a_bare_connect_belongs_to_the_flag_and_a_command_id_does_not() {
    use std::ffi::OsString;
    let words = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
    let attached = |v: &[&str]| crate::attach_connect_addresses(words(v)).into_iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert_eq!(attached(&["run", "--connect", "7980", "ui.inspect"]), ["run", "--connect=7980", "ui.inspect"]);
    assert_eq!(attached(&["run", "--connect", "h:1", "ui.inspect"]), ["run", "--connect=h:1", "ui.inspect"]);
    assert_eq!(attached(&["run", "--connect", "ui.inspect"]), ["run", "--connect", "ui.inspect"]);
    assert_eq!(attached(&["run", "--connect", "k=1:2"]), ["run", "--connect", "k=1:2"]);
    assert_eq!(attached(&["run", "--", "--connect", "7980"]), ["run", "--", "--connect", "7980"]);
}

#[test]
fn render_refuses_a_name_that_is_not_unicode_with_exit_2() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_vec(b"bad-\xff.png".to_vec());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = [std::ffi::OsString::from("lightcraft-cli"), "render".into(), bad, "-o".into(), "o.png".into()];
        assert_eq!(run(args, &mut out, &mut err), 2);
        assert!(String::from_utf8_lossy(&err).contains("is not valid Unicode"));
    }
}

#[test]
fn the_error_type_maps_to_the_exit_codes() {
    use crate::error::CliError;
    assert_eq!(CliError::Usage("x".into()).exit_code(), 2);
    assert_eq!(CliError::NotUnicode("x".into()).exit_code(), 2);
    assert_eq!(CliError::StepsFailed(1).exit_code(), 1);
    assert_eq!(CliError::from("boom").exit_code(), 1);
    assert_eq!(CliError::from(String::from("boom")).to_string(), "boom");
}

// What follows runs the commands themselves in-process, on files made for the test in a folder of its own.

fn work(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lightcraft-cli-unit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    dir
}

fn gradient(path: &std::path::Path) {
    let img = lightcraft_raster::Rgba8::from_fn(120, 80, |x, y| [(x * 2) as u8, (y * 3) as u8, 100, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap_or_default();
    std::fs::write(path, png).unwrap_or_default();
}

#[test]
fn render_develops_a_file_and_refuses_what_it_cannot_do() {
    let dir = work("render");
    let (input, out) = (dir.join("in.png"), dir.join("out.jpg"));
    gradient(&input);
    let (i, o) = (input.to_str().unwrap_or(""), out.to_str().unwrap_or(""));
    let (code, _, err) = go(&["render", i, "-o", o, "--set", "light.exposure=1", "--size", "60", "--quality", "80", "--opt", "colorSpace=srgb"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("wrote"), "{err}");
    assert!(out.exists());
    // A control that does not exist, a preset that does not exist, an extension that is no format, the input as the output.
    for args in [
        &["render", i, "-o", o, "--set", "nope=1"][..],
        &["render", i, "-o", o, "--preset", "no-such-preset"],
        &["render", i, "-o", &format!("{o}.nope")],
        &["render", i, "-o", i],
        &["render", "/no/such/file.png", "-o", o],
    ] {
        let (code, out, err) = go(args);
        assert_eq!(code, 1, "{args:?}: {err}");
        assert!(out.is_empty() && err.starts_with("lightcraft-cli: "), "{args:?}: {out}{err}");
    }
    // A settings file that is neither settings nor a preset.
    let settings = dir.join("settings.json");
    std::fs::write(&settings, "[1, 2]").unwrap_or_default();
    let (code, _, err) = go(&["render", i, "-o", o, "--settings", settings.to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    std::fs::write(&settings, r#"{"light": {"exposure": 0.5}}"#).unwrap_or_default();
    let (code, _, err) = go(&["render", i, "-o", o, "--settings", settings.to_str().unwrap_or("")]);
    assert_eq!(code, 0, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn synth_merge_writes_inputs_and_merge_reads_them() {
    let dir = work("synth");
    let (code, out, err) = go(&["synth-merge", "pano", "-o", dir.to_str().unwrap_or("")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 5, "{out}");
    let (code, out, err) = go(&["synth-merge", "hdr", "-o", dir.to_str().unwrap_or("")]);
    assert_eq!(code, 0, "{err}");
    let files: Vec<&str> = out.lines().collect();
    assert_eq!(files.len(), 3, "{out}");
    let preview = dir.join("preview.png");
    let mut args =
        vec!["merge", "hdr", "--deghost", "low", "--no-align", "--auto-crop", "--no-auto-settings", "--preview", preview.to_str().unwrap_or("")];
    args.extend(files.iter().copied());
    let (code, out, err) = go(&args);
    assert_eq!(code, 0, "{err}");
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok_and(|v| v["seconds"].is_number()), "{out}");
    let (code, _, err) = go(&["merge", "hdr", "/no/such/photo.dng"]);
    assert_eq!(code, 1, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_renders_the_demo_library_to_a_png_and_reports_a_bad_script() {
    let dir = work("snapshot");
    let png = dir.join("shot.png");
    let (code, _, err) = go(&["snapshot", "--size", "400x300", "-o", png.to_str().unwrap_or("")]);
    assert_eq!(code, 0, "{err}");
    assert!(png.exists(), "{err}");
    let script = dir.join("script.jsonl");
    std::fs::write(&script, "{\"method\": \"ui.set\", \"params\": {\"view\": \"detail\"}}\n{\"method\": \"no.such.method\"}\n").unwrap_or_default();
    let (code, out, err) = go(&["snapshot", "--size", "400x300", "--script", script.to_str().unwrap_or(""), "-o", png.to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(err.contains("1 scripted request(s) failed"), "{err}");
    let (code, _, err) = go(&["snapshot", "--script", dir.join("none.jsonl").to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    // A library below a file can be neither opened nor created, on any system.
    let file = dir.join("a-file");
    std::fs::write(&file, "x").unwrap_or_default();
    let (code, _, err) = go(&["snapshot", "-o", png.to_str().unwrap_or(""), "--library", file.join("no-library").to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_reads_a_script_file_and_refuses_a_bad_one() {
    let dir = work("script");
    let script = dir.join("steps.jsonl");
    std::fs::write(&script, "# comment\n{\"command\": \"engine.commands\"}\n\n").unwrap_or_default();
    let (code, out, err) = go(&["run", "--demo", "--script", script.to_str().unwrap_or("")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 1, "{out}");
    for (text, needle) in [
        ("not json\n", "steps.jsonl:1"),
        ("{\"x\": 1}\n", "needs `command` or `method`"),
        ("{\"command\": \"a\", \"params\": 3}\n", "must be a JSON object"),
    ] {
        std::fs::write(&script, text).unwrap_or_default();
        let (code, out, err) = go(&["run", "--demo", "--script", script.to_str().unwrap_or("")]);
        assert_eq!(code, 1, "{err}");
        assert!(out.is_empty() && err.contains(needle), "{needle}: {out}{err}");
    }
    let (code, _, err) = go(&["run", "--demo", "--script", dir.join("none").to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn calibrate_bench_cull_and_denoise_serve_fail_cleanly_without_inputs() {
    let dir = work("calibrate");
    let (code, _, err) = go(&["calibrate", "--max", "0", "--out", dir.to_str().unwrap_or(""), dir.to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("0 of 0 raw files") && err.contains("no profile written"), "{err}");
    let (code, _, err) = go(&["bench-cull", dir.join("none").to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("not a folder"), "{err}");
    let (code, _, err) = go(&["denoise-serve", "--token-env", "LC_UNIT_TEST_NO_SUCH_VARIABLE"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("LC_UNIT_TEST_NO_SUCH_VARIABLE"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_library_or_import_that_fails_ends_the_run_with_exit_1() {
    let dir = work("no-library");
    let file = dir.join("a-file");
    std::fs::write(&file, "x").unwrap_or_default();
    let (code, _, err) = go(&["mcp", "--library", file.join("no-library").to_str().unwrap_or("")]);
    assert_eq!(code, 1, "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    let (code, out, err) = go(&["run", "--demo", "--import", "/no/such/photo.jpg", "engine.commands"]);
    assert_eq!(code, 1, "{err}");
    assert!(out.is_empty(), "{out}");
}
