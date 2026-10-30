//! M0.9 acceptance: drive LightCraft over MCP (stdio framing), set exposure and render an image.
//! Runs against the headless backend directly and through the TCP control-channel transport
//! (`Remote`) to a stand-in control server.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lightcraft_mcp::{Backend, Headless, ProgressHook, Remote, Server, base64_decode, call_tool};
use serde_json::{Value, json};

/// Feed a whole session through `Server::serve` and return the replies by id.
fn session(server: &mut Server, msgs: &[Value]) -> Vec<Value> {
    let input: String = msgs.iter().map(|m| format!("{m}\n")).collect();
    let mut out = Vec::new();
    server.serve(Cursor::new(input), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

/// Mean sRGB value of the PNG in an MCP image result.
fn mean_of(result: &Value) -> (f64, u32, u32) {
    let img = result["content"].as_array().unwrap().iter().find(|c| c["type"] == "image").unwrap_or_else(|| panic!("no image in {result}"));
    assert_eq!(img["mimeType"], "image/png");
    let png = base64_decode(img["data"].as_str().unwrap()).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let d = lightcraft_codecs::decode(&png, Default::default()).unwrap();
    let rgba = d.to_srgb8();
    let sum: u64 = rgba.data.iter().map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64).sum();
    (sum as f64 / (3 * rgba.data.len()) as f64, d.width, d.height)
}

fn exposure_roundtrip(server: &mut Server) {
    let replies = session(
        server,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            call(3, "query_photos", json!({"limit": 1})),
        ],
    );
    assert_eq!(replies.len(), 3, "notifications get no reply");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "lightcraft");
    let tools: Vec<&str> = replies[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"set_develop") && tools.contains(&"render_photo") && tools.contains(&"cmd_develop_set"));
    let id = replies[2]["result"]["structuredContent"]["photos"][0]["id"].as_u64().expect("a photo");

    let replies = session(
        server,
        &[
            call(10, "render_photo", json!({"id": id, "size": 256})),
            call(11, "set_develop", json!({"id": id, "values": {"light.exposure": 1.5}})),
            call(12, "get_develop", json!({"id": id})),
            call(13, "render_photo", json!({"id": id, "size": 256})),
            // The generated per-command tool works too: exposure -1.5 via `cmd_develop_set`.
            call(14, "cmd_develop_set", json!({"control": "light.exposure", "value": -1.5})),
            call(15, "render_photo", json!({"size": 256})),
        ],
    );
    for r in &replies {
        assert!(r["error"].is_null() && r["result"]["isError"] == false, "{r}");
    }
    assert_eq!(replies[1]["result"]["structuredContent"]["controls"][0]["value"], 1.5);
    assert_eq!(replies[2]["result"]["structuredContent"]["light"]["exposure"], 1.5);
    let (before, w, h) = mean_of(&replies[0]["result"]);
    let (brighter, w2, h2) = mean_of(&replies[3]["result"]);
    let (darker, _, _) = mean_of(&replies[5]["result"]);
    assert_eq!((w, h), (w2, h2));
    assert!(w.max(h) <= 256 && w.max(h) >= 200, "{w}x{h}");
    println!("mean sRGB: exposure 0 → {before:.1}, +1.5 → {brighter:.1}, -1.5 → {darker:.1} ({w}×{h})");
    assert!(brighter > before + 15.0, "exposure +1.5 should brighten: {before} → {brighter}");
    assert!(darker < before - 15.0, "exposure -1.5 should darken: {before} → {darker}");
}

