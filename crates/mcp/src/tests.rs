use serde_json::{Value, json};

use crate::{Backend, Headless, PROTOCOL_VERSION, Server, call_tool, command_tool_name};

fn server() -> Server {
    Server::new(Box::new(Headless::demo()))
}

fn rpc(s: &mut Server, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let reply = s.handle_line(&line).expect("reply");
    let v: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], id);
    v
}

#[test]
fn initialize_negotiates_version() {
    let mut s = server();
    let r = rpc(&mut s, 1, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["result"]["serverInfo"]["name"], "lightcraft");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let r = rpc(&mut s, 2, "initialize", json!({"protocolVersion": "1999-01-01"}));
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    assert!(s.is_initialized());
}

#[test]
fn errors() {
    let mut s = server();
    let v: Value = serde_json::from_str(&s.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(v["error"]["code"], -32700);
    assert_eq!(rpc(&mut s, 1, "nope", json!({}))["error"]["code"], -32601);
    assert_eq!(rpc(&mut s, 2, "tools/call", json!({}))["error"]["code"], -32602);
    assert!(s.handle_line("   ").is_none());
    // Unknown tools and failing commands are tool errors, not protocol errors.
    let r = rpc(&mut s, 3, "tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(r["result"]["isError"], true);
    let r = rpc(&mut s, 4, "tools/call", json!({"name": "run_command", "arguments": {"command": "no.such"}}));
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("unknown command"));
}

#[test]
fn tools_list_has_helpers_and_every_command() {
    let mut s = server();
    let r = rpc(&mut s, 1, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["list_commands", "run_command", "import", "set_develop", "render_photo", "export"] {
        assert!(names.contains(&n), "{n}");
    }
    // Headless: no UI tools.
    assert!(!names.contains(&"screenshot"));
    let hl = Headless::demo();
    for c in hl.session.commands() {
        assert!(!c.id.contains('_'), "command ids must not contain `_` ({})", c.id);
        let n = command_tool_name(c.id);
        assert!(names.contains(&n.as_str()), "{n}");
        assert!(n.len() <= 64 && n.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'), "{n}");
    }
    assert!(names.contains(&"cmd_app_export"));
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
    }
    // Compact mode keeps only the helpers.
    let mut s = Server::new(Box::new(Headless::demo())).with_command_tools(false);
    let r = rpc(&mut s, 1, "tools/list", json!({}));
    assert!(r["result"]["tools"].as_array().unwrap().iter().all(|t| !t["name"].as_str().unwrap().starts_with("cmd_")));
}

/// The `import` helper passes the copy options through: a folder template files the copy.
#[test]
fn import_tool_copies_with_a_folder_template() {
    let base = std::env::temp_dir().join(format!("lc-mcp-import-tpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (src, dest) = (base.join("card"), base.join("out"));
    std::fs::create_dir_all(&src).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[20, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(src.join("a.png"), png).unwrap();
    let mut b = Headless::demo();
    b.session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
    let args =
        json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "{date:%Y}/{date:%Y%m%d}"});
    let r = call_tool(&mut b, "import", &args);
    assert!(!r.is_error, "{r:?}");
    assert!(dest.join("2026").join("20260114").join("a.png").is_file());
    let _ = std::fs::remove_dir_all(&base);
}

/// Issue #93: `render_photo`, `ui.render` and `app.export` with an exact `path` must not replace
/// a photo's original.
#[test]
fn path_writes_never_replace_an_original() {
    let base = std::env::temp_dir().join(format!("lc-mcp-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 30, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let orig = base.join("a.png");
    std::fs::write(&orig, &png).unwrap();
    let mut b = Headless::demo();
    let r = call_tool(&mut b, "import", &json!({"paths": [orig.to_string_lossy()]}));
    assert!(!r.is_error, "{r:?}");
    let id = b.session.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id.0;
    let path = orig.to_string_lossy().to_string();
    let r = call_tool(&mut b, "render_photo", &json!({"id": id, "path": path}));
    assert!(r.is_error, "{r:?}");
    assert!(b.call("ui.render", json!({"id": id, "path": path})).unwrap_err().contains("original"));
    let e = b.call("app.export", json!({"ids": [id], "path": path})).unwrap_err();
    assert!(e.contains("never writes over an original"), "{e}");
    assert_eq!(std::fs::read(&orig).unwrap(), png, "the original is untouched");
    // another path still works
    let r = call_tool(&mut b, "render_photo", &json!({"id": id, "path": base.join("render.png").to_string_lossy()}));
    assert!(!r.is_error, "{r:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// Issue #181: a misspelled or unreadable `app.export` param is an error through the headless
/// backend (what `lightcraft-cli run` and the MCP `export` tool use), not a silent default.
#[test]
fn export_refuses_unknown_params_and_bad_values() {
    let mut b = Headless::demo();
    let out = "/nonexistent-lc-test/x.jpg";
    let e = b.call("app.export", json!({"path": out, "longEdgee": 400})).unwrap_err();
    assert!(e.contains("unknown parameter `longEdgee` (did you mean `longEdge`?)"), "{e}");
    let e = b.call("app.export", json!({"path": out, "longEdge": "banana"})).unwrap_err();
    assert!(e.contains("`longEdge` must be a number"), "{e}");
    let e = b.call("app.export", json!({"path": out, "watermark": {"text": "x", "size": 3}})).unwrap_err();
    assert!(e.contains("`watermark.size` must be a number 0.005..0.5"), "{e}");
    // `export` tool and preset expansion go through the same check
    let r = call_tool(&mut b, "export", &json!({"path": out, "quality": 101}));
    assert!(r.is_error, "{r:?}");
    let e = b.session.execute("export.savePreset", &json!({"name": "Typo", "params": {"qualty": 5}})).unwrap_err().to_string();
    assert!(e.contains("did you mean `quality`"), "{e}");
}

/// Every option the `export` tool advertises reaches `app.export` (they were accepted, then dropped).
#[test]
fn export_tool_forwards_its_advertised_options() {
    let mut b = Headless::demo();
    let base = std::env::temp_dir().join(format!("lc-mcp-export-fwd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let r = call_tool(&mut b, "export", &json!({"dir": base.to_string_lossy(), "subfolder": "sub", "longEdge": 64, "format": "png"}));
    assert!(!r.is_error, "{r:?}");
    let files: Vec<_> = std::fs::read_dir(base.join("sub")).unwrap().collect();
    assert_eq!(files.len(), 1, "written into the subfolder");
    let _ = std::fs::remove_dir_all(&base);
}

/// Issue #236: `app.export {addToPhotos, photosAlbum}` through the headless backend (the MCP
/// `export` tool, `lightcraft-cli run`): the files written go to Photos and the result says how
/// it went; where Photos isn't (not macOS), it is refused before anything is written. A fake
/// runner replaces the real one first: tests never start osascript or Photos.
#[test]
fn export_adds_to_apple_photos_where_there_is_photos() {
    use std::sync::{Arc, Mutex};
    let mut b = Headless::demo();
    let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let c = calls.clone();
    b.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
        c.lock().unwrap().push(args.to_vec());
        Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID-1/L0/001\n".into(), stderr: String::new() })
    }));
    let base = std::env::temp_dir().join(format!("lc-mcp-export-photos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let r = call_tool(&mut b, "export", &json!({"dir": base.to_string_lossy(), "longEdge": 32, "addToPhotos": true, "photosAlbum": "From \"MCP\""}));
    if cfg!(target_os = "macos") {
        assert!(!r.is_error, "{r:?}");
        let out = r.structured.unwrap();
        assert_eq!(out["applePhotos"]["imported"], 1, "{out}");
        assert_eq!(out["applePhotos"]["album"], "From \"MCP\"");
        let written = out["files"][0]["path"].as_str().unwrap().to_string();
        let args = calls.lock().unwrap().clone();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].last(), Some(&written));
    } else {
        assert!(r.is_error, "{r:?}");
        assert!(format!("{:?}", r.content).contains("only available on macOS"), "{r:?}");
        assert!(!base.exists(), "nothing written");
        assert!(calls.lock().unwrap().is_empty());
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Review of #236: a one-shot process (`lightcraft-cli run`, an MCP server reaching EOF) must not
/// end in the middle of an Apple Photos import, abandoning osascript and the result. The test runs
/// itself as a child process (`LC_PHOTOS_CHILD`): the child adds a file with a fake runner that
/// takes a while and then leaves a marker, prints the reply and ends as the CLI does (dropping
/// its backend). The parent checks the import had finished: by default the call waited for
/// Photos, and with `wait: false` the backend waited before the process ended. Never osascript:
/// the child's session has no file-system hooks and only the fake runner.
#[test]
fn a_one_shot_process_never_ends_during_a_photos_import() {
    use std::sync::Arc;
    use std::time::Duration;
    if let (Ok(dir), Ok(mode)) = (std::env::var("LC_PHOTOS_CHILD"), std::env::var("LC_PHOTOS_CHILD_MODE")) {
        let dir = std::path::PathBuf::from(dir);
        let file = dir.join("a.jpg");
        let marker = dir.join("imported");
        let mut h = Headless::new(lightcraft_engine::Session::with_demo());
        h.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            std::thread::sleep(Duration::from_millis(400));
            std::fs::write(&marker, args.last().cloned().unwrap_or_default()).unwrap();
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID-1\n".into(), stderr: String::new() })
        }));
        let mut p = json!({"paths": [file.to_string_lossy()]});
        if mode == "nowait" {
            p["wait"] = json!(false);
        }
        let r = h.call("engine.execute", json!({"command": "export.addToPhotos", "params": p})).unwrap();
        println!("CHILD-REPLY {r}");
        drop(h); // what lightcraft-cli does before it exits
        return;
    }
    for mode in ["default", "nowait"] {
        let dir = std::env::temp_dir().join(format!("lc-mcp-photos-child-{mode}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jpg"), b"jpeg").unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::a_one_shot_process_never_ends_during_a_photos_import", "--nocapture", "--test-threads=1"])
            .env("LC_PHOTOS_CHILD", &dir)
            .env("LC_PHOTOS_CHILD_MODE", mode)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{mode}: {stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        let reply: Value = stdout
            .lines()
            .find_map(|l| l.split_once("CHILD-REPLY ").map(|(_, j)| serde_json::from_str(j).unwrap()))
            .unwrap_or_else(|| panic!("{mode}: no reply in {stdout}"));
        let marker = std::fs::read_to_string(dir.join("imported")).unwrap_or_else(|_| panic!("{mode}: the process ended before the import did"));
        assert!(marker.ends_with("a.jpg"), "{marker}");
        if mode == "default" {
            assert_eq!((reply["running"].as_bool(), reply["imported"].as_u64()), (Some(false), Some(1)), "waited: {reply}");
        } else {
            assert_eq!(reply["running"], true, "{reply}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The `import` helper moves: renamed into the folder template, the source removed.
#[test]
fn import_tool_moves_with_a_folder_template() {
    let base = std::env::temp_dir().join(format!("lc-mcp-import-move-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (src, dest) = (base.join("card"), base.join("out"));
    std::fs::create_dir_all(&src).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[21, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(src.join("sample.png"), png).unwrap();
    let mut b = Headless::demo();
    b.session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
    let args = json!({"paths": [src.to_string_lossy()], "mode": "move", "destination": dest.to_string_lossy(),
        "organize": "{date:%Y}/{date:%Y%m%d}", "rename": "{date:%Y%m%d}_{seq:3}"});
    let r = call_tool(&mut b, "import", &args);
    assert!(!r.is_error, "{r:?}");
    assert!(dest.join("2026").join("20260114").join("20260114_001.png").is_file());
    assert!(!src.join("sample.png").exists(), "moved");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn command_tools_run_commands() {
    let mut b = Headless::demo();
    let first = b.session.visible_cloned()[0];
    let r = call_tool(&mut b, "cmd_library_select", &json!({"ids": [first.0]}));
    assert!(!r.is_error, "{r:?}");
    let r = call_tool(&mut b, "cmd_photo_rate", &json!({"rating": 4}));
    assert!(!r.is_error, "{r:?}");
    assert_eq!(b.session.catalog.photo(first).unwrap().rating, 4);
    let r = call_tool(&mut b, "cmd_edit_undo", &json!({}));
    assert!(!r.is_error);
    assert_eq!(b.session.catalog.photo(first).unwrap().rating, 0);
}

#[test]
fn set_develop_values_and_settings() {
    let mut b = Headless::demo();
    let first = b.session.visible_cloned()[0];
    let r = call_tool(&mut b, "set_develop", &json!({"id": first.0, "values": {"light.exposure": 1.25}}));
    assert!(!r.is_error, "{r:?}");
    assert_eq!(r.structured.as_ref().unwrap()["controls"][0]["value"], 1.25);
    assert_eq!(b.session.develop_of(first).unwrap().light.exposure, 1.25);
    let r = call_tool(&mut b, "set_develop", &json!({"settings": {"light": {"contrast": 30.0}}}));
    assert!(!r.is_error, "{r:?}");
    let d = b.session.develop_of(first).unwrap();
    assert_eq!((d.light.exposure, d.light.contrast), (1.25, 30.0));
    assert!(call_tool(&mut b, "set_develop", &json!({})).is_error);
    assert!(call_tool(&mut b, "set_develop", &json!({"values": {"no.such": 1}})).is_error);
}

#[test]
fn crop_needs_a_rect_angle_or_reset() {
    let mut b = Headless::demo();
    let first = b.session.visible_cloned()[0];
    // A guessed parameter must not succeed silently.
    let r = call_tool(&mut b, "crop", &json!({"id": first.0, "aspect": "1:1"}));
    assert!(r.is_error, "{r:?}");
    assert!(call_tool(&mut b, "crop", &json!({"id": first.0, "reset": false})).is_error);
    assert!(!call_tool(&mut b, "crop", &json!({"id": first.0, "rect": [0.1, 0.0, 0.9, 1.0]})).is_error);
    assert!(!call_tool(&mut b, "crop", &json!({"id": first.0, "reset": true})).is_error);
}

#[test]
fn ui_tools_need_the_app() {
    let mut b = Headless::demo();
    let r = call_tool(&mut b, "screenshot", &json!({}));
    assert!(r.is_error);
    assert!(r.content[0]["text"].as_str().unwrap().contains("--connect"));
}

#[test]
fn resources() {
    let mut s = server();
    let r = rpc(&mut s, 1, "resources/list", json!({}));
    let list = r["result"]["resources"].as_array().unwrap();
    assert_eq!(list.len(), 7);
    for (i, res) in list.iter().enumerate() {
        let uri = res["uri"].as_str().unwrap();
        let r = rpc(&mut s, 10 + i as u64, "resources/read", json!({"uri": uri}));
        let text = r["result"]["contents"][0]["text"].as_str().unwrap_or_else(|| panic!("{uri}: {r}"));
        serde_json::from_str::<Value>(text).unwrap();
    }
    assert_eq!(rpc(&mut s, 99, "resources/read", json!({"uri": "lightcraft://nope"}))["error"]["code"], -32002);
}

/// `select_photos` with an id that is not in the library is a tool error, and the photo that was
/// active stays active (#182).
#[test]
fn select_photos_rejects_unknown_ids_and_keeps_the_active_photo() {
    let mut b = Headless::demo();
    let first = b.session.visible_cloned()[0];
    let r = call_tool(&mut b, "select_photos", &json!({"ids": [first.0]}));
    assert!(!r.is_error, "{r:?}");
    let r = call_tool(&mut b, "select_photos", &json!({"ids": [9999]}));
    assert!(r.is_error, "{r:?}");
    assert!(r.content[0]["text"].as_str().unwrap().contains("no such photo 9999"), "{r:?}");
    assert_eq!(b.session.active(), Some(first));
    let r = call_tool(&mut b, "get_develop", &json!({}));
    assert!(!r.is_error, "{r:?}");
    let r = call_tool(&mut b, "get_develop", &json!({"id": 9999}));
    assert!(r.is_error, "{r:?}");
    let r = call_tool(&mut b, "set_develop", &json!({"id": 9999, "values": {"light.exposure": 1.0}}));
    assert!(r.is_error, "{r:?}");
    assert_eq!(b.session.active(), Some(first));
}

#[test]
fn core_tools_have_hints_and_reject_unknown_keys() {
    for compact in [false, true] {
        let mut s = server().with_command_tools(!compact);
        let list = rpc(&mut s, 1, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().unwrap();
        for name in ["command_list", "command_run", "command_batch", "doc_inspect", "render_preview", "list_commands", "run_command", "render_photo"]
        {
            assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
        }
        for t in tools {
            assert!(!t["title"].as_str().unwrap().is_empty());
            for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
                assert!(t["annotations"][hint].is_boolean(), "{t}");
            }
        }
        for name in ["command_list", "command_run", "command_batch", "doc_inspect", "render_preview", "query_photos", "run_command"] {
            let r = rpc(&mut s, 2, "tools/call", json!({"name":name,"arguments":{"misspelled":true}}));
            assert_eq!(r["error"]["code"], -32602, "{r}");
            assert!(r["error"]["message"].as_str().unwrap().contains("misspelled"), "{r}");
        }
        let preview = tools.iter().find(|t| t["name"] == "render_preview").unwrap();
        assert_eq!(preview["annotations"]["readOnlyHint"], true);
        assert!(preview["inputSchema"]["properties"].get("path").is_none());
        let writer = tools.iter().find(|t| t["name"] == "render_photo").unwrap();
        assert_eq!(writer["annotations"]["readOnlyHint"], false);
        let export = tools.iter().find(|t| t["name"] == "export").unwrap();
        for key in lightcraft_engine::export::OPTION_PARAMS.iter().chain(lightcraft_engine::export::TARGET_PARAMS) {
            assert!(export["inputSchema"]["properties"].get(*key).is_some(), "missing export argument {key}");
        }
        let before = rpc(&mut s, 2, "resources/read", json!({"uri":"lightcraft://library"}));
        let image = rpc(&mut s, 2, "tools/call", json!({"name":"render_preview","arguments":{"max_side":64}}));
        assert_eq!(image["result"]["isError"], false, "{image}");
        assert!(image["result"]["content"].as_array().unwrap().iter().any(|c| c["type"] == "image"));
        assert_eq!(before, rpc(&mut s, 2, "resources/read", json!({"uri":"lightcraft://library"})));
        let batch = json!({"steps":[{"id":"catalog.stats"},{"id":"no.such"},{"id":"catalog.stats"}],"stop_on_error":false});
        let r = rpc(&mut s, 3, "tools/call", json!({"name":"command_batch","arguments":batch}));
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["structuredContent"]["completed"], 2);
        assert_eq!(r["result"]["structuredContent"]["failed"], 1);
        let r = rpc(&mut s, 4, "tools/call", json!({"name":"command_batch","arguments":{"steps":[{"id":"no.such"},{"id":"catalog.stats"}]}}));
        assert_eq!(r["result"]["structuredContent"]["completed"], 0);
        assert_eq!(r["result"]["structuredContent"]["results"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn core_resources_match_tools_after_parse_error() {
    let mut s = server();
    let bad: Value = serde_json::from_str(&s.handle_line("{bad").unwrap()).unwrap();
    assert_eq!(bad["error"]["code"], -32700);
    assert!(bad["id"].is_null());
    let list = rpc(&mut s, 1, "resources/list", json!({}));
    for (uri, tool) in [("lightcraft://document", "doc_inspect"), ("lightcraft://commands", "command_list")] {
        assert!(list["result"]["resources"].as_array().unwrap().iter().any(|r| r["uri"] == uri));
        let resource = rpc(&mut s, 2, "resources/read", json!({"uri":uri}));
        let call = rpc(&mut s, 3, "tools/call", json!({"name":tool,"arguments":{}}));
        let a: Value = serde_json::from_str(resource["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
        let b: Value = serde_json::from_str(call["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(a, b);
    }
    let old = rpc(&mut s, 4, "resources/read", json!({"uri":"lightcraft://library"}));
    assert!(old["result"]["contents"].is_array(), "{old}");
}

#[test]
fn escaped_tool_panic_is_an_error_and_session_survives() {
    struct Faulty {
        inner: Headless,
        fail: bool,
    }
    impl Backend for Faulty {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            if method == "engine.execute" && params["command"] == "catalog.stats" && std::mem::take(&mut self.fail) {
                panic!("synthetic backend panic");
            }
            self.inner.call(method, params)
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }
    let mut s = Server::new(Box::new(Faulty { inner: Headless::demo(), fail: true }));
    let params = json!({"name":"run_command","arguments":{"command":"catalog.stats"}});
    let r = rpc(&mut s, 1, "tools/call", params.clone());
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(r.to_string().contains("synthetic backend panic"));
    assert_eq!(rpc(&mut s, 2, "tools/call", params)["result"]["isError"], false);
    assert!(rpc(&mut s, 3, "ping", json!({}))["result"].is_object());
}

#[test]
fn modern_requests_receive_result_and_cache_fields() {
    let mut s = server();
    let meta = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28"});
    for (method, mut params) in [
        ("tools/list", json!({})),
        ("resources/list", json!({})),
        ("resources/templates/list", json!({})),
        ("resources/read", json!({"uri":"lightcraft://library"})),
    ] {
        params["_meta"] = meta.clone();
        let r = rpc(&mut s, 1, method, params);
        assert_eq!(r["result"]["resultType"], "complete", "{r}");
        assert_eq!(r["result"]["cacheScope"], "private");
        assert_eq!(r["result"]["ttlMs"], if method == "resources/read" { 0 } else { 600_000 });
    }
    assert!(rpc(&mut s, 2, "tools/list", json!({}))["result"].get("resultType").is_none());
}

/// A running app's control channel, as far as the MCP server can tell: it records each call.
struct Recorder(Vec<(String, Value)>);

impl Backend for Recorder {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.0.push((method.to_string(), params));
        Ok(Value::Null)
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "recorder".into()
    }
}

/// With the app attached, agents cut, copy and paste in its text fields (`clipboard`).
#[test]
fn the_clipboard_tool_cuts_copies_and_pastes_in_the_app() {
    let names: Vec<String> = crate::tools::helper_tools(true).iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
    assert!(names.iter().any(|n| n == "clipboard"), "{names:?}");
    assert!(!crate::tools::helper_tools(false).iter().any(|t| t["name"] == "clipboard"), "not without the app");
    let mut b = Recorder(vec![]);
    let r = call_tool(&mut b, "clipboard", &json!({"action": "paste", "text": "travel"}));
    assert!(!r.is_error, "{r:?}");
    assert_eq!(b.0, vec![("ui.clipboard".to_string(), json!({"action": "paste", "text": "travel"}))]);
}
