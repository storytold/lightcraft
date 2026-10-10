//! Preset import through the session (`preset.import`): the parsers live in `dac-engine-develop`.
use serde_json::json;

/// A zip archive (`deflate`: compress entries) — enough of a writer for the tests.
fn zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let (method, body) = if deflate { (8u16, miniz_oxide::deflate::compress_to_vec(data, 6)) } else { (0, data.to_vec()) };
        let off = out.len() as u32;
        let head = |sig: &[u8], out: &mut Vec<u8>, central: bool| {
            out.extend_from_slice(sig);
            if central {
                out.extend_from_slice(&20u16.to_le_bytes());
            }
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc (not checked)
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        };
        head(b"PK\x03\x04", &mut out, false);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        head(b"PK\x01\x02", &mut central, true);
        central.extend_from_slice(&[0; 10]); // comment length, disk, internal + external attributes
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

const TEMPLATE: &str = r#"s = {
	id = "0D2F9C1E-TEST",
	internalName = "Warm Fade",
	title = "$$$/Test/Warm=Warm Fade",
	type = "Develop",
	value = {
		settings = {
			Exposure2012 = 0.35,
			Contrast2012 = -20,
			ConvertToGrayscale = false,
			ToneCurvePV2012 = { 0, 20, 128, 128, 255, 240, },
			SplitToningShadowHue = 210,
			SplitToningShadowSaturation = 15,
			HueAdjustmentOrange = -8,
			CameraProfile = "Some Profile",
			RetouchInfo = {},
			ProcessVersion = "11.0",
			-- a comment
			EnableColorAdjustments = true,
		},
		uuid = "0D2F9C1E-TEST",
	},
	version = 0,
}
"#;

const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
 crs:Exposure2012="+0.50" crs:Vibrance="20" crs:WhiteBalance="As Shot" crs:Temperature="5600" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropRight="0.9" crs:CropTop="0" crs:CropBottom="1"/>
</rdf:RDF></x:xmpmeta>"#;

/// One effect's parameter as Luminar writes it.
fn param(name: &str, v: f64) -> String {
    format!("<key>{name}</key><dict><key>OptionalDataType</key><integer>0</integer><key>Value</key><real>{v}</real></dict>")
}

fn layer(id: &str, amount: f64, blend: &str, effects: &[(&str, String)], sub: &str) -> String {
    let fx: String =
        effects.iter().map(|(e, p)| format!("<dict><key>Identifier</key><string>{e}</string><key>Parameters</key><dict>{p}</dict></dict>")).collect();
    let sub =
        if sub.is_empty() { String::new() } else { format!("<key>Sublayers</key><dict><key>AdjustmentLayers</key><array>{sub}</array></dict>") };
    format!(
        "<dict><key>Amount</key><real>{amount}</real><key>BlendModeIdentifier</key><string>{blend}</string><key>Effects</key><array>{fx}</array>\
         <key>Enabled</key><true/><key>Identifier</key><string>{id}</string>{sub}</dict>"
    )
}

fn look(layers: &str, extra: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict><key>AdjustmentLayers</key><array>{layers}</array>{extra}<key>group_identifier</key><string>Custom</string></dict></plist>"
    )
}

/// A newer-style look (develop sliders nested in sublayers) written for this test.
fn modern() -> String {
    let curve = "<key>RGB</key><dict><key>OptionalData</key><array><real>0.5</real><real>0</real><real>0.1</real><real>0.5</real><real>0.5</real><real>1</real><real>0.9</real></array><key>OptionalDataType</key><integer>1</integer><key>Value</key><real>50</real></dict>";
    let sub = [
        layer(
            "DevelopAdjustmentSubLayer",
            1.0,
            "Normal",
            &[(
                "MIPLDevelopCommonEffectID",
                param("Exposure", 25.0) + &param("Contrast", 12.0) + &param("Highlights", -30.0) + &param("Temperature", 15.0),
            )],
            &layer("CurveLayer", 1.0, "Normal", &[("MIPLCurveEffect", curve.to_string())], ""),
        ),
        layer("Disabled", 1.0, "Normal", &[("MIPLDevelopCommonEffectID", param("Shadows", 50.0))], "").replace("<true/>", "<false/>"),
    ]
    .concat();
    let layers = [
        layer("DevelopAdjustmentLayer", 1.0, "Normal", &[], &sub),
        layer("AIStructureEffect", 1.0, "Normal", &[("MIPLAIStructureEffect", param("Amount", 20.0) + &param("Boost", 10.0))], ""),
        layer("Half", 0.5, "Normal", &[("MIPLVibranceEffect", param("Vibrance", 30.0)), ("MIPLSaturationEffect", param("Saturation", -10.0))], ""),
        layer("Hsl", 1.0, "Normal", &[("MIPLChannelsEffect", param("hOrange", -8.0) + &param("sBlue", 12.0) + &param("lGreen", 4.0))], ""),
        layer("Vignette", 1.0, "Normal", &[("MIPLVignetteEffect", param("Amount", -20.0) + &param("Vignette Size", 40.0))], ""),
        layer("Orton", 1.0, "Normal", &[("MIPLOrtonFilterEffect", param("Amount", 15.0))], ""),
        layer("Screened", 1.0, "Screen", &[("MIPLContrastEffect", param("Contrast", 40.0))], ""),
        layer("Untouched", 1.0, "Normal", &[("MIPLSkyEnhancerEffect", String::new())], ""),
    ]
    .concat();
    look(&layers, "<key>uuid</key><string>AB12-CD34</string><key>kMPPresetIdentifierKey</key><string>x 2.lmp</string>")
}

