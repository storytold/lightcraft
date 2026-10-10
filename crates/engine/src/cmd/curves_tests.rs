use super::*;
use lightcraft_develop::DevelopSettings;

fn sparse(points: &[(f64, f64)]) -> PointCurve {
    points.iter().map(|&(x, y)| Point::new(x, y)).collect::<Vec<_>>().into()
}

fn function() -> PointCurve {
    let base = sparse(&[(0.0, 0.08), (0.3, 0.17), (0.7, 0.83), (1.0, 0.94)]);
    let target = sparse(&[(0.0, 0.02), (0.2, 0.36), (0.6, 0.5), (1.0, 0.99)]);
    let curve = base.blend(&target, 0.37).unwrap();
    assert!(curve.is_function());
    curve
}

#[test]
fn curve_preset_preserves_function_model_and_legacy_sparse_wire() {
    let before =
        ToneCurve { master: function(), red: sparse(&[(0.25, 0.25), (0.75, 0.75)]), shadows: 12.0, refine_saturation: 38.0, ..Default::default() };
    let preset = CurvePreset::from_curve("Function and interior", &before);
    let text = to_file(std::slice::from_ref(&preset));
    let wire: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(wire["format"], FILE_FORMAT);
    assert_eq!(wire["version"], 2);
    assert!(wire.get("presets").is_none(), "v2 must not look like the old parser's presets envelope");
    assert_eq!(wire["curves"][0]["master"]["kind"], "function");
    assert_eq!(wire["curves"][0]["red"], json!([[0.25, 0.25], [0.75, 0.75]]));
    let parsed = parse_file(text.as_bytes()).unwrap();
    assert_eq!(parsed, vec![preset]);
    let mut after = ToneCurve { shadows: before.shadows, refine_saturation: before.refine_saturation, ..Default::default() };
    parsed[0].apply(&mut after);
    assert_eq!(after, before);
    assert!(parsed[0].matches(&after));
    assert!(!parsed[0].matches(&ToneCurve::default()));
    assert!(after.master.editable_points().len() < 16, "a saved function must not become a graph full of LUT samples");
}