#[test]
fn file_writers_advertise_destructive_effects() {
    for with_commands in [false, true] {
        let mut server = Server::new(Box::new(Headless::demo())).with_command_tools(with_commands);
        let replies = session(&mut server, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
        let tools = replies[0]["result"]["tools"].as_array().unwrap();
        for name in ["export", "render_photo"] {
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["annotations"]["readOnlyHint"], false, "{name}");
            assert_eq!(tool["annotations"]["destructiveHint"], true, "{name} can replace a file");
        }
        let preview = tools.iter().find(|t| t["name"] == "render_preview").unwrap();
        assert_eq!(preview["annotations"]["readOnlyHint"], true);
        assert_eq!(preview["annotations"]["destructiveHint"], false);
    }
    let addr = fake_app();
    let mut server = Server::new(Box::new(Remote::connect(&addr).unwrap()));
    let replies = session(&mut server, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    let tools = replies[0]["result"]["tools"].as_array().unwrap();
    let screenshot = tools.iter().find(|t| t["name"] == "screenshot").unwrap();
    assert_eq!(screenshot["annotations"]["readOnlyHint"], false);
    assert_eq!(screenshot["annotations"]["destructiveHint"], true);
    let image_only = tools.iter().find(|t| t["name"] == "ui_screenshot").unwrap();
    assert_eq!(image_only["annotations"]["readOnlyHint"], true);
    assert_eq!(image_only["annotations"]["destructiveHint"], false);
}

#[test]
fn headless_set_exposure_and_render() {
    exposure_roundtrip(&mut Server::new(Box::new(Headless::demo())));
}

#[test]
fn import_render_export_real_file() {
    let dir = std::env::temp_dir().join(format!("lightcraft-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // A synthetic gradient PNG to import.
    let img = lightcraft_raster::Rgba8::from_fn(96, 64, |x, y| [(x * 2) as u8, (y * 3) as u8, 128, 255]);
    lightcraft_mcp::write_image(&dir.join("sub/gradient.png"), &img, 90).unwrap();
    let out = dir.join("out.jpg");
    let mut server = Server::new(Box::new(Headless::default()));
    let replies = session(
        &mut server,
        &[
            call(1, "import", json!({"paths": [dir.to_string_lossy()]})),
            call(2, "set_develop", json!({"values": {"light.exposure": 0.5}})),
            call(3, "render_photo", json!({"format": "jpeg", "size": 64})),
            call(4, "export", json!({"path": out.to_string_lossy(), "longEdge": 48})),
        ],
    );
    for r in &replies {
        assert!(r["result"]["isError"] == false, "{r}");
    }
    assert_eq!(replies[0]["result"]["structuredContent"]["imported"].as_array().unwrap().len(), 1);
    assert_eq!(replies[2]["result"]["content"][0]["mimeType"], "image/jpeg");
    let jpg = std::fs::read(&out).unwrap();
    assert_eq!(&jpg[..2], &[0xff, 0xd8]);
    let d = lightcraft_codecs::decode(&jpg, Default::default()).unwrap();
    assert_eq!((d.width, d.height), (48, 32));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in for the desktop app's control server: JSON lines over TCP, answered by a headless
/// backend (the real app answers the same methods from its UI thread).
fn fake_app() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut backend = Headless::demo();
        for stream in listener.incoming().flatten() {
            let mut out = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let msg: Value = serde_json::from_str(&line).unwrap();
                let reply = match backend.call(msg["method"].as_str().unwrap(), msg["params"].clone()) {
                    Ok(v) => json!({"id": msg["id"], "ok": true, "result": v}),
                    Err(e) => json!({"id": msg["id"], "ok": false, "error": e}),
                };
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    addr
}

#[test]
fn remote_set_exposure_and_render() {
    let addr = fake_app();
    let remote = Remote::connect(&addr).unwrap();
    assert!(remote.has_ui());
    let mut server = Server::new(Box::new(remote));
    exposure_roundtrip(&mut server);
}

#[test]
fn remote_unreachable_is_a_tool_error() {
    // Bind and drop to get a port with (almost certainly) nothing listening.
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let mut server = Server::new(Box::new(Remote::lazy(&addr)));
    let r = session(&mut server, &[call(1, "query_photos", json!({}))]);
    assert_eq!(r[0]["result"]["isError"], true);
    assert!(r[0]["result"]["content"][0]["text"].as_str().unwrap().contains("not reachable"));
}

/// Eight distinct 1200×800 PNGs on disk, imported into a filesystem session with their real
/// metadata: a library of actual files for the batch export to read and develop. Generating the
/// procedural 6000×4000 demo library (and rendering from it) costs far more than this test's
/// contract — progress, cancellation and atomic 1200-edge PNG writes — needs.
fn file_backed_photos(dir: &std::path::Path) -> Headless {
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    let mut paths = Vec::new();
    for i in 0..8usize {
        let path = originals.join(format!("frame-{i:02}.png"));
        let img = lightcraft_raster::Rgba8::from_fn(1200, 800, |x, y| {
            [((x + i * 37) % 256) as u8, ((y * 3 + i * 11) % 256) as u8, ((x / 4 + y / 2 + i * 23) % 256) as u8, 255]
        });
        lightcraft_mcp::write_image(&path, &img, 90).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    let mut headless = Headless::new(lightcraft_engine::Session::new().with_fs());
    let imported = call_tool(&mut headless, "import", &json!({"paths": paths}));
    assert!(!imported.is_error, "{imported:?}");
    headless
}

/// [`Headless`] whose batch export, while `hold` is set, waits once its first photo is written:
/// it keeps asking the server's progress hook at that photo boundary (which answers pings and
/// reads cancels meanwhile) until the request is cancelled or the test clears `hold`. A ping or
/// cancel sent after the first progress then lands mid-batch, however fast the machine exports.
struct HoldAfterFirst {
    inner: Headless,
    hold: Arc<AtomicBool>,
}

impl Backend for HoldAfterFirst {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.inner.call(method, params)
    }

    fn has_ui(&self) -> bool {
        self.inner.has_ui()
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }

    fn set_progress(&mut self, hook: Option<ProgressHook>) {
        let hold = self.hold.clone();
        self.inner.set_progress(hook.map(|mut hook| -> ProgressHook {
            Box::new(move |done, total, name| {
                // bounded, so a cancel that is never read fails the test instead of hanging it
                let until = Instant::now() + Duration::from_secs(20);
                while done == 1 && done < total && hold.load(Ordering::SeqCst) && Instant::now() < until {
                    if !hook(done, total, name) {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                hook(done, total, name)
            })
        }));
    }
}

type Rx = std::sync::mpsc::Receiver<Value>;

fn next_msg(rx: &Rx) -> Value {
    rx.recv_timeout(Duration::from_secs(30)).expect("MCP reply/progress")
}

fn assert_query_photos(photos: &[Value]) {
    assert_eq!(photos.len(), 8);
    // Real files, real metadata: eight distinct 1200×800 PNGs probed from disk, not synthesized.
    let names: std::collections::HashSet<&str> = photos.iter().map(|p| p["fileName"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 8, "{names:?}");
    for p in photos {
        assert_eq!((p["width"].as_u64(), p["height"].as_u64()), (Some(1200), Some(800)), "{p}");
        assert_eq!(p["format"], "PNG", "{p}");
    }
}

fn run_complete_export(send: &mut impl Write, rx: &Rx, hold: &AtomicBool, ids: &[Value], dir: &std::path::Path) {
    let mut request = call(2, "export", json!({"ids":ids,"dir":dir.join("complete"),"longEdge":1200,"format":"png"}));
    request["params"]["_meta"] = json!({"progressToken":"photos","io.modelcontextprotocol/protocolVersion":"2026-07-28"});
    writeln!(send, "{request}").unwrap();
    let (mut last, mut notes, mut asked, mut answered) = (-1.0, 0, false, false);
    loop {
        let value = next_msg(rx);
        if value["method"] == "notifications/progress" {
            assert_eq!(value["params"]["progressToken"], "photos");
            let progress = value["params"]["progress"].as_f64().unwrap();
            assert!(progress > last && progress <= 8.0, "{value}");
            assert_eq!(value["params"]["total"], 8);
            last = progress;
            notes += 1;
            if !asked {
                writeln!(send, "{}", json!({"jsonrpc":"2.0","id":3,"method":"ping"})).unwrap();
                asked = true;
            }
        } else if value["id"] == 3 {
            answered = true;
            // the ping was answered mid-batch: let the export run to the end
            hold.store(false, Ordering::SeqCst);
        } else {
            assert_eq!(value["id"], 2, "{value}");
            assert_eq!(value["result"]["isError"], false, "{value}");
            assert_eq!(value["result"]["resultType"], "complete");
            break;
        }
    }
    assert!(notes >= 2 && answered, "{notes} progress messages; ping answered {answered}");
    assert_eq!(last, 8.0);
}

fn run_cancelled_export(send: &mut impl Write, rx: &Rx, hold: &AtomicBool, ids: &[Value], out: &std::path::Path) {
    // this export waits after its first photo until the cancel below is read
    hold.store(true, Ordering::SeqCst);
    let mut request = call(4, "command_run", json!({"id":"app.export","params":{"ids":ids,"dir":out,"longEdge":1200,"format":"png"}}));
    request["params"]["_meta"] = json!({"progressToken":44});
    writeln!(send, "{request}").unwrap();
    loop {
        let value = next_msg(rx);
        assert_ne!(value["id"], 4, "must be cancellable before completion: {value}");
        assert_eq!(value["params"]["progressToken"], 44, "{value}");
        if value["params"]["progress"].as_f64().unwrap() > 0.0 {
            break;
        }
    }
    writeln!(send, "{}", json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":4}})).unwrap();
    writeln!(send, "{}", call(5, "doc_inspect", json!({}))).unwrap();
    loop {
        let value = next_msg(rx);
        if value["method"] == "notifications/progress" {
            continue;
        }
        assert_eq!(value["id"], 5, "cancelled request must not reply: {value}");
        assert_eq!(value["result"]["isError"], false);
        break;
    }
}

fn assert_cancelled_output(out: &std::path::Path, decoy: &std::path::Path) {
    assert_eq!(std::fs::read(decoy).unwrap(), b"unrelated existing output");
    let photos: Vec<_> = std::fs::read_dir(out).unwrap().map(|e| e.unwrap().path()).filter(|p| p != decoy).collect();
    assert_eq!(photos.len(), 1, "only the photo completed before the cancel remains: {photos:?}");
    for path in &photos {
        let d = lightcraft_codecs::decode(&std::fs::read(path).unwrap(), Default::default()).expect("complete image, no partial/temp file");
        assert_eq!((d.width, d.height), (1200, 800), "{}", path.display());
    }
}

fn assert_complete_output(dir: &std::path::Path) {
    // The finished export's eight outputs are real 1200×800 PNGs, one per distinct original.
    let complete: Vec<_> = std::fs::read_dir(dir.join("complete")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(complete.len(), 8, "earlier export retained: {complete:?}");
    let mut exported = std::collections::HashSet::new();
    for path in &complete {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[1..4], b"PNG", "{}", path.display());
        let d = lightcraft_codecs::decode(&bytes, Default::default()).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!((d.width, d.height), (1200, 800), "{}", path.display());
        exported.insert(bytes);
    }
    assert_eq!(exported.len(), 8, "eight distinct outputs, not copies of one export");
}

/// Real stdio framing and real atomic exports: progress, ping during a batch, cancellation
/// between complete photos, unrelated output preservation, and a usable session afterwards.
/// Both exports wait after their first photo ([`HoldAfterFirst`]) until the ping is answered or
/// the cancel is read, so neither can finish first on a fast machine.
#[test]
fn export_progress_and_cancel() {
    let dir = std::env::temp_dir().join(format!("lc-mcp-progress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (input, mut send) = std::io::pipe().unwrap();
    let (output, write) = std::io::pipe().unwrap();
    let hold = Arc::new(AtomicBool::new(true));
    let backend = HoldAfterFirst { inner: file_backed_photos(&dir), hold: hold.clone() };
    let server = std::thread::spawn(move || Server::new(Box::new(backend)).serve(BufReader::new(input), write).unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    writeln!(send, "{}", call(1, "query_photos", json!({"limit":8}))).unwrap();
    let query = next_msg(&rx);
    let photos = query["result"]["structuredContent"]["photos"].as_array().unwrap();
    assert_query_photos(photos);
    let ids: Vec<_> = photos.iter().map(|p| p["id"].clone()).collect();
    run_complete_export(&mut send, &rx, &hold, &ids, &dir);
    let out = dir.join("cancelled");
    std::fs::create_dir_all(&out).unwrap();
    let decoy = out.join("photo999.png");
    std::fs::write(&decoy, b"unrelated existing output").unwrap();
    run_cancelled_export(&mut send, &rx, &hold, &ids, &out);
    assert_cancelled_output(&out, &decoy);
    assert_complete_output(&dir);
    drop(send);
    // The fixture's files stay in place until the server (which owns the session) and the reader
    // have finished: the session reads the originals; the output is what the checks above read.
    server.join().unwrap();
    reader.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn export_without_token_finishes_after_eof() {
    let dir = std::env::temp_dir().join(format!("lc-mcp-eof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut headless = Headless::demo();
    let ids: Vec<_> = headless.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    let request = call(1, "cmd_app_export", json!({"ids":ids,"dir":dir,"longEdge":64,"format":"png"}));
    // Headless sessions remain movable to a worker after installing progress support.
    let replies = std::thread::spawn(move || {
        let mut s = Server::new(Box::new(headless));
        session(&mut s, &[request, call(2, "doc_inspect", json!({}))])
    })
    .join()
    .unwrap();
    assert_eq!(replies.len(), 2, "no token means no notifications");
    assert_eq!(replies[0]["result"]["isError"], false, "{replies:?}");
    assert_eq!(replies[1]["id"], 2, "queued request survives EOF");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}
