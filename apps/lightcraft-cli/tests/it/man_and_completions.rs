//! Man pages and completions: the example itself, so the test writes the files exactly as the
//! packages get them.

#[allow(dead_code)]
#[path = "../../examples/man-and-completions.rs"]
mod example;

use std::path::Path;

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lightcraft-cli-{name}-{}", std::process::id()))
}

fn written(dir: &Path) -> Vec<String> {
    let mut written: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    written.sort();
    written
}

#[test]
fn the_command_is_consistent_and_every_shell_gets_completions() {
    use clap::ValueEnum;
    lightcraft_cli::command().debug_assert();
    for &shell in clap_complete::Shell::value_variants() {
        let mut out = Vec::new();
        clap_complete::generate(shell, &mut lightcraft_cli::command(), "lightcraft-cli", &mut out);
        assert!(!out.is_empty(), "{shell}");
    }
}

/// The example writes the five completion files under the names the packages install, a page for
/// the command and a page per subcommand, and none for the `help` subcommand.
#[test]
fn the_example_writes_the_files_the_packages_install() {
    let dir = tmp("man-and-completions");
    let _ = std::fs::remove_dir_all(&dir);
    example::generate(&dir, "lightcraft-cli").unwrap();
    let written = written(&dir);
    for completion in ["lightcraft-cli.bash", "_lightcraft-cli", "lightcraft-cli.fish", "lightcraft-cli.elv", "_lightcraft-cli.ps1"] {
        assert!(written.iter().any(|w| w == completion), "{completion} in {written:?}");
    }
    assert!(!written.iter().any(|w| w.ends_with(".1") && w.contains("-help")), "{written:?}");
    for sub in lightcraft_cli::command().get_subcommands().filter(|s| s.get_name() != "help" && !s.is_hide_set()) {
        let page = format!("lightcraft-cli-{}.1", sub.get_name());
        assert!(written.contains(&page), "{page} in {written:?}");
    }
    assert!(written.iter().any(|w| w == "lightcraft-cli.1"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Nightly packages install the command as `lightcraft-nightly-cli`: its pages and completions are written under that name.
#[test]
fn the_example_can_write_them_for_another_command_name() {
    let dir = tmp("man-and-completions-nightly");
    let _ = std::fs::remove_dir_all(&dir);
    example::generate(&dir, "lightcraft-nightly-cli").unwrap();
    let written = written(&dir);
    assert!(written.iter().any(|w| w == "lightcraft-nightly-cli.1"), "{written:?}");
    assert!(written.iter().any(|w| w == "_lightcraft-nightly-cli"), "{written:?}");
    assert!(!written.iter().any(|w| w.contains("lightcraft-cli")), "{written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The FreeBSD package (staged from stubs in a dry run) installs the bash, zsh and fish completions
/// and the man pages.
#[cfg(unix)]
#[test]
fn the_freebsd_package_installs_completions_and_man_pages() {
    let work = tmp("freebsd-package");
    let _ = std::fs::remove_dir_all(&work);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/freebsd/package.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("--dry-run")
        .env("DIST", work.join("dist"))
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env_remove("CARGO_BUILD_TARGET")
        .output()
        .unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    for path in [
        "share/bash-completion/completions/lightcraft-cli",
        "share/zsh/site-functions/_lightcraft-cli",
        "share/fish/vendor_completions.d/lightcraft-cli.fish",
        "share/man/man1/lightcraft-cli.1",
    ] {
        assert!(stdout.contains(path), "{path} missing from the package:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&work);
}
