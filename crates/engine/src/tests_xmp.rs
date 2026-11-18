//! XMP sidecars end to end: save → delete library → re-import restores edits; auto-write; foreign
//! `crs:` sidecars; DNG-embedded XMP; read metadata from file (undoable).

use std::path::{Path, PathBuf};

use lcc_catalog::{ColorLabel, Flag, PhotoId};
use serde_json::json;

use crate::Session;
use crate::rename::{MoveFs, RealFs};

/// Whether `dir` is on a case-insensitive volume (the default on macOS and Windows): its path in upper case still resolves.
pub(crate) fn case_insensitive(dir: &Path) -> bool {
    Path::new(&dir.to_string_lossy().to_uppercase()).exists()
}

/// The same path, or the same file under another spelling (`a.XMP` and `a.xmp` on a case-insensitive volume). Fault
/// injectors match this way, so an injected failure fires whichever spelling the code under test uses for a file; on a
/// case-sensitive volume only identical paths match.
pub(crate) fn same_path_or_file(a: &Path, b: &Path) -> bool {
    a == b || RealFs.same_file(a, b)
}

/// The same path, or on a case-insensitive volume a path that differs only in ASCII case: the code under test may spell
/// a sidecar `.XMP` for a file stored as `.xmp` there, while on a case-sensitive volume it offers only the spelling that
/// exists.
pub(crate) fn same_path_for_volume(a: &Path, b: &Path) -> bool {
    a == b || (a.parent().is_some_and(case_insensitive) && a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()))
}

/// A test's folder in Temp, removed when the test ends (other test modules use it too).
pub(crate) fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lc-xmp-{tag}"))
}

/// The temp volume ignores letter case (Windows, default macOS): `a.xmp` and `a.XMP` are one file
/// there. Fixtures that need both spellings as separate files skip; `rename::tests::FakeFs` models
/// such volumes instead.
pub(crate) fn case_insensitive_volume() -> bool {
    // probed once: parallel tests must not share (and delete) one probe folder
    static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| {
        let d = temp_dir("case-probe");
        std::fs::write(d.join("Probe.xmp"), b"").unwrap();
        let insensitive = d.join("probe.XMP").exists();
        let _ = std::fs::remove_dir_all(&d);
        insensitive
    })
}

/// Skip a real-disk fixture that writes case-variant sidecars as distinct files.
macro_rules! skip_on_case_insensitive_volume {
    () => {
        if crate::tests_xmp::case_insensitive_volume() {
            eprintln!("skipped: the temp volume ignores letter case (FakeFs models it)");
            return;
        }
    };
}
pub(crate) use skip_on_case_insensitive_volume;

pub(crate) fn write_png(path: &Path, seed: u8) {
    let (w, h) = (40usize, 24usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 9) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

pub(crate) fn synthetic_psd_with_metadata(metadata: &lcc_meta::Metadata) -> Vec<u8> {
    let exif = lcc_meta::write_exif(metadata);
    let xmp = lcc_meta::write_xmp(metadata, None);
    let pixels = [40, 80, 120];
    let image = lcc_codecs::EncodeImage::new(1, 1, 3, lcc_codecs::Samples::U8(&pixels));
    let embedded = lcc_codecs::EncodeMeta { exif: Some(&exif), xmp: Some(&xmp), ..Default::default() };
    lcc_codecs::encode_psd(&image, &embedded).unwrap()
}

pub(crate) fn synthetic_psb_with_metadata(metadata: &lcc_meta::Metadata) -> Vec<u8> {
    let exif = lcc_meta::write_exif(metadata);
    let xmp = lcc_meta::write_xmp(metadata, None);
    let pixels = [1024u16, 2048, 3072];
    let image = lcc_codecs::EncodeImage::new(1, 1, 3, lcc_codecs::Samples::U16(&pixels));
    let embedded = lcc_codecs::EncodeMeta { exif: Some(&exif), xmp: Some(&xmp), ..Default::default() };
    let psd = lcc_codecs::encode_psd(&image, &embedded).unwrap();
    let colour_length = usize::try_from(u32::from_be_bytes(psd[26..30].try_into().unwrap())).unwrap();
    let resource_length_offset = 30 + colour_length;
    let resource_length = usize::try_from(u32::from_be_bytes(psd[resource_length_offset..resource_length_offset + 4].try_into().unwrap())).unwrap();
    let resource_end = resource_length_offset + 4 + resource_length;

    let mut psb = psd[..resource_end].to_vec();
    psb[4..6].copy_from_slice(&2u16.to_be_bytes());
    psb.extend_from_slice(&0u64.to_be_bytes()); // PSB layer and mask information length
    psb.extend_from_slice(&psd[resource_end + 4..]); // compression and uncompressed 16-bit pixel planes
    psb
}

fn only(s: &Session) -> PhotoId {
    s.catalog.photos().next().unwrap().id
}

#[test]
fn save_delete_library_reimport_restores_edits() {
    let src = temp_dir("roundtrip-src");
    let lib = temp_dir("roundtrip-lib");
    write_png(&src.join("dune.png"), 1);
    write_png(&src.join("plain.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("dune.png").to_string_lossy()]})).unwrap();
    let id = only(&s);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    s.execute("photo.pick", &json!({})).unwrap();
    s.execute("photo.label", &json!({"label": "blue"})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Dune", "caption": "Evening light", "copyright": "© me", "keywords": ["sand", "dusk"]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.8})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 25})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let mask = s.develop_of(id).unwrap().masks[0].id;
    s.execute("develop.curve", &json!({"channel": "blue", "points": [[0.0, 0.1], [0.5, 0.6], [1.0, 1.0]], "mask": mask})).unwrap();
    let edited = s.catalog.photo(id).unwrap().clone();
    let r = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    assert_eq!(r["written"][0], src.join("dune.xmp").to_string_lossy().as_ref(), "{r}");
    assert!(src.join("dune.xmp").is_file());
    assert!(!src.join("plain.xmp").exists());
    drop(s);

    // delete the library; a fresh library re-imports the folder
    std::fs::remove_dir_all(&lib).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["sidecars"], 1, "{r}");
    let p = s.catalog.photos().find(|p| p.file_name == "dune.png").unwrap().clone();
    assert_eq!(p.develop, edited.develop, "develop settings restored exactly");
    assert_eq!(p.develop.masks.len(), 1);
    assert_eq!(p.develop.masks[0].curve.blue.len(), 3, "the mask's local Curve");
    assert_eq!((p.rating, p.flag, p.label), (4, Flag::Pick, Some(ColorLabel::Blue)));
    assert_eq!(p.meta.title, "Dune");
    assert_eq!(p.meta.caption, "Evening light");
    assert_eq!(p.meta.copyright, "© me");
    assert_eq!(p.meta.keywords, vec!["sand".to_string(), "dusk".to_string()]);
    assert!(p.edited.is_some());
    let plain = s.catalog.photos().find(|p| p.file_name == "plain.png").unwrap();
    assert!(plain.develop.is_unedited() && plain.rating == 0);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn auto_write_and_naming_preference() {
    let src = temp_dir("auto");
    let lib = temp_dir("auto-lib");
    write_png(&src.join("a.png"), 3);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    assert!(!src.join("a.xmp").exists(), "auto-write is off by default");

    let v = s.execute("library.xmpPreferences", &json!({"autoWrite": true, "naming": "full", "autoRead": true})).unwrap();
    assert_eq!(v, json!({"autoWrite": true, "naming": "full", "autoRead": true, "writeBrowsed": false}));
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    let sidecar = src.join("a.png.xmp");
    assert!(sidecar.is_file());
    assert!(std::fs::read_to_string(&sidecar).unwrap().contains("<xmp:Rating>5</xmp:Rating>"));
    // a slider drag writes once, at the end
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.5})).unwrap();
    let exposure = |p: &Path| match crate::sidecar::parse_sidecar(&std::fs::read_to_string(p).unwrap(), crate::crs::Target::Rendered).unwrap().develop
    {
        Some(crate::sidecar::DevelopPatch::Full(d)) => d.light.exposure,
        other => panic!("{other:?}"),
    };
    assert_eq!(exposure(&sidecar), 0.0, "not written mid-drag");
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert_eq!(exposure(&sidecar), 1.5);
    // undo rewrites too
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(exposure(&sidecar), 0.0);

    // the preference persists with the library
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.xmp.auto_write);
    assert_eq!(s.xmp.naming, crate::sidecar::SidecarNaming::Full);
    let v = s.execute("library.toggleAutoWriteXmp", &json!({})).unwrap();
    assert_eq!(v["autoWrite"], false);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Hand-written sidecar in the style other raw developers use (attribute form, `crs:` fields).
const FOREIGN: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="3" xmp:Label="Red"
    crs:ProcessVersion="11.0" crs:IncrementalTemperature="+20" crs:IncrementalTint="-5"
    crs:Exposure2012="+1.10" crs:Shadows2012="+35" crs:Vibrance="+15" crs:Clarity2012="+10"
    crs:SplitToningShadowHue="220" crs:SplitToningShadowSaturation="18"
    crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0" crs:CropRight="0.9" crs:CropBottom="1" crs:CropAngle="0">
   <dc:subject><rdf:Bag><rdf:li>harbour</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

#[test]
fn foreign_crs_sidecar_on_import_and_read_from_file() {
    let src = temp_dir("foreign");
    write_png(&src.join("harbour.png"), 4);
    std::fs::write(src.join("harbour.xmp"), FOREIGN).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["sidecars"], 1);
    let p = s.catalog.photos().next().unwrap().clone();
    assert_eq!((p.rating, p.label), (3, Some(ColorLabel::Red)));
    assert_eq!(p.meta.keywords, vec!["harbour".to_string()]);
    let d = &p.develop;
    assert_eq!((d.light.exposure, d.light.shadows, d.color.vibrance, d.effects.clarity), (1.1, 35.0, 15.0, 10.0));
    assert!(d.wb.temp > 6500.0 && d.wb.tint == -5.0, "relative WB for a rendered file: {:?}", d.wb);
    assert_eq!((d.grading.shadows.hue, d.grading.shadows.sat), (220.0, 18.0));
    assert_eq!((d.crop.geometry.rect.x0, d.crop.geometry.rect.x1), (0.1, 0.9));
    assert!(s.render_now(p.id, 32, 32).is_ok());

    // change things, then read the sidecar again: one undo step restores the edits
    s.execute("library.select", &json!({"ids": [p.id.0]})).unwrap();
    s.execute("develop.reset", &json!({})).unwrap();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    let r = s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    assert_eq!(r["read"].as_array().unwrap().len(), 1, "{r}");
    let q = s.catalog.photo(p.id).unwrap();
    assert_eq!(q.rating, 3);
    assert_eq!(q.develop.light.exposure, 1.1);
    s.execute("edit.undo", &json!({})).unwrap();
    let q = s.catalog.photo(p.id).unwrap();
    assert_eq!((q.rating, q.develop.light.exposure), (1, 0.0));
    // no sidecar → reported, not an error
    std::fs::remove_file(src.join("harbour.xmp")).unwrap();
    let r = s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    assert_eq!(r["failed"].as_array().unwrap().len(), 1);
    assert!(r.get("warnings").is_none(), "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

/// A sidecar with only `crs:` develop settings, as LRTimelapse writes them (issue #934).
fn crs_exposure_sidecar(exposure: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Version="15.0" crs:Exposure2012="{exposure}"/></rdf:RDF></x:xmpmeta>"#
    )
}

/// Issue #934: with auto-write on, Read Metadata from File wrote what it had just read straight back
/// into the sidecar, adding our `lc:settings`. Reading leaves the file byte for byte; an edit made
/// after it is still written.
#[test]
fn reading_metadata_leaves_the_sidecar_alone_with_auto_write_on() {
    let src = temp_dir("read-no-autowrite");
    write_png(&src.join("frame.png"), 6);
    let sidecar = src.join("frame.xmp");
    std::fs::write(&sidecar, crs_exposure_sidecar("0.5")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.5);
    // something for the read to change back (auto-write is still off, so the file stays as it is)
    s.execute("develop.reset", &json!({"ids": [id.0]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let before = std::fs::read(&sidecar).unwrap();
    let r = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["read"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.5, "the sidecar was read");
    let now = std::fs::read(&sidecar).unwrap();
    assert!(now == before, "reading leaves the sidecar byte for byte, but it is now:\n{}", String::from_utf8_lossy(&now));
    // a develop edit afterwards is still written (only the read is exempt)
    s.execute("develop.set", &json!({"ids": [id.0], "control": "light.exposure", "value": 0.9})).unwrap();
    let after = std::fs::read_to_string(&sidecar).unwrap();
    match crate::sidecar::parse_sidecar(&after, crate::crs::Target::Rendered).unwrap().develop {
        Some(crate::sidecar::DevelopPatch::Full(d)) => assert_eq!(d.light.exposure, 0.9),
        other => panic!("the edit wasn't auto-written: {other:?}\n{after}"),
    }
    assert!(after.contains(r#"crs:Exposure2012="0.5""#), "the other app's settings are kept: {after}");
    let _ = std::fs::remove_dir_all(&src);
}

/// Issue #934, the round trip the write-back broke: after a read with auto-write on, another app
/// changes `crs:Exposure2012` from 0.5 to 1.2 (keeping the rest of the file), and the next read
/// takes 1.2.
#[test]
fn a_crs_change_after_a_read_is_taken_with_auto_write_on() {
    let src = temp_dir("read-then-crs-change");
    write_png(&src.join("frame.png"), 7);
    let sidecar = src.join("frame.xmp");
    std::fs::write(&sidecar, crs_exposure_sidecar("0.5")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.5);
    // the other app edits the file as it is now
    let text = std::fs::read_to_string(&sidecar).unwrap();
    let changed = text.replace(r#"crs:Exposure2012="0.5""#, r#"crs:Exposure2012="1.2""#);
    assert_ne!(changed, text);
    std::fs::write(&sidecar, changed).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 1.2, "the second read takes the other app's exposure");
    let _ = std::fs::remove_dir_all(&src);
}

/// A sidecar with keywords as Lightroom Classic and darktable write them: names in `dc:subject`,
/// paths in `lr:hierarchicalSubject`.
const KEYWORDED: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="0" crs:Exposure2012="+0.40">
   <dc:subject><rdf:Bag><rdf:li>Places</rdf:li><rdf:li>Italy</rdf:li><rdf:li>Rome</rdf:li><rdf:li>sunset</rdf:li></rdf:Bag></dc:subject>
   <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Italy|Rome</rdf:li><rdf:li>sunset</rdf:li></rdf:Bag></lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

/// Saving a sidecar keeps keywords where other applications look for them: names in `dc:subject`
/// and paths in `lr:hierarchicalSubject`, each property once. older versions wrote the paths into
/// `dc:subject` (`Places|Italy|Rome`), so after any change, even a rating, a search for "Rome" in
/// another DAM no longer found the photo. A sidecar written that way is put right on the next save,
/// and keywords added here keep their hierarchy when the sidecar is read back.
#[test]
fn saving_a_sidecar_writes_keyword_names_flat_and_paths_hierarchical() {
    let src = temp_dir("keywords-flat");
    write_png(&src.join("rome.png"), 8);
    let sidecar = src.join("rome.xmp");
    std::fs::write(&sidecar, KEYWORDED).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert_eq!(s.catalog.photo(id).unwrap().meta.keywords, ["Places|Italy|Rome", "sunset"]);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();

    let lists = |text: &str| {
        let x = lcc_meta::parse_xmp(text).unwrap();
        (x.metadata.keywords, x.metadata.hierarchical_keywords)
    };
    let text = std::fs::read_to_string(&sidecar).unwrap();
    let (flat, hierarchical) = lists(&text);
    assert_eq!(flat, ["Places", "Italy", "Rome", "sunset"], "names, not paths, in dc:subject:\n{text}");
    assert_eq!(hierarchical, ["Places|Italy|Rome", "sunset"], "{text}");
    assert_eq!(text.matches("<dc:subject>").count(), 1, "{text}");
    assert_eq!(text.matches("lr:hierarchicalSubject>").count(), 2, "one lr:hierarchicalSubject, opened and closed:\n{text}");
    assert!(text.contains("<xmp:Rating>3</xmp:Rating>") && text.contains(r#"crs:Exposure2012="+0.40""#), "{text}");

    // a keyword added here keeps its hierarchy through the file
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": ["Places|Italy|Rome", "sunset", "people|Ada"]})).unwrap();
    let (flat, hierarchical) = lists(&std::fs::read_to_string(&sidecar).unwrap());
    assert_eq!(flat, ["Places", "Italy", "Rome", "sunset", "people", "Ada"]);
    assert_eq!(hierarchical, ["Places|Italy|Rome", "sunset", "people|Ada"]);
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": []})).unwrap();
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(!text.contains("dc:subject") && !text.contains("lr:hierarchicalSubject"), "clearing keywords clears both lists:\n{text}");
    s.execute("edit.undo", &json!({})).unwrap(); // undo rewrites the sidecar with the three keywords
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.keywords, ["Places|Italy|Rome", "sunset", "people|Ada"], "read back as written");

    // a sidecar from an older version (paths in dc:subject) reads as paths and is written properly
    let old = KEYWORDED
        .replace("<rdf:li>Places</rdf:li><rdf:li>Italy</rdf:li><rdf:li>Rome</rdf:li>", "<rdf:li>Places|Italy|Rome</rdf:li>")
        .replace("<lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Italy|Rome</rdf:li><rdf:li>sunset</rdf:li></rdf:Bag></lr:hierarchicalSubject>", "");
    std::fs::write(&sidecar, old).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.keywords, ["Places|Italy|Rome", "sunset"]);
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
    let (flat, hierarchical) = lists(&std::fs::read_to_string(&sidecar).unwrap());
    assert_eq!(flat, ["Places", "Italy", "Rome", "sunset"]);
    assert_eq!(hierarchical, ["Places|Italy|Rome", "sunset"]);
    let _ = std::fs::remove_dir_all(&src);
}

/// Lightroom's default profiles (`Adobe Standard`, `Adobe Color`) are named by nearly every sidecar
/// and our own starting look stands in for them: importing one gives no camera-profile warning.
#[test]
fn default_lightroom_profiles_are_not_reported() {
    let src = temp_dir("xmp-default-profile");
    for (i, name) in ["Adobe Standard", "Adobe Color"].into_iter().enumerate() {
        let photo = src.join(format!("p{i}.png"));
        write_png(&photo, 6);
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
        crs:CameraProfile="{name}" crs:Exposure2012="0.5"/>
    </rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(src.join(format!("p{i}.xmp")), xmp).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
        let none = Vec::new();
        let warnings = r["warnings"].as_array().unwrap_or(&none);
        assert!(!warnings.iter().any(|w| w["code"] == "cameraProfile"), "{name}: {r}");
        assert_eq!(s.catalog.photos().next().unwrap().develop.light.exposure, 0.5, "{name}: the edits still import");
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// Foreign profile names and saved Upright matrices stay visible without pretending their
/// proprietary colour or coordinate data was imported. Diagnostics are per-photo report data;
/// importing a file without XMP remains quiet, and the source packet is never rewritten.
#[test]
fn foreign_profile_and_upright_import_diagnostics_are_actionable() {
    let src = temp_dir("xmp-diagnostics");
    let photo = src.join("faithful.png");
    write_png(&photo, 6);
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
        crs:CameraProfile="Camera Faithful" crs:Exposure2012="1.68" crs:PerspectiveUpright="1"
        crs:UprightVersion="151388160" crs:UprightTransformCount="6"
        crs:UprightCenterNormX="0.554156" crs:UprightCenterNormY="0.529745"
        crs:UprightTransform_1="0.897233402,-0.013204641,-0.030578613,-0.096087436,0.953621255,0.003270197,-0.157075841,-0.043520282,1.000000000"/>
    </rdf:RDF></x:xmpmeta>"#;
    let sidecar = src.join("faithful.xmp");
    std::fs::write(&sidecar, xmp).unwrap();
    let before = std::fs::read(&sidecar).unwrap();
    let photo_path = photo.to_string_lossy().to_string();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    let warnings = r["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w["code"] == "cameraProfile" && w["profile"] == "Camera Faithful"), "{r}");
    // Auto with Lightroom's stored matrix is applied as Lightroom solved it: nothing to warn about
    assert!(!warnings.iter().any(|w| w["code"] == "uprightGeometry"), "{r}");
    assert!(warnings.iter().all(|w| w.get("fields").is_none()), "{r}");
    let import_warnings: Vec<crate::import::ImportWarning> =
        warnings.iter().cloned().map(serde_json::from_value).collect::<std::result::Result<_, _>>().unwrap();
    let import_profile = import_warnings.iter().find(|w| w.code == crate::import::ImportWarningCode::CameraProfile).unwrap();
    assert_eq!(import_profile.path, photo_path);
    assert_eq!(import_profile.profile.as_deref(), Some("Camera Faithful"));
    assert!(import_profile.fields.is_empty());
    let p = s.catalog.photos().next().unwrap().clone();
    assert!(p.develop.geometry.lrc_upright.is_some(), "Lightroom's Upright matrix is kept");
    assert_eq!(import_profile.photo, Some(p.id.0), "a warning names the photo it was imported as");
    assert_eq!(p.develop.profile.id, "lc.color");
    assert_eq!(p.develop.light.exposure, 1.68);
    assert_eq!(std::fs::read(&sidecar).unwrap(), before, "import never rewrites foreign XMP");

    // Metadata reread carries same structured diagnostics, with each saved Upright mode typed.
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [p.id.0]})).unwrap();
    let reread_wires = reread["warnings"].as_array().unwrap();
    assert!(reread_wires.iter().all(|w| w.get("fields").is_none()), "{reread}");
    let reread_warnings: Vec<crate::import::ImportWarning> =
        reread_wires.iter().cloned().map(serde_json::from_value).collect::<std::result::Result<_, _>>().unwrap();
    let reread_profile = reread_warnings.iter().find(|w| w.code == crate::import::ImportWarningCode::CameraProfile).unwrap();
    assert_eq!(reread_profile.path, photo_path);
    assert_eq!(reread_profile.profile.as_deref(), Some("Camera Faithful"));
    assert!(reread_profile.fields.is_empty());
    assert!(!reread_warnings.iter().any(|w| w.code == crate::import::ImportWarningCode::UprightGeometry), "{reread}");
    assert!(
        reread["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "cameraProfile" && w["profile"] == "Camera Faithful" && w["path"] == photo_path),
        "{reread}"
    );
    // Auto whose stored matrix can't be used (singular): we detect our own, so the warning names the mode
    let no_matrix = xmp.replace(
        "0.897233402,-0.013204641,-0.030578613,-0.096087436,0.953621255,0.003270197,-0.157075841,-0.043520282,1.000000000",
        "1,2,3,2,4,6,0,0,1",
    );
    std::fs::write(&sidecar, &no_matrix).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [p.id.0]})).unwrap();
    let reread_warnings: Vec<crate::import::ImportWarning> =
        reread["warnings"].as_array().unwrap().iter().cloned().map(serde_json::from_value).collect::<std::result::Result<_, _>>().unwrap();
    let reread_upright = reread_warnings.iter().find(|w| w.code == crate::import::ImportWarningCode::UprightGeometry).unwrap();
    assert_eq!(reread_upright.path, photo_path);
    assert_eq!(reread_upright.mode, Some(crate::import::ImportUprightMode::Auto));
    assert!(reread_upright.fields.is_empty());
    // the other modes have no stored matrix in this sidecar (only `_1`), so they warn too
    for (source, mode) in [("2", "level"), ("3", "vertical"), ("4", "full"), ("5", "guided"), ("9", "unknown")] {
        let mode_xmp = xmp.replace("crs:PerspectiveUpright=\"1\"", &format!("crs:PerspectiveUpright=\"{source}\""));
        std::fs::write(&sidecar, mode_xmp).unwrap();
        let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [p.id.0]})).unwrap();
        assert!(reread["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "uprightGeometry" && w["mode"] == mode), "{source}: {reread}");
    }
    // Upright Off: Lightroom keeps its analysis but doesn't apply it, so nothing is lost
    std::fs::write(&sidecar, xmp.replace("crs:PerspectiveUpright=\"1\"", "crs:PerspectiveUpright=\"0\"")).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [p.id.0]})).unwrap();
    let none = vec![];
    assert!(!reread["warnings"].as_array().unwrap_or(&none).iter().any(|w| w["code"] == "uprightGeometry"), "{reread}");
    std::fs::write(&sidecar, xmp).unwrap();

    // A foreign name colliding with a built-in profile label remains warning metadata only.
    let collision = src.join("collision.png");
    write_png(&collision, 8);
    std::fs::write(src.join("collision.xmp"), xmp.replace("Camera Faithful", "Color")).unwrap();
    let collision_report = s.execute("library.import", &json!({"paths": [collision.to_string_lossy()]})).unwrap();
    assert!(
        collision_report["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "cameraProfile" && w["profile"] == "Color"),
        "{collision_report}"
    );
    let collision_photo = s.catalog.photos().find(|photo| photo.file_name == "collision.png").unwrap();
    assert_eq!(collision_photo.develop.profile.id, "lc.color");

    let unmapped = crate::import::ImportWarning {
        path: photo_path.clone(),
        code: crate::import::ImportWarningCode::UnmappedXmp,
        profile: None,
        mode: None,
        fields: vec!["Tint".into()],
        photo: Some(7),
    };
    let unmapped_wire = serde_json::to_value(&unmapped).unwrap();
    assert_eq!(serde_json::from_value::<crate::import::ImportWarning>(unmapped_wire).unwrap(), unmapped);

    // A second import still diagnoses the untouched foreign packet, while an XMP-free photo does not.
    drop(s);
    let clean = src.join("clean.png");
    write_png(&clean, 7);
    let mut s = Session::new().with_fs();
    let clean_report = s.execute("library.import", &json!({"paths": [clean.to_string_lossy()]})).unwrap();
    assert!(clean_report.get("warnings").is_none(), "{clean_report}");
    let repeat = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    // the profile only: the Upright matrix is applied as Lightroom saved it
    assert_eq!(repeat["warnings"].as_array().unwrap().len(), 1, "{repeat}");
    let _ = std::fs::remove_dir_all(&src);
}

const MWG_REGIONS: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
    xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#"
    xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#">
   <mwg-rs:Regions>
    <rdf:Description>
     <mwg-rs:AppliedToDimensions stDim:w="100" stDim:h="100" stDim:unit="pixel"/>
     <mwg-rs:RegionList>
      <rdf:Bag>
       <rdf:li>
        <rdf:Description mwg-rs:Name="Jane Doe" mwg-rs:Type="Face">
         <mwg-rs:Area stArea:x="0.5" stArea:y="0.5" stArea:w="0.3" stArea:h="0.4" stArea:unit="normalized"/>
        </rdf:Description>
       </rdf:li>
      </rdf:Bag>
     </mwg-rs:RegionList>
    </rdf:Description>
   </mwg-rs:Regions>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

/// A face region written by Lightroom (MWG-RS) into a sidecar reaches `Photo.meta.regions` on
/// import — the same path real catalogs use, exercised end to end rather than just at the parser.
#[test]
fn regions_from_sidecar_are_read_on_import() {
    let src = temp_dir("regions");
    write_png(&src.join("portrait.png"), 9);
    std::fs::write(src.join("portrait.xmp"), MWG_REGIONS).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["sidecars"], 1);
    let p = s.catalog.photos().next().unwrap().clone();
    assert_eq!(p.meta.regions.len(), 1);
    let region = &p.meta.regions[0];
    assert_eq!(region.name.as_deref(), Some("Jane Doe"));
    assert_eq!(region.kind, lcc_meta::RegionKind::Face);
    assert!((region.rect.width() - 0.3).abs() < 1e-9, "{:?}", region.rect);
    let _ = std::fs::remove_dir_all(&src);
}

