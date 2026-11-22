//! The app's executable is also the command-line tool: started under the tool's name (the link the
//! Linux, BSD and macOS packages install) or with `--cli` first (what the Windows console launcher
//! passes), it behaves as `<cli>`: the same output on stdout and stderr and the same exit codes.

use std::process::{Command, Output, Stdio};

const APP: &str = env!("CARGO_BIN_EXE_app");

/// The app's executable started as the CLI with `--cli`.
fn with_option() -> Command {
    let mut c = Command::new(APP);
    c.arg("--cli");
    quiet(&mut c);
    c
}

/// No settings read or written, and no update check.
fn quiet(c: &mut Command) {
    c.env(lcc_brand::env_var("NO_PREFS"), "1").stdin(Stdio::null());
}

/// The ways the packages start the CLI: `--cli` everywhere, and a link under the CLI's name on Unix
/// (in a folder of the test's own, `tag`).
fn starts(tag: &str) -> Vec<(String, Command, Option<lcc_testkit::TempDir>)> {
    let option = ("--cli".to_string(), with_option(), None);
    #[cfg(unix)]
    {
        let dir = lcc_testkit::TempDir::new(&format!("app-as-cli-{tag}"));
        let link = dir.path().join(lcc_brand::CLI_BINARY);
        std::os::unix::fs::symlink(APP, &link).expect("a link to the app");
        let mut c = Command::new(&link);
        quiet(&mut c);
        vec![option, ("a link named <cli>".to_string(), c, Some(dir))]
    }
    #[cfg(not(unix))]
    {
        let _ = tag; // no links: Windows starts the CLI through the launcher, with `--cli`
        vec![option]
    }
}

fn run(mut c: Command, args: &[&str]) -> Output {
    c.args(args).output().expect("the app's executable starts")
}

#[test]
fn as_the_cli_it_prints_the_cli_s_version() {
    for (how, c, _dir) in starts("version") {
        let out = run(c, &["--version"]);
        assert!(out.status.success(), "{how}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{} {}\n", lcc_brand::CLI_BINARY, env!("CARGO_PKG_VERSION")), "{how}");
    }
}

#[test]
fn as_the_cli_its_results_go_to_stdout_as_json() {
    for (how, c, _dir) in starts("json") {
        let out = run(c, &["commands", "--json"]);
        assert!(out.status.success(), "{how}: {}", String::from_utf8_lossy(&out.stderr));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{how}: stdout is not JSON: {e}"));
        let ids: Vec<&str> = v.as_array().into_iter().flatten().filter_map(|c| c["id"].as_str()).collect();
        assert!(ids.contains(&"develop.auto"), "{how}: the engine's commands are listed");
    }
}

#[test]
fn as_the_cli_a_failing_command_exits_non_zero_with_its_error_on_stderr() {
    for (how, c, _dir) in starts("failing") {
        let out = run(c, &["run", "--demo", "no.such.command"]);
        assert_eq!(out.status.code(), Some(1), "{how}: {out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("\"ok\":false") && stdout.contains("no.such.command"), "{how}: the result line is on stdout: {stdout}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("1 command(s) failed"), "{how}: the summary is on stderr");
    }
}

#[test]
fn as_the_cli_an_unknown_subcommand_is_a_usage_error() {
    for (how, c, _dir) in starts("unknown") {
        let out = run(c, &["no-such-subcommand"]);
        assert_eq!(out.status.code(), Some(1), "{how}: {out:?}");
        assert!(out.stdout.is_empty(), "{how}: nothing on stdout");
        assert!(String::from_utf8_lossy(&out.stderr).contains("unknown subcommand `no-such-subcommand`"), "{how}");
    }
}

/// Under its own name and without `--cli` first, the executable is the app.
#[test]
fn under_its_own_name_it_is_the_app() {
    let mut c = Command::new(APP);
    quiet(&mut c);
    let out = run(c, &["--version"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{} {}\n", lcc_brand::BINARY, env!("CARGO_PKG_VERSION")));
}

/// The MCP server on stdio: protocol messages on stdout, its log on stderr, and a clean exit when the
/// client closes its input.
#[test]
fn as_the_cli_it_serves_mcp_on_stdio() {
    use std::io::{BufRead, Write};
    for (how, mut c, _dir) in starts("mcp") {
        let mut child = c.args(["mcp", "--demo"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("starts");
        let mut input = child.stdin.take().expect("stdin");
        input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n")
            .expect("request written");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().expect("stdout")).read_line(&mut line).expect("a reply");
        drop(input);
        let out = child.wait_with_output().expect("exits");
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{how}: the reply is not JSON ({e}): {line}"));
        assert_eq!(reply["id"], 1, "{how}: {reply}");
        assert!(reply["result"]["serverInfo"].is_object(), "{how}: an initialize result: {reply}");
        assert!(out.status.success(), "{how}: closing the input ends the server cleanly: {}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stderr).contains("serving MCP on stdio"), "{how}: the log is on stderr");
    }
}

/// A library the CLI holds names the CLI as its owner, not the app, whose executable it runs: the
/// CLI marks itself at its start (`lcc_updater::mark_command_line_tool`, which also keeps it from
/// updating the app).
#[test]
fn as_the_cli_it_names_itself_in_a_library_lock() {
    for (how, mut holder, _dir) in starts("lock") {
        let lib = lcc_testkit::TempDir::new(&format!("app-as-cli-lock-{}", how.len()));
        let lib_s = lib.path().join("lib");
        let lib_s = lib_s.to_str().expect("a UTF-8 temp path");
        let mut holder =
            holder.args(["mcp", "--library", lib_s]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("starts");
        let mut err = std::io::BufReader::new(holder.stderr.take().expect("stderr"));
        let mut line = String::new();
        while !line.contains("opened library") {
            line.clear();
            assert!(std::io::BufRead::read_line(&mut err, &mut line).expect("reads") > 0, "{how}: mcp exited before opening the library");
        }
        let out = run(with_option(), &["run", "--library", lib_s, "library.info"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{how}: the second process is refused");
        assert!(stderr.contains(&format!("already open in {}", lcc_brand::CLI_BINARY)), "{how}: the CLI holds it: {stderr}");
        drop(holder.stdin.take());
        assert!(holder.wait().expect("waits").success(), "{how}");
    }
}
