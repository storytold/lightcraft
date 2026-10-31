//! `cargo xtask test [-p PKG]… [--keep-going] [--doc] [-- ARGS]`: the workspace tests, or some
//! packages' tests, under the same caps as `ci` and in its machine-wide slot.
//!
//! Plain `cargo test --workspace` takes no CI slot and gets only `.cargo/config.toml`'s 4 test
//! threads per binary (libtest's own default, one per CPU, ran the ui-egui tests alone out of memory
//! on a 16-core machine, #3057, #3059), not `ci`'s RAM-aware caps. This runs them the
//! way `ci` does: build jobs by available RAM, at most 4 test threads, line-table debuginfo
//! ([`super::use_ci_caps`]), one run per machine ([`ci_lock`]).
//!
//! It builds every workspace test target once (`cargo test --workspace --no-run`, the same
//! artifacts as `ci`'s test step, so nothing rebuilds between them) and then runs each test binary
//! itself ([`run_binary`]), passing it `ARGS` (a filter, `--exact`, `--skip`, `--nocapture`, …).
//! It stops at the first failing binary, as `ci` does; `--keep-going` runs every binary and names
//! each that failed. `--doc` runs the doctests after them. With `LIGHTCRAFT_NEXTEST=1` (and
//! cargo-nextest installed) the binaries run under `cargo nextest run` instead, as in `ci`.
//!
//! `ci --quick` runs the changed packages' tests through the same functions ([`selected`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::ci_lock;

/// What `cargo xtask test` was asked to run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args<'a> {
    /// `-p` packages; empty for the whole workspace.
    pub packages: BTreeSet<String>,
    /// Run every test binary and report all that failed, rather than stop at the first.
    pub keep_going: bool,
    /// Run the doctests too, after the test binaries.
    pub doc: bool,
    /// What follows `--`, passed to every test binary (and the doctests).
    pub test_args: Vec<&'a str>,
}

/// Parse the arguments after `cargo xtask test`.
pub fn parse<'a>(args: &[&'a str]) -> Result<Args<'a>, String> {
    let mut parsed = Args::default();
    let mut it = args.iter().copied();
    while let Some(arg) = it.next() {
        match arg {
            "--" => {
                parsed.test_args = it.by_ref().collect();
            }
            // `--no-fail-fast` is cargo's name for it
            "--keep-going" | "--no-fail-fast" => parsed.keep_going = true,
            "--doc" => parsed.doc = true,
            "-p" | "--package" => {
                let Some(pkg) = it.next().filter(|p| !p.is_empty() && !p.starts_with('-')) else {
                    return Err(format!("test: {arg} needs a package name"));
                };
                parsed.packages.insert(pkg.to_owned());
            }
            _ => match arg.strip_prefix("--package=").or_else(|| arg.strip_prefix("-p")).map(|p| p.trim_start_matches('=')).filter(|p| !p.is_empty())
            {
                Some(pkg) => {
                    parsed.packages.insert(pkg.to_owned());
                }
                None => return Err(format!("test: unknown option `{arg}` (test arguments go after `--`)")),
            },
        }
    }
    Ok(parsed)
}

/// A workspace package: its name and whether it has a library (so doctests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub lib: bool,
}

/// The workspace packages in `metadata` (`cargo metadata --no-deps`) by manifest path, or only
/// those in `only` (an error names any that aren't workspace packages).
pub fn packages(metadata: &serde_json::Value, only: &BTreeSet<String>) -> Result<BTreeMap<String, Package>, String> {
    let all: BTreeMap<String, Package> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let lib = p["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|t| t["kind"].as_array().into_iter().flatten().any(|k| matches!(k.as_str(), Some("lib" | "rlib" | "proc-macro"))));
            Some((p["manifest_path"].as_str()?.to_owned(), Package { name: p["name"].as_str()?.to_owned(), lib }))
        })
        .collect();
    let names: BTreeSet<&str> = all.values().map(|p| p.name.as_str()).collect();
    let unknown: Vec<&str> = only.iter().map(String::as_str).filter(|p| !names.contains(p)).collect();
    if !unknown.is_empty() {
        return Err(format!(
            "test: not a workspace package: {} (packages: {})",
            unknown.join(", "),
            names.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(all.into_iter().filter(|(_, p)| only.is_empty() || only.contains(&p.name)).collect())
}

/// A test binary cargo built: its package, the package's manifest and the executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    pub package: String,
    pub manifest: String,
    pub exe: String,
}

impl Binary {
    /// `package (file)`, how the log and the summary name it.
    pub fn label(&self) -> String {
        let file = Path::new(&self.exe).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{} ({file})", self.package)
    }
}

/// The test binaries of `packages` among the artifacts in cargo's JSON messages (`stdout` of a
/// `--message-format=json` build), by package and file name.
pub fn binaries(stdout: &str, packages: &BTreeMap<String, Package>) -> Vec<Binary> {
    let mut found: Vec<Binary> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|msg| msg["reason"] == "compiler-artifact" && msg["profile"]["test"] == true)
        .filter_map(|msg| {
            let (exe, manifest) = (msg["executable"].as_str()?, msg["manifest_path"].as_str()?);
            let package = packages.get(manifest)?;
            Some(Binary { package: package.name.clone(), manifest: manifest.to_owned(), exe: exe.to_owned() })
        })
        .collect();
    found.sort_by(|a, b| (&a.package, &a.exe).cmp(&(&b.package, &b.exe)));
    found.dedup();
    found
}