/// MWG regions as an app that draws on the photo as stored writes them for [`write_turned_jpeg`]'s photo: its stored
/// 32 × 24 as `AppliedToDimensions` and no box saying how the photo was turned (no `mwg-rs:Rotation`). One face, its
/// box centred at (0.25, 0.4), 0.1 × 0.15 of that size.
const STORED_FRAME_REGIONS: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
    xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#"
    xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#">
   <mwg-rs:Regions rdf:parseType="Resource">
    <mwg-rs:AppliedToDimensions rdf:parseType="Resource" stDim:w="32" stDim:h="24" stDim:unit="pixel"/>
    <mwg-rs:RegionList>
     <rdf:Bag>
      <rdf:li rdf:parseType="Resource" mwg-rs:Name="Hannah" mwg-rs:Type="Face">
       <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.25" stArea:y="0.4" stArea:w="0.1" stArea:h="0.15" stArea:unit="normalized"/>
      </rdf:li>
     </rdf:Bag>
    </mwg-rs:RegionList>
   </mwg-rs:Regions>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

/// A JPEG of 32 × 24 pixels stored sideways (Exif orientation 6, shown 24 × 32), with `packet` embedded when given.
fn write_turned_jpeg(path: &Path, packet: Option<&str>) {
    let img = lcc_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 7) as u8, (y * 9) as u8, 90, 255]);
    let exif = lcc_meta::write_exif(&lcc_meta::Metadata { orientation: Some(lcc_meta::Orientation::Rotate90), ..Default::default() });
    let meta = lcc_codecs::EncodeMeta { exif: Some(&exif), xmp: packet, ..Default::default() };
    let bytes = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Put more than the first MiB of APP15 segments between the JPEG at `path`'s start and its Exif.
fn pad_before_exif(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let mut padded = bytes[..2].to_vec();
    for _ in 0..17 {
        padded.extend_from_slice(&[0xFF, 0xEF, 0xFF, 0xFF]);
        padded.extend(std::iter::repeat_n(0, 0xFFFD));
    }
    padded.extend_from_slice(&bytes[2..]);
    std::fs::write(path, padded).unwrap();
}

/// The face's box from [`STORED_FRAME_REGIONS`] on the upright photo: turned a quarter turn clockwise with it.
fn assert_on_the_upright_face(s: &Session, id: PhotoId, when: &str) {
    let p = s.catalog.photo(id).unwrap();
    assert_eq!((p.width, p.height), (24, 32), "the photo is upright");
    let r = p.meta.regions.first().map(|r| r.rect).unwrap_or_default();
    let want = [0.525, 0.2, 0.675, 0.3];
    let got = [r.x0, r.y0, r.x1, r.y1];
    assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-9), "{when}: the box is where the photo as stored had the face: {got:?}");
    assert_eq!(p.meta.regions.len(), 1, "{when}");
}

