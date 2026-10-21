//! Public command seam for migrated grain settings, transfer and undo/redo.
use crate::Session;
use lightcraft_develop::GrainProcess;
use serde_json::json;

#[test]
fn medium_grain_paste_presets_and_manual_sync_migrate_old_inputs() {
    for command in ["develop.paste", "develop.sync", "preset.apply"] {
        for (source, dest) in [("legacy", "mediumParticlesV1"), ("mediumParticlesV1", "legacy")] {
            let mut s = Session::with_demo();
            let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
            s.execute("library.select", &json!({"ids":[ids[0].0]})).unwrap();
            s.execute("develop.merge", &json!({"settings":{"grain":{"process":source,"amount":40}}})).unwrap();
            s.execute("develop.copy", &json!({"groups":["grain"]})).unwrap();
            let preset = s.execute("preset.create", &json!({"name":"Grain test","groups":["grain"]})).unwrap()["id"].clone();
            s.execute("library.select", &json!({"ids":[ids[1].0]})).unwrap();
            s.execute("develop.merge", &json!({"settings":{"grain":{"process":dest,"amount":20}}})).unwrap();
            if command == "develop.sync" {
                s.execute("library.select", &json!({"ids":[ids[0].0,ids[1].0],"active":ids[0].0})).unwrap();
            }
            let params = if command == "preset.apply" { json!({"id":preset}) } else { json!({"groups":["grain"]}) };
            s.execute(command, &params).unwrap();
            let expected = GrainProcess::MediumParticlesV1;
            let out = s.develop_of(ids[1]).unwrap();
            assert_eq!(out.grain.process, expected, "{command}: {source}");
            assert_eq!(out.grain.amount, 40.0);
            s.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(s.develop_of(ids[1]).unwrap().grain.amount, 20.0);
            assert_eq!(s.develop_of(ids[1]).unwrap().grain.process, expected);
            s.execute("edit.redo", &json!({})).unwrap();
            assert_eq!(s.develop_of(ids[1]).unwrap().grain.process, expected);
        }
    }
}

#[test]
fn medium_grain_auto_sync_tracks_numeric_changes_from_both_aliases_and_undo_redo() {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
    s.execute("library.select", &json!({"ids":[ids[0].0,ids[1].0],"active":ids[0].0})).unwrap();
    s.execute("develop.autoSync", &json!({"on":true})).unwrap();
    for (process, amount, previous) in [("mediumParticlesV1", 40.0, 0.0), ("legacy", 20.0, 40.0)] {
        s.execute("develop.merge", &json!({"settings":{"grain":{"process":process,"amount":amount}}})).unwrap();
        for id in &ids {
            assert_eq!(s.develop_of(*id).unwrap().grain.process, GrainProcess::MediumParticlesV1);
            assert_eq!(s.develop_of(*id).unwrap().grain.amount, amount);
        }
        s.execute("edit.undo", &json!({})).unwrap();
        for id in &ids {
            assert_eq!(s.develop_of(*id).unwrap().grain.process, GrainProcess::MediumParticlesV1);
            assert_eq!(s.develop_of(*id).unwrap().grain.amount, previous);
        }
        s.execute("edit.redo", &json!({})).unwrap();
        for id in &ids {
            assert_eq!(s.develop_of(*id).unwrap().grain.process, GrainProcess::MediumParticlesV1);
            assert_eq!(s.develop_of(*id).unwrap().grain.amount, amount);
        }
    }
}
