//! Slideshow templates: built-ins, user template files, validation, apply / previous.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-sstpl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A demo session whose user templates live in a fresh folder.
fn session(tag: &str) -> (Session, PathBuf) {
    let dir = temp_dir(tag);
    let mut s = Session::with_demo();
    s.slide_templates_dir = Some(dir.join(DIR_NAME));
    (s, dir)
}

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn err(s: &mut Session, id: &str, p: Value) -> String {
    match s.execute(id, &p) {
        Ok(v) => panic!("{id} {p} should fail, got {v}"),
        Err(e) => e.to_string(),
    }
}

fn ids(s: &mut Session) -> Vec<String> {
    run(s, "slideshow.templates.list", json!({}))["templates"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
}

/// A look different from the defaults in every section.
fn custom(s: &mut Session) {
    run(s, "slideshow.playback", json!({"slideDuration": 7.5, "fadeDuration": 0.5, "fadeColor": "#102030", "randomOrder": true, "repeat": false}));
    run(
        s,
        "slideshow.layout",
        json!({"margins": 0.1, "strokeBorder": true, "strokeColor": "#ff0000", "castShadow": true, "aspect": "4:3", "backdropColor": "#334455"}),
    );
    run(
        s,
        "slideshow.overlays",
        json!({"ratingStars": {"enabled": true}, "addText": {"text": "{caption} — {exposure}", "anchor": "top"}, "showWatermark": true, "watermark": "© me"}),
    );
    run(s, "slideshow.titles", json!({"intro": {"enabled": true, "content": "text", "text": "Hello", "duration": 3}}));
}

#[test]
fn builtins_are_valid_distinct_and_apply_cleanly() {
    let b = builtins();
    assert_eq!(b.len(), BUILTINS.len(), "every built-in parses");
    assert!((4..=6).contains(&b.len()));
    let mut names: Vec<_> = b.iter().map(|t| t.name.to_lowercase()).collect();
    names.dedup();
    assert_eq!(names.len(), b.len(), "unique names");
    // our own names: none of Lightroom Classic's built-in slideshow template names
    for lr in ["default", "simple", "caption and rating", "crop to fill", "exif metadata", "widescreen"] {
        assert!(!names.contains(&lr.to_string()), "{lr}");
    }
    for t in &b {
        assert!(!t.description.is_empty(), "{}", t.id);
        // no image or font files: nothing that needs an attribution or can go missing
        assert!(
            t.look.overlays.identity_plate.image.is_empty() && t.look.overlays.watermark.image.is_empty() && t.look.layout.background_image.is_none()
        );
        // the document form reads back to the same look
        let back = parse(&serde_json::to_vec(&t.to_doc()).unwrap()).unwrap();
        assert_eq!(back.look, t.look, "{}", t.id);
    }
    let looks: Vec<_> = b.iter().map(|t| &t.look).collect();
    for (i, a) in looks.iter().enumerate() {
        assert!(looks.iter().skip(i + 1).all(|o| o != a), "each built-in looks different");
    }
    let (mut s, dir) = session("builtins");
    let active = s.active().unwrap();
    for t in b {
        let r = run(&mut s, "slideshow.templates.apply", json!({"id": t.id}));
        assert_eq!(r["warnings"], json!([]), "{}: {r}", t.id);
        assert_eq!(
            SlideLook { playback: PlaybackSettings { prepare_previews: true, ..t.look.playback.clone() }, ..t.look.clone() },
            SlideLook::of(&s)
        );
        let img = run(&mut s, "slideshow.preview", json!({"id": active.0, "width": 160}));
        assert_eq!(img["width"], 160, "{}", t.id);
    }
    // by name, case-insensitive
    run(&mut s, "slideshow.templates.apply", json!({"name": "edge TO edge"}));
    assert!(s.slide_layout.zoom_to_fill);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn save_list_apply_restores_exactly_and_previous_goes_back() {
    let (mut s, dir) = session("roundtrip");
    s.slideshow.prepare_previews = false;
    custom(&mut s);
    let saved = SlideLook::of(&s);
    let r = run(&mut s, "slideshow.templates.save", json!({"name": "My Look"}));
    assert_eq!(r["id"], "user:My Look");
    let path = PathBuf::from(r["path"].as_str().unwrap());
    assert_eq!(path, dir.join(DIR_NAME).join("My Look.json"));
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!((doc["format"].as_str(), doc["version"].as_u64(), doc["name"].as_str()), (Some(FORMAT), Some(VERSION), Some("My Look")));
    assert!(doc["playback"].get("preparePreviews").is_none(), "a machine preference, not part of the look");
    assert!(ids(&mut s).contains(&"user:My Look".to_string()));

    run(&mut s, "slideshow.templates.apply", json!({"id": "builtin:plain-black"}));
    assert_ne!(SlideLook::of(&s), saved);
    assert!(!s.slideshow.prepare_previews, "prepare previews is kept");
    let r = run(&mut s, "slideshow.templates.apply", json!({"id": "user:My Look"}));
    assert_eq!(r["applied"], "user:My Look");
    assert_eq!(SlideLook::of(&s), saved, "applying restores every setting exactly");

    // previous: back to Plain Black's, then forward again
    let plain = builtins().into_iter().find(|t| t.id == "builtin:plain-black").unwrap();
    run(&mut s, "slideshow.templates.apply", json!({"id": PREVIOUS}));
    assert_eq!(s.slide_layout, plain.look.layout);
    run(&mut s, "slideshow.templates.apply", json!({"id": PREVIOUS}));
    assert_eq!(SlideLook::of(&s), saved);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn previous_needs_an_apply_first() {
    let (mut s, dir) = session("noprev");
    assert_eq!(run(&mut s, "slideshow.templates.list", json!({}))["previous"], false);
    assert!(err(&mut s, "slideshow.templates.apply", json!({"id": "previous"})).contains("no previous settings"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn update_rename_delete_export_import() {
    let (mut s, dir) = session("ops");
    run(&mut s, "slideshow.templates.save", json!({"name": "Mine"}));
    // a taken name, case-insensitively, and a built-in's are refused; replace overwrites
    assert!(err(&mut s, "slideshow.templates.save", json!({"name": "mine"})).contains("already exists"));
    assert!(err(&mut s, "slideshow.templates.save", json!({"name": "Plain Black"})).contains("built-in"));
    run(&mut s, "slideshow.layout", json!({"margins": 0.2}));
    run(&mut s, "slideshow.templates.save", json!({"name": "Mine", "replace": true}));
    assert_eq!(s.slide_template("user:Mine").unwrap().look.layout.margin_top, 0.2);
    // update
    run(&mut s, "slideshow.layout", json!({"margins": 0.05}));
    run(&mut s, "slideshow.templates.update", json!({"id": "user:Mine"}));
    assert_eq!(s.slide_template("user:Mine").unwrap().look.layout.margin_top, 0.05);
    assert!(err(&mut s, "slideshow.templates.update", json!({"id": "builtin:plain-black"})).contains("built-in"));
    // rename (also just the case)
    let r = run(&mut s, "slideshow.templates.rename", json!({"id": "user:Mine", "name": "Ours"}));
    assert_eq!(r["id"], "user:Ours");
    assert!(!dir.join(DIR_NAME).join("Mine.json").exists());
    let r = run(&mut s, "slideshow.templates.rename", json!({"id": "user:Ours", "name": "OURS"}));
    assert_eq!(r["id"], "user:OURS");
    assert_eq!(ids(&mut s).iter().filter(|i| i.starts_with(USER)).count(), 1);
    run(&mut s, "slideshow.templates.save", json!({"name": "Other"}));
    assert!(err(&mut s, "slideshow.templates.rename", json!({"id": "user:Other", "name": "ours"})).contains("already exists"));
    assert!(err(&mut s, "slideshow.templates.rename", json!({"id": "builtin:cinema-wide", "name": "X"})).contains("built-in"));
    // export a user template and a built-in, import them back
    let out = dir.join("out");
    let a = out.join("ours.json").to_string_lossy().to_string();
    run(&mut s, "slideshow.templates.export", json!({"id": "user:OURS", "path": a}));
    let b = out.join("cinema.json").to_string_lossy().to_string();
    run(&mut s, "slideshow.templates.export", json!({"id": "builtin:cinema-wide", "path": b}));
    let r = run(&mut s, "slideshow.templates.import", json!({"path": a}));
    assert_eq!((r["name"].as_str(), r["renamed"].as_bool()), (Some("OURS (2)"), Some(true)), "{r}");
    assert_eq!(s.slide_template("user:OURS (2)").unwrap().look, s.slide_template("user:OURS").unwrap().look);
    let r = run(&mut s, "slideshow.templates.import", json!({"path": b}));
    assert_eq!(r["name"], "Cinema Wide (2)", "a built-in's name is taken too");
    let cinema = s.slide_template("builtin:cinema-wide").unwrap();
    assert_eq!(s.slide_template("user:Cinema Wide (2)").unwrap().look, cinema.look);
    // delete
    run(&mut s, "slideshow.templates.delete", json!({"id": "user:OURS"}));
    assert!(!ids(&mut s).contains(&"user:OURS".to_string()));
    assert!(err(&mut s, "slideshow.templates.delete", json!({"id": "user:OURS"})).contains("no slideshow template"));
    assert!(err(&mut s, "slideshow.templates.delete", json!({"id": "builtin:plain-black"})).contains("built-in"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn file_names_are_sanitised_and_ids_never_leave_the_folder() {
    assert_eq!(file_stem("../../etc/passwd").as_deref(), Some("______etc_passwd"));
    assert_eq!(file_stem(r"C:\x\y").as_deref(), Some("C__x_y"));
    assert_eq!(file_stem("a:b*c?d\"e<f>g|h").as_deref(), Some("a_b_c_d_e_f_g_h"));
    assert_eq!(file_stem("  Holiday (2024)  ").as_deref(), Some("Holiday (2024)"));
    assert_eq!(file_stem("Été à Kyōto 京都").as_deref(), Some("Été à Kyōto 京都"));
    assert_eq!(file_stem("con").as_deref(), Some("con_"));
    assert_eq!(file_stem("LPT1").as_deref(), Some("LPT1_"));
    assert_eq!(file_stem("..."), None);
    assert_eq!(file_stem("///"), None);
    assert_eq!(file_stem(&"x".repeat(500)).map(|s| s.chars().count()), Some(64));
    let (mut s, dir) = session("escape");
    let folder = dir.join(DIR_NAME);
    let r = run(&mut s, "slideshow.templates.save", json!({"name": "../../escape"}));
    assert_eq!(r["id"], "user:______escape");
    assert!(folder.join("______escape.json").exists());
    assert!(!dir.join("escape.json").exists());
    for name in ["", "   ", "...", "/", "a\nb", &"y".repeat(81)] {
        err(&mut s, "slideshow.templates.save", json!({"name": name}));
    }
    // an id can't point outside the folder (or at a file LightCraft wouldn't name)
    std::fs::write(dir.join("victim.json"), b"{}").unwrap();
    for id in ["user:../victim", "user:..", "user:/etc/passwd", "user:a/b", "user:", "user:x.json"] {
        let e = err(&mut s, "slideshow.templates.delete", json!({"id": id}));
        assert!(e.contains("isn't a template id"), "{id}: {e}");
    }
    assert!(dir.join("victim.json").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_files_are_reported_and_skipped() {
    let (mut s, dir) = session("bad");
    let folder = dir.join(DIR_NAME);
    std::fs::create_dir_all(&folder).unwrap();
    run(&mut s, "slideshow.templates.save", json!({"name": "Good"}));
    let doc = |extra: Value| {
        let mut d = json!({"format": FORMAT, "version": 1, "name": "X"});
        for (k, v) in extra.as_object().unwrap() {
            d[k] = v.clone();
        }
        serde_json::to_vec(&d).unwrap()
    };
    let files: Vec<(&str, Vec<u8>, &str)> = vec![
        ("truncated.json", br#"{"format": "lightcraft.slideshowTemplate", "vers"#.to_vec(), "not a slideshow template"),
        ("binary.json", vec![0xff, 0xfe, 0x00, 0x01], "not UTF-8"),
        ("array.json", b"[1, 2]".to_vec(), "not a JSON object"),
        ("preset.json", br#"{"format": "lightcraft.preset", "version": 1}"#.to_vec(), "format `lightcraft.preset`"),
        ("newer.json", doc(json!({"version": 2})), "newer LightCraft"),
        ("noversion.json", br#"{"format": "lightcraft.slideshowTemplate"}"#.to_vec(), "no `version`"),
        ("badversion.json", doc(json!({"version": -1})), "`version`"),
        ("unknown.json", doc(json!({"music": {}})), "unknown key `music`"),
        ("nan.json", doc(json!({"layout": {"margins": "wide"}})), "layout"),
        ("badkey.json", doc(json!({"overlays": {"fireworks": true}})), "overlays"),
        ("badtype.json", doc(json!({"titles": [1]})), "`titles` must be an object"),
        ("toomanytexts.json", doc(json!({"overlays": {"texts": vec![json!({}); 40]}})), "at most"),
        ("badname.json", doc(json!({"name": 5})), "`name`"),
        ("huge.json", vec![b' '; MAX_FILE as usize + 1], "too large"),
        ("bad name!.json", doc(json!({})), "file name"),
    ];
    for (f, bytes, _) in &files {
        std::fs::write(folder.join(f), bytes).unwrap();
    }
    std::fs::create_dir_all(folder.join("folder.json")).unwrap();
    std::fs::write(folder.join("notes.txt"), b"ignored").unwrap();
    let r = run(&mut s, "slideshow.templates.list", json!({}));
    let user: Vec<_> = r["templates"].as_array().unwrap().iter().filter(|t| t["builtin"] == false).collect();
    assert_eq!(user.len(), 1, "only the good one: {r}");
    let problems: Vec<String> = r["problems"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    for (f, _, why) in &files {
        assert!(problems.iter().any(|p| p.starts_with(f) && p.contains(why)), "{f}: {why} in {problems:?}");
    }
    assert!(problems.iter().any(|p| p.starts_with("folder.json") && p.contains("not a regular file")), "{problems:?}");
    assert_eq!(problems.len(), files.len() + 1, "{problems:?}");
    // the same refusals on import
    for (f, _, why) in files.iter().filter(|(f, _, _)| *f != "bad name!.json") {
        let e = err(&mut s, "slideshow.templates.import", json!({"path": folder.join(f)}));
        assert!(e.contains(why), "{f}: {e}");
    }
    // out-of-range numbers in a hand-edited file are clamped, never applied as they are
    std::fs::write(folder.join("wild.json"), doc(json!({"layout": {"margins": 9, "shadowAngle": 1e300}, "playback": {"slideDuration": 1e9}})))
        .unwrap();
    // written by hand, not through LightCraft: the folder's time may not have moved since the
    // listing above (same clock tick), so the cached listing is dropped as a write would drop it
    s.slide_templates_cache = None;
    let t = s.slide_template("user:wild").unwrap();
    assert_eq!(t.look.layout.margin_left, lightcraft_slideshow::MARGIN_MAX);
    assert_eq!(t.look.playback.slide_duration, crate::slideshow::SLIDE_MAX);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn thousands_of_templates_are_capped_not_fatal() {
    let (mut s, dir) = session("many");
    let folder = dir.join(DIR_NAME);
    std::fs::create_dir_all(&folder).unwrap();
    let body = |i: usize| format!(r#"{{"format": "{FORMAT}", "version": 1, "name": "T{i}"}}"#);
    for i in 0..MAX_TEMPLATES + 50 {
        std::fs::write(folder.join(format!("T{i:05}.json")), body(i)).unwrap();
    }
    let r = run(&mut s, "slideshow.templates.list", json!({}));
    let user = r["templates"].as_array().unwrap().iter().filter(|t| t["builtin"] == false).count();
    assert_eq!(user, MAX_TEMPLATES);
    assert!(r["problems"][0].as_str().unwrap().contains("only the first"), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_files_apply_with_a_warning() {
    let (mut s, dir) = session("missing");
    let folder = dir.join(DIR_NAME);
    std::fs::create_dir_all(&folder).unwrap();
    let d = json!({
        "format": FORMAT, "version": 1, "name": "Logo",
        "layout": {"backgroundImage": 987654},
        "overlays": {"identityPlate": {"enabled": true, "image": "/no/such/logo.png"}, "showWatermark": true, "watermark": {"text": "", "image": "/no/such/mark.png"}}
    });
    std::fs::write(folder.join("Logo.json"), serde_json::to_vec(&d).unwrap()).unwrap();
    let r = run(&mut s, "slideshow.templates.apply", json!({"id": "user:Logo"}));
    let w = r["warnings"].to_string();
    assert!(w.contains("background image") && w.contains("logo.png") && w.contains("mark.png"), "{w}");
    assert_eq!(s.slide_overlays.identity_plate.image, "/no/such/logo.png", "applied anyway");
    // the slide still renders (the backdrop colour, no plate)
    let id = s.active().unwrap();
    run(&mut s, "slideshow.preview", json!({"id": id.0, "width": 64}));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn preview_of_a_template_leaves_the_settings_alone() {
    let (mut s, dir) = session("preview");
    custom(&mut s);
    let before = SlideLook::of(&s);
    let id = s.active().unwrap();
    let mine = run(&mut s, "slideshow.preview", json!({"id": id.0, "width": 200, "height": 120}));
    let r = run(&mut s, "slideshow.preview", json!({"id": id.0, "width": 200, "height": 120, "template": "builtin:edge-to-edge"}));
    assert_eq!(r["template"], "builtin:edge-to-edge");
    assert_eq!(r["photo"], json!([0.0, 0.0, 200.0, 120.0]), "zoom to fill, no margins: {r}");
    assert_ne!(r["photo"], mine["photo"]);
    assert_eq!(r["overlays"], json!([]), "the template's overlays (none), not the library's");
    let t = run(&mut s, "slideshow.preview", json!({"title": "ending", "template": "builtin:cinema-wide", "width": 160}));
    assert_eq!((t["enabled"].as_bool(), t["height"].as_u64()), (Some(true), Some(90)), "the template's ending, at its 16:9 aspect: {t}");
    assert_eq!(SlideLook::of(&s), before, "nothing changed");
    assert!(s.slide_templates_previous.is_none());
    assert!(err(&mut s, "slideshow.preview", json!({"template": "builtin:nope"})).contains("no slideshow template"));
    assert!(err(&mut s, "slideshow.preview", json!({"template": 3})).contains("template id"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_a_settings_folder_only_builtins() {
    let mut s = Session::with_demo();
    assert!(s.slide_templates_dir.is_none());
    let r = run(&mut s, "slideshow.templates.list", json!({}));
    assert_eq!(r["templates"].as_array().unwrap().len(), BUILTINS.len());
    assert!(err(&mut s, "slideshow.templates.save", json!({"name": "X"})).contains("settings folder"));
    run(&mut s, "slideshow.templates.apply", json!({"id": "builtin:gallery-wall"}));
    assert!(s.slide_overlays.rating_stars.enabled);
}

#[test]
fn a_saved_template_reads_back_through_its_section_validation() {
    let (mut s, dir) = session("sections");
    custom(&mut s);
    let look = SlideLook::of(&s);
    let parsed = parse(&serde_json::to_vec(&doc("N", "d", &look)).unwrap()).unwrap();
    assert_eq!(parsed.name.as_deref(), Some("N"));
    assert_eq!(parsed.description, "d");
    assert_eq!(parsed.look.layout, look.layout);
    assert_eq!(parsed.look.overlays, look.overlays);
    assert_eq!(parsed.look.titles, look.titles);
    assert_eq!(PlaybackSettings { prepare_previews: look.playback.prepare_previews, ..parsed.look.playback }, look.playback);
    // a BOM and a missing name are fine
    let mut bytes = "\u{feff}".as_bytes().to_vec();
    bytes.extend(br#"{"format": "lightcraft.slideshowTemplate", "version": 1}"#);
    assert_eq!(parse(&bytes).unwrap().name, None);
    let _ = std::fs::remove_dir_all(dir);
}