/// Issue #2674: face boxes an app drew on a photo as stored (its sideways size as `AppliedToDimensions`, no
/// `mwg-rs:Rotation`) land on the faces of the photo as the app shows it, upright: when imported, and when the
/// file's metadata is read again (the packet itself doesn't say how the photo turns; the file's Exif does). The
/// image's own size decides, not one the XMP states upright, and the Exif is found past the first MiB of the file.
#[test]
fn regions_drawn_on_the_photo_as_stored_land_on_the_upright_photo() {
    let src = temp_dir("regions-stored-frame");
    let embedded = src.join("embedded.jpg");
    write_turned_jpeg(&embedded, Some(STORED_FRAME_REGIONS));
    let beside = src.join("sidecar.jpg");
    write_turned_jpeg(&beside, None);
    std::fs::write(src.join("sidecar.xmp"), STORED_FRAME_REGIONS).unwrap();
    // an Exif without a pixel size, and an XMP size of the upright photo
    let stated = src.join("stated.jpg");
    let upright_size = r#"rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" tiff:ImageWidth="24" tiff:ImageLength="32""#;
    write_turned_jpeg(&stated, Some(&STORED_FRAME_REGIONS.replace(r#"rdf:about="""#, upright_size)));
    let late = src.join("late.jpg");
    write_turned_jpeg(&late, None);
    pad_before_exif(&late);
    std::fs::write(src.join("late.xmp"), STORED_FRAME_REGIONS).unwrap();
    for (file, what) in [(&embedded, "embedded XMP"), (&beside, "sidecar"), (&stated, "upright size in the XMP"), (&late, "Exif after the first MiB")]
    {
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
        let id = only(&s);
        assert_on_the_upright_face(&s, id, &format!("{what}, imported"));
        let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(read["failed"], json!([]), "{read}");
        assert_eq!(read["read"].as_array().map(Vec::len), Some(1), "{what}: {read}");
        assert_on_the_upright_face(&s, id, &format!("{what}, read again"));
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// A hand-drawn face region is a normal face region: undoable, catalog-only (the sidecar is untouched), listed
/// with the rest, and refused when it is not a real box.
#[test]
fn adding_a_region_draws_a_face_and_refuses_bad_boxes() {
    let src = temp_dir("region-add");
    write_png(&src.join("portrait.png"), 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let n = s.catalog.photo(id).unwrap().meta.regions.len();
    let commits = s.commits();
    for bad in [
        json!({}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": 0.1, "y1": 0.5}}),
        json!({"rect": {"x0": -0.1, "y0": 0.1, "x1": 0.5, "y1": 0.5}}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": 1.5, "y1": 0.5}}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": "a", "y1": 0.5}}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": 0.5}}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": 0.5, "y1": 0.5}, "kind": "tree"}),
        json!({"rect": {"x0": 0.1, "y0": 0.1, "x1": 0.5, "y1": 0.5}, "name": 7}),
    ] {
        assert!(s.execute("photo.addRegion", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.commits(), commits, "nothing changed");
    // corners given in any order
    let r = s.execute("photo.addRegion", &json!({"rect": {"x0": 0.5, "y0": 0.6, "x1": 0.2, "y1": 0.1}})).unwrap();
    assert_eq!(r["index"], n);
    let region = s.catalog.photo(id).unwrap().meta.regions.get(n).unwrap().clone();
    assert_eq!(region.kind, lcc_meta::RegionKind::Face);
    assert!(region.name.is_none() && region.rect.x0 < region.rect.x1 && region.rect.y0 < region.rect.y1);
    s.execute("photo.addRegion", &json!({"rect": {"x0": 0.6, "y0": 0.1, "x1": 0.9, "y1": 0.4}, "name": " Ann "})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.get(n + 1).unwrap().name.as_deref(), Some("Ann"));
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), n);
    let _ = std::fs::remove_dir_all(&src);
}

/// Removing a face box is a catalog-only, undoable edit: it never rewrites the sidecar (Lightroom's
/// regions and everything else in it stay byte for byte), even with auto-write on.
#[test]
fn removing_a_region_is_undoable_and_leaves_the_sidecar_alone() {
    let src = temp_dir("region-remove");
    write_png(&src.join("portrait.png"), 9);
    std::fs::write(src.join("portrait.xmp"), MWG_REGIONS).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let before = std::fs::read(src.join("portrait.xmp")).unwrap();
    let n = s.catalog.photo(id).unwrap().meta.regions.len();
    assert!(n >= 1);
    assert!(s.execute("photo.removeRegion", &json!({"index": n})).is_err(), "out of range");
    assert!(s.execute("photo.removeRegion", &json!({})).is_err(), "no index");
    let r = s.execute("photo.removeRegion", &json!({"index": 0})).unwrap();
    assert_eq!(r["removed"], "Jane Doe");
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), n - 1);
    assert_eq!(std::fs::read(src.join("portrait.xmp")).unwrap(), before, "the sidecar is untouched");
    // an ordinary edit still auto-writes (the guard is per command)
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    assert_ne!(std::fs::read(src.join("portrait.xmp")).unwrap(), before, "auto-write still works for other edits");
    // undo brings the region back
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), n);
    let _ = std::fs::remove_dir_all(&src);
}

/// Resizing a face box is a catalog-only, undoable edit with strict input handling; it leaves the
/// sidecar alone like removal does.
#[test]
fn resizing_a_region_validates_clamps_undoes_and_leaves_the_sidecar_alone() {
    let src = temp_dir("region-resize");
    write_png(&src.join("portrait.png"), 9);
    std::fs::write(src.join("portrait.xmp"), MWG_REGIONS).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let before = std::fs::read(src.join("portrait.xmp")).unwrap();
    let original = s.catalog.photo(id).unwrap().meta.regions[0].rect;
    let set = |s: &mut Session, rect: serde_json::Value| s.execute("photo.setRegion", &json!({"index": 0, "rect": rect}));
    // hostile input is an error and changes nothing
    for bad in [
        json!({"x0": 0.1, "y0": 0.1, "x1": 0.1, "y1": 0.5}),
        json!({"x0": 0.1, "y0": 0.1, "x1": 0.1001, "y1": 0.5}),
        json!({"x0": 0.1, "y0": 0.1, "x1": 0.5}),
        json!({"x0": "a", "y0": 0.1, "x1": 0.5, "y1": 0.5}),
        json!({"x0": null, "y0": 0.1, "x1": 0.5, "y1": 0.5}),
    ] {
        assert!(set(&mut s, bad.clone()).is_err(), "{bad}");
    }
    assert!(s.execute("photo.setRegion", &json!({"index": 9, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.5, "y1": 0.5}})).is_err(), "no such region");
    assert!(s.execute("photo.setRegion", &json!({"index": 0})).is_err(), "no rect");
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions[0].rect, original);
    // corners given in any order and outside the photo are clamped into it
    let r = set(&mut s, json!({"x0": 0.6, "y0": 1.5, "x1": -0.2, "y1": 0.2})).unwrap();
    assert_eq!(r["rect"], json!({"x0": 0.0, "y0": 0.2, "x1": 0.6, "y1": 1.0}));
    let now = s.catalog.photo(id).unwrap().meta.regions[0].rect;
    assert_eq!((now.x0, now.y0, now.x1, now.y1), (0.0, 0.2, 0.6, 1.0));
    assert_eq!(std::fs::read(src.join("portrait.xmp")).unwrap(), before, "the sidecar is untouched");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions[0].rect, original);
    let _ = std::fs::remove_dir_all(&src);
}

/// Re-reading a sidecar: one that has `mwg-rs:Regions` is authoritative, so an emptied region list
/// clears the stale regions; one without `mwg-rs:Regions` (an app that doesn't do regions) leaves the
/// photo's regions alone.
#[test]
fn rereading_a_sidecar_clears_regions_only_when_it_states_them() {
    let src = temp_dir("region-reread");
    write_png(&src.join("portrait.png"), 9);
    std::fs::write(src.join("portrait.xmp"), MWG_REGIONS).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), 1);
    // another app rewrote the sidecar without any notion of regions: they stay
    std::fs::write(
        src.join("portrait.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="3"/>
</rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.rating, 3, "the sidecar was read");
    assert_eq!(p.meta.regions.len(), 1, "a sidecar without mwg-rs:Regions keeps the photo's regions");
    // a region-aware app removed every region: the sidecar says so, and the catalog follows
    let emptied = MWG_REGIONS.replace(
        r#"<rdf:li>
        <rdf:Description mwg-rs:Name="Jane Doe" mwg-rs:Type="Face">
         <mwg-rs:Area stArea:x="0.5" stArea:y="0.5" stArea:w="0.3" stArea:h="0.4" stArea:unit="normalized"/>
        </rdf:Description>
       </rdf:li>"#,
        "",
    );
    assert_ne!(emptied, MWG_REGIONS);
    std::fs::write(src.join("portrait.xmp"), emptied).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    assert!(s.catalog.photo(id).unwrap().meta.regions.is_empty(), "an emptied region list clears stale regions");
    // undo brings them back
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().meta.regions.len(), 1);
    let _ = std::fs::remove_dir_all(&src);
}

fn synthetic_dng(xmp: &str) -> Vec<u8> {
    synthetic_dng_with(Some(xmp), lcc_meta::Metadata::default())
}

/// A tiny Bayer DNG written by our own DNG writer (optional embedded XMP, camera metadata).
pub(crate) fn synthetic_dng_with(xmp: Option<&str>, metadata: lcc_meta::Metadata) -> Vec<u8> {
    let (w, h) = (32usize, 24usize);
    let data: Vec<u16> = (0..w * h).map(|i| 256 + ((i % w) * 300 + (i / w) * 200) as u16).collect();
    synthetic_dng_of(w, h, "RGGB", data, xmp, metadata)
}