#[test]
fn curve_files_validate_envelopes_and_bare_fields_without_breaking_v1() {
    let preset = json!({"name":"Legacy", "master":[[0,0.1],[1,0.9]]});
    for value in [
        preset.clone(),
        json!([preset.clone()]),
        json!({"format":FILE_FORMAT,"version":1,"presets":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":2,"curves":[preset.clone()]}),
    ] {
        let read = parse_file(value.to_string().as_bytes()).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "Legacy");
        assert_eq!(read[0].master.sparse_points().unwrap(), [Point::new(0.0, 0.1), Point::new(1.0, 0.9)]);
    }
    for value in [
        json!({"format":"other","version":1,"presets":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":999,"presets":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":999,"curves":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":2,"presets":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":1,"curves":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":2,"curves":[preset.clone()],"presets":[preset.clone()]}),
        json!({"format":FILE_FORMAT,"version":2,"curves":[preset.clone()],"future":true}),
        json!({"presets":[preset]}),
        json!({"name":"Future", "futureCurve":true}),
        json!({"name":"Broken", "master":[[0,0,0],[1,1]]}),
    ] {
        assert!(parse_file(value.to_string().as_bytes()).is_err(), "{value}");
    }
    let duplicate = format!(r#"{{"format":"{FILE_FORMAT}","version":999,"version":2,"curves":[{{"name":"Duplicate"}}]}}"#);
    assert!(parse_file(duplicate.as_bytes()).is_err(), "duplicate versions cannot erase the unsupported marker");
    let mut value = serde_json::to_value(CurvePreset::from_curve("Future model", &ToneCurve { master: function(), ..Default::default() })).unwrap();
    value["master"]["version"] = json!(999);
    assert!(parse_file(value.to_string().as_bytes()).is_err());
}

#[test]
fn curve_reset_removes_the_full_function_model_only_for_selected_channels() {
    let original = ToneCurve {
        master: function(),
        red: function(),
        green: function(),
        blue: function(),
        shadows: 12.0,
        refine_saturation: 38.0,
        ..Default::default()
    };
    let mut curve = original.clone();
    reset_channel(&mut curve, "parametric").unwrap();
    assert_eq!(curve.master, original.master);
    assert_eq!(curve.red, original.red);
    assert_eq!(curve.shadows, 0.0);
    assert_eq!(curve.refine_saturation, original.refine_saturation);
    reset_channel(&mut curve, "red").unwrap();
    assert!(curve.red.is_identity() && !curve.red.is_function());
    assert_eq!(curve.master, original.master);
    reset_channel(&mut curve, "point").unwrap();
    for point in [&curve.master, &curve.red, &curve.green, &curve.blue] {
        assert!(point.is_identity() && !point.is_function());
    }
    assert_eq!(curve.refine_saturation, original.refine_saturation);
    reset_channel(&mut curve, "all").unwrap();
    assert_eq!(curve, ToneCurve::default());
    let before = original.clone();
    let mut curve = original;
    assert!(reset_channel(&mut curve, "future").is_err());
    assert_eq!(curve, before);
}

#[test]
fn point_command_preserves_function_base_and_undoes_exactly() {
    let mut session = Session::with_demo();
    let id = session.active().unwrap();
    let mut settings = (*session.develop_of(id).unwrap()).clone();
    settings.curve.master = function();
    let original = settings.curve.master.clone();
    let original_wire = serde_json::to_value(&original).unwrap();
    session.set_develop(id, settings, "Function fixture").unwrap();
    let result = session.execute("develop.curvePoint", &json!({"channel":"rgb","action":"add","x":0.5,"y":0.72})).unwrap();
    assert_eq!(result["channel"], "master");
    let index = result["index"].as_u64().unwrap();
    let added = session.develop_of(id).unwrap().curve.master.clone();
    assert!(added.is_function());
    assert_eq!(serde_json::to_value(&added).unwrap()["base"], original_wire["base"]);
    session.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(session.develop_of(id).unwrap().curve.master, original);
    session.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(session.develop_of(id).unwrap().curve.master, added);
    let moved = session.execute("develop.curvePoint", &json!({"action":"move","index":index,"x":0.55,"y":0.76})).unwrap();
    assert_eq!(moved["curve"]["base"], original_wire["base"]);
    let removed = session.execute("develop.curvePoint", &json!({"action":"remove","index":moved["index"]})).unwrap();
    assert!(removed["index"].is_null());
    let after = session.develop_of(id).unwrap().curve.master.clone();
    assert_eq!(serde_json::to_value(&after).unwrap()["base"], original_wire["base"]);
    assert_eq!(
        after.effective_function().unwrap(),
        original.effective_function().unwrap(),
        "removing the only adjustment recovers the exact base function"
    );
    assert_eq!(after.editable_points().len(), 2);
}

#[test]
fn point_command_rejects_invalid_inputs_without_any_saved_change() {
    let mut session = Session::with_demo();
    let id = session.active().unwrap();
    let mut settings = DevelopSettings::default();
    settings.curve.master = function();
    session.set_develop(id, settings, "Function fixture").unwrap();
    for params in [
        json!({"channel":"future","action":"add","x":0.4,"y":0.6}),
        json!({"action":"future"}),
        json!({"action":"add","x":0.4}),
        json!({"action":"add","x":-1,"y":0.6}),
        json!({"action":"add","x":0.4,"y":2}),
        json!({"action":"add","x":null,"y":0.6}),
        json!({"action":"move","index":-1,"x":0.4,"y":0.6}),
        json!({"action":"move","index":0.5,"x":0.4,"y":0.6}),
        json!({"action":"move","index":u64::MAX,"x":0.4,"y":0.6}),
        json!({"action":"remove","index":0}),
        json!({"action":"remove","index":u64::MAX}),
    ] {
        let before = session.develop_of(id).unwrap();
        let undo = session.undo.len();
        assert!(session.execute("develop.curvePoint", &params).is_err(), "{params}");
        assert_eq!(session.develop_of(id).unwrap(), before, "{params}");
        assert_eq!(session.undo.len(), undo, "{params}");
    }
}

#[test]
fn targeted_curve_adjustment_keeps_the_function_base() {
    let mut session = Session::with_demo();
    let id = session.active().unwrap();
    let mut settings = (*session.develop_of(id).unwrap()).clone();
    settings.curve.master = function();
    let base = serde_json::to_value(&settings.curve.master).unwrap()["base"].clone();
    session.set_develop(id, settings, "Function fixture").unwrap();
    let response = session.execute("develop.targeted", &json!({"target":"curve","channel":"master","x":0.5,"y":0.35,"delta":12.75})).unwrap();
    assert_eq!(response["curve.master"]["base"], base);
    let after = session.develop_of(id).unwrap().curve.master.clone();
    assert!(after.is_function());
    assert_eq!(serde_json::to_value(after).unwrap()["base"], base);
}

#[test]
fn function_preset_survives_commands_library_reopen_and_failed_import() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("lightcraft-function-curves-{}-{nonce}", std::process::id()));
    let export = dir.join("function.lccurve");
    let invalid = dir.join("future.lccurve");
    let original = function();
    {
        let mut session = Session::new().with_fs();
        session.open_library(&dir, true).unwrap();
        let id = session.active().unwrap();
        let mut settings = (*session.develop_of(id).unwrap()).clone();
        settings.curve.master = original.clone();
        session.set_develop(id, settings, "Function fixture").unwrap();
        session.execute("curve.savePreset", &json!({"name":"Function"})).unwrap();
        session.execute("curve.exportPresets", &json!({"path":export})).unwrap();
        session.execute("curve.deletePreset", &json!({"name":"Function"})).unwrap();
        let imported = session.execute("curve.importPresets", &json!({"path":export})).unwrap();
        assert_eq!(imported["imported"], json!(["Function"]));
        assert_eq!(session.curve_presets[0].master, original);

        let before = session.curve_presets.clone();
        let prefs_before = std::fs::read(dir.join("prefs.json")).unwrap();
        let mut future = serde_json::to_value(&before[0]).unwrap();
        future["master"]["version"] = json!(999);
        std::fs::write(&invalid, json!({"format":FILE_FORMAT,"version":2,"curves":[{"name":"Function"}, future]}).to_string()).unwrap();
        let failed = session.execute("curve.importPresets", &json!({"path":invalid})).unwrap();
        assert_eq!(failed["imported"], json!([]));
        assert_eq!(failed["failed"].as_array().unwrap().len(), 1);
        assert_eq!(session.curve_presets, before, "a later unsupported preset cannot partially replace earlier presets");
        assert_eq!(std::fs::read(dir.join("prefs.json")).unwrap(), prefs_before);
        session.close_library().unwrap();
    }
    let mut session = Session::new().with_fs();
    session.open_library(&dir, false).unwrap();
    assert_eq!(session.curve_presets.len(), 1);
    assert_eq!(session.curve_presets[0].master, original);
    session.execute("curve.reset", &json!({"channel":"point"})).unwrap();
    session.execute("curve.applyPreset", &json!({"name":"Function"})).unwrap();
    assert_eq!(session.develop_of(session.active().unwrap()).unwrap().curve.master, original);
    session.close_library().unwrap();
    drop(session);
    std::fs::remove_dir_all(dir).unwrap();
}
