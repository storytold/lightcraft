use super::*;

// Original two-entry identity grid; no external profile sample or fixture.
const FINGERPRINT: &str = "E96CB8AF6484F8402BB0E6ECFC8AD0FC";
const ENCODED: &str = "M0000KZk$u}gwKGjMAj4lLuC58PZ00Y}2T0";
const NS: &str = "xmlns:r='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:c='http://ns.adobe.com/camera-raw-settings/1.0/'";

fn expected() -> LookFingerprint {
    FINGERPRINT.parse().unwrap()
}
fn attribute() -> String {
    format!("<r:Description {NS} c:Table_{FINGERPRINT}=\"{ENCODED}\"/>")
}
fn child(content: &str) -> String {
    format!("<r:Description {NS}><c:Table_{FINGERPRINT}>{content}</c:Table_{FINGERPRINT}></r:Description>")
}

#[test]
fn scalar_attribute_child_entities_and_namespace_aliases_resolve() {
    let inputs = [
        attribute(),
        child(ENCODED),
        child(&format!("<![CDATA[{ENCODED}]]>")),
        child(&ENCODED.replacen('M', "&#77;", 1)),
        attribute().replace("c:", "other:").replace("xmlns:c=", "xmlns:other="),
        attribute().replace("settings/1.0/", "settings/1.0&#47;"),
    ];
    for xmp in inputs {
        let table = resolve_type0_xmp(xmp.as_bytes(), expected()).unwrap();
        assert_eq!(table.fingerprint(), expected());
        assert_eq!(table.hsv().data, vec![[0.0, 1.0, 1.0]; 2]);
    }
    let text = format!("<?xml version='1.0' encoding='UTF-8'?>\n{}", attribute());
    assert!(resolve_type0_xmp(text.as_bytes(), expected()).is_ok());
}

#[test]
fn exact_namespace_unique_scalar_and_requested_digest_are_required() {
    let wrong_ns = attribute().replace("http://ns.adobe.com/camera-raw-settings/1.0/", "https://example.invalid/foreign");
    assert!(matches!(resolve_type0_xmp(wrong_ns.as_bytes(), expected()), Err(ResolveError::Missing)));
    let duplicate = format!(
        "<r:Description {NS} c:Table_{FINGERPRINT}=\"{ENCODED}\"><c:Table_{}>{ENCODED}</c:Table_{}></r:Description>",
        FINGERPRINT.to_ascii_lowercase(),
        FINGERPRINT.to_ascii_lowercase()
    );
    assert!(matches!(resolve_type0_xmp(duplicate.as_bytes(), expected()), Err(ResolveError::Duplicate)));
    let escaped_alias = format!(
        "<r:Description {NS} xmlns:d='http://ns.adobe.com/camera-raw-settings/1.0&#47;' c:Table_{FINGERPRINT}=\"{ENCODED}\" d:Table_{FINGERPRINT}=\"{ENCODED}\"/>"
    );
    assert!(matches!(resolve_type0_xmp(escaped_alias.as_bytes(), expected()), Err(ResolveError::Duplicate)));
    for shaped in [
        child("<r:Seq/>"),
        format!("<c:Table_{FINGERPRINT} {NS}>{ENCODED}</c:Table_{FINGERPRINT}>"),
        child(ENCODED).replace(&format!("<c:Table_{FINGERPRINT}>"), &format!("<c:Table_{FINGERPRINT} r:parseType='Resource'>")),
    ] {
        assert!(matches!(resolve_type0_xmp(shaped.as_bytes(), expected()), Err(ResolveError::Shape)));
    }
    let mismatch = attribute().replace(FINGERPRINT, "00000000000000000000000000000000");
    assert!(matches!(
        resolve_type0_xmp(mismatch.as_bytes(), "00000000000000000000000000000000".parse().unwrap()),
        Err(ResolveError::Table(TableError::Mismatch))
    ));
    // A bad requested table never falls back to an unrelated well-formed one.
    let bad = format!("<r:Description {NS} c:Table_{FINGERPRINT}='_' c:Table_00000000000000000000000000000000=\"{ENCODED}\"/>");
    assert!(matches!(resolve_type0_xmp(bad.as_bytes(), expected()), Err(ResolveError::Table(TableError::Base85))));
}