/// Build every workspace test target once and return the test binaries of `packages`. Building
/// with `-p` instead would resolve features per selection: different artifacts from `ci`'s
/// workspace build, so it would recompile ~130 crates.
pub fn build(packages: &BTreeMap<String, Package>, whole_workspace: bool) -> Result<Vec<Binary>, String> {
    let what = "cargo test --workspace --no-run --lib --bins --tests";
    let then = if whole_workspace { "every test binary".to_owned() } else { format!("the test binaries of {}", names(packages.values())) };
    eprintln!("$ {what} (then {then})");
    let out = super::cargo()
        .args(crate::ci::with_test_targets(&["test", "--workspace", "--no-run", "--message-format=json-render-diagnostics"]))
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what}: exited with {}", out.status));
    }
    Ok(binaries(&String::from_utf8_lossy(&out.stdout), packages))
}

fn names<'a>(packages: impl Iterator<Item = &'a Package>) -> String {
    packages.map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// Run one test binary as `cargo test` would (from its package's directory, with its
/// `CARGO_MANIFEST_DIR` and `CARGO_PKG_NAME`) under CI's caps ([`super::apply_ci_caps`]: where the
/// caller doesn't set it, `RUST_TEST_THREADS` is the RAM-aware `min(build jobs by available RAM, 4)`
/// of `ci`'s cargo runs; `.cargo/config.toml`'s default doesn't count, [`super::caller_sets`]), with
/// `args`; skips the tests record go to `skip_log`. The one place `test` and `ci --quick` start a
/// test binary and wait for it. The error says how it ended: its exit status, the signal that killed
/// it, or why it didn't start.
pub fn run_binary(root: &Path, binary: &Binary, args: &[&str], skip_log: Option<&Path>) -> Result<(), String> {
    let dir = Path::new(&binary.manifest).parent().unwrap_or(root);
    let mut c = Command::new(&binary.exe);
    c.current_dir(dir).env("CARGO_MANIFEST_DIR", dir).env("CARGO_PKG_NAME", &binary.package).args(args);
    if let Some(log) = skip_log {
        c.env(crate::ci_report::SKIP_LOG_ENV, log);
    }
    super::apply_ci_caps(&mut c);
    let shown = std::iter::once(binary.label()).chain(args.iter().map(|a| (*a).to_owned())).collect::<Vec<_>>().join(" ");
    eprintln!("$ {shown}");
    let status = c.status().map_err(|e| format!("failed to start: {e}"))?;
    if status.success() { Ok(()) } else { Err(describe(status)) }
}

/// How a process that failed ended: `exit status: 101`, or on Unix the signal that killed it
/// (`signal: 9 (SIGKILL)`), with a hint for SIGKILL, which is what the out-of-memory killer sends.
pub fn describe(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal() == Some(9) {
            return format!("{status}: killed, maybe for running out of memory");
        }
    }
    status.to_string()
}

/// The outcome of running a list of test binaries.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub passed: usize,
    /// The labels of the binaries that failed, each with how it ended.
    pub failed: Vec<(String, String)>,
    /// Binaries not run: the run stopped at the first failure.
    pub not_run: usize,
}

