//! Opening raw photos in the loupe, timed through the real app logic, headless (no compositor, so no frame
//! throttling) but with the GPU on: import FILES into an in-memory library, open the first in Detail, then step to
//! the next one once its neighbour prefetch is done and once more at once.
//! `cargo run --release -p lightcraft-ui-egui --example open_timeline -- a.ARW b.ARW c.ARW`
//! (`LIGHTCRAFT_PROFILE=1` adds the engine's stage timings; every line carries ms since start on stderr).
use lightcraft_engine::Session;
use lightcraft_ui_egui::{LightcraftApp, Services, headless::Headless};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let t0 = Instant::now();
    let ms = move || t0.elapsed().as_secs_f64() * 1e3;
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": files})).expect("import");
    eprintln!("{:9.1} imported {}", ms(), r["imported"]);
    let mut h = Headless::new(LightcraftApp::new(s, Services::default()), [1600.0, 1000.0], 1.0);
    let req = |h: &mut Headless, m: &str, p: Value| h.request(m, p, Duration::from_secs(60));
    let cmd = |h: &mut Headless, c: &str| req(h, "engine.execute", json!({"command": c}))["result"].clone();
    cmd(&mut h, "view.photoGrid");
    h.settle(Duration::from_secs(60));
    cmd(&mut h, "library.selectNone");
    let r = cmd(&mut h, "library.next");
    eprintln!("{:9.1} first: {r}", ms());
    let first = r["active"].as_u64().unwrap_or(1);
    h.settle(Duration::from_secs(60));
    let gpu = req(&mut h, "ui.inspect", json!({}))["result"]["perf"]["gpu"].clone();
    eprintln!("{:9.1} grid settled, gpu {gpu}", ms());
    // what the loupe shows, every frame, until `photo` is rendered for good
    let watch = |h: &mut Headless, label: &str, photo: u64| {
        let start = ms();
        let mut last = String::new();
        loop {
            let i = req(h, "ui.inspect", json!({}))["result"].clone();
            let l = &i["loupe"];
            let state = format!("photo={} source={} pending={}", l["photo"], l["source"], l["pending"]);
            if state != last {
                eprintln!(
                    "{:9.1} [{label}] +{:7.1} ms {state} (queue {}, in flight {})",
                    ms(),
                    ms() - start,
                    i["perf"]["renderQueue"],
                    i["perf"]["rendersInFlight"]
                );
                last = state;
            }
            let done = l["photo"].as_u64() == Some(photo) && matches!(l["source"].as_str(), Some("render" | "cached")) && l["pending"] == false;
            if done || ms() - start > 30_000.0 {
                return ms() - start;
            }
        }
    };
    eprintln!("{:9.1} open {first}", ms());
    cmd(&mut h, "view.detail");
    let open = watch(&mut h, "open", first);
    let t = ms();
    h.settle(Duration::from_secs(60));
    eprintln!("{:9.1} neighbours prefetched after {:.0} ms", ms(), ms() - t);
    // step towards the side that has photos (the sort may put the first one last)
    let mut dir = "library.next";
    let mut r = cmd(&mut h, dir);
    if r["active"].as_u64() == Some(first) {
        dir = "library.previous";
        r = cmd(&mut h, dir);
    }
    eprintln!("{:9.1} {dir}: {r}", ms());
    let n1 = r["active"].as_u64().unwrap_or(0);
    let step1 = watch(&mut h, "next (prefetched)", n1);
    let n2 = cmd(&mut h, dir)["active"].as_u64().unwrap_or(0);
    let step2 = watch(&mut h, "next at once", n2);
    h.settle(Duration::from_secs(60));
    eprintln!("{:9.1} summary: open {open:.0} ms, next prefetched {step1:.0} ms, next at once {step2:.0} ms", ms());
}
