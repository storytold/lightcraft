//! P6.2 end to end: a library whose catalog store was truncated or garbled (a crash on a
//! network share, a bad disk, a sync tool) is refused with an error that says where its newest
//! backup is; nothing panics and the damaged files are left as they were for recovery.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dac-cli-robust-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn info(lib: &Path) -> (bool, String, String) {
    let o = Command::new(BIN).args(["run", "--library", lib.to_str().unwrap(), "library.info"]).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn damaged_catalog_is_refused_with_the_backup_path() {
    let lib = scratch("lib");
    let (ok, out, err) = info(&lib);
    assert!(ok, "creating the library: {out} {err}");
    let db = lib.join("catalog.redb");
    let good = std::fs::read(&db).unwrap();
    // a backup as the app writes them: `backups/<sortable UTC time>/`
    let backup = lib.join("backups").join("2026-10-01 120000");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("catalog.redb"), &good).unwrap();
    let damaged: [Vec<u8>; 3] =
        [good[..good.len() / 2].to_vec(), b"definitely not a database, just some text".repeat(100), good.iter().map(|b| b ^ 0x5a).collect()];
    for bad in damaged {
        std::fs::write(&db, &bad).unwrap();
        let (ok, out, err) = info(&lib);
        assert!(!ok, "a damaged catalog opened: {out}");
        assert!(!err.contains("panicked"), "{err}");
        assert!(err.contains(&backup.display().to_string()), "the error doesn't name the backup: {err}");
        // left as it was, for recovery tools
        assert_eq!(std::fs::read(&db).unwrap(), bad);
    }
    // restoring the backup brings the library back
    std::fs::copy(backup.join("catalog.redb"), &db).unwrap();
    let (ok, out, err) = info(&lib);
    assert!(ok, "{out} {err}");
    let _ = std::fs::remove_dir_all(&lib);
}