/// Run `binaries` in order with `run_one`; stop at the first failure unless `keep_going`.
pub fn run_all(binaries: &[Binary], keep_going: bool, mut run_one: impl FnMut(&Binary) -> Result<(), String>) -> Outcome {
    let mut outcome = Outcome::default();
    for (i, binary) in binaries.iter().enumerate() {
        let Err(error) = run_one(binary) else {
            outcome.passed += 1;
            continue;
        };
        outcome.failed.push((binary.label(), error));
        if !keep_going {
            outcome.not_run = binaries.len().saturating_sub(i + 1);
            break;
        }
    }
    outcome
}

/// `ci --quick`'s tests: the test binaries of `packages`, every one of them (each failure named).
/// Doctests are left to the full `ci`.
pub fn selected(root: &Path, packages: &BTreeSet<String>, skip_log: &Path) -> Result<(), String> {
    let packages = self::packages(&super::metadata()?, packages)?;
    let binaries = build(&packages, false)?;
    if binaries.is_empty() {
        println!("no test binaries in {}", names(packages.values()));
        return Ok(());
    }
    let outcome = run_all(&binaries, true, |b| run_binary(root, b, &[], Some(skip_log)));
    if outcome.failed.is_empty() {
        Ok(())
    } else {
        Err(format!("tests failed in {}", outcome.failed.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>().join(", ")))
    }
}

/// The `cargo nextest run` arguments for `args`: the workspace build, the `-p` packages as a
/// filter expression (so the build is still the workspace's), `--no-fail-fast` for `--keep-going`.
///
/// Nextest doesn't hand `ARGS` to the test binaries: after its `--` it takes only filters,
/// `--exact`, `--skip NAME`, `--ignored` and `--include-ignored`. `--test-threads N` (or `=N`) and
/// `--nocapture` become its runner options before `--`; any other option is an error naming it,
/// rather than nextest's own refusal after the slot was taken.
pub fn nextest_args<'a>(args: &Args<'a>) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = crate::ci::with_test_targets(&["--workspace"]).into_iter().map(str::to_owned).collect();
    if !args.packages.is_empty() {
        let filter = args.packages.iter().map(|p| format!("package(={p})")).collect::<Vec<_>>().join(" | ");
        v.extend(["-E".to_owned(), filter]);
    }
    if args.keep_going {
        v.push("--no-fail-fast".to_owned());
    }
    let mut filters = Vec::new();
    let mut it = args.test_args.iter().copied();
    while let Some(arg) = it.next() {
        match arg {
            "--" => {
                // nextest reads what follows its own `--` as filters too, `-`-prefixed or not
                filters.push(arg.to_owned());
                filters.extend(it.by_ref().map(str::to_owned));
            }
            "--exact" | "--ignored" | "--include-ignored" => filters.push(arg.to_owned()),
            "--skip" => {
                let Some(name) = it.next() else {
                    return Err("test: --skip needs a test name".to_owned());
                };
                filters.extend([arg.to_owned(), name.to_owned()]);
            }
            "--nocapture" | "--no-capture" => v.push("--no-capture".to_owned()),
            "--test-threads" => {
                let Some(n) = it.next() else {
                    return Err("test: --test-threads needs a number".to_owned());
                };
                v.extend(["--test-threads".to_owned(), n.to_owned()]);
            }
            _ if arg.starts_with("--test-threads=") => {
                v.extend(["--test-threads".to_owned(), arg.trim_start_matches("--test-threads=").to_owned()]);
            }
            _ if arg.starts_with('-') => {
                return Err(format!(
                    "test: `{arg}` can't reach the tests under nextest (LIGHTCRAFT_NEXTEST=1), which takes filters, --exact, --skip, --ignored, --include-ignored, --nocapture and --test-threads"
                ));
            }
            _ => filters.push(arg.to_owned()),
        }
    }
    if !filters.is_empty() {
        v.push("--".to_owned());
        v.extend(filters);
    }
    Ok(v)
}

/// The `cargo test --doc` arguments for `args`: the workspace's doctests, or those of the `-p`
/// packages that have a library (`None` when none has); `--no-fail-fast` for `--keep-going`, so a
/// package's failing doctests don't stop the next package's.
pub fn doc_args<'a>(args: &Args<'a>, packages: &BTreeMap<String, Package>) -> Option<Vec<String>> {
    let mut v = vec!["test".to_owned(), "--doc".to_owned()];
    if args.packages.is_empty() {
        v.push("--workspace".to_owned());
    } else {
        let libs: Vec<&Package> = packages.values().filter(|p| p.lib).collect();
        if libs.is_empty() {
            return None;
        }
        for p in libs {
            v.extend(["-p".to_owned(), p.name.clone()]);
        }
    }
    if args.keep_going {
        v.push("--no-fail-fast".to_owned());
    }
    if !args.test_args.is_empty() {
        v.push("--".to_owned());
        v.extend(args.test_args.iter().map(|a| (*a).to_owned()));
    }
    Some(v)
}

