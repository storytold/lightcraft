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