#[test]
fn import_command_reads_folders_bundles_and_applies() {
    let dir = std::env::temp_dir().join(format!("lc-preset-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Looks/Film")).unwrap();
    std::fs::write(dir.join("Looks/Film/Warm Fade.lrtemplate"), TEMPLATE).unwrap();
    std::fs::write(dir.join("Looks/bundle.zip"), zip(&[("Street/Bright.xmp", XMP.as_bytes())], true)).unwrap();
    std::fs::write(dir.join("Looks/notes.txt"), "not a preset").unwrap();
    let mut s = crate::Session::with_demo();
    let before = s.presets.len();
    let paths = json!([dir.join("Looks").to_string_lossy()]);
    // a dry run reports without adding
    let r = s.execute("preset.import", &json!({"paths": paths, "dryRun": true})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(s.presets.len(), before);
    let r = s.execute("preset.import", &json!({"paths": paths})).unwrap();
    let got: Vec<(String, String)> =
        r["imported"].as_array().unwrap().iter().map(|i| (i["name"].as_str().unwrap().into(), i["group"].as_str().unwrap().into())).collect();
    assert_eq!(got, [("Warm Fade".to_string(), "Film".to_string()), ("Bright".into(), "Street".into())]);
    assert_eq!(r["imported"][0]["unmapped"], json!(["CameraProfile"]));
    assert_eq!(s.presets.len(), before + 2);
    // importing again adds nothing
    let r = s.execute("preset.import", &json!({"paths": paths})).unwrap();
    assert_eq!((r["imported"].as_array().unwrap().len(), r["skipped"].as_u64()), (0, Some(2)));
    // and the imported look applies
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let pid = s.presets.iter().find(|p| p.name == "Warm Fade").unwrap().id.clone();
    s.execute("preset.apply", &json!({"id": pid})).unwrap();
    let d = s.develop_of(id).unwrap();
    assert_eq!((d.light.exposure, d.light.contrast), (0.35, -20.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_command_reads_looks_and_bundle_folders() {
    let dir = std::env::temp_dir().join(format!("lc-luminar-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Wild/Vintage.lmp/Contents/resources")).unwrap();
    std::fs::write(dir.join("Wild/Vintage.lmp/Contents/preset.lmp"), modern()).unwrap();
    std::fs::write(dir.join("Wild/Vintage.lmp/Contents/Info.plist"), "<plist><dict/></plist>").unwrap();
    std::fs::write(dir.join("Wild/Vintage.lmp/Contents/PkgInfo"), "LMP?????").unwrap();
    let mut s = crate::Session::with_demo();
    let r = s.execute("preset.import", &json!({"paths": [dir.join("Wild").to_string_lossy()]})).unwrap();
    let im = &r["imported"][0];
    assert_eq!((im["name"].as_str(), im["group"].as_str()), (Some("Vintage"), Some("Wild")), "{r}");
    assert!(im["unmapped"].as_array().unwrap().iter().any(|u| u == "OrtonFilter.Amount"));
    // a dropped bundle folder on its own
    let r = s.execute("preset.import", &json!({"paths": [dir.join("Wild/Vintage.lmp").to_string_lossy()], "dryRun": true})).unwrap();
    assert_eq!(r["imported"][0]["group"], json!("Imported Presets"), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}