/// A synthetic Bayer DNG for denoise and cache tests.
pub(crate) fn synthetic_dng_of(w: usize, h: usize, cfa: &str, data: Vec<u16>, xmp: Option<&str>, metadata: lcc_meta::Metadata) -> Vec<u8> {
    use lcc_raw::*;
    let cfa = Cfa::bayer(cfa).unwrap();
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(256.0),
        white: vec![16000.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData {
            illuminant: [17, 21],
            color_matrix: [
                Some(lcc_color::Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
                Some(lcc_color::Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
            ],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: Default::default(),
    };
    write_dng(&raw, &DngWriteOptions { xmp: xmp.map(str::to_string), ..Default::default() }).unwrap()
}

#[test]
fn dng_embedded_crs_settings_are_read_on_import() {
    let src = temp_dir("dng");
    let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
        xmp:Rating="2" crs:WhiteBalance="Custom" crs:Temperature="4300" crs:Tint="+7" crs:Contrast2012="+30" crs:Dehaze="+12"/>
      </rdf:RDF></x:xmpmeta>"#;
    std::fs::write(src.join("synth.dng"), synthetic_dng(x)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((r["imported"].as_array().unwrap().len(), r["sidecars"].as_u64()), (1, Some(1)), "{r}");
    let p = s.catalog.photos().next().unwrap();
    assert_eq!(p.kind, lcc_catalog::MediaKind::Raw);
    assert_eq!((p.develop.wb.temp, p.develop.wb.tint), (4300.0, 7.0));
    assert_eq!((p.develop.light.contrast, p.develop.effects.dehaze), (30.0, 12.0));
    assert_eq!(p.develop.detail.sharpen_amount, 40.0, "raw defaults kept for fields the XMP doesn't set");
    assert_eq!(p.rating, 2);

    // a sidecar next to the DNG takes precedence over the embedded XMP
    let id = p.id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.contrast", "value": -10})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let mut s2 = Session::new().with_fs();
    s2.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(s2.catalog.photos().next().unwrap().develop.light.contrast, -10.0);
    let _ = std::fs::remove_dir_all(&src);

    // A sidecar that can't be read keeps precedence too, as on Read Metadata from File
    // (`sidecar::read_packet`): the DNG imports as one without settings, not with its embedded ones (its rating
    // is file metadata the probe reads, not a setting, so it is not compared).
    let other = temp_dir("dng-unreadable-sidecar");
    std::fs::write(other.join("synth.dng"), synthetic_dng(x)).unwrap();
    std::fs::write(other.join("synth.xmp"), [0xff, 0xfe, 0xfd]).unwrap();
    std::fs::write(other.join("plain.dng"), synthetic_dng_with(None, lcc_meta::Metadata::default())).unwrap();
    let mut s3 = Session::new().with_fs();
    let r = s3.execute("library.import", &json!({"paths": [other.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 2, "{r}");
    let by_name = |name: &str| s3.catalog.photos().find(|p| p.file_name == name).unwrap().clone();
    let (unreadable, plain) = (by_name("synth.dng"), by_name("plain.dng"));
    assert_eq!(
        (unreadable.develop.wb.temp, unreadable.develop.light.contrast, unreadable.develop.effects.dehaze),
        (plain.develop.wb.temp, plain.develop.light.contrast, plain.develop.effects.dehaze),
        "the embedded settings don't apply behind a sidecar that can't be read"
    );
    let _ = std::fs::remove_dir_all(&other);
}

fn jpeg_xmp_packet(rating: i8, title: &str) -> String {
    lcc_meta::write_xmp(&lcc_meta::Metadata { rating: Some(rating), title: Some(title.into()), ..Default::default() }, None)
}

fn write_jpeg_xmp(path: &Path, packet: &str) -> Vec<u8> {
    let img = lcc_raster::Rgba8::from_fn(32, 24, |x, y| [(x * 7) as u8, (y * 9) as u8, 90, 255]);
    let meta = lcc_codecs::EncodeMeta { xmp: Some(packet), ..Default::default() };
    let bytes = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap();
    assert_eq!(lcc_meta::embedded(&bytes).xmp.as_deref(), Some(packet), "the procedural JPEG carries the requested packet");
    std::fs::write(path, &bytes).unwrap();
    bytes
}

#[test]
fn embedded_jpeg_xmp_reread_refreshes_metadata_and_undoes_once() {
    let src = temp_dir("jpeg-xmp-refresh");
    let file = src.join("photo.jpg");
    let initial = write_jpeg_xmp(&file, &jpeg_xmp_packet(1, "Initial embedded title"));
    let mut s = Session::new().with_fs();
    let imported = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    assert_eq!(imported["imported"].as_array().unwrap().len(), 1, "{imported}");
    let id = only(&s);
    assert_eq!(s.catalog.photo(id).unwrap().kind, lcc_catalog::MediaKind::Image);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Catalog title"})).unwrap();
    let before = s.catalog.photo(id).unwrap().clone();
    assert!(!s.xmp.auto_write);

    // Change the packet after import: rereading must use the file, not the imported metadata.
    let changed = write_jpeg_xmp(&file, &jpeg_xmp_packet(4, "Changed embedded title"));
    assert_ne!(initial, changed);
    assert!(!src.join("photo.xmp").exists() && !src.join("photo.jpg.xmp").exists());
    let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(read["read"], json!([{"id": id.0, "from": file.to_string_lossy()}]), "embedded JPEG XMP must be reread: {read}");
    assert_eq!(read["failed"], json!([]), "{read}");
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, photo.meta.title.as_str()), (4, "Changed embedded title"));
    assert_eq!(photo.develop, before.develop);

    s.execute("edit.undo", &json!({})).unwrap();
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, &photo.meta.title), (before.rating, &before.meta.title), "one undo restores both fields");
    assert_eq!(std::fs::read(&file).unwrap(), changed, "reading and undo never rewrite the original");
    assert!(!src.join("photo.xmp").exists() && !src.join("photo.jpg.xmp").exists());
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn embedded_jpeg_xmp_reread_keeps_sidecar_precedence() {
    let src = temp_dir("jpeg-xmp-sidecar");
    let file = src.join("photo.jpg");
    let original = write_jpeg_xmp(&file, &jpeg_xmp_packet(4, "Embedded title"));
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = only(&s);
    let sidecar = src.join("photo.xmp");
    let packet = jpeg_xmp_packet(5, "Sidecar title");
    std::fs::write(&sidecar, &packet).unwrap();
    let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(read["read"], json!([{"id": id.0, "from": sidecar.to_string_lossy()}]), "{read}");
    assert_eq!(read["failed"], json!([]), "{read}");
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, photo.meta.title.as_str()), (5, "Sidecar title"));
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), packet);

    // A malformed sidecar must report its failure, not fall back to the valid embedded packet.
    let malformed = "<a>".repeat(1000);
    assert!(lcc_meta::parse_xmp(&malformed).is_err());
    std::fs::write(&sidecar, &malformed).unwrap();
    let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(read["read"], json!([]), "{read}");
    assert_eq!(read["failed"].as_array().unwrap().len(), 1, "{read}");
    assert_eq!(read["failed"][0]["id"], id.0);
    assert!(!read["failed"][0]["error"].as_str().unwrap().is_empty());
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, photo.meta.title.as_str()), (5, "Sidecar title"));
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), malformed);
    assert_eq!(std::fs::read(&file).unwrap(), original);
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn embedded_jpeg_xmp_reread_malformed_input_preserves_state_and_undo() {
    let src = temp_dir("jpeg-xmp-malformed");
    let file = src.join("photo.jpg");
    write_jpeg_xmp(&file, &jpeg_xmp_packet(1, "Original title"));
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = only(&s);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let previous_title = s.catalog.photo(id).unwrap().meta.title.clone();
    s.execute("photo.setMeta", &json!({"title": "Catalog title"})).unwrap();
    let before = s.catalog.photo(id).unwrap().clone();
    let malformed = "<a>".repeat(1000);
    assert!(lcc_meta::parse_xmp(&malformed).is_err());
    let broken_packet = write_jpeg_xmp(&file, &malformed);
    let truncated_container = vec![0xff, 0xd8, 0xff, 0xe1, 0, 40, b'x'];
    for bytes in [broken_packet, truncated_container] {
        std::fs::write(&file, &bytes).unwrap();
        let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
        assert_eq!(read["read"], json!([]), "{read}");
        assert_eq!(read["failed"].as_array().unwrap().len(), 1, "{read}");
        assert_eq!(read["failed"][0]["id"], id.0);
        assert!(!read["failed"][0]["error"].as_str().unwrap().is_empty());
        let photo = s.catalog.photo(id).unwrap();
        assert_eq!((photo.rating, &photo.meta.title), (before.rating, &before.meta.title));
        assert_eq!(photo.develop, before.develop);
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert!(!src.join("photo.xmp").exists() && !src.join("photo.jpg.xmp").exists());
    }
    // Failed reads do not add an undo entry: the last successful title edit is still next.
    s.execute("edit.undo", &json!({})).unwrap();
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, &photo.meta.title), (2, &previous_title));
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn embedded_jpeg_xmp_reread_non_utf8_sidecar_preserves_state_and_undo() {
    let src = temp_dir("jpeg-xmp-non-utf8-sidecar");
    let file = src.join("photo.jpg");
    let original = write_jpeg_xmp(&file, &jpeg_xmp_packet(4, "Embedded title"));
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert!(!s.xmp.auto_write);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let previous_title = s.catalog.photo(id).unwrap().meta.title.clone();
    s.execute("photo.setMeta", &json!({"title": "Catalog title"})).unwrap();
    let before = s.catalog.photo(id).unwrap().clone();
    let sidecar = src.join("photo.xmp");
    let invalid = [0xff, 0xfe, 0x80];
    std::fs::write(&sidecar, invalid).unwrap();
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(crate::sidecar::find_sidecar(file.to_str().unwrap(), s.sidecar_naming(id)), Some(sidecar.clone()));

    let read = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(read["read"], json!([]), "an existing non-UTF-8 sidecar must not fall back to embedded JPEG XMP: {read}");
    assert_eq!(read["failed"].as_array().unwrap().len(), 1, "{read}");
    assert_eq!(read["failed"][0]["id"], id.0);
    assert!(!read["failed"][0]["error"].as_str().unwrap().is_empty());
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, &photo.meta.title), (before.rating, &before.meta.title));
    assert_eq!(photo.develop, before.develop);

    s.execute("edit.undo", &json!({})).unwrap();
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, &photo.meta.title), (2, &previous_title), "failed reads must not add an undo entry");
    assert_eq!(photo.develop, before.develop);
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(std::fs::read(&sidecar).unwrap(), invalid);
    assert!(!src.join("photo.jpg.xmp").exists());
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn embedded_jpeg_xmp_reread_all_metadata_uses_supplied_bytes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let src = temp_dir("jpeg-xmp-all-metadata");
    let file = src.join("photo.jpg");
    let original = write_jpeg_xmp(&file, &jpeg_xmp_packet(1, "On-disk title"));
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert!(!s.xmp.auto_write);
    let before = s.catalog.photo(id).unwrap().clone();
    let supplied = write_jpeg_xmp(&src.join("supplied.jpg"), &jpeg_xmp_packet(4, "Supplied title"));
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let expected_path = file.to_string_lossy().into_owned();
    s.media.file_bytes = Some(Arc::new(move |path| {
        assert_eq!(path, expected_path);
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(supplied.clone())
    }));

    let query = s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let xmp = query["xmp"].as_array().unwrap();
    assert!(
        xmp.iter().any(|x| x["name"] == "dc:title" && x["value"] == "Supplied title"),
        "All Metadata must use the bytes already supplied: {query}"
    );
    assert!(xmp.iter().any(|x| x["name"] == "xmp:Rating" && x["value"] == "4"), "{query}");
    assert!(!xmp.iter().any(|x| x["value"] == "On-disk title"), "{query}");

    let sidecar = src.join("photo.xmp");
    let packet = jpeg_xmp_packet(5, "Sidecar title");
    std::fs::write(&sidecar, &packet).unwrap();
    let query = s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    let xmp = query["xmp"].as_array().unwrap();
    assert!(xmp.iter().any(|x| x["name"] == "dc:title" && x["value"] == "Sidecar title"), "{query}");
    assert!(xmp.iter().any(|x| x["name"] == "xmp:Rating" && x["value"] == "5"), "{query}");
    assert!(!xmp.iter().any(|x| x["value"] == "Supplied title"), "{query}");
    let photo = s.catalog.photo(id).unwrap();
    assert_eq!((photo.rating, &photo.meta.title), (before.rating, &before.meta.title));
    assert_eq!(photo.develop, before.develop);
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), packet);
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn demo_photos_have_no_sidecar() {
    let mut s = Session::with_demo();
    let r = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    assert_eq!(r["written"].as_array().unwrap().len(), 0);
    assert_eq!(r["failed"].as_array().unwrap().len(), 1);
}

