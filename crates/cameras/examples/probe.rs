//! List the connected cameras and what `lightcraft-cameras` reads from them, for checking a new
//! device by hand:
//!
//!     cargo run -p lightcraft-cameras --example probe            # list cameras and their files
//!     cargo run -p lightcraft-cameras --example probe -- copy DIR # also copy the newest file to DIR
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let copy_to = (args.first().map(String::as_str) == Some("copy")).then(|| args.get(1).cloned()).flatten();
    let t = Instant::now();
    let cams = match lightcraft_cameras::cameras() {
        Ok(c) => c,
        Err(e) => return eprintln!("cameras: {e}"),
    };
    println!("{} camera(s) in {:.2?}", cams.len(), t.elapsed());
    let cancel = AtomicBool::new(false);
    for c in cams {
        println!("\n{} — {} — {}", c.name, c.manufacturer, c.id);
        let t = Instant::now();
        let s = match lightcraft_cameras::Session::open(&c.id) {
            Ok(s) => s,
            Err(e) => {
                println!("  open: {e}");
                continue;
            }
        };
        let files = match s.files(&cancel, &mut |_| {}) {
            Ok(f) => f,
            Err(e) => {
                println!("  files: {e}");
                continue;
            }
        };
        let bytes: u64 = files.iter().filter_map(|f| f.size).sum();
        let dated = files.iter().filter(|f| f.created.is_some()).count();
        println!("  {} files, {:.1} GB, {dated} with a date, listed in {:.2?}", files.len(), bytes as f64 / 1e9, t.elapsed());
        for f in files.iter().take(3).chain(files.iter().rev().take(3)) {
            println!("  {}/{}  {:?} bytes  {:?}", f.folder, f.name, f.size, f.created);
        }
        if let (Some(dir), Some(f)) = (&copy_to, files.last()) {
            let path = std::path::Path::new(dir).join(&f.name);
            let t = Instant::now();
            let r = std::fs::File::create(&path)
                .map_err(|e| lightcraft_cameras::Error::Write(e.to_string()))
                .and_then(|mut out| s.copy_to(f, &mut out, &cancel, &mut |_| {}));
            match r {
                Ok(n) => println!(
                    "  copied {} ({n} bytes) in {:.2?} = {:.1} MB/s",
                    path.display(),
                    t.elapsed(),
                    n as f64 / 1e6 / t.elapsed().as_secs_f64()
                ),
                Err(e) => println!("  copy: {e}"),
            }
        }
    }
}