#[test]
fn malformed_xml_and_complexity_fail_before_decoding() {
    for malformed in [
        "",
        "<r>",
        "<r></s>",
        "<a/><b/>",
        "<a unknown:x='1'/>",
        "<!DOCTYPE a><a/>",
        "<?xml version='1.0' encoding='UTF-16'?><a/>",
        "<a x='1' x='2'/>",
        "<a>&unknown;</a>",
        "<a>&#0;</a>",
        "<a x='&#0;'/>",
        "<?xml version='1.0'?><?xml version='1.0'?><a/>",
        "<a>\0</a>",
    ] {
        assert!(matches!(resolve_type0_xmp(malformed.as_bytes(), expected()), Err(ResolveError::Xml)), "{malformed:?}");
    }
    assert!(matches!(resolve_type0_xmp(&[0xff], expected()), Err(ResolveError::Xml)));
    let deep = format!("{}{}", "<a>".repeat(MAX_DEPTH + 1), "</a>".repeat(MAX_DEPTH + 1));
    assert!(matches!(resolve_type0_xmp(deep.as_bytes(), expected()), Err(ResolveError::Limit)));
    let many = format!("<a>{}</a>", "<b/>".repeat(MAX_EVENTS));
    assert!(matches!(resolve_type0_xmp(many.as_bytes(), expected()), Err(ResolveError::Limit)));
    assert!(matches!(resolve_type0_xmp(&vec![b' '; MAX_XMP_BYTES + 1], expected()), Err(ResolveError::Limit)));
    assert!(matches!(resolve_type0_xmp(child(&" ".repeat(MAX_ENCODED_BYTES + 1)).as_bytes(), expected()), Err(ResolveError::Limit)));
}