/// All Metadata: EXIF rows from the file and its XMP, for a photo on disk.
#[test]
fn all_metadata_lists_exif_and_xmp() {
    let dir = temp_dir("allmeta");
    let path = dir.join("tagged.jpg");
    let img = lcc_raster::Rgba8 { width: 16, height: 8, data: vec![[90, 120, 200, 255]; 128] };
    let exif = lcc_meta::write_exif(&lcc_meta::Metadata { make: Some("Maker".into()), iso: Some(800), ..Default::default() });
    let meta = lcc_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    let jpg = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap();
    std::fs::write(&path, jpg).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "A title"})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let r = s.execute("photo.allMetadata", &json!({})).unwrap();
    let exif = r["exif"].as_array().unwrap();
    assert!(exif.iter().any(|e| e["name"] == "Make" && e["value"] == "Maker"), "{r}");
    assert!(exif.iter().any(|e| e["name"] == "ISO Speed" && e["value"] == "800"));
    assert!(r["xmp"].as_array().unwrap().iter().any(|x| x["name"] == "dc:title" && x["value"] == "A title"), "the sidecar's fields: {r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Convert to DNG: a corpus raw (skipped without the corpus) becomes a DNG next to it with the
/// photo relinked and its edits kept; undo goes back to the original; non-raws are skipped.
#[test]
fn convert_raw_to_dng() {
    let corpus = lcc_brand::env_os("CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("raw/nef-nikon-d5100-uncompressed.nef");
    let mut s = Session::new().with_fs();
    // a non-raw is skipped
    let dir = temp_dir("todng");
    let png = dir.join("a.png");
    write_png(&png, 3);
    s.execute("library.import", &json!({"paths": [png.to_string_lossy()]})).unwrap();
    let r = s.execute("photo.convertToDng", &json!({})).unwrap();
    assert_eq!((r["converted"].as_array().unwrap().len(), r["skipped"].as_array().unwrap().len()), (0, 1));
    if !corpus.exists() {
        lcc_testkit::skip!(Corpus, "{} absent", corpus.display());
        return;
    }
    let nef = dir.join("shot.nef");
    std::fs::copy(&corpus, &nef).unwrap();
    s.execute("library.import", &json!({"paths": [nef.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.6})).unwrap();
    let r = s.execute("photo.convertToDng", &json!({})).unwrap();
    assert_eq!(r["converted"].as_array().unwrap().len(), 1, "{r}");
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.file_name.as_str(), p.format.as_str()), ("shot.dng", "DNG"));
    assert!(dir.join("shot.dng").exists() && nef.exists(), "the original is kept");
    assert_eq!(p.develop.light.exposure, 0.6, "edits kept");
    let bytes = std::fs::read(dir.join("shot.dng")).unwrap();
    assert_eq!(lcc_raw::probe(&bytes), Some(lcc_raw::RawFormat::Dng));
    assert!(s.render_now(id, 64, 64).is_ok(), "the DNG renders");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().file_name, "shot.nef");
    // Copy as DNG at import: only the DNG lands in the destination
    let card = dir.join("card");
    std::fs::create_dir_all(&card).unwrap();
    std::fs::copy(&corpus, card.join("DSC_1.NEF")).unwrap();
    let dest = dir.join("dest");
    let mut s = Session::new().with_fs();
    let r = s
        .execute(
            "library.import",
            &json!({"paths": [card.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat", "dng": true}),
        )
        .unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 1, "{r}");
    let names: Vec<String> = std::fs::read_dir(&dest).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, ["DSC_1.dng"]);
    assert!(card.join("DSC_1.NEF").exists(), "the card is untouched");
    let id2 = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    assert_eq!(s.catalog.photo(id2).unwrap().format, "DNG");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #106: Convert to DNG wrote straight to the final name (no temp file, sync or check) and
/// relinked the photo even when the write was cut short. A failed write now leaves no DNG and
/// the photo on its raw; a good one is verified and never replaces an existing file.
#[test]
fn convert_to_dng_is_verified_and_atomic() {
    let dir = temp_dir("todng-safe");
    // a synthetic raw (stored as DNG, catalogued as a NEF so Convert to DNG takes it)
    let raw = dir.join("shot.dng");
    let bytes = synthetic_dng_with(None, Default::default());
    std::fs::write(&raw, &bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let path = raw.to_string_lossy().to_string();
    s.catalog
        .apply(lcc_catalog::Op::Relink {
            id,
            file_name: "shot.dng".into(),
            source: lcc_catalog::Source::File { path: path.clone() },
            format: Some("NEF".into()),
        })
        .unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let names = || {
        let mut v: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    };
    {
        // the drive fills up part-way
        let _fault = lcc_catalog::safe_file::fail_writes_after(200);
        let r = s.execute("photo.convertToDng", &json!({})).unwrap();
        assert_eq!(r["converted"].as_array().unwrap().len(), 0, "{r}");
        assert!(r["skipped"][0][1].as_str().unwrap().contains("the raw is kept"), "{r}");
    }
    assert_eq!(names(), ["shot.dng"], "no partial DNG");
    assert_eq!(s.catalog.photo(id).unwrap().source, lcc_catalog::Source::File { path: path.clone() }, "not relinked");
    // a good conversion: a new name (the existing file is not replaced), decodable, relinked
    let r = s.execute("photo.convertToDng", &json!({})).unwrap();
    let out = r["converted"][0]["path"].as_str().unwrap().to_string();
    assert!(out.ends_with("shot-2.dng"), "{r}");
    assert_eq!(std::fs::read(&raw).unwrap(), bytes, "the raw is untouched");
    let back = lcc_raw::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(back.data, lcc_raw::decode(&bytes).unwrap().data);
    assert_eq!(s.catalog.photo(id).unwrap().source, lcc_catalog::Source::File { path: out });
    assert_eq!(names(), ["shot-2.dng", "shot.dng"], "no temp file left");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Edit in External Editor (engine half): a 16-bit TIFF `-Edit` copy with the edits, next to
/// the original, added and stacked on top of it; a second one doesn't overwrite the first.
#[test]
fn external_edit_copy_is_stacked() {
    let dir = temp_dir("extedit");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let r = s.execute("photo.editExternal", &json!({})).unwrap();
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("beach-Edit.tif"), "{path}");
    let bytes = std::fs::read(&path).unwrap();
    let d = lcc_codecs::decode(&bytes, Default::default()).unwrap();
    assert_eq!(d.bit_depth, 16);
    let new = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    assert_eq!(s.active(), Some(new));
    let st = s.catalog.stack_of(new).expect("stacked");
    assert_eq!((st.top(), st.photos.contains(&orig)), (new, true));
    let r2 = s.execute("photo.editExternal", &json!({"colorSpace": "proPhoto"})).unwrap();
    assert!(r2["path"].as_str().unwrap().ends_with("beach-Edit-Edit.tif") || r2["path"].as_str().unwrap().contains("-2"), "{r2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #3617: a second Edit In on a photo whose first copy is unchanged renders the same bytes. Import skips
/// them as a duplicate; the command used to fail with "invalid parameters" and leave the new file behind.
#[test]
fn external_edit_again_on_an_unchanged_copy_still_works() {
    let dir = temp_dir("extedit-again");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    let first = s.execute("photo.editExternal", &json!({})).unwrap();
    let first_path = first["path"].as_str().unwrap().to_string();
    // the editor was closed without saving: same photo, same settings
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let files = || std::fs::read_dir(&dir).unwrap().count();
    let before = files();
    let again = s.execute("photo.editExternal", &json!({})).expect("a second Edit In works");
    let again_path = again["path"].as_str().unwrap();
    assert!(std::path::Path::new(again_path).exists(), "{again}");
    assert_eq!(std::fs::read(again_path).unwrap(), std::fs::read(&first_path).unwrap(), "the same render");
    let id = lcc_catalog::PhotoId(again["id"].as_u64().unwrap());
    assert!(s.catalog.photo(id).is_some(), "the copy it hands to the editor is in the library: {again}");
    assert!(files() <= before, "no orphan copy is left on disk ({} files before, {} after)", before, files());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #3617, review of #3628: the unchanged copy was moved to Recently Deleted and its file is gone. Import then
/// restores that photo by pointing it at the file just rendered, so that file must be kept.
#[test]
fn external_edit_again_after_the_copy_was_trashed_keeps_the_file_the_restored_copy_uses() {
    let dir = temp_dir("extedit-trashed");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    let first = s.execute("photo.editExternal", &json!({})).unwrap();
    let copy = first["id"].as_u64().unwrap();
    s.execute("photo.delete", &json!({"ids": [copy]})).unwrap();
    std::fs::remove_file(first["path"].as_str().unwrap()).unwrap();
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let again = s.execute("photo.editExternal", &json!({})).expect("Edit In works after the copy was trashed");
    let path = again["path"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists(), "the copy handed to the editor has its file: {again}");
    let id = lcc_catalog::PhotoId(again["id"].as_u64().unwrap());
    let photo = s.catalog.photo(id).expect("in the library");
    assert!(!photo.deleted, "restored from Recently Deleted");
    assert_eq!(photo.source, lcc_catalog::Source::File { path: path.to_string() }, "and its file is there");
    // it left the stack when it was deleted: it is stacked on the original again
    let st = s.catalog.stack_of(id).expect("restacked");
    assert_eq!((st.top(), st.photos.contains(&orig)), (id, true));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review of #3628: the unchanged copy is still in the original's stack, but under it and collapsed. As for a new copy,
/// the copy is put on top and the stack opened, so the editor's file shows in the grid.
#[test]
fn external_edit_again_puts_a_hidden_copy_back_on_top_of_its_stack() {
    let dir = temp_dir("extedit-hidden");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    let first = s.execute("photo.editExternal", &json!({})).unwrap();
    let copy = lcc_catalog::PhotoId(first["id"].as_u64().unwrap());
    // the original on top, the stack collapsed: the copy is hidden under it
    s.execute("stack.group", &json!({"ids": [orig.0, copy.0], "top": orig.0, "collapsed": true})).unwrap();
    let hidden = s.catalog.stack_of(copy).expect("stacked");
    assert_eq!((hidden.top(), hidden.collapsed), (orig, true));
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let again = s.execute("photo.editExternal", &json!({})).unwrap();
    assert_eq!(again["id"].as_u64(), Some(copy.0), "the same copy");
    let st = s.catalog.stack_of(copy).expect("still stacked");
    assert_eq!((st.top(), st.collapsed, st.photos.contains(&orig)), (copy, false, true));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review of #3628: same bytes alone don't make another photo this photo's edit copy. An unrelated photo that holds the
/// render is not handed to the editor or stacked; the command fails and leaves no stray file.
#[test]
fn external_edit_again_does_not_reuse_an_unrelated_photo_with_the_same_bytes() {
    let dir = temp_dir("extedit-unrelated");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    let first = s.execute("photo.editExternal", &json!({})).unwrap();
    let first_path = first["path"].as_str().unwrap().to_string();
    // the render's bytes now belong to a photo that is not the original's copy
    let unrelated = dir.join("holiday.tif");
    std::fs::copy(&first_path, &unrelated).unwrap();
    s.execute("photo.deletePermanently", &json!({"ids": [first["id"].as_u64().unwrap()]})).unwrap();
    let _ = std::fs::remove_file(&first_path);
    let imported = s.execute("library.import", &json!({"paths": [unrelated.to_string_lossy()]})).unwrap();
    let other = lcc_catalog::PhotoId(imported["imported"][0].as_u64().expect("the unrelated photo is imported"));
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let files = || std::fs::read_dir(&dir).unwrap().count();
    let before = files();
    assert!(s.execute("photo.editExternal", &json!({})).is_err(), "not reused");
    assert!(s.catalog.stack_of(other).is_none(), "the unrelated photo is not stacked on the original");
    assert!(files() <= before, "no stray file left");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review of #3628: the catalog still has the copy but its file was removed outside the app. The editor is handed a file
/// that exists (import adds the new render as a copy), never a path to the missing one, and nothing is orphaned.
#[test]
fn external_edit_again_hands_over_a_file_that_exists_when_the_copys_file_is_gone() {
    let dir = temp_dir("extedit-gone");
    let src = dir.join("beach.png");
    write_png(&src, 9);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    let first = s.execute("photo.editExternal", &json!({})).unwrap();
    std::fs::remove_file(first["path"].as_str().unwrap()).unwrap();
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let files = || std::fs::read_dir(&dir).unwrap().count();
    let before = files();
    let again = s.execute("photo.editExternal", &json!({})).expect("Edit In works when the copy's file is gone");
    let path = again["path"].as_str().unwrap();
    assert!(std::path::Path::new(path).is_file(), "the editor gets a file that exists: {again}");
    assert_eq!(files(), before + 1, "one new copy, nothing orphaned");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #920: with several photos selected, each gets its own `-Edit` copy stacked on its
/// original, the copies end up selected and are all reported; `ids` picks photos explicitly.
#[test]
fn external_edit_copies_every_selected_photo() {
    let dir = temp_dir("extedit-many");
    let paths: Vec<String> = ["a", "b", "c"]
        .iter()
        .zip(9u8..)
        .map(|(n, seed)| {
            let p = dir.join(format!("{n}.png"));
            // different bytes: the import keeps one of identical files
            write_png(&p, seed);
            p.to_string_lossy().into_owned()
        })
        .collect();
    let mut s = Session::new().with_fs();
    let imported = s.execute("library.import", &json!({"paths": paths})).unwrap();
    let originals: Vec<u64> = imported["imported"].as_array().unwrap().iter().filter_map(serde_json::Value::as_u64).collect();
    assert_eq!(originals.len(), 3);
    s.execute("library.select", &json!({"ids": originals})).unwrap();
    let r = s.execute("photo.editExternal", &json!({})).unwrap();
    let copies = r["copies"].as_array().unwrap();
    assert_eq!(copies.len(), 3, "{r}");
    for (copy, orig) in copies.iter().zip(&originals) {
        assert_eq!(copy["original"].as_u64(), Some(*orig));
        assert!(copy["path"].as_str().unwrap().ends_with("-Edit.tif"), "{copy}");
        let new = lcc_catalog::PhotoId(copy["id"].as_u64().unwrap());
        let st = s.catalog.stack_of(new).expect("stacked");
        assert_eq!((st.top(), st.photos.contains(&lcc_catalog::PhotoId(*orig))), (new, true));
        assert!(s.selection.contains(new), "the copies are selected");
    }
    assert_eq!(r["path"], copies[0]["path"], "the single-copy fields name the first copy");
    // an edit since the first copy: identical bytes would be skipped as a duplicate import
    s.execute("develop.set", &json!({"ids": [originals[1]], "control": "light.exposure", "value": 0.5})).unwrap();
    let one = s.execute("photo.editExternal", &json!({"ids": [originals[1]]})).unwrap();
    assert_eq!(one["copies"].as_array().map(Vec::len), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Edit in PhotoCraft (engine half): a flat 16-bit PSD `-Edit` copy, which PhotoCraft saves back
/// to in place (it writes a TIFF only through Save As); its saved pixels come back on Reload.
#[test]
fn external_edit_copy_as_psd_round_trips() {
    let dir = temp_dir("extedit-psd");
    let src = dir.join("pier.png");
    let capture = lcc_meta::DateTime::parse_iso("2023-05-14T14:40:39").unwrap();
    let source_meta = lcc_meta::Metadata { capture_time: Some(capture), title: Some("Dated original".into()), ..Default::default() };
    let exif = lcc_meta::write_exif(&source_meta);
    let xmp = lcc_meta::write_xmp(&source_meta, None);
    let rgba = lcc_raster::Rgba8::from_fn(40, 24, |x, y| [(x * 6) as u8, (y * 9) as u8, 40, 255]);
    let png = lcc_codecs::encode_png(
        &lcc_codecs::EncodeImage::rgba8(&rgba),
        &lcc_codecs::EncodeMeta { exif: Some(&exif), xmp: Some(&xmp), ..Default::default() },
    )
    .unwrap();
    std::fs::write(&src, png).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    assert_eq!(s.catalog.photo(orig).unwrap().captured.as_deref(), Some("2023-05-14T14:40:39"));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    let r = s.execute("photo.editExternal", &json!({"format": "psd", "bitDepth": 16})).unwrap();
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("pier-Edit.psd"), "{path}");
    let d = lcc_codecs::decode(&std::fs::read(&path).unwrap(), Default::default()).unwrap();
    assert_eq!(d.bit_depth, 16);
    assert!(d.icc.is_some(), "the colour space travels with the file");
    let new = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    assert_eq!(s.catalog.photo(new).unwrap().captured.as_deref(), Some("2023-05-14T14:40:39"));
    let all_metadata = s.execute("photo.allMetadata", &json!({"id": new.0})).unwrap();
    assert!(all_metadata["exif"].as_array().unwrap().iter().any(|row| row["tag"] == json!(0x9003)), "{all_metadata}");
    assert!(
        all_metadata["xmp"].as_array().unwrap().iter().any(|row| row["name"] == "dc:title" && row["value"] == "Dated original"),
        "{all_metadata}"
    );
    assert_eq!(s.catalog.stack_of(new).map(|st| (st.top(), st.photos.contains(&orig))), Some((new, true)));
    // the editor saves the document back over the same file: Reload shows its pixels
    let before = s.render_now(new, 16, 16).unwrap().image.data;
    let (w, h) = (d.width as u32, d.height as u32);
    let px = vec![1000u16; (w * h * 3) as usize];
    let saved = lcc_codecs::encode_psd(&lcc_codecs::EncodeImage::new(w, h, 3, lcc_codecs::Samples::U16(&px)), &Default::default()).unwrap();
    std::fs::write(&path, saved).unwrap();
    assert_eq!(s.execute("photo.reload", &json!({"ids": [new.0]})).unwrap()["reloaded"], json!([new.0]));
    assert_ne!(s.render_now(new, 16, 16).unwrap().image.data, before);
    // 8-bit, and what it can't do
    s.execute("library.select", &json!({"ids": [orig.0]})).unwrap();
    let r8 = s.execute("photo.editExternal", &json!({"format": "psd", "bitDepth": 8})).unwrap();
    let d8 = lcc_codecs::decode(&std::fs::read(r8["path"].as_str().unwrap()).unwrap(), Default::default()).unwrap();
    assert_eq!(d8.bit_depth, 8);
    assert!(s.execute("photo.editExternal", &json!({"format": "jpeg"})).is_err());
    assert!(s.execute("photo.editExternal", &json!({"bitDepth": 32})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn psb_import_retains_capture_time_and_all_metadata() {
    let dir = temp_dir("import-psb-metadata");
    let path = dir.join("dated.psb");
    let capture = lcc_meta::DateTime::parse_exif("2023:05:14 14:40:39");
    let metadata = lcc_meta::Metadata {
        capture_time: capture,
        gps: Some(lcc_meta::Gps { latitude: 45.0, longitude: 7.0, altitude: None }),
        title: Some("PSB original".into()),
        ..Default::default()
    };
    let bytes = synthetic_psb_with_metadata(&metadata);
    assert_eq!(lcc_meta::extract(&bytes).capture_time, capture);
    assert_eq!(lcc_meta::extract(&bytes).gps, metadata.gps);
    std::fs::write(&path, bytes).unwrap();

    let mut session = Session::new().with_fs();
    let imported = session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert_eq!(imported["imported"].as_array().map(Vec::len), Some(1), "{imported}");
    let id = session.catalog.photos().next().unwrap().id;
    assert_eq!(session.catalog.photo(id).unwrap().captured.as_deref(), Some("2023-05-14T14:40:39"));
    let all_metadata = session.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    assert!(all_metadata["exif"].as_array().unwrap().iter().any(|row| row["tag"] == json!(0x9003)), "{all_metadata}");
    assert!(all_metadata["xmp"].as_array().unwrap().iter().any(|row| row["name"] == "dc:title" && row["value"] == "PSB original"), "{all_metadata}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reload: a file changed on disk (an external editor saved it) gets its new size / hash, the
/// cached source is dropped and renders change; unchanged files are left alone.
#[test]
fn reload_picks_up_changed_files() {
    let dir = temp_dir("reload");
    let f = dir.join("x.png");
    write_png(&f, 10);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [f.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let before = s.render_now(id, 32, 32).unwrap().image.data;
    let key0 = s.thumb_job(id, 128).unwrap().key;
    assert_eq!(s.execute("photo.reload", &json!({})).unwrap()["reloaded"], json!([]), "unchanged");
    write_png(&f, 200);
    let r = s.execute("photo.reload", &json!({})).unwrap();
    assert_eq!(r["reloaded"], json!([id.0]));
    assert_ne!(s.thumb_job(id, 128).unwrap().key, key0, "the UI sees a new render key");
    assert_ne!(s.render_now(id, 32, 32).unwrap().image.data, before, "renders the new pixels");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplicate_copies_the_file_and_the_edits() {
    let dir = temp_dir("dup");
    let f = dir.join("pic.png");
    write_png(&f, 5);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [f.to_string_lossy()]})).unwrap();
    let orig = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.8})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let alb = s.execute("album.create", &json!({"name": "Keep", "addSelected": true})).unwrap()["id"].as_u64().unwrap();
    let r = s.execute("photo.duplicate", &json!({})).unwrap();
    let dup = lcc_catalog::PhotoId(r["ids"][0].as_u64().unwrap());
    let d = s.catalog.photo(dup).unwrap().clone();
    assert_eq!(d.file_name, "pic-copy.png");
    assert!(dir.join("pic-copy.png").exists());
    assert_eq!((d.develop.light.exposure, d.rating), (0.8, 4));
    assert_eq!(s.catalog.album_count(lcc_catalog::AlbumId(alb)), 2);
    assert_ne!(dup, orig);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(dup).is_none(), "one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gps_typed_in_is_saved_to_xmp() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap_or_else(|| s.catalog.photos().next().unwrap().id);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.setMeta", &json!({"gps": "48°51'30\"N 2°17'40\"E"})).unwrap();
    let (la, lo) = s.catalog.photo(id).unwrap().meta.gps.unwrap();
    assert!((la - 48.8583).abs() < 1e-3 && (lo - 2.2944).abs() < 1e-3, "{la} {lo}");
    let x = crate::sidecar::sidecar_packet(s.catalog.photo(id).unwrap(), &s.catalog);
    let back = lcc_meta::parse_xmp(&x).unwrap().metadata.gps.unwrap();
    assert!((back.latitude - la).abs() < 1e-5 && (back.longitude - lo).abs() < 1e-5);
    assert!(s.execute("photo.setMeta", &json!({"gps": "north pole-ish"})).is_err());
    s.execute("photo.setMeta", &json!({"gps": null})).unwrap();
    assert!(s.catalog.photo(id).unwrap().meta.gps.is_none());
}

/// A PNG whose EXIF places it at (`lat`, `lon`), as a camera's GPS does.
fn write_png_at(path: &Path, lat: f64, lon: f64) {
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 150, 255]; 64] };
    let gps = lcc_meta::Metadata { gps: Some(lcc_meta::Gps { latitude: lat, longitude: lon, altitude: None }), ..Default::default() };
    let exif = lcc_meta::write_exif(&gps);
    let meta = lcc_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    std::fs::write(path, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &meta).unwrap()).unwrap();
}

/// A sidecar's GPS wins over the camera's (MWG: XMP over Exif), on import and on Read Metadata from
/// File; saving leaves the sidecar's position as it was written, and writes it only when it was
/// changed here.
#[test]
fn sidecar_gps_wins_over_the_cameras_and_survives_a_save() {
    let src = temp_dir("sidecar-gps");
    write_png_at(&src.join("walk.png"), 10.5, 20.25);
    let sidecar = |lat: &str, lon: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2">
   <exif:GPSLatitude>{lat}</exif:GPSLatitude>
   <exif:GPSLongitude>{lon}</exif:GPSLongitude>
  </rdf:Description>
</rdf:RDF></x:xmpmeta>"#
        )
    };
    std::fs::write(src.join("walk.xmp"), sidecar("12,30.12345N", "45,15.54321W")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    let near = |(a, b): (f64, f64), (c, d): (f64, f64)| (a - c).abs() < 1e-6 && (b - d).abs() < 1e-6;
    let gps = |s: &Session| s.catalog.photo(id).unwrap().meta.gps.unwrap();
    assert!(near(gps(&s), (12.5020575, -45.2590535)), "the sidecar's position, not the camera's: {:?}", gps(&s));
    // a save that didn't change the position leaves it in the file exactly as written
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let saved = std::fs::read_to_string(src.join("walk.xmp")).unwrap();
    assert!(saved.contains("<exif:GPSLatitude>12,30.12345N</exif:GPSLatitude>"), "{saved}");
    assert!(saved.contains("<exif:GPSLongitude>45,15.54321W</exif:GPSLongitude>"), "{saved}");
    assert_eq!(lcc_meta::parse_xmp(&saved).unwrap().metadata.rating, Some(4), "the save happened");
    // another app moved the photo: reading the sidecar again takes its position
    std::fs::write(src.join("walk.xmp"), sidecar("5,6.60000S", "100,30.30000E")).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert!(near(gps(&s), (-5.11, 100.505)), "{:?}", gps(&s));
    // a position typed in here is written
    s.execute("photo.setMeta", &json!({"ids": [id.0], "gps": "1.2345, 6.789"})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let g = lcc_meta::parse_xmp(&std::fs::read_to_string(src.join("walk.xmp")).unwrap()).unwrap().metadata.gps.unwrap();
    assert!(near((g.latitude, g.longitude), (1.2345, 6.789)), "{g:?}");
    let _ = std::fs::remove_dir_all(&src);
}

/// A position cleared here stays cleared: saving removes the GPS the sidecar still states for it, so it is not read
/// back the next time. A different position there (another app wrote it after the clear) is not ours to remove.
#[test]
fn a_cleared_gps_position_leaves_the_sidecar() {
    let src = temp_dir("sidecar-gps-clear");
    write_png_at(&src.join("walk.png"), 10.5, 20.25);
    let sidecar = |lat: &str, lon: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2">
   <exif:GPSLatitude>{lat}</exif:GPSLatitude>
   <exif:GPSLongitude>{lon}</exif:GPSLongitude>
  </rdf:Description>
</rdf:RDF></x:xmpmeta>"#
        )
    };
    let stated = || lcc_meta::parse_xmp(&std::fs::read_to_string(src.join("walk.xmp")).unwrap()).unwrap().metadata.gps;
    std::fs::write(src.join("walk.xmp"), sidecar("12,30N", "45,15W")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert!(s.catalog.photo(id).unwrap().meta.gps.is_some());
    s.execute("photo.setMeta", &json!({"ids": [id.0], "gps": null})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert!(stated().is_none(), "the cleared position is gone from the sidecar");
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert!(s.catalog.photo(id).unwrap().meta.gps.is_none(), "and does not come back");
    assert_eq!(s.catalog.photo(id).unwrap().rating, 2, "the rest of the sidecar is read as before");

    // another application geotags the photo after the clear: a save keeps its position
    std::fs::write(src.join("walk.xmp"), sidecar("5,6S", "100,30E")).unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert!(stated().is_some(), "a position this app did not clear stays");
    let _ = std::fs::remove_dir_all(&src);
}

/// Lightroom Classic writes a sidecar's GPS as attributes on `rdf:Description`, with GPSVersionID and
/// GPSAltitude beside it; a file with no position of its own (a raw without GPS) takes it (fork issue #247).
#[test]
fn lightroom_attribute_gps_in_a_sidecar_is_read() {
    let src = temp_dir("sidecar-gps-lr");
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 150, 255]; 64] };
    std::fs::write(src.join("nef.png"), lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    std::fs::write(
        src.join("nef.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/"
   exif:GPSLatitude="41,53.6349N"
   exif:GPSLongitude="12,29.60904E"
   exif:GPSVersionID="2.2.0.0"
   exif:GPSAltitude="441000/10000"/>
</rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let (lat, lon) = s.catalog.photo(only(&s)).unwrap().meta.gps.expect("the sidecar's position");
    assert!((lat - (41.0 + 53.6349 / 60.0)).abs() < 1e-6 && (lon - (12.0 + 29.60904 / 60.0)).abs() < 1e-6, "{lat} {lon}");
    let _ = std::fs::remove_dir_all(&src);
}

/// A photo with no capture time in the file or its sidecar is dated by the file's modification time, but
/// that guess is never written to the sidecar as its capture time; a time read from the sidecar later, or
/// set here, replaces it and is written.
#[test]
fn a_file_date_guess_is_never_saved_as_the_capture_time() {
    let src = temp_dir("file-date");
    write_png(&src.join("scan.png"), 3);
    let rated = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2"/>
</rdf:RDF></x:xmpmeta>"#;
    std::fs::write(src.join("scan.xmp"), rated).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = only(&s);
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.captured.is_some(), "dated by the file, as Lightroom does");
    assert_eq!(p.captured_fallback, p.captured);
    let dates = |src: &Path| {
        let d = lcc_meta::parse_xmp(&std::fs::read_to_string(src.join("scan.xmp")).unwrap()).unwrap();
        ["exif:DateTimeOriginal", "photoshop:DateCreated"].map(|k| d.properties.get(k).and_then(|v| v.first()).cloned())
    };
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(dates(&src), [None, None], "no capture time is made up");
    // the sidecar learns the real time elsewhere: reading it replaces the guess
    std::fs::write(
        src.join("scan.xmp"),
        rated.replace(
            "xmp:Rating=\"2\"",
            "xmp:Rating=\"2\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" exif:DateTimeOriginal=\"2019-07-01T10:20:30\"",
        ),
    )
    .unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2019-07-01T10:20:30"));
    // a time set here is the user's, and is written
    s.execute("photo.setCaptureTime", &json!({"ids": [id.0], "time": "2020-01-02T03:04:05"})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(dates(&src), [Some("2020-01-02T03:04:05".to_string()), Some("2020-01-02T03:04:05".to_string())]);
    let _ = std::fs::remove_dir_all(&src);
}

/// DNG export compression: lossless, zip and none all decode; none is the biggest.
#[test]
fn dng_export_compression_choices() {
    let corpus = lcc_brand::env_os("CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("raw/nef-nikon-d5100-uncompressed.nef");
    if !corpus.exists() {
        lcc_testkit::skip!(Corpus, "{} absent", corpus.display());
        return;
    }
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [corpus.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let mut sizes = Vec::new();
    for c in ["lossless", "deflate", "uncompressed", "lossy"] {
        let o = crate::export::ExportOptions::from_json(&json!({"format": "dng", "dngCompression": c}));
        let mut out = Vec::new();
        let mut write = |_: &str, b: &[u8]| {
            out = b.to_vec();
            Ok(())
        };
        crate::export::export_batch(&mut s, &[id], &o, &crate::export::Destination { dir: "x".into(), exact: None }, &mut write, &|_| false).unwrap();
        let raw = lcc_raw::decode(&out).unwrap_or_else(|e| panic!("{c}: {e}"));
        assert!(raw.width > 1000, "{c}");
        sizes.push((c, out.len()));
    }
    assert!(sizes[2].1 > sizes[0].1 && sizes[2].1 > sizes[1].1, "uncompressed is the biggest: {sizes:?}");
    assert!(sizes[3].1 < sizes[0].1 / 2, "lossy is much smaller than lossless: {sizes:?}");
}

/// Issue #1745: the DNG of a raw without colour matrices of its own (a Nikon Z 9 NEF) had no `ColorMatrix1`, and
/// other raw converters refused it. Export as DNG and Convert / Copy as DNG now write the colour the raw opens with:
/// the measured matrices of a camera that has them (Z f), the neutral fallback of one that doesn't (Z 9, whose
/// look is fitted to its JPEG when there is one).
#[test]
fn dngs_of_raws_without_colour_matrices_carry_the_colour_they_open_with() {
    use lcc_raw::color::{as_shot_white_xy, camera_transform, has_matrix};
    let dir = temp_dir("dng-colour");
    let samples = [[0.2, 0.3, 0.1], [0.05, 0.02, 0.4], [0.6, 0.6, 0.6], [0.01, 0.5, 0.03]];
    let render = |t: &lcc_raw::color::CameraTransform, c: [f64; 3]| {
        t.matrix.apply(std::array::from_fn(|i| c[i] * t.wb[i] as f64)).map(|v| v * t.baseline_exposure.exp2())
    };
    for (model, measured) in [("NIKON Z f", true), ("NIKON Z 9", false)] {
        let metadata = lcc_meta::Metadata { make: Some("NIKON CORPORATION".into()), model: Some(model.into()), ..Default::default() };
        let mut raw = lcc_raw::decode(&synthetic_dng_with(None, metadata)).unwrap();
        // a vendor raw's colour: a white balance, no matrices
        raw.color = lcc_raw::ColorData { as_shot_neutral: Some([0.5, 1.0, 0.7]), ..Default::default() };
        let bytes = lcc_raw::write_dng(&raw, &Default::default()).unwrap();
        let path = dir.join(format!("{model}.dng"));
        std::fs::write(&path, &bytes).unwrap();
        let mut opened = lcc_raw::decode(&bytes).unwrap();
        assert!(!has_matrix(&opened.color));
        let (_, want, look) = crate::camera_preview::starting_colour(&mut opened, &bytes, None, None, lcc_develop::CameraColour::Auto, false);
        assert_eq!((want.matrix_is_fallback, look.is_none()), (!measured, true), "{model}");

        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        let id = s.active().unwrap();
        let o = crate::export::ExportOptions::from_json(&json!({"format": "dng"}));
        let mut exported = Vec::new();
        let mut write = |_: &str, b: &[u8]| {
            exported = b.to_vec();
            Ok(())
        };
        crate::export::export_batch(&mut s, &[id], &o, &crate::export::Destination { dir: "x".into(), exact: None }, &mut write, &|_| false).unwrap();
        let converted = crate::cmd::convert::write_dng_with(None, &path.to_string_lossy(), String::new()).unwrap();
        for (how, dng) in [("export", exported), ("convert", std::fs::read(&converted).unwrap())] {
            let back = lcc_raw::decode(&dng).unwrap();
            assert!(back.color.color_matrix[0].is_some() && back.color.illuminant[0] != 0, "{model} {how}: {:?}", back.color);
            let got = camera_transform(&back, as_shot_white_xy(&back));
            for c in samples {
                let (a, b) = (render(&want, c), render(&got, c));
                assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-3), "{model} {how} {c:?}: {a:?} vs {b:?}");
            }
        }
        std::fs::remove_file(&converted).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn write_jpeg(path: &Path, capture: Option<&str>) {
    let (w, h) = (32usize, 24usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 7) as u8, (i / w * 9) as u8, 90, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let exif = capture.map(|c| lcc_meta::write_exif(&lcc_meta::Metadata { capture_time: lcc_meta::DateTime::parse_iso(c), ..Default::default() }));
    let meta = lcc_codecs::EncodeMeta { exif: exif.as_deref(), ..Default::default() };
    let bytes = lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &meta).unwrap();
    std::fs::write(path, bytes).unwrap();
}

const DATED_SIDECAR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:exif="http://ns.adobe.com/exif/1.0/"
      exif:DateTimeOriginal="2026-01-14T05:58:48" />
  </rdf:RDF>
</x:xmpmeta>"#;

#[test]
fn sidecar_capture_time_fills_in_when_the_file_has_none() {
    let src = temp_dir("sidecar-date");
    write_jpeg(&src.join("sample.jpg"), None);
    std::fs::write(src.join("sample.jpg.xmp"), DATED_SIDECAR).unwrap();
    // the embedded capture time wins over the sidecar's
    write_jpeg(&src.join("dated.jpg"), Some("2020-05-06T07:08:09"));
    std::fs::write(src.join("dated.jpg.xmp"), DATED_SIDECAR).unwrap();
    let lib = temp_dir("sidecar-date-lib");
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    // copy: the sidecar's date also files the copy
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "copy"})).unwrap();
    assert_eq!(r["sidecars"], 2, "{r}");
    let by_name = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).unwrap().clone();
    let sample = by_name(&s, "sample.jpg");
    assert_eq!(sample.captured.as_deref(), Some("2026-01-14T05:58:48"));
    let lcc_catalog::Source::File { path } = &sample.source else { panic!("not a file") };
    assert!(Path::new(path).starts_with(lib.join("Originals").join("2026").join("2026-01-14")), "{path}");
    assert_eq!(by_name(&s, "dated.jpg").captured.as_deref(), Some("2020-05-06T07:08:09"));

    // Read Metadata from File fills in a missing capture time too
    let id = sample.id;
    s.catalog.apply(lcc_catalog::Op::SetCaptured { id, captured: None }).unwrap();
    std::fs::copy(src.join("sample.jpg.xmp"), format!("{path}.xmp")).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2026-01-14T05:58:48"));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// A year or a month alone (`1983-04`, a scanned print's date) is saved to the sidecar in
/// `photoshop:DateCreated` as stated, with no `exif:DateTimeOriginal` (an earlier full time's is
/// removed, or it would win on the next read), and reads back as the same text; a full time
/// writes both again.
#[test]
fn a_partial_capture_time_is_saved_to_the_sidecar_as_stated() {
    let src = temp_dir("partial-date");
    let lib = temp_dir("partial-date-lib");
    write_png(&src.join("scan.png"), 1);
    std::fs::write(src.join("scan.xmp"), DATED_SIDECAR).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("scan.png").to_string_lossy()]})).unwrap();
    let id = only(&s);
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2026-01-14T05:58:48"));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let saved = |s: &mut Session| {
        s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
        let out = std::fs::read_to_string(src.join("scan.xmp")).unwrap();
        let props = lcc_meta::parse_xmp(&out).unwrap().properties;
        (props.get("photoshop:DateCreated").cloned(), props.get("exif:DateTimeOriginal").cloned(), out)
    };
    for partial in ["1983-04", "1983"] {
        s.execute("photo.setCaptureTime", &json!({"time": partial})).unwrap();
        let (created, original, out) = saved(&mut s);
        assert_eq!(created, Some(vec![partial.to_string()]), "{out}");
        assert_eq!(original, None, "the earlier full time is gone: {out}");
        // reading it back gives the same text
        s.catalog.apply(lcc_catalog::Op::SetCaptured { id, captured: None }).unwrap();
        s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some(partial));
    }
    s.execute("photo.setCaptureTime", &json!({"time": "1983-04-12 10:00"})).unwrap();
    let (created, original, out) = saved(&mut s);
    assert_eq!((created, original), (Some(vec!["1983-04-12T10:00:00".to_string()]), Some(vec!["1983-04-12T10:00:00".to_string()])), "{out}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Copyright status, rights usage terms and copyright info URL: edited with `photo.setMeta` (one
/// undo step), copied / pasted, kept in metadata presets, written to the sidecar as XMP Rights
/// Management fields and read back on a fresh import.
#[test]
fn copyright_status_usage_terms_and_url_round_trip() {
    use lcc_catalog::CopyrightStatus;
    let src = temp_dir("rights-src");
    let lib = temp_dir("rights-lib");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (a, b) = (id(&s, "a.png"), id(&s, "b.png"));
    let meta = |s: &Session, id: PhotoId| s.catalog.photo(id).unwrap().meta.clone();
    assert_eq!(meta(&s, a).copyright_status, CopyrightStatus::Unknown);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute(
        "photo.setMeta",
        &json!({"copyright": "© 2026 A. Person", "copyrightStatus": "copyrighted", "usageTerms": "Editorial use only", "copyrightUrl": "https://example.com/rights"}),
    )
    .unwrap();
    let m = meta(&s, a);
    assert_eq!(
        (m.copyright_status, m.usage_terms.as_str(), m.copyright_url.as_str()),
        (CopyrightStatus::Copyrighted, "Editorial use only", "https://example.com/rights")
    );
    assert!(s.execute("photo.setMeta", &json!({"copyrightStatus": "maybe"})).is_err());
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(meta(&s, a).copyright_status, CopyrightStatus::Unknown, "one undo step");
    s.execute("edit.redo", &json!({})).unwrap();
    // copy → paste onto b
    let clip = s.execute("photo.copyMetadata", &json!({})).unwrap();
    assert_eq!(clip["copyrightStatus"], "copyrighted");
    s.execute("photo.pasteMetadata", &json!({"ids": [b.0], "fields": ["copyrightStatus", "usageTerms"]})).unwrap();
    let mb = meta(&s, b);
    assert_eq!((mb.copyright_status, mb.usage_terms.as_str(), mb.copyright_url.as_str()), (CopyrightStatus::Copyrighted, "Editorial use only", ""));
    // a metadata preset from the active photo carries the copyright fields; applying it sets them
    s.execute("metadata.savePreset", &json!({"name": "Rights"})).unwrap();
    let presets = s.execute("metadata.presets", &json!({})).unwrap();
    assert_eq!(presets[0]["fields"]["copyrightStatus"], "copyrighted", "{presets}");
    assert_eq!(presets[0]["fields"]["copyrightUrl"], "https://example.com/rights");
    s.execute("photo.setMeta", &json!({"ids": [b.0], "copyrightStatus": "publicDomain", "usageTerms": ""})).unwrap();
    s.execute("metadata.applyPreset", &json!({"name": "Rights", "ids": [b.0]})).unwrap();
    let mb = meta(&s, b);
    assert_eq!((mb.copyright_status, mb.copyright_url.as_str()), (CopyrightStatus::Copyrighted, "https://example.com/rights"));
    assert!(s.execute("metadata.savePreset", &json!({"name": "Bad", "fields": {"copyrightStatus": "sort of"}})).is_err());
    // b becomes public domain; smart-album rule on the status
    s.execute("photo.setMeta", &json!({"ids": [b.0], "copyrightStatus": "public domain"})).unwrap();
    let r = s
        .execute(
            "album.createSmart",
            &json!({"name": "PD", "rules": {"ruleSet": {"rules": [{"field": "copyrightStatus", "op": "is", "value": "publicDomain"}]}}}),
        )
        .unwrap();
    assert_eq!(r["count"], 1, "{r}");
    // sidecars: XMP Rights Management fields, read back by a fresh library
    s.execute("photo.saveMetadataToFile", &json!({"ids": [a.0, b.0]})).unwrap();
    let xmp = std::fs::read_to_string(src.join("a.xmp")).unwrap();
    assert!(xmp.contains("<xmpRights:Marked>True</xmpRights:Marked>"), "{xmp}");
    assert!(xmp.contains("<xmpRights:WebStatement>https://example.com/rights</xmpRights:WebStatement>"), "{xmp}");
    assert!(std::fs::read_to_string(src.join("b.xmp")).unwrap().contains("<xmpRights:Marked>False</xmpRights:Marked>"));
    let expect_a = meta(&s, a);
    drop(s);
    std::fs::remove_dir_all(&lib).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ma = meta(&s, id(&s, "a.png"));
    assert_eq!(
        (ma.copyright.as_str(), ma.copyright_status, ma.usage_terms.as_str(), ma.copyright_url.as_str()),
        (expect_a.copyright.as_str(), CopyrightStatus::Copyrighted, "Editorial use only", "https://example.com/rights")
    );
    assert_eq!(meta(&s, id(&s, "b.png")).copyright_status, CopyrightStatus::PublicDomain);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// The IPTC Content fields (issue #687): headline, description writer, category, supplemental
/// categories and subject codes are edited with `photo.setMeta` (also as comma-separated text, as
/// the metadata panel sends it), copied with `photo.copyMetadata`, written to the sidecar as XMP
/// and read back on a fresh import.
#[test]
fn iptc_content_fields_round_trip() {
    let src = temp_dir("content-src");
    let lib = temp_dir("content-lib");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (a, b) = (id(&s, "a.png"), id(&s, "b.png"));
    let meta = |s: &Session, id: PhotoId| s.catalog.photo(id).unwrap().meta.clone();
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute(
        "photo.setMeta",
        &json!({
            "headline": "Sunrise over the harbour",
            "descriptionWriter": "Jane Doe",
            "category": "CAT",
            "supplementalCategories": "Sports, Nature, Sports",
            "subjectCode": ["01001000", "02002000"],
        }),
    )
    .unwrap();
    let m = meta(&s, a);
    assert_eq!(m.headline.as_str(), "Sunrise over the harbour");
    assert_eq!(m.description_writer.as_str(), "Jane Doe");
    assert_eq!(m.category.as_str(), "CAT");
    assert_eq!(m.supplemental_categories, vec!["Sports".to_string(), "Nature".to_string()]);
    assert_eq!(m.subject_code, vec!["01001000".to_string(), "02002000".to_string()]);
    // copy → paste onto b (arrays included)
    let clip = s.execute("photo.copyMetadata", &json!({})).unwrap();
    assert_eq!(clip["headline"], "Sunrise over the harbour");
    s.execute("photo.pasteMetadata", &json!({"ids": [b.0], "fields": ["headline", "supplementalCategories"]})).unwrap();
    let mb = meta(&s, b);
    assert_eq!(
        (mb.headline.as_str(), mb.supplemental_categories.clone()),
        ("Sunrise over the harbour", vec!["Sports".to_string(), "Nature".to_string()])
    );
    assert_eq!(mb.description_writer, "", "fields outside `fields` stay");
    // sidecar: the XMP Content fields, read back by a fresh library
    s.execute("photo.saveMetadataToFile", &json!({"ids": [a.0]})).unwrap();
    let xmp = std::fs::read_to_string(src.join("a.xmp")).unwrap();
    assert!(xmp.contains("<photoshop:Headline>Sunrise over the harbour</photoshop:Headline>"), "{xmp}");
    assert!(xmp.contains("<photoshop:CaptionWriter>Jane Doe</photoshop:CaptionWriter>"), "{xmp}");
    assert!(xmp.contains("<photoshop:Category>CAT</photoshop:Category>"), "{xmp}");
    assert!(xmp.contains("<photoshop:SupplementalCategories>"), "{xmp}");
    assert!(xmp.contains("<Iptc4xmpCore:SubjectCode>"), "{xmp}");
    let expect = meta(&s, a);
    drop(s);
    std::fs::remove_dir_all(&lib).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ma = meta(&s, id(&s, "a.png"));
    assert_eq!(
        (ma.headline.as_str(), ma.description_writer.as_str(), ma.category.as_str(), ma.supplemental_categories.clone(), ma.subject_code.clone()),
        ("Sunrise over the harbour", "Jane Doe", "CAT", expect.supplemental_categories.clone(), expect.subject_code.clone())
    );
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// A sidecar another raw developer wrote: develop settings in attributes and elements, a
/// structured edit history, an unknown namespace.
const OTHER_APP_SIDECAR: &str = r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Other Toolkit">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:stEvt="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:other="http://example.com/other/1.0/"
    xmp:Rating="2"
    crs:Version="99.0"
    crs:Exposure2012="+0.65"
    crs:HasSettings="True"
    other:Secret="keep me">
   <xmpMM:History>
    <rdf:Seq>
     <rdf:li stEvt:action="derived" stEvt:parameters="converted from image/x-raw to image/png"/>
    </rdf:Seq>
   </xmpMM:History>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>128, 140</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

/// A foreign namespace may use a familiar prefix without becoming the app's own metadata.
#[test]
fn saving_preserves_foreign_namespace_prefix_collisions() {
    let src = temp_dir("namespace-src");
    let lib = temp_dir("namespace-lib");
    write_png(&src.join("shot.png"), 1);
    let foreign = r#"<rdf:Description xmlns:lc="https://example.org/vendor/" xmlns:xmp="https://example.org/vendor/"
        lc:Secret="keep-me" xmp:Rating="2"><lc:Other>also keep</lc:Other></rdf:Description>"#;
    let packet = format!(r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">{foreign}</rdf:RDF>"#);
    let path = src.join("shot.xmp");
    std::fs::write(&path, packet).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("shot.png")]})).unwrap();
    let id = only(&s);
    assert_eq!(s.catalog.photo(id).unwrap().rating, 0, "a foreign rating must not become catalog metadata");
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    for _ in 0..2 {
        let result = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
        assert_eq!(result["failed"], json!([]), "{result}");
        assert_eq!(result["backups"], json!([]), "a valid packet must merge: {result}");
        assert_eq!(result["merged"], json!([path.to_string_lossy()]), "{result}");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(foreign), "lost foreign bytes: {saved}");
        assert_eq!(lcc_meta::parse_xmp(&saved).unwrap().metadata.rating, Some(5));
    }
    drop(s);
    std::fs::remove_dir_all(src).unwrap();
    std::fs::remove_dir_all(lib).unwrap();
}

