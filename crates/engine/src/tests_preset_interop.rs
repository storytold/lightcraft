//! Metadata and export presets: Lightroom Classic `.lrtemplate` and XMP template import, our own
//! `.lcmeta` / `.lcexport` files, rename, apply to a selection, and exported files carrying the
//! preset's metadata. Every preset file is written by the test itself (no fixtures).

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::{export_preset_files, metadata_files};
use crate::export::{ExportOptions, export_photo};
use crate::tests_xmp::temp_dir;

/// A Lightroom Classic metadata preset as the person's file would hold it (written here).
const LR_METADATA: &str = r#"s = {
	id = "6E1B0C55-0000-0000-0000-000000000001",
	internalName = "Studio Rights",
	title = "Studio Rights",
	type = "Metadata",
	value = {
		["com.adobe.copyright"] = "© 2026 Ann Lee",
		["com.adobe.copyrightState"] = "copyrighted",
		["com.adobe.rightsUsageTerms"] = "Editorial use only",
		["com.adobe.copyrightInfoUrl"] = "https://example.org/rights",
		["com.adobe.creator"] = "Ann Lee",
		["com.adobe.creatorJobTitle"] = "Photographer",
		["com.adobe.creatorAddress"] = "1 Harbour Road",
		["com.adobe.creatorCity"] = "Hobart",
		["com.adobe.creatorState"] = "TAS",
		["com.adobe.creatorZip"] = 7000,
		["com.adobe.creatorCountry"] = "Australia",
		["com.adobe.creatorWorkPhone"] = "+61 3 0000 0000",
		["com.adobe.creatorWorkEmail"] = "ann@example.org",
		["com.adobe.creatorWorkWebsite"] = "https://example.org",
		["com.adobe.headline"] = "Studio day",
		["com.adobe.descriptionWriter"] = "Ann Lee",
		["com.adobe.iptcCategory"] = "ACE",
		["com.adobe.iptcOtherCategories"] = "Portraits, Studio",
		["com.adobe.iptcSubjectCode"] = "01000000; 13000000",
		["com.adobe.intellectualGenre"] = "Not here",
		["com.example.plugin.field"] = "x",
	},
	version = 0,
}
"#;

/// A Lightroom Classic export preset (written here).
fn lr_export(folder: &str) -> String {
    format!(
        r#"s = {{
	id = "6E1B0C55-0000-0000-0000-000000000002",
	internalName = "Client JPEG",
	title = "Client JPEG",
	type = "export",
	value = {{
		exportServiceProvider = "com.adobe.ag.export.file",
		export_destinationType = "specificFolder",
		export_destinationPathPrefix = "{folder}",
		export_useSubfolder = true,
		export_destinationPathSuffix = "Client",
		export_collisionHandling = "rename",
		LR_format = "JPEG",
		LR_jpeg_quality = 0.8,
		LR_jpeg_useLimitSize = true,
		LR_jpeg_limitSize = 500,
		LR_export_colorSpace = "sRGB",
		LR_size_doConstrain = true,
		LR_size_resizeType = "longEdge",
		LR_size_maxWidth = 48,
		LR_size_maxHeight = 48,
		LR_size_units = "pixels",
		LR_size_resolution = 300,
		LR_size_resolutionUnits = "inch",
		LR_size_doNotEnlarge = true,
		LR_outputSharpeningOn = true,
		LR_outputSharpeningMedia = "screen",
		LR_outputSharpeningLevel = 1,
		LR_embeddedMetadataOption = "copyrightAndContactOnly",
		LR_removeLocationMetadata = true,
		LR_renamingTokensOn = true,
		LR_tokens = "{{{{custom_token}}}}-{{{{naming_sequenceNumber_2Digits}}}}{{{{unknown_thing}}}}",
		LR_tokenCustomString = "Client",
		LR_initialSequenceNumber = 1,
		LR_useWatermark = true,
	}},
	version = 0,
}}
"#,
        folder = folder.replace('\\', "\\\\")
    )
}

fn write(path: &std::path::Path, text: &str) -> String {
    std::fs::write(path, text).unwrap();
    path.to_string_lossy().to_string()
}

