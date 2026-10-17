//! Phase 0 gate: an install of a previous identity (`[legacy]` in brand.toml) is picked up on first
//! start — its settings folder is copied (never moved), its env-var prefix is still read, and its
//! catalog and preset file extensions still open.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-cli-legacy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The CLI with its config root pointed at `home` (Linux/BSD layout) and no inherited brand variables.
fn cli(home: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("HOME", home).env("XDG_CONFIG_HOME", home.join("cfg")).env("APPDATA", home.join("cfg"));
    for prefix in std::iter::once(lcc_brand::ENV_PREFIX).chain(lcc_brand::LEGACY_ENV_PREFIXES.iter().copied()) {
        c.env_remove(format!("{prefix}_NO_PREFS")).env_remove(format!("{prefix}_CONFIG_DIR"));
    }
    c
}

fn script(mut c: Command, lines: &str) -> Output {
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(lines.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn legacy_settings_folder_is_copied_on_first_start() {
    let home = scratch("settings");
    let legacy = lcc_brand::LEGACY_SETTINGS_DIRS.last().copied().unwrap();
    let old = home.join("cfg").join(lcc_brand::settings_dir_name(legacy));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("marker.json"), "{\"kept\": true}").unwrap();
    let o = cli(&home).arg("--version").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let new = home.join("cfg").join(lcc_brand::settings_dir_name(lcc_brand::SETTINGS_DIR));
    assert_eq!(std::fs::read_to_string(new.join("marker.json")).unwrap(), "{\"kept\": true}");
    assert!(new.join(lcc_brand::MIGRATED_MARKER).is_file());
    assert!(old.join("marker.json").is_file(), "the legacy folder is copied, never moved");
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn legacy_env_prefix_is_still_read() {
    let home = scratch("env");
    let legacy = lcc_brand::LEGACY_SETTINGS_DIRS.last().copied().unwrap();
    std::fs::create_dir_all(home.join("cfg").join(lcc_brand::settings_dir_name(legacy))).unwrap();
    // <LEGACY>_NO_PREFS=1 must act like <PREFIX>_NO_PREFS=1: no settings folder is created
    let var = format!("{}_NO_PREFS", lcc_brand::LEGACY_ENV_PREFIXES.last().copied().unwrap());
    let o = cli(&home).env(&var, "1").arg("--version").output().unwrap();
    assert!(o.status.success());
    assert!(!home.join("cfg").join(lcc_brand::settings_dir_name(lcc_brand::SETTINGS_DIR)).exists(), "{var} was ignored");
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("{var} is deprecated")));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_catalog_extension_opens() {
    let home = scratch("catalog");
    let lib = home.join("lib");
    let o = cli(&home).args(["run", "--library", lib.to_str().unwrap(), "library.info"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let entry = lcc_catalog_entry(&lib);
    for ext in lcc_brand::LEGACY_CATALOG_EXTS {
        let old = lib.join(format!("old.{ext}"));
        std::fs::rename(lcc_catalog_entry_any(&lib), &old).unwrap();
        assert!(!entry.exists());
        assert_eq!(lcc_catalog_entry_any(&lib), old);
        // the folder holds only the legacy entry point: it must be found and opened, not refused
        let o = cli(&home).args(["run", "--library", lib.to_str().unwrap(), "library.info"]).output().unwrap();
        assert!(o.status.success(), ".{ext}: {}", String::from_utf8_lossy(&o.stderr));
        assert!(old.is_file() || entry.is_file());
    }
    let _ = std::fs::remove_dir_all(&home);
}

fn lcc_catalog_entry(lib: &Path) -> PathBuf {
    lib.join(format!("{}.{}", lib.file_name().unwrap().to_string_lossy(), lcc_brand::CATALOG_EXT))
}

/// The entry-point file, whatever catalog extension it currently has.
fn lcc_catalog_entry_any(lib: &Path) -> PathBuf {
    std::fs::read_dir(lib)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| lcc_brand::catalog_exts().any(|c| c == x)))
        .unwrap()
}

#[test]
fn legacy_preset_extensions_import() {
    let home = scratch("preset");
    let current = home.join(format!("warm.{}", lcc_brand::PRESET_EXT));
    let make = format!(
        "{{\"command\": \"preset.create\", \"params\": {{\"name\": \"Warm\"}}}}\n{{\"command\": \"preset.export\", \"params\": {{\"path\": {}}}}}\n",
        serde_json::to_string(current.to_str().unwrap()).unwrap()
    );
    let mut c = cli(&home);
    c.args(["run", "--demo", "--script", "-"]);
    let o = script(c, &make);
    assert!(o.status.success(), "{} {}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(current.is_file());
    for ext in lcc_brand::LEGACY_PRESET_EXTS {
        let old = home.join(format!("warm.{ext}"));
        std::fs::copy(&current, &old).unwrap();
        let line = format!(
            "{{\"command\": \"preset.import\", \"params\": {{\"paths\": [{}], \"dryRun\": true}}}}\n",
            serde_json::to_string(old.to_str().unwrap()).unwrap()
        );
        let mut c = cli(&home);
        c.args(["run", "--demo", "--script", "-"]);
        let o = script(c, &line);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(o.status.success(), ".{ext}: {out} {}", String::from_utf8_lossy(&o.stderr));
        let v: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(v["result"]["imported"].as_array().map(Vec::len), Some(1), ".{ext}: {v}");
    }
    let _ = std::fs::remove_dir_all(&home);
}
