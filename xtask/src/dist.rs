//! Branded binaries and packaging templates.
//!
//! Cargo `[[bin]]` names can't read `brand.toml`, so the binaries keep neutral internal names (`app`, `app-cli`).
//! These commands copy them under `brand.binary` / `brand.cli_binary`, and render the `*.in` templates under
//! `packaging/` (with `{{key}}` placeholders from `brand.toml`):
//!
//! - `cargo xtask run [--release] [ARGS…]`: build, copy to `target/<profile>/branded/`, run the branded app;
//! - `cargo xtask install [--prefix DIR]`: release build into `DIR/bin` (default `~/.local`), plus man page,
//!   shell completions and (Linux) the desktop entry, MIME type, metainfo and icons;
//! - `cargo xtask package [--skip-build]`: release build and every rendered template into `target/package/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::brand::{Brand, render};

const APP_BIN: &str = "app";
const CLI_BIN: &str = "app-cli";

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).map_or_else(|| root.join("target"), |d| if d.is_absolute() { d } else { root.join(d) })
}

fn build(release: bool) -> Result<(), String> {
    let mut c = crate::cargo();
    c.args(["build", "-p", "dac-app", "-p", "dac-cli"]);
    if release {
        c.arg("--release");
    }
    crate::run(c, if release { "cargo build --release -p dac-app -p dac-cli" } else { "cargo build -p dac-app -p dac-cli" })
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::copy(from, to).map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Copies `app`/`app-cli` from `bin_dir` to `out/<binary>` and `out/<cli_binary>`; returns the two paths.
fn branded_copies(bin_dir: &Path, out: &Path, brand: &Brand) -> Result<(PathBuf, PathBuf), String> {
    let app = out.join(exe(brand.req("binary")?));
    let cli = out.join(exe(brand.req("cli_binary")?));
    copy(&bin_dir.join(exe(APP_BIN)), &app)?;
    copy(&bin_dir.join(exe(CLI_BIN)), &cli)?;
    Ok((app, cli))
}

/// Template variables: the brand plus `version`, `short_version` (numeric X.Y.Z), `date` and `build_sha`.
pub fn vars(root: &Path, brand: &Brand) -> Result<BTreeMap<String, String>, String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).map_err(|e| format!("Cargo.toml: {e}"))?;
    let version = crate::version::read(&manifest)?;
    let mut vars = brand.vars();
    vars.insert("short_version".into(), version.split('-').next().unwrap_or(&version).to_string());
    vars.insert("version".into(), version);
    let date = std::env::var("BUILD_DATE").ok().filter(|d| !d.is_empty()).unwrap_or_else(today);
    vars.insert("date".into(), date);
    let sha = Command::new("git").current_dir(root).args(["rev-parse", "HEAD"]).output().ok().filter(|o| o.status.success());
    let sha = sha.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "unknown".into());
    vars.insert("build_sha".into(), sha);
    Ok(vars)
}

/// Today's UTC date as YYYY-MM-DD (civil-from-days, no dependencies).
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Every `*.in` template below `packaging/`, as `(source, output path relative to the output dir)`. The output
/// drops `.in` and replaces `{app_id}` in the file name with the brand's app id.
pub fn templates(root: &Path, app_id: &str) -> Vec<(PathBuf, PathBuf)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "in") {
                out.push(p);
            }
        }
    }
    let base = root.join("packaging");
    let mut files = Vec::new();
    walk(&base, &mut files);
    files
        .into_iter()
        .filter_map(|src| {
            let rel = src.strip_prefix(&base).ok()?.to_string_lossy().replace('\\', "/");
            let rel = rel.strip_suffix(".in")?.replace("{app_id}", app_id);
            Some((src, PathBuf::from(rel)))
        })
        .collect()
}

/// Renders every packaging template into `out`; returns the written paths.
pub fn render_templates(root: &Path, out: &Path, vars: &BTreeMap<String, String>) -> Result<Vec<PathBuf>, String> {
    let app_id = vars.get("app_id").ok_or("brand.toml: missing app_id")?;
    let mut written = Vec::new();
    for (src, rel) in templates(root, app_id) {
        let text = std::fs::read_to_string(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        let text = render(&text, vars).map_err(|e| format!("{}: {e}", src.display()))?;
        let dest = out.join(rel);
        write(&dest, &text)?;
        #[cfg(unix)]
        if dest.extension().is_some_and(|e| e == "sh") {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
        }
        written.push(dest);
    }
    Ok(written)
}

// ---- man page and completions ---------------------------------------------------------------------

/// Subcommands named in the CLI's help: lines that start with `  <cli> <word>`.
pub fn subcommands(help: &str, cli: &str) -> Vec<String> {
    let mut subs: Vec<String> = help
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix(cli)?.strip_prefix(' ').map(str::trim_start))
        .filter_map(|rest| {
            let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            (!word.is_empty() && !word.starts_with('-') && word.chars().next().is_some_and(|c| c.is_ascii_lowercase())).then_some(word)
        })
        .collect();
    subs.sort();
    subs.dedup();
    subs
}