fn png(path: &std::path::Path, seed: u8) {
    let img = lcc_raster::Rgba8::from_fn(64, 40, |x, y| [(x as u8).wrapping_mul(3).wrapping_add(seed), (y * 5) as u8, 90, 255]);
    let bytes = crate::export::encode_image(&img, &ExportOptions::from_json(&json!({"format": "png"}))).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library with two PNG photos, both selected.
fn two_photos(tag: &str) -> (Session, lcc_testkit::TempDir, Vec<lcc_catalog::PhotoId>) {
    let dir = temp_dir(tag);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    png(&src.join("a.png"), 1);
    png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    assert_eq!(ids.len(), 2);
    s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    (s, dir, ids)
}

#[test]
fn lightroom_metadata_preset_imports_and_applies_to_the_selection_in_one_step() {
    let (mut s, dir, ids) = two_photos("meta-lr");
    let f = write(&dir.join("Studio Rights.lrtemplate"), LR_METADATA);
    // through Import Profiles & Presets… (preset.import), the route the UI and drag & drop take
    let r = s.execute("preset.import", &json!({"paths": [f]})).unwrap();
    let rows = r["metadataPresets"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{r}");
    assert_eq!(rows[0]["name"], "Studio Rights");
    let unmapped: Vec<&str> = rows[0]["unmapped"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(unmapped.contains(&"intellectualGenre") && unmapped.contains(&"com.example.plugin.field"), "{unmapped:?}");
    assert!(!unmapped.contains(&"headline") && !unmapped.contains(&"iptcSubjectCode"), "{unmapped:?}");
    assert!(r["imported"].as_array().unwrap().is_empty(), "not a develop preset: {r}");
    assert!(r["failed"].as_array().unwrap().is_empty(), "{r}");
    let fields = &s.metadata_presets[0].fields;
    assert_eq!(fields["creatorEmail"], "ann@example.org");
    assert_eq!(fields["creatorPostalCode"], "7000", "a number in the file becomes text");
    assert_eq!(fields["copyrightStatus"], "copyrighted");
    // apply to both selected photos: one undo step
    s.execute("metadata.applyPreset", &json!({"name": "studio rights"})).unwrap();
    for id in &ids {
        let m = &s.catalog.photo(*id).unwrap().meta;
        assert_eq!((m.creator.as_str(), m.usage_terms.as_str()), ("Ann Lee", "Editorial use only"));
        assert_eq!((m.contact.email.as_str(), m.contact.city.as_str(), m.contact.job_title.as_str()), ("ann@example.org", "Hobart", "Photographer"));
        assert_eq!((m.headline.as_str(), m.description_writer.as_str(), m.category.as_str()), ("Studio day", "Ann Lee", "ACE"));
        assert_eq!(m.supplemental_categories, ["Portraits", "Studio"]);
        assert_eq!(m.subject_code, ["01000000", "13000000"]);
    }
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(ids.iter().all(|id| s.catalog.photo(*id).unwrap().meta.contact.is_empty()), "both undone at once");
    // importing the same file again adds nothing
    let r = s.execute("metadata.importPresets", &json!({"paths": [f]})).unwrap();
    assert_eq!(r["imported"][0]["skipped"], true, "{r}");
    assert_eq!(s.metadata_presets.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xmp_metadata_template_imports_and_sidecars_do_not() {
    let dir = temp_dir("meta-xmp");
    let mut s = Session::new();
    let m = lcc_meta::Metadata {
        copyright: Some("© Studio".into()),
        artist: Some("Sam".into()),
        contact: lcc_meta::CreatorContact { phone: "+1 555".into(), ..Default::default() },
        keywords: vec!["studio".into()],
        ..Default::default()
    };
    let mut packet = lcc_meta::write_xmp(&m, None);
    packet = packet.replace("   <dc:rights>", "   <photoshop:Instructions>Hi</photoshop:Instructions>\n   <dc:rights>");
    let f = write(&dir.join("Studio.xmp"), &packet);
    let r = s.execute("metadata.importPresets", &json!({"paths": [f]})).unwrap();
    assert_eq!(r["imported"][0]["name"], "Studio", "{r}");
    assert_eq!(r["imported"][0]["unmapped"], json!(["photoshop:Instructions"]), "{r}");
    let fields = &s.metadata_presets[0].fields;
    assert_eq!((fields["copyright"].as_str(), fields["creatorPhone"].as_str()), (Some("© Studio"), Some("+1 555")));
    assert_eq!(fields["keywords"], json!(["studio"]));
    // a photo's sidecar (rating, capture data) or a develop preset is not a metadata template
    let sidecar = lcc_meta::write_xmp(&lcc_meta::Metadata { rating: Some(3), copyright: Some("x".into()), ..Default::default() }, None);
    let f = write(&dir.join("IMG_1.xmp"), &sidecar);
    let r = s.execute("metadata.importPresets", &json!({"paths": [f]})).unwrap();
    assert_eq!(r["failed"][0][1], "not a metadata preset", "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Other categories and subject codes survive a template's import, our own file and the preset an
/// import applies to each new photo, which doesn't go through `photo.setMeta`.
#[test]
fn metadata_preset_lists_reach_the_photo_at_import() {
    let dir = temp_dir("meta-lists");
    let mut s = Session::new();
    let m = lcc_meta::Metadata {
        supplemental_categories: vec!["Rain".into(), "Rivers".into()],
        subject_code: vec!["17000000".into()],
        ..Default::default()
    };
    let f = write(&dir.join("Weather.xmp"), &lcc_meta::write_xmp(&m, None));
    let r = s.execute("metadata.importPresets", &json!({"paths": [f]})).unwrap();
    assert_eq!(r["imported"][0]["unmapped"], json!([]), "{r}");
    let fields = s.metadata_presets[0].fields.clone();

    let (kept, unmapped) = metadata_files::clean_fields(fields.as_object().unwrap());
    assert!(unmapped.is_empty(), "{unmapped:?}");
    assert_eq!(Value::Object(kept), fields, "an `.lcmeta` file keeps them");
    let (_, unmapped) = metadata_files::clean_fields(json!({"subjectCode": "17000000", "supplementalCategories": [1]}).as_object().unwrap());
    assert_eq!(unmapped, ["subjectCode", "supplementalCategories"], "a list of text or nothing");

    let mut meta = lcc_catalog::Meta { subject_code: vec!["old".into()], ..Default::default() };
    crate::cmd::metadata::apply_to(&mut meta, &fields);
    assert_eq!(meta.supplemental_categories, ["Rain", "Rivers"]);
    assert_eq!(meta.subject_code, ["17000000"], "replaced, like text");
    crate::cmd::metadata::apply_to(&mut meta, &json!({"subjectCode": "not a list", "supplementalCategories": [" a ", "a", ""]}));
    assert_eq!((meta.subject_code.as_slice(), meta.supplemental_categories.as_slice()), (&["17000000".to_string()][..], &["a".to_string()][..]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lightroom_export_preset_maps_every_listed_setting() {
    let dir = temp_dir("export-lr");
    let out = dir.join("out");
    let mut s = Session::new();
    let f = write(&dir.join("Client JPEG.lrtemplate"), &lr_export(&out.to_string_lossy()));
    let r = s.execute("export.importPresets", &json!({"paths": [f]})).unwrap();
    let row = &r["imported"][0];
    assert_eq!(row["name"], "Client JPEG", "{r}");
    let p = &s.export_presets[0].params;
    assert_eq!((p["format"].as_str(), p["quality"].as_i64(), p["limitKb"].as_i64()), (Some("jpeg"), Some(80), Some(500)));
    assert_eq!((p["longEdge"].as_i64(), p["ppi"].as_i64(), p["dontEnlarge"].as_bool()), (Some(48), Some(300), Some(true)));
    assert_eq!((p["sharpen"].as_str(), p["sharpenAmount"].as_str()), (Some("screen"), Some("low")));
    assert_eq!((p["metadata"].as_str(), p["removeLocation"].as_bool()), (Some("copyrightContact"), Some(true)));
    assert_eq!((p["naming"].as_str(), p["customText"].as_str()), (Some("{custom}-{seq:2}"), Some("Client")));
    assert_eq!((p["destinationMode"].as_str(), p["subfolder"].as_str()), (Some("specificFolder"), Some("Client")));
    assert_eq!(p["dir"].as_str(), Some(out.to_string_lossy().as_ref()));
    let unmapped: Vec<&str> = row["unmapped"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(unmapped.iter().any(|u| u.contains("watermark")), "{unmapped:?}");
    assert!(unmapped.iter().any(|u| u.contains("unknown_thing")), "{unmapped:?}");
    // the preset expands as app.export params, and they validate
    let params = s.export_params(&json!({"preset": "Client JPEG"})).unwrap();
    ExportOptions::validate("app.export", &params).unwrap();
    // a single-file export says where itself: the preset's folder is dropped
    let one = s.export_params(&json!({"preset": "Client JPEG", "path": "x.jpg"})).unwrap();
    assert!(one.get("dir").is_none(), "{one}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Lightroom Classic `.lrtemplate` export preset files saved on disk store their keys without the
/// `LR_` prefix (e.g. `format`, `jpeg_quality`, `size_doConstrain`, `outputSharpeningOn`,
/// `embeddedMetadataOption`, `renamingTokensOn`, `tokens`, `collisionHandling`, `useWatermark`)
/// alongside `export_bitDepth = 16`. Importing such a preset preserves every supported setting and
/// clearly lists every unsupported setting.
#[test]
fn unprefixed_lightroom_classic_export_presets_retain_supported_settings_and_list_unsupported_ones() {
    use crate::export::{
        Conflict, DestinationMode, ExportFormat, MetadataPolicy, OutputSpace, ResizeMode, SharpenAmount, SharpenFor, TiffCompression,
    };
    let dir = temp_dir("export-lr-unprefixed");
    let out = dir.join("exports");
    std::fs::create_dir_all(&out).unwrap();
    let jpeg_template = format!(
        r#"s = {{
	id = "3A1B0C55-0000-0000-0000-000000000099",
	internalName = "Gallery Web",
	title = "Gallery Web",
	type = "Export",
	value = {{
		collisionHandling = "overwrite",
		embeddedMetadataOption = "allExceptCameraInfo",
		exportFiltersFromPlugins = {{
			"com.example.plugin",
		}},
		exportServiceProvider = "com.adobe.ag.export.file",
		export_bitDepth = 16,
		export_colorSpace = "AdobeRGB",
		export_destinationPathPrefix = "{folder}",
		export_destinationPathSuffix = "Gallery",
		export_destinationType = "specificFolder",
		export_postProcessing = "openInPhotoshop",
		export_useSubfolder = true,
		extensionCase = "uppercase",
		format = "JPEG",
		includeVideoFiles = false,
		initialSequenceNumber = 5,
		jpeg_limitSize = 1200,
		jpeg_quality = 0.85,
		jpeg_useLimitSize = true,
		outputSharpeningLevel = 3,
		outputSharpeningMedia = "glossy",
		outputSharpeningOn = true,
		reimportExportedPhoto = true,
		removeFaceMetadata = false,
		removeLocationMetadata = true,
		renamingTokensOn = true,
		size_doConstrain = true,
		size_doNotEnlarge = true,
		size_maxHeight = 1080,
		size_maxWidth = 1920,
		size_resizeType = "wh",
		size_resolution = 240,
		size_resolutionUnits = "inch",
		size_units = "pixels",
		tokenCustomString = "Gallery",
		tokens = "{{{{custom_token}}}}_{{{{image_filename_number_suffix}}}}-{{{{date_YYYYMMDD}}}}-{{{{naming_sequenceNumber_3Digits}}}}",
		useWatermark = true,
	}},
	version = 0,
}}
"#,
        folder = out.to_string_lossy().replace('\\', "\\\\")
    );
    let tiff_template = r#"s = {
	id = "3A1B0C55-0000-0000-0000-000000000100",
	internalName = "Print Master TIFF",
	title = "Print Master TIFF",
	type = "Export",
	value = {
		collisionHandling = "skip",
		exportServiceProvider = "com.adobe.ag.export.file",
		export_bitDepth = 16,
		export_colorSpace = "ProPhotoRGB",
		export_destinationType = "sourceFolder",
		format = "TIFF",
		minimizeEmbeddedMetadata = true,
		outputSharpeningLevel = "standard",
		outputSharpeningMedia = "matte",
		outputSharpeningOn = true,
		renamingTokensOn = false,
		size_doConstrain = true,
		size_doNotEnlarge = false,
		size_maxHeight = 8,
		size_maxWidth = 10,
		size_resizeType = "dimensions",
		size_resolution = 300,
		size_resolutionUnits = "inch",
		size_units = "inch",
		tiff_compressionMethod = "compressionMethod_LZW",
	},
	version = 0,
}
"#;
    let f1 = write(&dir.join("Gallery Web.lrtemplate"), &jpeg_template);
    let f2 = write(&dir.join("Print Master TIFF.lrtemplate"), tiff_template);
    let mut s = Session::new();
    let r = s.execute("preset.import", &json!({"paths": [f1, f2]})).unwrap();
    let rows = r["exportPresets"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{r}");

    // 1. Gallery Web JPEG preset: dormant export_bitDepth = 16 does not reject JPEG, and all settings carry over
    let params1 = s.export_params(&json!({"preset": "Gallery Web"})).unwrap();
    let opts1 = ExportOptions::from_params(&params1).unwrap();
    assert_eq!(opts1.format, ExportFormat::Jpeg);
    assert_eq!(opts1.quality, 85);
    assert_eq!(opts1.limit_kb, Some(1200));
    assert_eq!(opts1.color_space, OutputSpace::AdobeRgb);
    assert_eq!(opts1.ppi, 240);
    let resize1 = opts1.resize.expect("resizing preserved");
    assert_eq!(resize1.mode, ResizeMode::WidthHeight);
    assert_eq!((resize1.value as u32, resize1.height, resize1.dont_enlarge), (1920, 1080, true));
    assert_eq!((opts1.sharpen, opts1.sharpen_amount), (SharpenFor::Glossy, SharpenAmount::High));
    assert_eq!((opts1.metadata, opts1.remove_location), (MetadataPolicy::AllExceptCamera, true));
    assert_eq!(opts1.naming, "{custom}_{num}-{date:%Y%m%d}-{seq:3}");
    assert_eq!((opts1.custom_text.as_str(), opts1.start_number), ("Gallery", 5));
    assert_eq!(opts1.destination_mode, DestinationMode::SpecificFolder);
    assert_eq!(opts1.subfolder, "Gallery");
    assert_eq!(opts1.conflict, Conflict::Overwrite);
    assert_eq!(params1["dir"].as_str(), Some(out.to_string_lossy().as_ref()));

    // Unsupported settings are clearly listed on the import report and on the stored preset
    let unmapped1: Vec<&str> = rows[0]["unmapped"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for expected in ["watermark", "plug-in", "after export", "add to this catalog", "person info", "uppercase", "video"] {
        assert!(unmapped1.iter().any(|u| u.contains(expected)), "missing `{expected}` in {unmapped1:?}");
    }

    // 2. Print Master TIFF preset: 16-bit LZW TIFF, inch Dimensions sizing, minimizeEmbeddedMetadata -> Copyright
    let params2 = s.export_params(&json!({"preset": "Print Master TIFF"})).unwrap();
    let opts2 = ExportOptions::from_params(&params2).unwrap();
    assert_eq!(opts2.format, ExportFormat::Tiff);
    assert_eq!(opts2.bit_depth, Some(16));
    assert_eq!(opts2.tiff_compression, TiffCompression::Lzw);
    assert_eq!(opts2.color_space, OutputSpace::ProPhoto);
    assert_eq!(opts2.ppi, 300);
    let resize2 = opts2.resize.expect("inch dimensions sizing preserved");
    assert_eq!(resize2.mode, ResizeMode::Dimensions);
    assert_eq!((resize2.value as u32, resize2.height, resize2.dont_enlarge), (3000, 2400, false));
    assert_eq!((opts2.sharpen, opts2.sharpen_amount), (SharpenFor::Matte, SharpenAmount::Standard));
    assert_eq!(opts2.metadata, MetadataPolicy::Copyright);
    assert_eq!(opts2.destination_mode, DestinationMode::SameAsOriginal);
    assert_eq!(opts2.conflict, Conflict::Skip);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn naming_tokens_map_or_are_reported() {
    let (n, unknown) = export_preset_files::lr_naming("{{image_name}}_{{date_YYYY}}{{date_MM}}{{date_DD}}-{{naming_sequenceNumber_4Digits}}");
    assert_eq!(n, "{name}_{date:%Y}{date:%m}{date:%d}-{seq:4}");
    assert!(unknown.is_empty());
    let (n, unknown) = export_preset_files::lr_naming("{{com.adobe.title}} {{mystery}} {{unclosed");
    assert_eq!(n, "{title}  {{unclosed");
    assert_eq!(unknown, ["mystery"]);
}

#[test]
fn own_preset_files_round_trip_with_rename() {
    let dir = temp_dir("preset-files");
    let mut s = Session::new();
    s.execute("metadata.savePreset", &json!({"name": "Rights", "fields": {"copyright": "© A", "creatorEmail": "a@b.c"}})).unwrap();
    s.execute("metadata.renamePreset", &json!({"name": "rights", "newName": "My Rights"})).unwrap();
    s.execute(
        "export.savePreset",
        &json!({"name": "Web", "params": {"format": "jpeg", "quality": 70, "longEdge": 1600, "dir": "/tmp/web", "ids": [1]}}),
    )
    .unwrap();
    assert!(s.export_presets[0].params.get("ids").is_none(), "a selection is not a setting");
    assert_eq!(s.export_presets[0].params["dir"], "/tmp/web", "the folder is");
    assert!(s.execute("export.renamePreset", &json!({"name": "Web", "newName": "DNG"})).is_err(), "a built-in's name");
    s.execute("export.renamePreset", &json!({"name": "web", "newName": "Web 1600"})).unwrap();
    let meta = s.execute("metadata.exportPresets", &json!({"path": dir.join("mine").to_string_lossy()})).unwrap();
    let exp = s.execute("export.exportPresets", &json!({"path": dir.join("mine").to_string_lossy()})).unwrap();
    assert!(meta["path"].as_str().unwrap().ends_with(".lcmeta") && exp["path"].as_str().unwrap().ends_with(".lcexport"));
    // into a fresh session, through the general import
    let mut t = Session::new();
    let r = t.execute("preset.import", &json!({"paths": [meta["path"], exp["path"]]})).unwrap();
    assert_eq!(r["metadataPresets"][0]["name"], "My Rights", "{r}");
    assert_eq!(r["exportPresets"][0]["name"], "Web 1600", "{r}");
    assert_eq!(t.metadata_presets[0].fields, s.metadata_presets[0].fields);
    assert_eq!(t.export_presets[0].params, s.export_presets[0].params);
    // a different preset of a name in use is kept beside it
    t.execute("metadata.savePreset", &json!({"name": "My Rights", "fields": {"copyright": "© B"}})).unwrap();
    let r = t.execute("metadata.importPresets", &json!({"paths": [meta["path"]]})).unwrap();
    assert_eq!(r["imported"][0]["name"], "My Rights (2)", "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// End to end: a metadata preset applied to the selection, exported as JPEG and TIFF with an
/// export preset; the files carry the creator, copyright, usage terms and contact details.
#[test]
fn exported_files_carry_the_applied_metadata_preset() {
    let (mut s, dir, ids) = two_photos("meta-export");
    let f = write(&dir.join("Studio Rights.lrtemplate"), LR_METADATA);
    s.execute("preset.import", &json!({"paths": [f]})).unwrap();
    s.execute("metadata.applyPreset", &json!({"name": "Studio Rights"})).unwrap();
    for (format, policy) in [("jpeg", "copyrightContact"), ("tiff", "all"), ("png", "all")] {
        s.execute("export.savePreset", &json!({"name": format!("P {format}"), "params": {"format": format, "longEdge": 32, "metadata": policy}}))
            .unwrap();
        let params = s.export_params(&json!({"preset": format!("P {format}")})).unwrap();
        let o = ExportOptions::from_params(&params).unwrap();
        for id in &ids {
            let e = export_photo(&mut s, *id, &o, 1).unwrap();
            let m = lcc_meta::extract(&e.bytes);
            assert_eq!(m.artist.as_deref(), Some("Ann Lee"), "{format}: {m:?}");
            assert_eq!(m.copyright.as_deref(), Some("© 2026 Ann Lee"), "{format}");
            assert_eq!(m.usage_terms.as_deref(), Some("Editorial use only"), "{format}");
            assert_eq!(m.copyright_marked, Some(true), "{format}");
            assert_eq!(
                (m.contact.email.as_str(), m.contact.phone.as_str(), m.contact.website.as_str()),
                ("ann@example.org", "+61 3 0000 0000", "https://example.org"),
                "{format}"
            );
            assert_eq!(m.contact.job_title, "Photographer", "{format}");
        }
    }
    // "copyright only" leaves the contact details out
    let o = ExportOptions::from_params(&json!({"format": "jpeg", "longEdge": 32, "metadata": "copyright"})).unwrap();
    let m = lcc_meta::extract(&export_photo(&mut s, ids[0], &o, 1).unwrap().bytes);
    assert!(m.contact.is_empty() && m.copyright.is_some(), "{m:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Preset files are hostile input: truncated, binary, deeply nested or wrongly typed files are
/// an error or "not this kind", never a panic.
#[test]
fn hostile_preset_files_do_not_panic() {
    let deep = format!("s = {{ type = \"Metadata\", value = {}{}", "{".repeat(5000), "}".repeat(5000));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("a.lrtemplate", b"".to_vec()),
        ("a.lrtemplate", b"s = { type = \"Metadata\", value = ".to_vec()),
        ("a.lrtemplate", b"s = { type = \"Metadata\", value = 5 }".to_vec()),
        ("a.lrtemplate", b"s = { type = \"Metadata\", value = { [\"com.adobe.copyrightState\"] = {}, [\"com.adobe.keywords\"] = true } }".to_vec()),
        ("a.lrtemplate", b"s = { type = \"export\", value = { LR_format = 7, LR_jpeg_quality = -1e308, LR_size_doConstrain = true, LR_size_resizeType = \"longEdge\", LR_size_maxWidth = 1e300, LR_size_units = \"cm\", LR_size_resolution = 0, LR_tokens = \"{{\", LR_renamingTokensOn = true, export_useSubfolder = true, export_destinationPathSuffix = \"../..\" } }".to_vec()),
        ("a.lrtemplate", b"s = { type = \"export\" }".to_vec()),
        ("a.lrtemplate", deep.into_bytes()),
        ("a.lrtemplate", vec![0xff, 0xfe, 0x00, 0x80, 0x7b]),
        ("a.xmp", b"<x:xmpmeta><rdf:RDF><rdf:Description".to_vec()),
        ("a.xmp", vec![0u8; 64]),
        ("a.lcmeta", b"{\"format\": \"photo.metadataPresets\", \"presets\": [{\"name\": 5}, {\"name\": \"x\", \"fields\": [1]}, 3]}".to_vec()),
        ("a.lcmeta", b"{\"format\": \"photo.metadataPresets\", \"presets\": {}}".to_vec()),
        ("a.lcexport", b"{\"format\": \"photo.exportPresets\", \"presets\": [{\"name\": \"x\", \"params\": {\"quality\": \"high\", \"format\": []}}]}".to_vec()),
        ("a.json", b"[[[[[[".to_vec()),
        ("a.zip", b"PK\x03\x04garbage".to_vec()),
    ];
    for (name, bytes) in &cases {
        let _ = metadata_files::parse_bytes(name, bytes);
        let _ = export_preset_files::parse_bytes(name, bytes);
    }
    // through the commands too, from disk
    let dir = temp_dir("preset-hostile");
    let mut s = Session::new();
    let paths: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, (name, bytes))| {
            let p = dir.join(format!("{i}-{name}"));
            std::fs::write(&p, bytes).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    s.execute("metadata.importPresets", &json!({"paths": paths})).unwrap();
    s.execute("export.importPresets", &json!({"paths": paths})).unwrap();
    let _ = s.execute("preset.import", &json!({"paths": paths}));
    assert!(s.export_presets.iter().all(|p| ExportOptions::validate("app.export", &p.params).is_ok()), "only presets the exporter takes");
    let _ = std::fs::remove_dir_all(&dir);
}
