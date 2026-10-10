//! Catalog v4 upgrade with progress on stderr (fork-owned; `main.rs` calls it before opening a library).

/// Upgrade an older library's catalog before it is opened, with the progress on stderr: a
/// one-time step that takes tens of seconds for a library of hundreds of thousands of photos.
pub fn migrate_with_progress(dir: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let path = std::path::Path::new(dir);
    if !dac_engine::library::needs_migration(path) {
        return Ok(());
    }
    eprintln!("{}: upgrading the catalog of {dir} to the current format (once; the old files are kept under backups/)", super::CLI);
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let printer = {
        let done = done.clone();
        std::thread::spawn(move || {
            let mut last = String::new();
            while !done.load(Ordering::Relaxed) {
                if let Some(p) = dac_engine::library::migration_progress() {
                    let line = p.describe();
                    if line != last {
                        eprintln!("  {line}");
                        last = line;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        })
    };
    let t = std::time::Instant::now();
    let r = dac_engine::library::migrate_library(path);
    done.store(true, Ordering::Relaxed);
    let _ = printer.join();
    r.map_err(|e| super::library_error(dir, e))?;
    eprintln!("{}: catalog upgraded in {:.1} s", super::CLI, t.elapsed().as_secs_f64());
    Ok(())
}
