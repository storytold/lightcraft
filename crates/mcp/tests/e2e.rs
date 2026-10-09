//! M0.9 acceptance: drive LightCraft over MCP (stdio framing), set exposure and render an image.
//! Runs against the headless backend directly and through the TCP control-channel transport
//! (`Remote`) to a stand-in control server.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;

use dac_mcp::{Backend, Headless, Remote, Server, base64_decode};
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
    let d = dac_codecs::decode(&png, Default::default()).unwrap();
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
fn headless_set_exposure_and_render() {
    exposure_roundtrip(&mut Server::new(Box::new(Headless::demo())));
}

#[test]
fn import_render_export_real_file() {
    let dir = std::env::temp_dir().join(format!("dac-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // A synthetic gradient PNG to import.
    let img = dac_raster::Rgba8::from_fn(96, 64, |x, y| [(x * 2) as u8, (y * 3) as u8, 128, 255]);
    dac_mcp::write_image(&dir.join("sub/gradient.png"), &img, 90).unwrap();
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
    let d = dac_codecs::decode(&jpg, Default::default()).unwrap();
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

/// Real stdio framing and real atomic exports: progress, ping during a batch, cancellation
/// between complete photos, unrelated output preservation, and a usable session afterwards.
#[test]
fn export_progress_and_cancel() {
    let dir = std::env::temp_dir().join(format!("lc-mcp-progress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (input, mut send) = std::io::pipe().unwrap();
    let (output, write) = std::io::pipe().unwrap();
    let server = std::thread::spawn(move || Server::new(Box::new(Headless::demo())).serve(BufReader::new(input), write).unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let next = || rx.recv_timeout(std::time::Duration::from_secs(30)).expect("MCP reply/progress");
    writeln!(send, "{}", call(1, "query_photos", json!({"limit":8}))).unwrap();
    let query = next();
    let ids: Vec<_> = query["result"]["structuredContent"]["photos"].as_array().unwrap().iter().map(|p| p["id"].clone()).collect();
    assert_eq!(ids.len(), 8);
    let mut request = call(2, "export", json!({"ids":ids,"dir":dir.join("complete"),"longEdge":1200,"format":"png"}));
    request["params"]["_meta"] = json!({"progressToken":"photos","io.modelcontextprotocol/protocolVersion":"2026-07-28"});
    writeln!(send, "{request}").unwrap();
    let (mut last, mut notes, mut asked, mut answered) = (-1.0, 0, false, false);
    loop {
        let value = next();
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
        } else {
            assert_eq!(value["id"], 2, "{value}");
            assert_eq!(value["result"]["isError"], false, "{value}");
            assert_eq!(value["result"]["resultType"], "complete");
            break;
        }
    }
    assert!(notes >= 2 && answered, "{notes} progress messages; ping answered {answered}");
    assert_eq!(last, 8.0);
    let out = dir.join("cancelled");
    std::fs::create_dir_all(&out).unwrap();
    let decoy = out.join("photo999.png");
    std::fs::write(&decoy, b"unrelated existing output").unwrap();
    let mut request = call(4, "command_run", json!({"id":"app.export","params":{"ids":ids,"dir":out,"longEdge":1200,"format":"png"}}));
    request["params"]["_meta"] = json!({"progressToken":44});
    writeln!(send, "{request}").unwrap();
    loop {
        let value = next();
        assert_ne!(value["id"], 4, "must be cancellable before completion: {value}");
        assert_eq!(value["params"]["progressToken"], 44, "{value}");
        if value["params"]["progress"].as_f64().unwrap() > 0.0 {
            break;
        }
    }
    writeln!(send, "{}", json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":4}})).unwrap();
    writeln!(send, "{}", call(5, "doc_inspect", json!({}))).unwrap();
    loop {
        let value = next();
        if value["method"] == "notifications/progress" {
            continue;
        }
        assert_eq!(value["id"], 5, "cancelled request must not reply: {value}");
        assert_eq!(value["result"]["isError"], false);
        break;
    }
    assert_eq!(std::fs::read(&decoy).unwrap(), b"unrelated existing output");
    let photos: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).filter(|p| *p != decoy).collect();
    assert!(!photos.is_empty() && photos.len() < ids.len(), "only completed photos remain: {photos:?}");
    for path in &photos {
        dac_codecs::decode(&std::fs::read(path).unwrap(), Default::default()).expect("complete image, no partial/temp file");
    }
    assert_eq!(std::fs::read_dir(dir.join("complete")).unwrap().count(), 8, "earlier export retained");
    drop(send);
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