fn roff_escape(s: &str) -> String {
    s.lines()
        .map(|l| {
            let l = l.replace('\\', "\\e");
            if l.starts_with('.') || l.starts_with('\'') { format!("\\&{l}") } else { l }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A man(7) page: NAME, SYNOPSIS and the `--help` text verbatim.
pub fn man_page(name: &str, summary: &str, version: &str, date: &str, help: &str) -> String {
    format!(
        ".TH \"{upper}\" 1 \"{date}\" \"{name} {version}\" \"User Commands\"\n.SH NAME\n{name} \\- {summary}\n.SH SYNOPSIS\n.B {name}\n\
         .I COMMAND\n[\\fIOPTIONS\\fR]\n.SH DESCRIPTION\n.nf\n{body}\n.fi\n",
        upper = name.to_uppercase(),
        summary = roff_escape(summary),
        body = roff_escape(help.trim_end()),
    )
}

pub fn completions(cli: &str, subs: &[String]) -> [(String, String); 3] {
    let words = subs.join(" ");
    let func = cli.replace(['-', '.'], "_");
    let bash = format!(
        "# bash completion for {cli}\n_{func}() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  if [ \"$COMP_CWORD\" -eq 1 ]; then\n    \
         COMPREPLY=($(compgen -W \"{words} --help --version\" -- \"$cur\"))\n  else\n    COMPREPLY=($(compgen -f -- \"$cur\"))\n  fi\n}}\n\
         complete -o filenames -F _{func} {cli}\n"
    );
    let zsh = format!(
        "#compdef {cli}\n_{func}() {{\n  if (( CURRENT == 2 )); then\n    compadd -- {words} --help --version\n  else\n    _files\n  fi\n}}\n_{func} \"$@\"\n"
    );
    let fish = format!("complete -c {cli} -f -n __fish_use_subcommand -a '{words}'\ncomplete -c {cli} -n 'not __fish_use_subcommand' -F\n");
    [
        (format!("bash-completion/completions/{cli}"), bash),
        (format!("zsh/site-functions/_{cli}"), zsh),
        (format!("fish/vendor_completions.d/{cli}.fish"), fish),
    ]
}

/// Writes `share/man/man1/<cli>.1` and the bash/zsh/fish completions below `share` from the branded CLI's help.
fn docs_for_cli(cli: &Path, share: &Path, brand: &Brand, vars: &BTreeMap<String, String>) -> Result<Vec<PathBuf>, String> {
    let out = Command::new(cli).arg("--help").output().map_err(|e| format!("{} --help: {e}", cli.display()))?;
    let help = String::from_utf8_lossy(&out.stdout);
    let name = brand.req("cli_binary")?;
    let summary = format!("{} command line ({})", brand.req("display_name")?, brand.get("tagline").unwrap_or(""));
    let mut written = Vec::new();
    let man = share.join(format!("man/man1/{name}.1"));
    write(&man, &man_page(name, &summary, &vars["version"], &vars["date"], &help))?;
    written.push(man);
    for (rel, text) in completions(name, &subcommands(&help, name)) {
        let p = share.join(rel);
        write(&p, &text)?;
        written.push(p);
    }
    Ok(written)
}

// ---- commands -------------------------------------------------------------------------------------

pub fn cmd_run(root: &Path, args: &[&str]) -> Result<(), String> {
    let release = args.first() == Some(&"--release");
    let rest = if release { &args[1..] } else { args };
    let rest = rest.strip_prefix(&["--"]).unwrap_or(rest);
    build(release)?;
    let brand = Brand::current()?;
    let bin_dir = target_dir(root).join(if release { "release" } else { "debug" });
    let (app, _) = branded_copies(&bin_dir, &bin_dir.join("branded"), &brand)?;
    eprintln!("$ {} {}", app.display(), rest.join(" "));
    let status = Command::new(&app).args(rest).status().map_err(|e| format!("{}: {e}", app.display()))?;
    if status.success() { Ok(()) } else { Err(format!("{} exited with {status}", app.display())) }
}

pub fn cmd_install(root: &Path, args: &[&str]) -> Result<(), String> {
    let mut prefix: Option<PathBuf> = None;
    let mut skip_build = false;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--prefix" => {
                i += 1;
                prefix = Some(PathBuf::from(args.get(i).ok_or("--prefix needs a directory")?));
            }
            a if a.starts_with("--prefix=") => prefix = Some(PathBuf::from(&a["--prefix=".len()..])),
            "--skip-build" => skip_build = true,
            a => return Err(format!("install: unknown argument `{a}`")),
        }
        i += 1;
    }
    let prefix = match prefix {
        Some(p) => p,
        None => PathBuf::from(std::env::var_os("HOME").ok_or("install: HOME is not set; pass --prefix")?).join(".local"),
    };
    if !skip_build {
        build(true)?;
    }
    let brand = Brand::current()?;
    let vars = vars(root, &brand)?;
    let (_, cli) = branded_copies(&target_dir(root).join("release"), &prefix.join("bin"), &brand)?;
    let share = prefix.join("share");
    let mut written = docs_for_cli(&cli, &share, &brand, &vars)?;
    if cfg!(target_os = "linux") || cfg!(target_os = "freebsd") {
        written.extend(install_freedesktop(root, &share, &vars)?);
    }
    println!("installed {} and {} into {}", brand.req("binary")?, brand.req("cli_binary")?, prefix.join("bin").display());
    for p in &written {
        println!("  {}", p.display());
    }
    Ok(())
}