/// Issue #92: saving merges into another app's sidecar instead of replacing it.
#[test]
fn saving_merges_into_another_apps_sidecar() {
    let src = temp_dir("merge");
    let lib = temp_dir("merge-lib");
    write_png(&src.join("shot.png"), 1);
    std::fs::write(src.join("shot.xmp"), OTHER_APP_SIDECAR).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("shot.png").to_string_lossy()]})).unwrap();
    let id = only(&s);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Harbour", "keywords": ["boats"]})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 30})).unwrap();
    let r = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    assert_eq!(r["merged"][0], src.join("shot.xmp").to_string_lossy().as_ref(), "{r}");
    let out = std::fs::read_to_string(src.join("shot.xmp")).unwrap();
    // the other app's data, byte for byte
    for part in [
        "crs:Exposure2012=\"+0.65\"",
        "other:Secret=\"keep me\"",
        "   <xmpMM:History>\n    <rdf:Seq>\n     <rdf:li stEvt:action=\"derived\" stEvt:parameters=\"converted from image/x-raw to image/png\"/>\n    </rdf:Seq>\n   </xmpMM:History>",
        "   <crs:ToneCurvePV2012>\n    <rdf:Seq>\n     <rdf:li>0, 0</rdf:li>\n     <rdf:li>128, 140</rdf:li>",
    ] {
        assert!(out.contains(part), "lost {part:?}:\n{out}");
    }
    let d = lcc_meta::parse_xmp(&out).unwrap();
    assert_eq!(d.metadata.rating, Some(5), "ours replaces theirs");
    assert_eq!(d.metadata.title.as_deref(), Some("Harbour"));
    assert_eq!(d.properties.get("crs:Version"), Some(&vec!["99.0".to_string()]));
    let saved = s.catalog.photo(id).unwrap().develop.clone();
    // reading it back restores our settings (lc:settings wins over crs:)
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 0})).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().develop, saved);
    // saving again keeps one the app description and the other app's data
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let again = std::fs::read_to_string(src.join("shot.xmp")).unwrap();
    assert_eq!(again.matches("<rdf:Description").count(), 2, "{again}");
    assert!(again.contains("other:Secret=\"keep me\"") && again.contains("crs:Exposure2012=\"+0.65\""));
    assert_eq!(lcc_meta::parse_xmp(&again).unwrap().metadata.rating, Some(3));

    // a sidecar that isn't XMP is kept as a backup before it is replaced
    std::fs::write(src.join("shot.xmp"), b"<not xmp").unwrap();
    let r = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let backup = r["backups"][0]["backup"].as_str().unwrap().to_string();
    assert!(backup.contains("shot.xmp.bak-"), "{r}");
    assert_eq!(std::fs::read(&backup).unwrap(), b"<not xmp");
    assert_eq!(lcc_meta::parse_xmp(&std::fs::read_to_string(src.join("shot.xmp")).unwrap()).unwrap().metadata.rating, Some(3));
    // … and another one never overwrites the first backup
    std::fs::write(src.join("shot.xmp"), b"<still not xmp").unwrap();
    let r = s.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let second = r["backups"][0]["backup"].as_str().unwrap().to_string();
    assert_ne!(second, backup);
    assert_eq!(std::fs::read(&backup).unwrap(), b"<not xmp");
    assert_eq!(std::fs::read(&second).unwrap(), b"<still not xmp");
    // a sidecar over the parser's complexity limit (upstream #3696) holds metadata that could be read: the
    // save leaves it as it is, without a backup, rather than replacing it
    let decls: String = (0..600).map(|i| format!(" xmlns:n{i}=\"urn:n:{i}\"")).collect();
    let complex = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"{decls}><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>"
    );
    std::fs::write(src.join("shot.xmp"), &complex).unwrap();
    let before = std::fs::read_dir(&src).unwrap().count();
    let r = s.execute("photo.saveMetadataToFile", &json!({}));
    assert_eq!(std::fs::read_to_string(src.join("shot.xmp")).unwrap(), complex, "{r:?}");
    assert_eq!(std::fs::read_dir(&src).unwrap().count(), before, "no backup was made");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Issue #92: two catalogued files with one stem (`IMG_1.jpg` + `IMG_1.png`) don't share a
