//! Fuzz target `lightroom` (`cargo xtask fuzz lightroom`): mutated Lightroom Classic catalogs (the
//! synthetic SQLite file from `lightroom_fixture`, plus corpus `.lrcat` files) through the pure-Rust
//! SQLite reader (schema, every table) and, every eighth input, the whole catalog read as an import
//! runs it. A short run in `cargo test`; time-boxed long runs under `LCC_FUZZ_SECS` (see
//! `lcc-fuzzkit`). Crash inputs go into the catalog reader's hostile-input tests.

use crate::lightroom_fixture::{dir, one_photo, write};
use crate::lightroom_sqlite::Database;

#[test]
fn fuzz_target_lightroom() {
    let d = dir("fuzz");
    let seed_path = d.join("seed.lrcat");
    write(&seed_path, &one_photo(&d, "s = { Exposure2012 = 0.5, ToneCurvePV2012 = { 0, 0, 255, 255 } }"));
    let seeds = [std::fs::read(&seed_path).unwrap()];
    let probe = d.join("probe.lrcat");
    let mut n = 0usize;
    lcc_fuzzkit::run_target("lightroom", &seeds, &["lrcat"], 300, |bytes| {
        if let Ok(db) = Database::open_with_wal(bytes.to_vec(), &[])
            && let Ok(tables) = db.live_tables()
        {
            for t in tables.iter().take(64) {
                let cols = t.column_names.as_ref().map_or(4, Vec::len);
                let _ = db.read_table(t.rootpage, cols);
            }
        }
        n += 1;
        if n.is_multiple_of(8) && std::fs::write(&probe, bytes).is_ok() {
            let _ = crate::lightroom_catalog::read(&probe);
        }
    });
    let _ = std::fs::remove_dir_all(&d);
}
