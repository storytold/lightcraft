//! The `faces.*` model commands: list, inspect, install behind the licence, select, remove, the on/off
//! setting, and the refusals (no folder, licence not accepted, unsupported or hostile files and ids).

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-facemodels-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn session(dir: &std::path::Path) -> Session {
    let mut s = Session::new();
    s.face_models_dir = Some(dir.join("models"));
    s
}

fn model_file(dir: &std::path::Path, name: &str, dim: u64) -> String {
    let p = dir.join(name);
    std::fs::write(&p, lightcraft_faces::synthetic::embedder_model(dim)).unwrap();
    p.to_string_lossy().into_owned()
}

fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["models"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or(&Value::Null)
}

#[test]
fn the_list_knows_the_known_models_and_is_honest_about_the_runtime() {
    let d = temp("list");
    let mut s = session(&d);
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(l["enabled"], false);
    assert_eq!(l["runtime"], false);
    assert_eq!(find(&l, "yunet-2023mar")["bundled"], true);
    assert_eq!(find(&l, "yunet-2023mar")["installed"], true);
    assert_eq!(find(&l, "auraface-v1")["installed"], false);
    assert_eq!(find(&l, "auraface-v1")["licence"]["commercial"], "yes");
    assert_eq!(find(&l, "sface-2021dec")["licence"]["commercial"], "unknown");
    // a build with no folder lists too, and cannot install
    let mut web = Session::new();
    assert!(web.execute("faces.models.list", &json!({})).is_ok());
    let f = model_file(&d, "m.onnx", 512);
    assert!(web.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).is_err());
    assert!(web.execute("faces.enable", &json!({"enabled": true})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn inspect_describes_a_file_and_installs_nothing() {
    let d = temp("inspect");
    let mut s = session(&d);
    let f = model_file(&d, "My Model.onnx", 512);
    let r = s.execute("faces.models.inspect", &json!({"path": f})).unwrap();
    assert_eq!(r["kind"], "draft");
    assert_eq!(r["model"]["licence"]["commercial"], "unknown");
    assert_eq!(r["model"]["known"], false);
    assert!(r["assumptions"].as_array().unwrap().iter().any(|a| a.as_str().unwrap().contains("127.5")));
    assert_eq!(r["alreadyInstalled"], false);
    assert!(!d.join("models").exists(), "inspecting writes nothing");
    // not a model: a plain answer, not an error
    let junk = d.join("junk.onnx");
    std::fs::write(&junk, b"this is not a model").unwrap();
    let r = s.execute("faces.models.inspect", &json!({"path": junk.to_string_lossy()})).unwrap();
    assert_eq!(r["kind"], "unsupported");
    assert!(r["reason"].as_str().unwrap().len() > 5);
    // missing and empty files and folders are errors
    assert!(s.execute("faces.models.inspect", &json!({"path": d.join("nope.onnx").to_string_lossy()})).is_err());
    assert!(s.execute("faces.models.inspect", &json!({"path": d.to_string_lossy()})).is_err());
    let empty = d.join("empty.onnx");
    std::fs::write(&empty, b"").unwrap();
    assert!(s.execute("faces.models.inspect", &json!({"path": empty.to_string_lossy()})).is_err());
    assert!(s.execute("faces.models.inspect", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn install_needs_the_licence_accepted_then_select_enable_and_remove_work() {
    let d = temp("flow");
    let mut s = session(&d);
    let f = model_file(&d, "Mine.onnx", 512);
    assert!(s.execute("faces.models.install", &json!({"path": f})).is_err(), "no acknowledgement");
    assert!(s.execute("faces.models.install", &json!({"path": f, "acknowledged": false})).is_err());
    assert!(s.execute("faces.models.install", &json!({"path": f, "acknowledged": "yes"})).is_err(), "must be the boolean true");
    assert!(!d.join("models").exists(), "refused installs leave nothing behind");

    let r = s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    let id = r["installed"]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("custom-mine-"));
    let home = d.join("models").join(&id);
    assert!(home.join("model.onnx").is_file() && home.join("face-model.json").is_file() && home.join("installed.json").is_file());
    assert!(!home.join("model.onnx.part").exists());
    assert_eq!(std::fs::read(home.join("model.onnx")).unwrap(), std::fs::read(&f).unwrap());
    let rec: Value = serde_json::from_slice(&std::fs::read(home.join("installed.json")).unwrap()).unwrap();
    assert_eq!(rec["commercial"], "unknown");
    assert_eq!(rec["fileName"], "Mine.onnx");

    // listed as installed and custom; installing again is harmless
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!(find(&l, &id)["installed"], true);
    assert_eq!(find(&l, &id)["known"], false);
    s.execute("faces.models.install", &json!({"path": f, "acknowledged": true})).unwrap();
    assert_eq!(s.execute("faces.models.inspect", &json!({"path": f})).unwrap()["alreadyInstalled"], true);

    // choose it; the choice and the on/off setting survive a new session on the same folder
    assert!(s.execute("faces.models.select", &json!({"id": "auraface-v1"})).is_err(), "not installed");
    assert!(s.execute("faces.models.select", &json!({"id": "yunet-2023mar"})).is_err(), "a detector is not a recogniser");
    s.execute("faces.models.select", &json!({"id": id})).unwrap();
    assert_eq!(s.execute("faces.enable", &json!({"enabled": true})).unwrap()["enabled"], true);
    let mut again = session(&d);
    let l = again.execute("faces.models.list", &json!({})).unwrap();
    assert_eq!((l["enabled"].clone(), l["embedder"].clone()), (json!(true), json!(id)));
    assert_eq!(again.execute("faces.enable", &json!({})).unwrap()["enabled"], false, "toggles when omitted");

    // removing it clears the choice
    assert_eq!(s.execute("faces.models.remove", &json!({"id": id})).unwrap()["removed"], id);
    assert!(!home.exists());
    assert_eq!(s.execute("faces.models.list", &json!({})).unwrap()["embedder"], Value::Null);
    assert!(s.execute("faces.models.remove", &json!({"id": id})).is_err(), "already gone");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hostile_ids_unsupported_models_and_odd_folders_are_handled() {
    let d = temp("hostile");
    let mut s = session(&d);
    let models = d.join("models");
    std::fs::create_dir_all(&models).unwrap();
    // something that is not a model sits in the folder: it is never listed and never deleted
    let precious = models.join("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("keep.txt"), b"mine").unwrap();
    for id in ["precious", "..", ".", "", "../x", "a/b", r"a\b", ".hidden", "yunet-2023mar", "UPPER"] {
        assert!(s.execute("faces.models.remove", &json!({"id": id})).is_err(), "{id:?}");
    }
    assert!(precious.join("keep.txt").is_file());
    assert!(d.join("models").is_dir());
    // a folder whose manifest lies about its id is ignored
    let liar = models.join("liar");
    std::fs::create_dir_all(&liar).unwrap();
    std::fs::write(liar.join("model.onnx"), lightcraft_faces::synthetic::embedder_model(64)).unwrap();
    let mut m = lightcraft_faces::known::auraface();
    m.id = "someone-else".into();
    std::fs::write(liar.join("face-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(models.join("settings.json"), b"{ not json").unwrap();
    let l = s.execute("faces.models.list", &json!({})).unwrap();
    assert!(find(&l, "someone-else").is_null() && find(&l, "liar").is_null());
    assert_eq!(l["enabled"], false, "a corrupt settings file is ignored");
    // not-a-model and wrong-shaped files cannot be installed
    let junk = d.join("junk.onnx");
    std::fs::write(&junk, b"nonsense").unwrap();
    let e = s.execute("faces.models.install", &json!({"path": junk.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("cannot be used yet"), "{e}");
    let tokens = d.join("tokens.onnx");
    {
        use lightcraft_faces::synthetic::{len_field, value_info_bytes};
        let mut graph = Vec::new();
        len_field(11, &value_info_bytes("pixels", 1, &[Err("N"), Ok(3), Ok(224), Ok(224)]), &mut graph);
        len_field(12, &value_info_bytes("tokens", 1, &[Err("N"), Ok(257), Ok(384)]), &mut graph);
        let mut model = Vec::new();
        len_field(7, &graph, &mut model);
        std::fs::write(&tokens, model).unwrap();
    }
    assert!(s.execute("faces.models.install", &json!({"path": tokens.to_string_lossy(), "acknowledged": true})).is_err());
    assert!(s.execute("faces.models.select", &json!({"id": 5})).is_err());
    assert!(s.execute("faces.models.select", &json!({"id": ""})).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
