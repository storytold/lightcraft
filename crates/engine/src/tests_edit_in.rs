//! Edit In (P4.4) and Actions (P4.5) through the command registry.

use std::path::{Path, PathBuf};

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::cmd::edit_in::{Control, control_open};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-editin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(path: &Path, seed: u8, w: usize, h: usize) {
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 9) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn session(dir: &Path) -> Session {
    let mut s = Session::new().with_fs();
    s.workflow = crate::cmd::actions::Workflow::in_dir(Some(dir.join("settings")));
    s
}

fn import(s: &mut Session, p: &Path) -> PhotoId {
    let r = s.execute("library.import", &json!({"paths": [p.to_string_lossy()]})).unwrap();
    PhotoId(r["imported"][0].as_u64().unwrap())
}

/// The PhotoCraft preset writes a layered 16-bit PSD with the edits, carrying the metadata, stacked
/// on the original; the library reads it back.
#[test]
fn edit_in_photocraft_writes_a_stacked_layered_psd() {
    let dir = temp_dir("psd");
    let src = dir.join("beach.png");
    write_png(&src, 9, 40, 24);
    let mut s = session(&dir);
    let orig = import(&mut s, &src);
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let r = s.execute("photo.editIn", &json!({"preset": "PhotoCraft"})).unwrap();
    let path = r["path"].as_str().unwrap();
    assert!(path.ends_with("beach-Edit.psd"), "{r}");
    assert_eq!(r["openWith"]["app"], "photocraft");
    assert_eq!(r["opened"], serde_json::Value::Null, "not launched unless asked");
    let f = lcc_psd::PsdFile::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!((f.header.width, f.header.height, f.header.depth), (40, 24, 16));
    assert_eq!(f.layer(0).unwrap().name(), "beach");
    let new = PhotoId(r["id"].as_u64().unwrap());
    assert_eq!(s.active(), Some(new));
    let st = s.catalog.stack_of(new).expect("stacked");
    assert_eq!((st.top(), st.photos.contains(&orig)), (new, true));
    let ph = s.catalog.photo(new).unwrap();
    assert_eq!((ph.width, ph.height), (40, 24));
    assert_eq!(ph.rating, 4, "metadata carried over through the PSD's XMP");
    // brighter than the unedited original: the edits are in the pixels
    let a = s.render_now(orig, 8, 8).unwrap();
    let b = s.render_now(new, 8, 8).unwrap();
    let mean = |r: &lcc_pipeline::Rendered| r.image.data.iter().map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).sum::<u32>();
    assert!(mean(&b) + 50 >= mean(&a), "the PSD holds the edited render");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Edit a Copy copies the file's bytes; Edit Original hands over the file itself; neither works