/// `cargo xtask test`.
pub fn run(raw: &[&str]) -> Result<(), String> {
    let args = parse(raw)?;
    super::use_ci_caps();
    let root = super::root();
    let nextest = if crate::ci::nextest() { Some(nextest_args(&args)?) } else { None };
    let command = std::iter::once("cargo xtask test").chain(raw.iter().copied()).collect::<Vec<_>>().join(" ");
    // held until the run ends: one ci or test run at a time per machine (ci_lock.rs)
    let _slot = ci_lock::acquire(&root, &command)?;
    let packages = packages(&super::metadata()?, &args.packages)?;
    let mut failed = Vec::new();
    let mut summary = Vec::new();
    if let Some(nextest) = nextest {
        let mut c = super::cargo();
        c.args(["nextest", "run"]).args(&nextest);
        if let Err(e) = super::run(c, &format!("cargo nextest run {}", nextest.join(" "))) {
            failed.push(("cargo nextest run".to_owned(), e));
        }
    } else {
        let binaries = build(&packages, args.packages.is_empty())?;
        if binaries.is_empty() {
            println!("no test binaries in {}", names(packages.values()));
        }
        let outcome = run_all(&binaries, args.keep_going, |b| run_binary(&root, b, &args.test_args, None));
        summary.push(format!("{} of {} test binaries passed", outcome.passed, binaries.len()));
        if outcome.not_run > 0 {
            summary.push(format!("{} not run after the first failure (--keep-going runs them)", outcome.not_run));
        }
        failed.extend(outcome.failed);
    }
    if args.doc && !failed.is_empty() && !args.keep_going {
        summary.push("doctests not run after the failure".to_owned());
    } else if args.doc {
        match doc_args(&args, &packages) {
            Some(doc) => {
                let mut c = super::cargo();
                c.args(&doc);
                if let Err(e) = super::run(c, &format!("cargo {}", doc.join(" "))) {
                    failed.push(("doctests".to_owned(), e));
                }
            }
            None => println!("no doctests: none of {} has a library", names(packages.values())),
        }
    }
    println!("\ntest summary: {}", if summary.is_empty() { "done".to_owned() } else { summary.join("; ") });
    if failed.is_empty() {
        println!("  all passed");
        return Ok(());
    }
    for (label, error) in &failed {
        println!("  FAIL  {label}: {error}");
    }
    Err(format!("tests failed in {}", failed.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>().join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_packages_keep_going_doc_and_test_args() {
        let a = parse(&[
            "-p",
            "lightcraft-geom",
            "--package",
            "xtask",
            "--package=lightcraft-raw",
            "-plightcraft-color",
            "--keep-going",
            "--doc",
            "--",
            "foo::",
            "--exact",
        ])
        .unwrap();
        assert_eq!(a.packages.iter().map(String::as_str).collect::<Vec<_>>(), ["lightcraft-color", "lightcraft-geom", "lightcraft-raw", "xtask"]);
        assert!(a.keep_going && a.doc);
        assert_eq!(a.test_args, ["foo::", "--exact"]);
        assert_eq!(parse(&[]).unwrap(), Args::default(), "no arguments: every package, fail fast, no doctests");
        assert!(parse(&["--no-fail-fast"]).unwrap().keep_going, "cargo's name for it");
        assert_eq!(parse(&["--", "--keep-going", "-p"]).unwrap().test_args, ["--keep-going", "-p"], "after `--` everything is the binaries'");
    }

    #[test]
    fn rejects_unknown_options_and_a_missing_package() {
        assert!(parse(&["--workspace"]).unwrap_err().contains("unknown option `--workspace`"));
        assert!(parse(&["foo"]).unwrap_err().contains("after `--`"), "a bare filter must follow `--`");
        assert!(parse(&["-p"]).unwrap_err().contains("needs a package name"));
        assert!(parse(&["-p", "--doc"]).unwrap_err().contains("needs a package name"));
        assert!(parse(&["--package="]).is_err());
        assert!(parse(&["-p="]).is_err());
    }

    fn metadata() -> serde_json::Value {
        serde_json::json!({"packages": [
            {"name": "a", "manifest_path": "/w/a/Cargo.toml", "targets": [{"kind": ["lib"]}, {"kind": ["test"]}]},
            {"name": "b", "manifest_path": "/w/b/Cargo.toml", "targets": [{"kind": ["bin"]}]},
            {"name": "c", "manifest_path": "/w/c/Cargo.toml", "targets": [{"kind": ["proc-macro"]}]},
        ]})
    }

    #[test]
    fn selects_workspace_packages_and_names_unknown_ones() {
        let all = packages(&metadata(), &BTreeSet::new()).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all["/w/b/Cargo.toml"], Package { name: "b".into(), lib: false });
        assert!(all["/w/c/Cargo.toml"].lib, "a proc-macro crate has doctests");
        let only = packages(&metadata(), &["b".to_owned()].into()).unwrap();
        assert_eq!(only.keys().collect::<Vec<_>>(), ["/w/b/Cargo.toml"]);
        let err = packages(&metadata(), &["a".to_owned(), "nope".to_owned()].into()).unwrap_err();
        assert!(err.contains("not a workspace package: nope") && err.contains("packages: a, b, c"), "{err}");
    }

    #[test]
    fn finds_the_test_binaries_of_the_selected_packages() {
        let sel = packages(&metadata(), &["a".to_owned()].into()).unwrap();
        let stdout = [
            r#"{"reason":"compiler-artifact","profile":{"test":true},"manifest_path":"/w/a/Cargo.toml","executable":"/t/debug/deps/it-2"}"#,
            r#"{"reason":"compiler-artifact","profile":{"test":false},"manifest_path":"/w/a/Cargo.toml","executable":null}"#,
            r#"{"reason":"compiler-artifact","profile":{"test":true},"manifest_path":"/w/b/Cargo.toml","executable":"/t/debug/deps/b-1"}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json",
            r#"{"reason":"compiler-artifact","profile":{"test":true},"manifest_path":"/w/a/Cargo.toml","executable":"/t/debug/deps/a-1"}"#,
        ]
        .join("\n");
        let found = binaries(&stdout, &sel);
        assert_eq!(found.iter().map(Binary::label).collect::<Vec<_>>(), ["a (a-1)", "a (it-2)"]);
        assert_eq!(found.first().map(|b| b.manifest.as_str()), Some("/w/a/Cargo.toml"));
    }

    fn bins(names: &[&str]) -> Vec<Binary> {
        names.iter().map(|n| Binary { package: (*n).to_owned(), manifest: format!("/w/{n}/Cargo.toml"), exe: format!("/t/{n}-0") }).collect()
    }

    #[test]
    fn keep_going_runs_every_binary_and_names_each_failure() {
        let binaries = bins(&["a", "b", "c", "d"]);
        let fail = |b: &Binary| if b.package == "b" || b.package == "d" { Err("exit 101".to_owned()) } else { Ok(()) };
        let mut ran = Vec::new();
        let outcome = run_all(&binaries, true, |b| {
            ran.push(b.package.clone());
            fail(b)
        });
        assert_eq!(ran, ["a", "b", "c", "d"]);
        let failed = vec![("b (b-0)".into(), "exit 101".into()), ("d (d-0)".into(), "exit 101".into())];
        assert_eq!(outcome, Outcome { passed: 2, failed, not_run: 0 });
    }

    #[test]
    fn without_keep_going_the_first_failure_stops_the_run() {
        let binaries = bins(&["a", "b", "c", "d"]);
        let mut ran = Vec::new();
        let outcome = run_all(&binaries, false, |b| {
            ran.push(b.package.clone());
            if b.package == "b" { Err("exit 101".to_owned()) } else { Ok(()) }
        });
        assert_eq!(ran, ["a", "b"]);
        assert_eq!(outcome, Outcome { passed: 1, failed: vec![("b (b-0)".into(), "exit 101".into())], not_run: 2 });
        assert_eq!(run_all(&binaries, false, |_| Ok(())), Outcome { passed: 4, failed: vec![], not_run: 0 });
    }

    #[test]
    fn nextest_keeps_the_workspace_build_and_filters_packages() {
        let all = parse(&[]).unwrap();
        assert_eq!(nextest_args(&all).unwrap(), ["--workspace", "--lib", "--bins", "--tests"]);
        let some = parse(&["-p", "b", "-p", "a", "--keep-going", "--", "foo"]).unwrap();
        assert_eq!(
            nextest_args(&some).unwrap(),
            ["--workspace", "--lib", "--bins", "--tests", "-E", "package(=a) | package(=b)", "--no-fail-fast", "--", "foo"]
        );
    }

    #[test]
    fn nextest_takes_test_threads_and_nocapture_before_its_separator() {
        let targets = ["--workspace", "--lib", "--bins", "--tests"];
        let nextest = |a: &[&str]| nextest_args(&parse(a).unwrap()).map(|v| v[targets.len()..].to_vec());
        assert_eq!(nextest(&["--", "--test-threads=1"]).unwrap(), ["--test-threads", "1"], "a runner option, not a filter");
        assert_eq!(
            nextest(&["--", "foo", "--test-threads", "2", "--nocapture", "--exact", "--skip", "bar", "--ignored"]).unwrap(),
            ["--test-threads", "2", "--no-capture", "--", "foo", "--exact", "--skip", "bar", "--ignored"]
        );
        assert_eq!(nextest(&["--", "--include-ignored", "--", "-odd"]).unwrap(), ["--", "--include-ignored", "--", "-odd"]);
        let err = nextest(&["--", "--show-output"]).unwrap_err();
        assert!(err.contains("`--show-output`") && err.contains("--test-threads"), "{err}");
        assert!(nextest(&["--", "--test-threads"]).unwrap_err().contains("needs a number"));
        assert!(nextest(&["--", "--skip"]).unwrap_err().contains("needs a test name"));
    }

    #[test]
    fn doctests_cover_the_workspace_or_the_selected_libraries() {
        let all = packages(&metadata(), &BTreeSet::new()).unwrap();
        assert_eq!(doc_args(&parse(&["--doc"]).unwrap(), &all).unwrap(), ["test", "--doc", "--workspace"]);
        let args = parse(&["-p", "a", "-p", "b", "--doc", "--", "x"]).unwrap();
        let sel = packages(&metadata(), &args.packages).unwrap();
        assert_eq!(doc_args(&args, &sel).unwrap(), ["test", "--doc", "-p", "a", "--", "x"], "b has no library");
        let keep_going = parse(&["-p", "a", "--doc", "--keep-going", "--", "x"]).unwrap();
        assert_eq!(doc_args(&keep_going, &sel).unwrap(), ["test", "--doc", "-p", "a", "--no-fail-fast", "--", "x"], "cargo's flag, before `--`");
        assert_eq!(doc_args(&parse(&["--keep-going"]).unwrap(), &all).unwrap(), ["test", "--doc", "--workspace", "--no-fail-fast"]);
        let bin_only = parse(&["-p", "b"]).unwrap();
        assert_eq!(doc_args(&bin_only, &packages(&metadata(), &bin_only.packages).unwrap()), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_says_how_the_binary_ended() {
        use std::os::unix::process::ExitStatusExt;
        // wait(2) statuses: an exit code in the high byte, a terminating signal in the low bits
        assert_eq!(describe(std::process::ExitStatus::from_raw(101 << 8)), "exit status: 101");
        let killed = describe(std::process::ExitStatus::from_raw(9));
        assert!(killed.contains("SIGKILL") && killed.contains("out of memory"), "{killed}");
        assert!(describe(std::process::ExitStatus::from_raw(11)).contains("SIGSEGV"));
    }

    #[test]
    fn a_binary_that_cannot_start_reports_why() {
        let missing = Binary { package: "a".into(), manifest: "/nonexistent/a/Cargo.toml".into(), exe: "/nonexistent/a/target/a-0".into() };
        let err = run_binary(Path::new("/nonexistent"), &missing, &[], None).unwrap_err();
        assert!(err.starts_with("failed to start:"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_binary_reports_its_exit_status() {
        let binary = Binary { package: "sh".into(), manifest: "/Cargo.toml".into(), exe: "/bin/sh".into() };
        let err = run_binary(Path::new("/"), &binary, &["-c", "exit 3"], None).unwrap_err();
        assert_eq!(err, "exit status: 3");
        let err = run_binary(Path::new("/"), &binary, &["-c", "kill -9 $$"], None).unwrap_err();
        assert!(err.contains("SIGKILL"), "{err}");
    }
}
