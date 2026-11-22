//! The launcher against a stand-in for the app: a script under the app's name in the same folder.
#![cfg(unix)]
// Windows, the launcher's only shipping target, has no stand-in here: a script can't take an
// `.exe` name. Its CI build checks the launcher is a console program (`package.ps1`).

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A folder with the launcher and, beside it under the app's name, a script that echoes its
/// arguments and standard input to standard output, writes to standard error and exits with 7.
fn install(tag: &str) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("cli-launcher-{tag}"));
    std::fs::copy(env!("CARGO_BIN_EXE_cli-launcher"), dir.path().join(lcc_brand::CLI_BINARY)).unwrap();
    let app = dir.path().join(lcc_brand::BINARY);
    std::fs::write(&app, "#!/bin/sh\necho \"args: $*\"\necho \"stdin: $(cat)\"\necho 'to stderr' >&2\nexit 7\n").unwrap();
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[test]
fn the_launcher_runs_the_app_as_the_cli_and_passes_everything_through() {
    let dir = install("through");
    let mut child = Command::new(dir.path().join(lcc_brand::CLI_BINARY))
        .args(["render", "a b.jpg", "-o", "out.jpg"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"from the terminal").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("args: --cli render a b.jpg -o out.jpg"), "the app gets --cli then the arguments as given: {stdout}");
    assert!(stdout.contains("stdin: from the terminal"), "the app reads the launcher's standard input: {stdout}");
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "to stderr", "standard error stays separate");
    assert_eq!(out.status.code(), Some(7), "the app's exit code is the launcher's");
}

#[test]
fn a_missing_app_is_an_error_that_names_it() {
    let dir = install("missing");
    std::fs::remove_file(dir.path().join(lcc_brand::BINARY)).unwrap();
    let out = Command::new(dir.path().join(lcc_brand::CLI_BINARY)).arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(1), "a launcher that can't start the app fails");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("could not start") && stderr.contains(lcc_brand::BINARY), "says what it couldn't start: {stderr}");
}