/// for a file an editor can't open.
#[test]
fn edit_copy_and_original_modes_and_launching_through_the_desktop_seam() {
    let dir = temp_dir("modes");
    let src = dir.join("pic.png");
    write_png(&src, 3, 16, 16);
    let mut s = session(&dir);
    let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
    s.desktop = desktop.clone();
    let orig = import(&mut s, &src);
    let r = s.execute("photo.editIn", &json!({"mode": "copy"})).unwrap();
    assert!(r["path"].as_str().unwrap().ends_with("pic-Edit.png"), "{r}");
    assert_eq!(std::fs::read(r["path"].as_str().unwrap()).unwrap(), std::fs::read(&src).unwrap());
    let r = s.execute("photo.editIn", &json!({"id": orig.0, "mode": "original"})).unwrap();
    assert_eq!(r["path"].as_str().unwrap(), src.to_string_lossy());
    assert_eq!(r["id"], json!(orig.0));
    assert_eq!(r["openWith"]["reload"], json!([orig.0]));
    assert!(s.execute("photo.editIn", &json!({"preset": "nope"})).is_err());
    assert!(s.execute("photo.editIn", &json!({"bitDepth": 12})).is_err());
    // Handing a file to its editor goes through the session's desktop: a test session records the launch instead of
    // starting a real program (and nothing above was launched: `launch` was not asked for).
    assert!(desktop.requests().is_empty(), "{:?}", desktop.requests());
    let r = s.execute("photo.editIn", &json!({"id": orig.0, "mode": "copy", "launch": true})).unwrap();
    assert_eq!(r["opened"], json!("launched"), "{r}");
    let path = r["path"].as_str().unwrap().to_string();
    let calls = desktop.requests();
    assert!(matches!(calls.as_slice(), [crate::desktop::Request::Launch(line)] if line.contains(&path)), "{calls:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editor_presets_save_reload_and_delete() {
    let dir = temp_dir("presets");
    let mut s = session(&dir);
    let p = s.execute("editIn.savePreset", &json!({"name": "Gimp 8", "app": "/usr/bin/gimp", "bitDepth": 8, "colorSpace": "srgb"})).unwrap();
    assert_eq!(p["bitDepth"], 8);
    assert!(s.execute("editIn.savePreset", &json!({"name": "PhotoCraft"})).is_err(), "built-in names are taken");
    assert!(s.execute("editIn.savePreset", &json!({"name": "x", "naming": "../up"})).is_err());
    let mut again = session(&dir);
    let l = again.execute("editIn.presets", &json!({})).unwrap();
    assert_eq!(l["user"][0]["app"], "/usr/bin/gimp");
    again.execute("editIn.deletePreset", &json!({"name": "Gimp 8"})).unwrap();
    assert!(again.execute("editIn.deletePreset", &json!({"name": "Gimp 8"})).is_err());
    std::fs::write(dir.join("settings/editors.json"), b"{broken").unwrap();
    let mut broken = session(&dir);
    assert!(broken.execute("editIn.presets", &json!({})).is_err(), "a damaged file is reported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_as_layers_builds_one_psd_from_the_selection() {
    let dir = temp_dir("layers");
    let (a, b) = (dir.join("a.png"), dir.join("b.png"));
    write_png(&a, 1, 30, 20);
    write_png(&b, 200, 20, 30);
    let mut s = session(&dir);
    let ia = import(&mut s, &a);
    let ib = import(&mut s, &b);
    assert!(s.execute("photo.openAsLayers", &json!({"ids": [ia.0]})).is_err(), "needs two");
    let r = s.execute("photo.openAsLayers", &json!({"ids": [ia.0, ib.0]})).unwrap();
    assert_eq!(r["layers"], 2);
    let f = lcc_psd::PsdFile::from_bytes(&std::fs::read(r["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!((f.header.width, f.header.height), (30, 30));
    assert_eq!(f.layer(1).unwrap().name(), "a", "first selected on top");
    assert_eq!(f.layer(0).unwrap().name(), "b");
    let r8 = s.execute("photo.openAsLayers", &json!({"ids": [ia.0, ib.0], "bitDepth": 8})).unwrap();
    let f8 = lcc_psd::PsdFile::from_bytes(&std::fs::read(r8["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(f8.header.depth, 8, "an 8-bit preset writes 8-bit layers");
    let new = PhotoId(r["id"].as_u64().unwrap());
    assert!(s.catalog.stack_of(new).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// PhotoCraft's control channel: auth, then app.open with the path relative to its read root.
#[test]
fn control_channel_opens_in_a_running_photocraft() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp_dir("control");
    std::fs::write(dir.join("token"), "abc\n").unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let srv = std::thread::spawn(move || {
        let (c, _) = l.accept().unwrap();
        let mut w = c.try_clone().unwrap();
        let mut r = BufReader::new(c);
        let mut got = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            writeln!(w, "{}", json!({"id": v["id"], "ok": true, "result": {"echo": v["params"]}})).unwrap();
            got.push(v);
        }
        got
    });
    let c = Control { port, token_file: dir.join("token").to_string_lossy().into(), root: dir.to_string_lossy().into() };
    let r = control_open(&c, &dir.join("sub/x.psd")).unwrap();
    assert_eq!(r["echo"]["path"], "sub/x.psd");
    let got = srv.join().unwrap();
    assert_eq!((got[0]["method"].as_str(), got[0]["params"]["token"].as_str()), (Some("auth"), Some("abc")));
    assert!(control_open(&c, Path::new("/elsewhere/x.psd")).is_err(), "outside the read root");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Record a few edits, stop, and play the action on two other photos (each alone selected);
/// parameters, a failing step and nesting limits.
#[test]
fn actions_record_parameterise_and_play_on_a_selection() {
    let dir = temp_dir("actions");
    let mut s = session(&dir);
    let ids: Vec<PhotoId> = (0..3)
        .map(|i| {
            let f = dir.join(format!("p{i}.png"));
            write_png(&f, i * 40, 12, 12);
            import(&mut s, &f)
        })
        .collect();
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("actions.record", &json!({"name": "Warm & rate"})).unwrap();
    assert!(s.execute("actions.record", &json!({"name": "other"})).is_err(), "one recording at a time");
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let r = s.execute("actions.stop", &json!({})).unwrap();
    assert_eq!(r["steps"], 2, "selection commands are not recorded: {r}");
    let a = s.execute("actions.get", &json!({"name": "Warm & rate"})).unwrap();
    assert_eq!(a["steps"][1]["command"], "photo.rate");

    // play on the other two photos
    s.execute("library.select", &json!({"ids": [ids[1].0, ids[2].0]})).unwrap();
    let r = s.execute("actions.play", &json!({"name": "Warm & rate"})).unwrap();
    assert_eq!((r["photos"].as_u64(), r["ran"].as_u64()), (Some(2), Some(4)));
    for id in &ids[1..] {
        let ph = s.catalog.photo(*id).unwrap();
        assert_eq!(ph.rating, 3);
        assert!((ph.develop.light.exposure - 0.7).abs() < 1e-6);
    }
    assert_eq!(s.selection.ids, vec![ids[1], ids[2]], "selection restored");

    // a parameterised action, saved directly, played by explicit ids
    s.execute(
        "actions.save",
        &json!({"action": {"name": "Rate", "params": [{"name": "stars", "default": 1}], "steps": [{"command": "photo.rate", "params": {"rating": "{{stars}}"}}]}}),
    )
    .unwrap();
    s.execute("actions.play", &json!({"name": "Rate", "ids": [ids[0].0], "args": {"stars": 5}})).unwrap();
    assert_eq!(s.catalog.photo(ids[0]).unwrap().rating, 5);
    assert!(s.execute("actions.play", &json!({"name": "Rate", "args": {"nope": 1}})).is_err());

    // a failing step names itself; a self-calling action stops
    s.execute("actions.save", &json!({"action": {"name": "Bad", "steps": [{"command": "no.such"}]}})).unwrap();
    let e = s.execute("actions.play", &json!({"name": "Bad"})).unwrap_err().to_string();
    assert!(e.contains("step 1"), "{e}");
    s.execute(
        "actions.save",
        &json!({"action": {"name": "Loop", "perPhoto": false, "steps": [{"command": "actions.play", "params": {"name": "Loop"}}]}}),
    )
    .unwrap();
    assert!(s.execute("actions.play", &json!({"name": "Loop"})).is_err());
    assert!(s.workflow.actions.playing.is_empty());

    // kept on disk
    let mut again = session(&dir);
    let l = again.execute("actions.list", &json!({})).unwrap();
    assert_eq!(l["actions"].as_array().unwrap().len(), 4);
    again.execute("actions.delete", &json!({"name": "Bad"})).unwrap();
    assert!(again.execute("actions.stop", &json!({})).is_err(), "not recording");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What an agent may ask for (MCP, the control channel, an action): every parameter that chooses a
/// program is refused, on every command that takes one; a saved preset by name is not.
#[test]
fn agents_cannot_choose_the_program_edit_in_starts_or_a_preset_names() {
    use crate::cmd::edit_in::refuse_program_choice;
    let dir = temp_dir("refuse");
    let mut s = session(&dir);
    for (command, keys) in [
        ("photo.editIn", &["app", "args", "control"][..]),
        ("photo.openAsLayers", &["app", "args", "control"][..]),
        ("photo.editInPhotoCraft", &["app"][..]),
        ("photo.editInExternal", &["app"][..]),
        ("photo.openOriginalInExternal", &["app"][..]),
    ] {
        for key in keys {
            let e = refuse_program_choice(&mut s, command, &json!({"preset": "Mine", "launch": true, *key: "x"})).unwrap_err();
            assert!(e.contains(command) && e.contains(&format!("`{key}`")) && e.contains("preset"), "{e}");
        }
        refuse_program_choice(&mut s, command, &json!({"preset": "Mine", "launch": true})).unwrap();
        refuse_program_choice(&mut s, command, &json!({"app": null})).unwrap();
    }
    refuse_program_choice(&mut s, "photo.rate", &json!({"app": "x"})).unwrap();

    // An agent can still save a preset's other settings, but not set or change the program it starts.
    // the user's own preset, typed in the dialog
    s.execute("editIn.savePreset", &json!({"name": "Gimp", "app": "/usr/bin/gimp", "args": ["-n"]})).unwrap();
    let save = "editIn.savePreset";
    for p in [
        json!({"name": "Gimp", "app": "/bin/sh", "args": ["-n"]}),
        json!({"name": "Gimp", "app": "/usr/bin/gimp", "args": ["-c", "x"]}),
        json!({"name": "Gimp", "app": "/usr/bin/gimp", "args": ["-n"], "control": {"port": 9, "tokenFile": "/t", "root": "/"}}),
        json!({"name": "Gimp", "bitDepth": 8}), // would drop the program for the default opener
        json!({"name": "New", "app": "/bin/sh"}),
        json!({"name": " Gimp ", "app": "/bin/sh", "args": ["-n"]}),
    ] {
        let e = refuse_program_choice(&mut s, save, &p).unwrap_err();
        assert!(e.contains("preset"), "{p}: {e}");
    }
    refuse_program_choice(&mut s, save, &json!({"name": "Gimp", "app": "/usr/bin/gimp", "args": ["-n"], "bitDepth": 8})).unwrap();
    refuse_program_choice(&mut s, save, &json!({"name": "New", "bitDepth": 8})).unwrap();
    // a malformed preset is left for the command to report
    refuse_program_choice(&mut s, save, &json!({"name": 3})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A saved action starts only programs the user set up: a step that chooses one stops the action
/// there, after the steps before it ran; the command line can still choose one.
#[test]
fn an_action_step_that_chooses_a_program_stops_the_action() {
    let dir = temp_dir("refuse-action");
    let src = dir.join("pic.png");
    write_png(&src, 5, 16, 16);
    let mut s = session(&dir);
    let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
    s.desktop = desktop.clone();
    let id = import(&mut s, &src);
    for (i, (command, params)) in [
        ("photo.editIn", json!({"mode": "copy", "app": "/bin/sh", "args": ["-c", "x"], "launch": true})),
        ("photo.openAsLayers", json!({"args": ["-c", "x"], "launch": true})),
        ("editIn.savePreset", json!({"name": "Sneaky", "app": "/bin/sh"})),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("Rate then {command}");
        let steps = json!([{"command": "photo.rate", "params": {"rating": i + 1}}, {"command": command, "params": params}]);
        s.execute("actions.save", &json!({"action": {"name": name, "steps": steps}})).unwrap();
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        let e = s.execute("actions.play", &json!({"name": name})).unwrap_err().to_string();
        assert!(e.contains("step 2") && e.contains(command) && e.contains("preset"), "{e}");
        assert_eq!(s.catalog.photo(id).unwrap().rating, i as u8 + 1, "the step before it ran");
    }
    assert!(desktop.requests().is_empty(), "nothing started: {:?}", desktop.requests());
    assert!(s.execute("editIn.presets", &json!({})).unwrap()["user"].as_array().unwrap().is_empty(), "no preset saved");
    assert_eq!(s.catalog.len(), 1, "no copy written");

    // the command line (a direct call) still chooses the program
    let r = s.execute("photo.editIn", &json!({"mode": "copy", "app": "/opt/ed", "args": ["-n"], "launch": true})).unwrap();
    let calls = desktop.requests();
    assert!(matches!(calls.as_slice(), [crate::desktop::Request::Launch(line)] if line.first().is_some_and(|a| a == "/opt/ed")), "{calls:?} {r}");
    let _ = std::fs::remove_dir_all(&dir);
}