/// Stem-named sidecar: the first by name keeps `IMG_1.xmp`, the other writes and reads
/// `IMG_1.png.xmp`.
#[test]
fn files_sharing_a_stem_get_their_own_sidecars() {
    let src = temp_dir("stem");
    let lib = temp_dir("stem-lib");
    write_png(&src.join("IMG_1.png"), 1);
    let img = lcc_raster::Rgba8::from_fn(40, 24, |x, y| [(x * 6) as u8, (y * 9) as u8, 7, 255]);
    let jpg =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Jpeg, ..Default::default() }).unwrap();
    std::fs::write(src.join("IMG_1.jpg"), jpg).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (j, p) = (id(&s, "IMG_1.jpg"), id(&s, "IMG_1.png"));
    assert_eq!(s.sidecar_naming(j), crate::sidecar::SidecarNaming::Stem);
    assert_eq!(s.sidecar_naming(p), crate::sidecar::SidecarNaming::Full);
    s.execute("photo.rate", &json!({"ids": [j.0], "rating": 2})).unwrap();
    s.execute("photo.rate", &json!({"ids": [p.0], "rating": 5})).unwrap();
    let r = s.execute("photo.saveMetadataToFile", &json!({"ids": [j.0, p.0]})).unwrap();
    assert_eq!(r["written"].as_array().unwrap().len(), 2, "{r}");
    let rating = |f: &str| lcc_meta::parse_xmp(&std::fs::read_to_string(src.join(f)).unwrap()).unwrap().metadata.rating;
    assert_eq!(rating("IMG_1.xmp"), Some(2));
    assert_eq!(rating("IMG_1.png.xmp"), Some(5));
    // each reads its own back
    s.execute("photo.rate", &json!({"ids": [j.0, p.0], "rating": 0})).unwrap();
    s.execute("photo.readMetadataFromFile", &json!({"ids": [j.0, p.0]})).unwrap();
    assert_eq!((s.catalog.photo(j).unwrap().rating, s.catalog.photo(p).unwrap().rating), (2, 5));
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Lossy DNG export: demosaiced JPEG tiles at the resize long edge, smaller than lossless, colour tags kept.
#[test]
fn lossy_dng_export_scales_and_shrinks() {
    let src = temp_dir("lossydng");
    std::fs::write(src.join("synth.dng"), synthetic_dng_with(None, Default::default())).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let mut export = |p: serde_json::Value| {
        let o = crate::export::ExportOptions::from_json(&p);
        let mut out = Vec::new();
        let mut write = |_: &str, b: &[u8]| {
            out = b.to_vec();
            Ok(())
        };
        crate::export::export_batch(&mut s, &[id], &o, &crate::export::Destination { dir: "x".into(), exact: None }, &mut write, &|_| false).unwrap();
        out
    };
    let lossless = export(json!({"format": "dng"}));
    let lossy = export(json!({"format": "dng", "dngCompression": "lossy", "quality": 80, "longEdge": 32}));
    let raw = lcc_raw::decode(&lossy).unwrap();
    assert_eq!((raw.cpp, raw.cfa.is_none(), raw.width.max(raw.height)), (3, true, 32));
    assert_eq!(raw.color.as_shot_neutral, Some([0.5, 1.0, 0.7]));
    assert!(lossy.len() < lossless.len(), "{} vs {}", lossy.len(), lossless.len());
    let _ = std::fs::remove_dir_all(&src);
}

