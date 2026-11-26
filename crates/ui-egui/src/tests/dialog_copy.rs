//! A dialog's confirm button names what it does ("Rename", "Create", "Create Stacks"…), not "OK":
//! the button is the last thing read before a change is made. Every check reads the text painted
//! inside the `button:dialogOk` widget itself, not just any text in the window.

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
    h.settle(T);
    h
}

fn show(h: &mut Headless, dialog: Dialog) {
    h.app.ui.dialog = Some(dialog);
    h.step();
    h.step();
}

fn dialog(value: serde_json::Value) -> Dialog {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("bad dialog json: {e}"))
}

/// The text painted inside the confirm button.
fn confirm_label(h: &Headless) -> String {
    let rect = h.app.widgets.iter().find(|(w, _)| w == "button:dialogOk").map(|(_, r)| *r).unwrap_or_else(|| panic!("no confirm button"));
    h.view
        .painted_shapes()
        .into_iter()
        .filter_map(|s| match s {
            egui::epaint::Shape::Text(t) if rect.contains(t.pos + t.galley.rect.center().to_vec2()) => Some(t.galley.job.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn select(h: &mut Headless, n: usize) {
    let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(n).collect();
    h.app.session.selection.ids = ids;
}

fn text_with(h: &Headless, start: &str) -> String {
    h.painted_text().into_iter().find(|t| t.starts_with(start)).unwrap_or_else(|| panic!("no text starting {start:?}"))
}

#[test]
fn confirm_buttons_name_their_action() {
    let mut h = demo();
    h.app
        .run("photo.setMeta", json!({"ids": [h.app.session.catalog.photos().next().map(|p| p.id.0).unwrap_or(0)], "addKeywords": ["taken"]}))
        .unwrap();
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        ("new album", json!({"kind": "newAlbum", "name": "", "folder": false}), "Create"),
        ("new folder", json!({"kind": "newAlbum", "name": "", "folder": true}), "Create"),
        ("rename album", json!({"kind": "renameAlbum", "id": 1, "name": "A"}), "Rename"),
        ("new smart album", json!({"kind": "newSmartAlbum", "name": ""}), "Create"),
        ("auto-stack", json!({"kind": "autoStack", "gap": 60.0}), "Create Stacks"),
        ("capture time", json!({"kind": "captureTime", "mode": "set", "time": "", "days": 0, "hours": 0, "minutes": 0, "zone": 0.0}), "Change Time"),
        ("label names", json!({"kind": "labelNames", "names": [], "save_as": ""}), "Save"),
        ("rename photos", json!({"kind": "rename", "template": "{name}", "start": 1}), "Rename"),
        ("rename keyword", json!({"kind": "renameKeyword", "from": "a", "to": "b"}), "Rename"),
        ("rename keyword onto an existing one", json!({"kind": "renameKeyword", "from": "a", "to": "taken"}), "Merge Keywords"),
        ("rename person", json!({"kind": "renamePerson", "from": "Ann", "to": "Anna", "merge": false}), "Rename"),
        ("merge keywords", json!({"kind": "mergeKeywords", "from": ["a"], "into": "b"}), "Merge Keywords"),
        ("assisted culling", json!({"kind": "cull", "reject_below": 0.0, "pick_best": true, "burst_gap": 10.0, "similarity": 0.93}), "Cull"),
        ("new preset", json!({"kind": "createPreset", "name": "", "group": ""}), "Create"),
        ("copy settings", json!({"kind": "copySettings", "groups": []}), "Copy"),
        ("paste settings", json!({"kind": "pasteSettings", "groups": []}), "Paste"),
    ];
    for (what, value, want) in cases {
        show(&mut h, dialog(value));
        assert_eq!(confirm_label(&h), want, "{what}");
    }
    // a new smart album's rules are created; an existing one's are saved
    for (id, want) in [(None, "Create"), (Some(1), "Save")] {
        let d = Dialog::SmartRules { id, name: "A".into(), rules: Default::default(), parent: None };
        show(&mut h, d);
        assert_eq!(confirm_label(&h), want, "smart album rules {id:?}");
    }
}

#[test]
fn one_field_prompts_name_what_they_do() {
    let mut h = demo();
    let prompts = [
        ("album.rename", "Rename"),
        ("folder.rename", "Rename"),
        ("folder.create", "Create"),
        ("version.rename", "Rename"),
        ("preset.rename", "Rename"),
        ("mask.renameEffect", "Rename"),
        ("mask.setEffectGroup", "Create"),
        ("preset.move", "Move"),
        ("app.newLibrary", "Create"),
        ("keyword.saveSet", "Save"),
        ("filter.savePreset", "Save"),
        ("curve.savePreset", "Save"),
        ("metadata.savePreset", "Save"),
        ("photo.tagFromTracklog", "Add Locations"),
    ];
    for (command, want) in prompts {
        let prompt = Dialog::TextPrompt {
            title: "Title".into(),
            hint: "Hint".into(),
            value: String::new(),
            command: command.into(),
            params: json!({}),
            key: "name".into(),
        };
        show(&mut h, prompt);
        assert_eq!(confirm_label(&h), want, "{command}");
    }
}

#[test]
fn export_confirm_says_how_many_photos() {
    let mut h = demo();
    for (n, want) in [(1, "Export 1 Photo"), (3, "Export 3 Photos")] {
        select(&mut h, n);
        h.app.run("dialog.export", json!({})).unwrap();
        h.step();
        h.step();
        assert_eq!(confirm_label(&h), want);
    }
}

/// With Move, a checked file whose photo is in Recently Deleted is restored, not moved: the confirm counts it apart.
#[test]
fn the_move_confirm_counts_restored_photos_apart_from_moved_ones() {
    let mut h = demo();
    let review = |copy: bool, move_files: bool, on_deleted: &str, checked: [bool; 3]| {
        let candidates = (0..3).map(|i| lightcraft_engine::import::ImportCandidate { path: format!("img{i}.jpg"), ..Default::default() }).collect();
        let mut opts = crate::import::ImportDialog::new(candidates);
        opts.copy = copy;
        opts.move_files = move_files;
        opts.trashed = vec![true, false, false];
        opts.on_deleted = on_deleted.into();
        opts.checked = checked.to_vec();
        Dialog::Import { opts: Box::new(opts) }
    };
    for (copy, move_files, on_deleted, checked, want) in [
        (true, true, "restore", [true, true, true], "Move 2 Photos · Restore 1 Photo"),
        (true, true, "restore", [true, true, false], "Move 1 Photo · Restore 1 Photo"),
        (true, true, "restore", [true, false, false], "Restore 1 Photo"),
        (true, true, "restore", [false, true, true], "Move 2 Photos"),
        (true, true, "fresh", [true, true, true], "Move 3 Photos"),
        // "Leave them" still restores a file that was checked by hand: the engine's default
        (true, true, "", [true, true, true], "Move 2 Photos · Restore 1 Photo"),
        (true, false, "restore", [true, true, true], "Import 3 Photos"),
    ] {
        show(&mut h, review(copy, move_files, on_deleted, checked));
        // the wide review sits over the photo grid: its text (view tab, file names) is painted inside the button's rectangle too,
        // so the label is what the rectangle's text ends with, and it names each verb as often as `want` does
        let label = confirm_label(&h);
        let said = |verb: &str| (label.matches(verb).count(), want.matches(verb).count());
        let what = format!("copy {copy}, move {move_files}, {on_deleted}, {checked:?}: {label:?}");
        assert!(label.ends_with(want), "{what}");
        assert_eq!(said("Move").0, said("Move").1, "{what}");
        assert_eq!(said("Restore").0, said("Restore").1, "{what}");
    }
}

#[test]
fn an_unusable_model_file_only_closes() {
    let mut h = demo();
    for kind in ["denoiseModel", "faceModel"] {
        let mut d = json!({"kind": kind, "info": {"kind": "unsupported", "fileName": "x.onnx", "sizeBytes": 1}, "accepted": false});
        d["path"] = json!("x.onnx");
        show(&mut h, dialog(d));
        assert_eq!(confirm_label(&h), "Close", "{kind}");
    }
}

#[test]
fn the_confirm_is_the_verb_in_another_language() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::i18n::set_language(crate::i18n::Locale::En);
        }
    }
    let _restore = Restore;
    let mut h = demo();
    h.app.ui.language = crate::i18n::Locale::De;
    show(&mut h, dialog(json!({"kind": "autoStack", "gap": 60.0})));
    assert_eq!(confirm_label(&h), "Stapel erstellen");
    show(&mut h, dialog(json!({"kind": "captureTime", "mode": "set", "time": "", "days": 0, "hours": 0, "minutes": 0, "zone": 0.0})));
    assert_eq!(confirm_label(&h), "Zeit ändern");
    show(&mut h, dialog(json!({"kind": "renameAlbum", "id": 1, "name": "A"})));
    assert_eq!(confirm_label(&h), "Umbenennen");
}