/// Desktop entry, MIME type, metainfo and hicolor icons (renamed to the app id).
fn install_freedesktop(root: &Path, share: &Path, vars: &BTreeMap<String, String>) -> Result<Vec<PathBuf>, String> {
    let app_id = &vars["app_id"];
    let rendered = target_dir(root).join("package");
    render_templates(root, &rendered, vars)?;
    let mut written = Vec::new();
    for (from, to) in [
        (format!("linux/{app_id}.desktop"), format!("applications/{app_id}.desktop")),
        (format!("linux/{app_id}.mime.xml"), format!("mime/packages/{app_id}.xml")),
        (format!("linux/{app_id}.metainfo.xml"), format!("metainfo/{app_id}.metainfo.xml")),
    ] {
        copy(&rendered.join(&from), &share.join(&to))?;
        written.push(share.join(to));
    }
    written.extend(copy_icons(root, &share.join("icons"), app_id)?);
    Ok(written)
}

/// `assets/app-icon/hicolor/<size>/apps/app.{png,svg}` → `<dest>/hicolor/<size>/apps/<app_id>.{png,svg}`.
fn copy_icons(root: &Path, dest: &Path, app_id: &str) -> Result<Vec<PathBuf>, String> {
    let src = root.join("assets/app-icon/hicolor");
    let mut written = Vec::new();
    let Ok(sizes) = std::fs::read_dir(&src) else { return Ok(written) };
    for size in sizes.flatten() {
        let apps = size.path().join("apps");
        let Ok(files) = std::fs::read_dir(&apps) else { continue };
        for f in files.flatten() {
            let p = f.path();
            let Some(ext) = p.extension().and_then(|e| e.to_str()) else { continue };
            let to = dest.join("hicolor").join(size.file_name()).join("apps").join(format!("{app_id}.{ext}"));
            copy(&p, &to)?;
            written.push(to);
        }
    }
    Ok(written)
}

pub fn cmd_package(root: &Path, args: &[&str]) -> Result<(), String> {
    let skip_build = args.contains(&"--skip-build");
    let render_only = args.contains(&"--render-only");
    if let Some(a) = args.iter().find(|a| !matches!(**a, "--skip-build" | "--render-only")) {
        return Err(format!("package: unknown argument `{a}`"));
    }
    let brand = Brand::current()?;
    let vars = vars(root, &brand)?;
    let out = target_dir(root).join("package");
    let _ = std::fs::remove_dir_all(&out);
    let mut written = render_templates(root, &out, &vars)?;
    if !render_only {
        if !skip_build {
            build(true)?;
        }
        let (app, cli) = branded_copies(&target_dir(root).join("release"), &out.join("bin"), &brand)?;
        written.push(app);
        written.push(cli.clone());
        written.extend(docs_for_cli(&cli, &out.join("share"), &brand, &vars)?);
        written.extend(copy_icons(root, &out.join("share/icons"), &vars["app_id"])?);
    }
    println!("{} {} packaging files in {}:", brand.req("display_name")?, vars["version"], out.display());
    for p in &written {
        println!("  {}", p.strip_prefix(&out).unwrap_or(p).display());
    }
    println!(
        "platform installers: packaging/linux/package.sh, packaging/macos/package.sh, packaging/windows/package.ps1, packaging/freebsd/package.sh"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_subcommands_in_help() {
        let help = "x-cli — headless\n\nUSAGE:\n  x-cli run [OPTIONS] COMMAND\n  x-cli mcp [OPTIONS]\n  x-cli --version | --help\n  x-cli commands [--json]   list\n";
        assert_eq!(subcommands(help, "x-cli"), vec!["commands", "mcp", "run"]);
    }

    #[test]
    fn man_page_escapes_leading_dots() {
        let page = man_page("x-cli", "tool", "1.0", "2026-01-01", ".hidden\n  a\\b");
        assert!(page.contains("\\&.hidden"));
        assert!(page.contains("a\\eb"));
        assert!(page.starts_with(".TH \"X-CLI\" 1"));
    }

    #[test]
    fn date_is_iso() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }

    #[test]
    fn every_packaging_template_renders() {
        let root = crate::root();
        let brand = Brand::load(&root.join(crate::brand::TEST_BRAND)).unwrap();
        let vars = vars(&root, &brand).unwrap();
        let list = templates(&root, &vars["app_id"]);
        assert!(!list.is_empty());
        for (src, rel) in list {
            let text = std::fs::read_to_string(&src).unwrap();
            let out = render(&text, &vars).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
            let real = Brand::load(&root.join("brand.toml")).unwrap();
            let name = real.get("binary").unwrap().to_ascii_lowercase();
            assert!(!out.to_ascii_lowercase().contains(&name), "{}: leaks the workspace brand", src.display());
            assert!(!rel.to_string_lossy().contains('{'), "{}", rel.display());
        }
    }
}