/// An automatic sidecar write that fails (here the folder is read-only, as a network share that dropped out or went
/// read-only) waits instead of being forgotten, and `library.retrySidecars` writes it once the folder takes writes
/// again.
#[cfg(unix)]
#[test]
fn a_failed_auto_write_waits_and_is_written_on_retry() {
    use std::os::unix::fs::PermissionsExt;
    let src = temp_dir("auto-retry");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o555)).unwrap();
    // (root writes anyway: nothing to test there)
    if std::fs::write(src.join(".probe"), b"x").is_ok() {
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let sidecar = src.join("a.xmp");
    assert!(!sidecar.exists());
    assert_eq!(s.unsaved_sidecars(), 1, "the failed write waits");
    let r = s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(r, json!({"written": 0, "waiting": 1}), "still read-only");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(r, json!({"written": 1, "waiting": 0}));
    assert!(std::fs::read_to_string(&sidecar).unwrap().contains("<xmp:Rating>4</xmp:Rating>"));
    // a later edit that writes fine leaves nothing waiting; turning auto-write off drops what still waits
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o555)).unwrap();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    assert_eq!(s.unsaved_sidecars(), 1);
    s.execute("library.xmpPreferences", &json!({"autoWrite": false})).unwrap();
    s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(s.unsaved_sidecars(), 0);
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&src);
}

/// With background sidecar writes on (the desktop app's setting), an edit returns before its sidecar is written; the
/// write lands on the writer thread, a photo changed again before its write started writes only its newest packet,
/// and a file that is about to move has its queued write made first, so nothing lands at the old name.
#[test]
fn background_sidecar_writes_land_and_never_at_a_stale_path() {
    let src = temp_dir("auto-background");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    for rating in 1..=5 {
        s.execute("photo.rate", &json!({"rating": rating})).unwrap();
    }
    s.flush_sidecar_writes();
    assert_eq!(s.sidecar_writes_pending(), 0);
    let sidecar = src.join("a.xmp");
    assert!(std::fs::read_to_string(&sidecar).unwrap().contains("<xmp:Rating>5</xmp:Rating>"), "the newest rating");
    assert_eq!(s.unsaved_sidecars(), 0);
    // an edit queued just before a rename: the write is made before the file (and its sidecar) moves
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("photo.rename", &json!({"ids": [id.0], "template": "renamed"})).unwrap();
    s.flush_sidecar_writes();
    assert!(!sidecar.exists(), "no sidecar left at the old name");
    assert!(std::fs::read_to_string(src.join("renamed.xmp")).unwrap().contains("<xmp:Rating>2</xmp:Rating>"));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&src);
}

/// A background write that fails (the folder is read-only, as a network share that went away) waits for a retry,
/// as a write made in the command does.
#[cfg(unix)]
#[test]
fn a_failed_background_write_waits_for_a_retry() {
    use std::os::unix::fs::PermissionsExt;
    let src = temp_dir("auto-background-fail");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(src.join(".probe"), b"x").is_ok() {
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    s.flush_sidecar_writes();
    assert_eq!(s.unsaved_sidecars(), 1, "the failed write waits");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(r, json!({"written": 1, "waiting": 0}));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&src);
}

/// A background write whose original can't be reached when the edit is made (a share that dropped out) waits for a
/// retry, as a failed write does: the sidecar mustn't stay behind the edit once the file is back.
#[test]
fn a_background_write_with_its_original_away_waits_for_a_retry() {
    let src = temp_dir("auto-background-away");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    s.flush_sidecar_writes();
    std::fs::rename(src.join("a.png"), src.join("away.png")).unwrap();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    s.flush_sidecar_writes();
    assert_eq!(s.unsaved_sidecars(), 1, "the write waits for the original");
    std::fs::rename(src.join("away.png"), src.join("a.png")).unwrap();
    let r = s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(r, json!({"written": 1, "waiting": 0}));
    assert!(std::fs::read_to_string(src.join("a.xmp")).unwrap().contains("<xmp:Rating>3</xmp:Rating>"));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&src);
}

/// Opening another library makes the old one's queued background writes first, and drops its waiting retries: photo
/// ids are per library, so a failure from the old one must not mark a photo of the new one.
#[cfg(unix)]
#[test]
fn opening_another_library_drains_background_writes_and_old_retries() {
    use std::os::unix::fs::PermissionsExt;
    let base = temp_dir("auto-background-switch");
    let (photos, locked) = (base.join("photos"), base.join("locked"));
    std::fs::create_dir_all(&photos).unwrap();
    std::fs::create_dir_all(&locked).unwrap();
    write_png(&photos.join("a.png"), 4);
    write_png(&locked.join("b.png"), 5);
    let mut s = Session::new().with_fs();
    // both libraries write sidecars automatically (a library that doesn't drops its retries anyway)
    s.open_library(base.join("two"), false).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.open_library(base.join("one"), false).unwrap();
    s.execute("library.import", &json!({"paths": [photos.to_string_lossy(), locked.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    s.flush_sidecar_writes();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(locked.join(".probe"), b"x").is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    s.execute("photo.rate", &json!({"ids": ids, "rating": 4})).unwrap();
    // the writes are made but not yet taken in when the other library opens
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while s.sidecar_writes_pending() > 0 && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.open_library(base.join("two"), false).unwrap();
    s.flush_sidecar_writes();
    assert!(std::fs::read_to_string(photos.join("a.xmp")).unwrap().contains("<xmp:Rating>4</xmp:Rating>"), "written before the switch");
    assert_eq!(s.unsaved_sidecars(), 0, "the old library's retries don't carry over");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&base);
}

/// A write queued while its original was there, whose original is gone by the time the writer gets to it (a share
/// that dropped out), waits for a retry instead of being dropped: the edit is in the catalog, the sidecar isn't.
#[cfg(unix)]
#[test]
fn a_queued_write_whose_original_went_away_waits_for_a_retry() {
    let src = temp_dir("auto-background-gone");
    write_png(&src.join("a.png"), 4);
    write_png(&src.join("b.png"), 5);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    s.flush_sidecar_writes();
    let id = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id.0;
    let (a, b) = (id(&s, "a.png"), id(&s, "b.png"));
    // a.png's sidecar write is held: it blocks the writer until released
    let held = src.join("a.xmp");
    let _ = std::fs::remove_file(&held);
    crate::sidecar_writer::stall::hold(&held);
    s.execute("photo.rate", &json!({"ids": [a], "rating": 1})).unwrap();
    s.execute("photo.rate", &json!({"ids": [b], "rating": 2})).unwrap();
    // b's original goes away while its write waits in the queue
    std::fs::rename(src.join("b.png"), src.join("away.png")).unwrap();
    crate::sidecar_writer::stall::release(&held);
    s.flush_sidecar_writes();
    assert_eq!(s.unsaved_sidecars(), 1, "b's write waits for its original");
    std::fs::rename(src.join("away.png"), src.join("b.png")).unwrap();
    let r = s.execute("library.retrySidecars", &json!({})).unwrap();
    assert_eq!(r, json!({"written": 1, "waiting": 0}));
    assert!(std::fs::read_to_string(src.join("b.xmp")).unwrap().contains("<xmp:Rating>2</xmp:Rating>"));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&src);
}

/// While a background sidecar write is stuck (a share that stopped answering; here a held write), the file
/// operations it could race refuse with an error that says so, instead of going ahead after the wait: a rename, a
/// sidecar read or save on request, a change of the sidecar preferences. Once the write lands they work again.
#[cfg(unix)]
#[test]
fn file_operations_refuse_while_a_sidecar_write_is_stuck() {
    let src = temp_dir("auto-background-stuck");
    write_png(&src.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    s.flush_sidecar_writes();
    s.set_sidecar_flush_limit(std::time::Duration::from_millis(100));
    let held = src.join("a.xmp");
    let _ = std::fs::remove_file(&held);
    crate::sidecar_writer::stall::hold(&held);
    let id = s.catalog.photos().next().unwrap().id.0;
    s.execute("photo.rate", &json!({"ids": [id], "rating": 3})).unwrap();
    for (command, params) in [
        ("photo.rename", json!({"ids": [id], "template": "renamed"})),
        ("photo.readMetadataFromFile", json!({"ids": [id]})),
        ("photo.saveMetadataToFile", json!({"ids": [id]})),
        ("library.xmpPreferences", json!({"naming": "full"})),
    ] {
        let e = s.execute(command, &params).expect_err(command).to_string();
        assert!(e.contains("still running") && e.contains(&src.to_string_lossy().to_string()), "{command}: {e}");
    }
    assert!(src.join("a.png").exists(), "nothing was renamed");
    // the share answers: the write lands and the rename goes ahead
    crate::sidecar_writer::stall::release(&held);
    s.set_sidecar_flush_limit(std::time::Duration::from_secs(10));
    s.execute("photo.rename", &json!({"ids": [id], "template": "renamed"})).unwrap();
    assert!(std::fs::read_to_string(src.join("renamed.xmp")).unwrap().contains("<xmp:Rating>3</xmp:Rating>"));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&src);
}

/// Opening another library while a background sidecar write of this one is stuck (a share that stopped answering;
/// here a held write) refuses, as the file operations do: the write's result belongs to this library's
/// photos, so switching would lose it or pin it on a photo of the next library. Once the write lands it opens.
#[cfg(unix)]
#[test]
fn opening_another_library_refuses_while_a_sidecar_write_is_stuck() {
    let base = temp_dir("auto-background-switch-stuck");
    let photos = base.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    write_png(&photos.join("a.png"), 4);
    let mut s = Session::new().with_fs();
    s.open_library(base.join("one"), false).unwrap();
    s.execute("library.import", &json!({"paths": [photos.to_string_lossy()]})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.set_background_sidecar_writes(true);
    s.flush_sidecar_writes();
    s.set_sidecar_flush_limit(std::time::Duration::from_millis(100));
    let held = photos.join("a.xmp");
    let _ = std::fs::remove_file(&held);
    crate::sidecar_writer::stall::hold(&held);
    let id = s.catalog.photos().next().unwrap().id;
    let smart_dir = s.media.smart_dir.clone();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 5})).unwrap();
    let e = s.open_library(base.join("two"), false).map(|_| ()).expect_err("refused while the write is stuck").to_string();
    assert!(e.contains("still running"), "{e}");
    assert!(s.catalog.photo(id).is_some(), "still the first library");
    assert_eq!(s.media.smart_dir, smart_dir, "and still its smart previews");
    crate::sidecar_writer::stall::release(&held);
    s.set_sidecar_flush_limit(std::time::Duration::from_secs(10));
    s.open_library(base.join("two"), false).unwrap();
    assert!(std::fs::read_to_string(photos.join("a.xmp")).unwrap().contains("<xmp:Rating>5</xmp:Rating>"));
    s.set_background_sidecar_writes(false);
    let _ = std::fs::remove_dir_all(&base);
}

/// Upstream #2993: a JPEG carrying its develop settings in embedded `crs:` XMP, with no sidecar, imports with them;
/// settings marked as already applied to the pixels are not applied again; a sidecar wins over the embedded packet.
#[test]
fn embedded_jpeg_develop_settings_apply_on_import() {
    let packet = |extra: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
        xmp:Rating="3" crs:HasSettings="True" crs:Contrast2012="+30" {extra}/>
      </rdf:RDF></x:xmpmeta>"#
        )
    };
    let src = temp_dir("jpeg-xmp-import-develop");
    write_jpeg_xmp(&src.join("edited.jpg"), &packet(""));
    write_jpeg_xmp(&src.join("rendered.jpg"), &packet(r#"crs:AlreadyApplied="True""#));
    write_jpeg_xmp(&src.join("both.jpg"), &packet(r#"xmp:Label="Red""#));
    std::fs::write(src.join("both.xmp"), packet("").replace(r#"crs:Contrast2012="+30""#, r#"crs:Contrast2012="-20""#)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 3, "{r}");
    let by_name = |name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().clone();
    assert_eq!(by_name("edited.jpg").develop.light.contrast, 30.0, "the embedded settings apply");
    assert_eq!(by_name("rendered.jpg").develop.light.contrast, 0.0, "already in the pixels");
    assert_eq!(by_name("both.jpg").develop.light.contrast, -20.0, "the sidecar wins over the embedded packet");
    let _ = std::fs::remove_dir_all(&src);
}

/// All Metadata shows what the catalog holds, and still answers when the original can't be read.
#[test]
fn all_metadata_gives_the_catalog_values_even_without_the_file() {
    let mut s = Session::with_demo();
    s.execute("photo.setMeta", &json!({"title": "A title", "city": "Paris", "addKeywords": ["alps"]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let rows =
        |r: &serde_json::Value| r["catalog"].as_array().unwrap().iter().map(|x| (x["name"].to_string(), x["value"].to_string())).collect::<Vec<_>>();
    let r = s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    assert!(rows(&r).contains(&("\"Title\"".into(), "\"A title\"".into())) && rows(&r).iter().any(|(n, _)| n == "\"Keywords\""), "{r}");
    let gone = lcc_catalog::Source::File { path: "/definitely/not/there.jpg".into() };
    s.catalog.apply(lcc_catalog::Op::SetFile { id, file_name: "there.jpg".into(), source: gone }).unwrap();
    let r = s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    assert!(r["note"].as_str().is_some_and(|n| n.contains("can't be read")), "{r}");
    assert!(rows(&r).iter().any(|(n, v)| n == "\"City\"" && v == "\"Paris\""), "{r}");
}