#[test]
fn explicit_file_is_read_only_and_errors_do_not_echo_payloads() {
    let path = std::env::temp_dir().join(format!("lightcraft-explicit-hsv-table-{}.xmp", std::process::id()));
    let source = attribute();
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    std::io::Write::write_all(&mut file, source.as_bytes()).unwrap();
    drop(file);
    let loaded = load_type0_xmp_file(&path, expected());
    let retained = std::fs::read(&path).unwrap();
    // The same explicit resource is session-only: it changes render identity, never saved edits.
    let mut session = crate::Session::with_demo();
    let id = session.active().unwrap();
    session
        .execute("develop.merge", &serde_json::json!({"settings":{"process":2,"profile":{"id":"lc.imported","imported":{"look_table":FINGERPRINT}}}}))
        .unwrap();
    let saved = session.catalog.to_snapshot();
    let undo = session.undo.len();
    let missing = session.thumb_job(id, 64).unwrap();
    let missing_key = missing.key;
    let missing_generation = missing.cache_generation;
    assert!(missing.request.imported_table.is_none());
    let inspected = session.execute("photo.inspect", &serde_json::json!({"id":id.0})).unwrap();
    assert!(inspected["unsupported_edits"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().contains("missing resource")));
    let queried = session.execute("catalog.query", &serde_json::json!({})).unwrap();
    let summary = queried["photos"].as_array().unwrap().iter().find(|p| p["id"] == id.0).unwrap();
    assert_eq!(summary["unsupportedEdits"], inspected["unsupported_edits"]);
    let (cache, cache_key) = missing.cache.as_ref().unwrap();
    cache.put_at(missing.cache_generation, *cache_key, std::sync::Arc::new(lightcraft_raster::Rgba8::filled(1, 1, [10, 20, 30, 255])));
    let cached = missing.run();
    assert!(cached.loaded.is_none());
    assert!(cached.rendered.unwrap().unsupported_edits.iter().any(|s| s.contains("missing resource")));
    let result = session.execute("profile.loadTable", &serde_json::json!({"fingerprint":FINGERPRINT.to_lowercase(),"path":path})).unwrap();
    assert_eq!(result["changed"], true);
    let loaded_job = session.thumb_job(id, 64).unwrap();
    assert_ne!(loaded_job.key, missing_key);
    assert!(loaded_job.request.imported_table.is_some());
    assert_ne!(loaded_job.cache_generation, missing_generation);
    let loaded_key = loaded_job.key;
    let variant = session.variant_job(id, &session.develop_of(id).unwrap(), 64).unwrap();
    assert!(variant.request.imported_table.is_some());
    let quick = session.quick_view_job(id, 64, true).unwrap();
    assert!(quick.embedded.is_none());
    assert!(quick.unsupported_edits.is_empty());
    assert!(lightcraft_pipeline::profile_table_diagnostics(&loaded_job.settings, &loaded_job.request).is_empty());
    let mut hdr = (*session.develop_of(id).unwrap()).clone();
    hdr.hdr.enabled = true;
    assert!(lightcraft_pipeline::profile_table_diagnostics(&hdr, &loaded_job.request)[0].contains("HDR"));
    hdr.hdr.enabled = false;
    hdr.profile.amount = 150.0;
    assert!(lightcraft_pipeline::profile_table_diagnostics(&hdr, &loaded_job.request)[0].contains("declared Amount range"));
    let generation = session.media.rendered.generation();
    session.execute("profile.loadTable", &serde_json::json!({"fingerprint":FINGERPRINT,"path":path})).unwrap();
    assert_eq!(session.media.rendered.generation(), generation, "identical reload does not invalidate jobs");
    assert!(session.execute("profile.loadTable", &serde_json::json!({"fingerprint":FINGERPRINT,"path":path.with_extension("missing")})).is_err());
    assert_eq!(session.thumb_job(id, 64).unwrap().key, loaded_key, "failed replacement is atomic");
    assert!(session.original_guard().check(&path).is_err());
    let settings = session.develop_of(id).unwrap();
    let mut off = (*settings).clone();
    off.profile.amount = 0.0;
    assert!(session.preview_job(id, 64, 64, true, &off).unwrap().request.imported_table.is_none());
    assert!(lightcraft_pipeline::profile_table_diagnostics(&off, &loaded_job.request).is_empty());
    off.profile.amount = 100.0;
    off.set_section_enabled("profile", false);
    assert!(session.profile_table(&off).is_none());
    let mut legacy = (*settings).clone();
    legacy.process = lightcraft_develop::ProcessVersion::V1;
    assert!(lightcraft_pipeline::profile_table_diagnostics(&legacy, &loaded_job.request)[0].contains("Process 2"));
    session.execute("profile.unloadTable", &serde_json::json!({"fingerprint":FINGERPRINT})).unwrap();
    assert_eq!(session.thumb_job(id, 64).unwrap().key, missing_key);
    assert!(loaded_job.request.imported_table.is_some(), "old jobs own immutable resource snapshots");
    assert_eq!(session.catalog.to_snapshot(), saved);
    assert_eq!(session.undo.len(), undo);
    assert_eq!(std::fs::read(&path).unwrap(), source.as_bytes());
    std::fs::remove_file(&path).unwrap();
    assert_eq!(loaded.unwrap().fingerprint(), expected());
    assert_eq!(retained, source.as_bytes());
    assert!(matches!(load_type0_xmp_file(&path, expected()), Err(ResolveError::Io(std::io::ErrorKind::NotFound))));
    assert!(matches!(load_type0_xmp_file(&std::env::temp_dir(), expected()), Err(ResolveError::NotFile)));
    // Original two-entry quarter-value grid, Amount0..2: actual stage and entry-relative Amount.
    let quarter_id = "C3551A4D129484966CC3464EE0C3357B";
    let quarter = std::sync::Arc::new(
        lightcraft_raw::profile::big_table::decode_type0(":0000KZk$uzz]:0Y^dy8Ros:F8|TGJzJY]sdY{6", quarter_id.parse().unwrap()).unwrap(),
    );
    let mut s = (*settings).clone();
    s.profile.imported.as_mut().unwrap().look_table = Some(quarter_id.parse().unwrap());
    s.light.contrast = 40.0;
    let src = std::sync::Arc::new(lightcraft_raster::Rgb32f::filled(16, 8, [0.05, 0.12, 0.28]));
    let quarter_src = lightcraft_raster::Rgb32f::filled(16, 8, [0.0125, 0.03, 0.07]);
    let info = lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() };
    let mut req = lightcraft_pipeline::RenderRequest::fit(16, 8);
    req.depth = lightcraft_pipeline::OutputDepth::U16;
    req.imported_table = Some(quarter);
    let pixels = |r: lightcraft_pipeline::Rendered| match r.deep.unwrap().samples {
        lightcraft_pipeline::DeepSamples::U16(v) => v,
        _ => panic!("RGB16 required"),
    };
    let cache = lightcraft_pipeline::StageCache::default();
    let applied = pixels(lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cache));
    let mut bypass = s.clone();
    bypass.profile.amount = 0.0;
    let wanted = pixels(lightcraft_pipeline::render(&quarter_src, &info, &bypass, &req));
    assert!(applied.iter().zip(&wanted).all(|(a, b)| a.abs_diff(*b) <= 2), "table precedes nonlinear raw tone");
    let neutral = pixels(lightcraft_pipeline::render_cached(&src, &info, &bypass, &req, &cache));
    assert!(applied.iter().zip(&neutral).any(|(a, b)| a.abs_diff(*b) > 500), "fixture must exercise the table");
    let mut missing_req = req.clone();
    missing_req.imported_table = None;
    assert_eq!(neutral, pixels(lightcraft_pipeline::render(&src, &info, &bypass, &missing_req)), "Amount zero skips all new arithmetic");
    s.process = lightcraft_develop::ProcessVersion::V1;
    assert_eq!(
        pixels(lightcraft_pipeline::render(&src, &info, &s, &req)),
        pixels(lightcraft_pipeline::render(&src, &info, &s, &missing_req)),
        "V1 resource bypass retains legacy arithmetic"
    );
    s.process = lightcraft_develop::ProcessVersion::V2;
    assert_eq!(applied, pixels(lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cache)));
    s.profile.amount = 120.0;
    let amount120 = lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cache);
    assert!(amount120.unsupported_edits.is_empty());
    let tenth_src = lightcraft_raster::Rgb32f::filled(16, 8, [0.005, 0.012, 0.028]);
    let wanted120 = pixels(lightcraft_pipeline::render(&tenth_src, &info, &bypass, &req));
    assert!(pixels(amount120).iter().zip(&wanted120).all(|(a, b)| a.abs_diff(*b) <= 2), "Amount120 scales value as 1+1.2*(0.25-1)=0.1");
    s.profile.amount = 200.0;
    let rejected = lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cache);
    assert!(rejected.unsupported_edits[0].contains("invalid HSV scales"));
    assert_eq!(
        pixels(rejected),
        pixels(lightcraft_pipeline::render(&src, &info, &s, &missing_req)),
        "negative extrapolation is rejected, not clamped"
    );
    assert_eq!(req.imported_table.as_ref().unwrap().hsv().data, vec![[0.0, 1.0, 0.25]; 2], "Amount never mutates the loaded resource");
    s.profile.amount = f64::NAN;
    assert!(lightcraft_pipeline::profile_table_diagnostics(&s, &req)[0].contains("Amount unsupported"));

    let secret = "private-payload-not-in-errors";
    let error = resolve_type0_xmp(child(secret).as_bytes(), expected()).unwrap_err();
    assert!(!format!("{error:?}: {error}").contains(secret));
}