/// "N noun(s)" in `text`: the number before `noun` and whether `noun` is written in the plural.
fn count_before(text: &str, noun: &str) -> (u64, bool) {
    let at = text.find(noun).unwrap_or_else(|| panic!("{text:?} has no {noun:?}"));
    let n = text[..at].trim_end().rsplit(' ').next().and_then(|w| w.parse().ok()).unwrap_or_else(|| panic!("no number before {noun:?} in {text:?}"));
    (n, text[at + noun.len()..].starts_with('s'))
}

#[test]
fn counts_agree_with_their_nouns() {
    let mut h = demo();
    // contact sheet: one photo, then two
    for n in [1usize, 2] {
        select(&mut h, n);
        let options = serde_json::to_value(lightcraft_engine::contact_sheet::Options::default()).unwrap();
        show(&mut h, dialog(json!({"kind": "contactSheet", "options": options})));
        let text = text_with(&h, &format!("{n} selected photo"));
        assert_eq!(text, if n == 1 { "1 selected photo · 1 page" } else { "2 selected photos · 1 page" });
    }
    // auto-stack: a tiny gap and a huge one give different counts; each agrees with its noun
    for gap in [1.0, 86400.0] {
        show(&mut h, dialog(json!({"kind": "autoStack", "gap": gap})));
        let text = text_with(&h, "Creates ");
        let (stacks, plural) = count_before(&text, " stack");
        assert_eq!(plural, stacks != 1, "{text:?}");
    }
    // slideshow export: photos and slides
    show(
        &mut h,
        dialog(
            json!({"kind": "slideshowExport", "settings": serde_json::to_value(lightcraft_engine::slideshow_export::ExportSettings::default()).unwrap()}),
        ),
    );
    let text = h.painted_text().into_iter().find(|t| t.contains("in view")).unwrap_or_default();
    let (photos, p1) = count_before(&text, " photo");
    let (slides, p2) = count_before(&text, " slide");
    assert_eq!((p1, p2), (photos != 1, slides != 1), "{text:?}");
}
