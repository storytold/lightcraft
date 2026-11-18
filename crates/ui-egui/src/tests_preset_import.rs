//! File ▸ Import Profiles & Presets…: `.cube` LUTs become profiles, everything else presets, and
//! one toast says what happened to both (issue #942).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h
}

fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    let d = lcc_testkit::TempDir::new(&format!("lc-ui-preset-import-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn import(h: &mut Headless, paths: &[&std::path::Path]) -> serde_json::Value {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    let r = h.request("engine.execute", json!({"command": "file.importPresets", "params": {"paths": paths}}), T);
    // a few frames apply the import to the UI; a full settle could outlast the toast on a slow
    // runner (it is shown for 1.4 s of frame time), and the tests read it
    for _ in 0..5 {
        h.step();
    }
    r
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast().cloned().map(|t| t.0).unwrap_or_default()
}

/// A 2×2×2 identity LUT.
fn identity_cube() -> String {
    let mut s = String::from("TITLE \"Identity\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                s += &format!("{r} {g} {b}\n");
            }
        }
    }
    s
}

/// Importing only `.cube` files reports the profiles, not "No new presets".
#[test]
fn a_cube_only_import_reports_the_profiles() {
    let dir = temp_dir("ok");
    let cube = dir.join("Warm.cube");
    std::fs::write(&cube, identity_cube()).unwrap();
    let mut h = app();
    let r = import(&mut h, &[&cube]);
    assert_eq!(r["ok"], true, "{r}");
    let t = toast(&h);
    assert!(t.contains("1 profile"), "{t}");
    assert!(!t.contains("preset"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.cube` that can't be imported says which file and why.
#[test]
fn a_broken_cube_says_why() {
    let dir = temp_dir("bad");
    let bad = dir.join("Broken.cube");
    std::fs::write(&bad, "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
    let mut h = app();
    let r = import(&mut h, &[&bad]);
    assert_eq!(r["ok"], true, "{r}");
    let t = toast(&h);
    assert!(t.contains("Broken"), "{t}");
    assert!(!t.contains("No new presets"), "{t}");
    let reason = r["result"]["profiles"]["failed"][0][1].as_str().unwrap_or_default().to_string();
    assert!(!reason.is_empty() && t.contains(&reason), "{t} / {r}");
    // with a good one alongside, both outcomes are in the one toast
    let good = dir.join("Good.cube");
    std::fs::write(&good, identity_cube()).unwrap();
    import(&mut h, &[&good, &bad]);
    let t = toast(&h);
    assert!(t.contains("1 profile") && t.contains("Broken"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Importing presets while Develop's left column shows the Presets panel keeps them there: the
/// presets column does not open and move the panel to the right (issue #469).
#[test]
fn importing_presets_in_develop_keeps_them_in_the_left_column() {
    let dir = temp_dir("left");
    let xmp = dir.join("Soft.xmp");
    std::fs::write(
        &xmp,
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
         <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
           crs:PresetType="Normal" crs:Name="Soft" crs:UUID="AB12" crs:Exposure2012="0.5"/>
         </rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    let mut h = app();
    h.request("engine.execute", json!({"command": "module.develop", "params": {}}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    h.app.ui.left_panel = true;
    h.settle(SETTLE);
    let r = import(&mut h, &[&xmp]);
    assert_eq!(r["ok"], true, "{r}");
    assert!(toast(&h).contains("Imported 1 preset"), "{}", toast(&h));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "the Edit panels stay on the right");
    // with the left column hidden, it comes back to show them: the presets never go to the right
    h.app.ui.left_panel = false;
    h.settle(SETTLE);
    let other = dir.join("Other.xmp");
    std::fs::write(&other, std::fs::read_to_string(&xmp).unwrap().replace("Soft", "Other").replace("AB12", "CD34")).unwrap();
    import(&mut h, &[&other]);
    assert!(h.app.ui.left_panel && crate::panels::presets::shown(&h.app), "{}", toast(&h));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Importing a Lightroom Classic `.lrtemplate` export preset preserves its supported settings in
/// the Export dialog and clearly lists any unsupported settings so the user can see what needs to
/// be configured manually.
#[test]
fn importing_lightroom_export_preset_populates_export_dialog_and_lists_unsupported_settings() {
    use lcc_engine::export::{Conflict, DestinationMode, ExportFormat, MetadataPolicy, OutputSpace, ResizeMode, SharpenAmount, SharpenFor};
    let dir = temp_dir("lr-export-preset");
    let out_dir = dir.join("client_out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let template = format!(
        r#"s = {{
	id = "3A1B0C55-0000-0000-0000-000000000201",
	internalName = "Client Proof",
	title = "Client Proof",
	type = "Export",
	value = {{
		collisionHandling = "overwrite",
		embeddedMetadataOption = "copyrightOnly",
		exportServiceProvider = "com.adobe.ag.export.file",
		export_bitDepth = 16,
		export_colorSpace = "DisplayP3",
		export_destinationPathPrefix = "{folder}",
		export_destinationPathSuffix = "Proofs",
		export_destinationType = "specificFolder",
		export_postProcessing = "openInPhotoshop",
		export_useSubfolder = true,
		format = "JPEG",
		initialSequenceNumber = 10,
		jpeg_limitSize = 800,
		jpeg_quality = 0.82,
		jpeg_useLimitSize = true,
		outputSharpeningLevel = 1,
		outputSharpeningMedia = "screen",
		outputSharpeningOn = true,
		reimportExportedPhoto = true,
		removeLocationMetadata = true,
		renamingTokensOn = true,
		size_doConstrain = true,
		size_doNotEnlarge = true,
		size_maxHeight = 1600,
		size_maxWidth = 1600,
		size_resizeType = "longEdge",
		size_resolution = 300,
		size_resolutionUnits = "inch",
		size_units = "pixels",
		tokenCustomString = "Proof",
		tokens = "{{{{custom_token}}}}-{{{{naming_sequenceNumber_3Digits}}}}",
		useWatermark = true,
	}},
	version = 0,
}}
"#,
        folder = out_dir.to_string_lossy().replace('\\', "\\\\")
    );
    let file = dir.join("Client Proof.lrtemplate");
    std::fs::write(&file, template).unwrap();

    let mut h = app();
    h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    h.settle(SETTLE);

    let r = h.request("engine.execute", json!({"command": "file.importPresets", "params": {"paths": [file.to_string_lossy()]}}), T);
    assert_eq!(r["ok"], true, "{r}");

    // The toast clearly lists the unsupported settings
    let t = toast(&h);
    assert!(t.contains("Imported 1 export preset"), "{t}");
    assert!(t.contains("watermark") && t.contains("after export action") && t.contains("add to this catalog"), "{t}");

    // The Export dialog retains all supported settings from the imported preset
    let Some(crate::state::Dialog::Export { opts, full_size, resize, limit_kb, dir: export_dir, .. }) = &h.app.ui.dialog else {
        panic!("expected Export dialog to be open");
    };
    assert_eq!(opts.format, ExportFormat::Jpeg);
    assert_eq!(opts.quality, 82);
    assert_eq!(*limit_kb, 800);
    assert_eq!(opts.color_space, OutputSpace::DisplayP3);
    assert!(!*full_size);
    assert_eq!((resize.mode, resize.value as u32, resize.dont_enlarge), (ResizeMode::LongEdge, 1600, true));
    assert_eq!(opts.ppi, 300);
    assert_eq!((opts.sharpen, opts.sharpen_amount), (SharpenFor::Screen, SharpenAmount::Low));
    assert_eq!((opts.metadata, opts.remove_location), (MetadataPolicy::Copyright, true));
    assert_eq!((opts.naming.as_str(), opts.custom_text.as_str(), opts.start_number), ("{custom}-{seq:3}", "Proof", 10));
    assert_eq!(opts.destination_mode, DestinationMode::SpecificFolder);
    assert_eq!(export_dir.as_str(), out_dir.to_string_lossy().as_ref());
    assert_eq!(opts.subfolder, "Proofs");
    assert_eq!(opts.conflict, Conflict::Overwrite);

    // And the Export dialog itself displays the unsupported-settings note
    let w = h.request("ui.widgets", json!({"filter": "label:exportPresetUnmapped"}), T);
    assert!(w["result"].as_array().is_some_and(|a| !a.is_empty()), "unsupported settings note shown in Export dialog: {w}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Importing presets from Library must not open the presets column: Develop draws that column in
/// place of its Edit panel, and with no tool strip nothing on screen brings the panel back, so the
/// next visit to Develop had the presets and no sliders (issue #875). The presets go to Develop's
/// left column, unfolded, and Develop opens with its Edit panel.
#[test]
fn importing_presets_in_library_leaves_develop_its_edit_panel() {
    let dir = temp_dir("library");
    let xmp = dir.join("Soft.xmp");
    std::fs::write(
        &xmp,
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
         <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
           crs:PresetType="Normal" crs:Name="Soft" crs:UUID="AB12" crs:Exposure2012="0.5"/>
         </rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    let mut h = app();
    h.request("engine.execute", json!({"command": "module.library"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library);
    let r = import(&mut h, &[&xmp]);
    assert_eq!(r["ok"], true, "{r}");
    assert!(toast(&h).contains("Imported 1 preset"), "{}", toast(&h));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library, "the import leaves the screen as it is");
    assert!(crate::panels::classic::is_open_with_key(&h.app, crate::panels::presets::IN_COLUMN_KEY), "Develop's Presets panel is unfolded");

    h.request("engine.execute", json!({"command": "module.develop"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "Develop opens with its Edit panel");
    assert!(
        crate::panels::classic::is_open_with_key(&h.app, crate::panels::presets::IN_COLUMN_KEY),
        "the Presets panel is unfolded in Develop's left column"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
