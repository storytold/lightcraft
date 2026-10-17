//! Lightroom's saved Upright matrices (issue #205): which slot each mode reads, what is refused,
//! the conversion into LightCraft's frame, the geometry against measurements of Lightroom's own
//! exports, and the import, re-read and save paths.
//!
//! The matrices below come from public Lightroom Classic 12 sidecars of public raws
//! (github.com/zjqc/photographing-art, MIT): numbers only, no image data.

use std::path::Path;

use lightcraft_develop::{DevelopSettings, Upright};
use lightcraft_geom::{Homography, Orientation, Point};
use serde_json::{Value, json};

use crate::Session;
use crate::crs::{Props, SavedUpright, Target, saved_upright, to_partial, upright_in_frame, upright_mode};
use crate::tests_xmp::temp_dir;

const IDENTITY: &str = "1.000000000,0.000000000,0.000000000,0.000000000,1.000000000,0.000000000,0.000000000,0.000000000,1.000000000";

/// The six matrices Lightroom saved for DSC_1457.NEF (slots 0 Off, 1 Auto, 2 Full, 3 Level,
/// 4 Vertical, 5 Guided).
const SLOTS: [&str; 6] = [
    IDENTITY,
    "1.010477639,-0.008540717,-0.006124940,0.014938050,1.011645918,-0.015359712,-0.007190889,-0.003396748,1.000000000",
    "1.000410884,-0.014998297,0.000328989,0.012818795,1.012962929,-0.022290590,-0.000090999,-0.018590990,1.000000000",
    "1.015108937,-0.006862808,-0.004123065,0.015441320,1.015108937,-0.015275128,0.000000000,0.000000000,1.000000000",
    "1.000505332,-0.014999682,0.000327389,0.012654172,1.013013074,-0.022230588,-0.000000003,-0.018592354,1.000000000",
    IDENTITY,
];

const ORIENTATIONS: [Orientation; 8] = [
    Orientation::Normal,
    Orientation::FlipH,
    Orientation::Rotate180,
    Orientation::FlipV,
    Orientation::Transpose,
    Orientation::Rotate90,
    Orientation::Transverse,
    Orientation::Rotate270,
];

/// An XMP packet in Lightroom's form: `fields` (attribute text) plus the six saved slots.
fn packet(fields: &str) -> String {
    let slots: String = SLOTS.iter().enumerate().map(|(i, m)| format!(r#" crs:UprightTransform_{i}="{m}""#)).collect();
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
        crs:Exposure2012="+0.30" crs:UprightCenterMode="0" crs:UprightCenterNormX="0.5" crs:UprightCenterNormY="0.5"
        crs:UprightFocalMode="0" crs:UprightFocalLength35mm="267.885" crs:UprightPreview="False"
        crs:UprightFourSegmentsCount="0" {fields}{slots}/>
    </rdf:RDF></x:xmpmeta>"#
    )
}

fn lightroom(mode: u8) -> String {
    packet(&format!(r#"crs:PerspectiveUpright="{mode}" crs:UprightVersion="151388160" crs:UprightTransformCount="6""#))
}

fn props(x: &str) -> Props {
    lightcraft_meta::parse_xmp(x).unwrap().properties
}

fn matrix(text: &str) -> [f64; 9] {
    let v: Vec<f64> = text.split(',').map(|t| t.trim().parse().unwrap()).collect();
    v.try_into().unwrap()
}

fn apply(m: &[f64; 9], p: Point) -> Point {
    Homography(*m).apply(p)
}

#[test]
fn perspective_upright_values_name_lightroom_modes() {
    let expect = [Upright::Off, Upright::Auto, Upright::Full, Upright::Level, Upright::Vertical, Upright::Guided];
    for (value, mode) in expect.into_iter().enumerate() {
        assert_eq!(upright_mode(value as f64), Some(mode));
    }
    for odd in [6.0, -1.0, 1.5, f64::NAN, f64::INFINITY] {
        assert_eq!(upright_mode(odd), None, "{odd}");
    }
    // a stated mode never keeps a transform solved for another one
    for (value, name) in [(2, "full"), (3, "level"), (4, "vertical")] {
        let partial = to_partial(&props(&lightroom(value)), Target::Any);
        assert_eq!(partial["geometry"]["upright"], name, "{value}");
        assert!(partial["geometry"]["upright_transform"].is_null(), "{partial}");
        let mut before = DevelopSettings::default();
        before.geometry.upright_transform = Some(Homography([1.1, 0.0, 0.0, 0.0, 1.1, 0.0, 0.0, 0.0, 1.0]));
        assert_eq!(lightcraft_develop::apply_partial(&before, &partial, 1.0).geometry.upright_transform, None);
    }
}

#[test]
fn each_mode_reads_its_own_slot() {
    for (value, mode) in [(1, Upright::Auto), (2, Upright::Full), (3, Upright::Level), (4, Upright::Vertical)] {
        let saved = saved_upright(&props(&lightroom(value))).unwrap();
        assert_eq!(saved, SavedUpright { mode, matrix: matrix(SLOTS[value as usize]) });
    }
    // Off has nothing to apply; Guided's slot holds no result (Lightroom solves it from the guides)
    assert_eq!(saved_upright(&props(&lightroom(0))), None);
    assert_eq!(saved_upright(&props(&lightroom(5))), None);
    // the shapes that identify the slots: Level is a rotation about the centre, Vertical has no
    // horizontal keystone, Full has both
    let level = matrix(SLOTS[3]);
    assert_eq!((level[6], level[7]), (0.0, 0.0));
    assert!(apply(&level, Point::new(0.5, 0.5)).dist(Point::new(0.5, 0.5)) < 1e-8);
    assert!(matrix(SLOTS[4])[6].abs() < 1e-6 && matrix(SLOTS[4])[7].abs() > 1e-3);
    assert!(matrix(SLOTS[2])[6].abs() > 1e-5 && matrix(SLOTS[2])[7].abs() > 1e-3);
}

#[test]
fn saved_matrices_lightcraft_cannot_apply_are_refused() {
    let base = lightroom(1);
    let auto = SLOTS[1];
    let refused = |what: &str, xmp: String| assert_eq!(saved_upright(&props(&xmp)), None, "{what}");
    refused("no version", base.replace(r#" crs:UprightVersion="151388160""#, ""));
    refused("another version", base.replace("151388160", "151388161"));
    refused("a version that isn't a number", base.replace("151388160", "9.6"));
    refused("no count", base.replace(r#" crs:UprightTransformCount="6""#, ""));
    refused("a count without the slot", base.replace(r#"crs:UprightTransformCount="6""#, r#"crs:UprightTransformCount="1""#));
    refused("a count that isn't a number", base.replace(r#"crs:UprightTransformCount="6""#, r#"crs:UprightTransformCount="six""#));
    refused("a negative count", base.replace(r#"crs:UprightTransformCount="6""#, r#"crs:UprightTransformCount="-6""#));
    refused("no matrix for the mode", base.replace(&format!(r#" crs:UprightTransform_1="{auto}""#), ""));
    refused("an empty matrix", base.replace(auto, ""));
    refused("an unknown mode", base.replace(r#"crs:PerspectiveUpright="1""#, r#"crs:PerspectiveUpright="9""#));
    refused("a fractional mode", base.replace(r#"crs:PerspectiveUpright="1""#, r#"crs:PerspectiveUpright="1.5""#));
    for (what, m) in [
        ("eight numbers", "1,0,0,0,1,0,0,0"),
        ("ten numbers", "1,0,0,0,1,0,0,0,1,0"),
        ("text", "1,0,0,0,one,0,0,0,1"),
        ("NaN", "1,0,0,0,NaN,0,0,0,1"),
        ("infinity", "1,0,0,0,inf,0,0,0,1"),
        ("an overflow", "1,0,0,0,1e400,0,0,0,1"),
        ("a zero last element", "1,0,0,0,1,0,0,0,0"),
        ("a huge element", "1,0,250,0,1,0,0,0,1"),
        ("the horizon inside the photo", "1,0,0,0,1,0,-2,0,1"),
        ("a mirror", "-1,0,1,0,1,0,0,0,1"),
        ("a collapse", "1,1,0,1,1,0,0,0,1"),
        ("separators only", ",,,,,,,,"),
    ] {
        refused(what, base.replace(auto, m));
    }
    // a matrix scaled by its last element is the same correction
    let scaled: Vec<String> = matrix(auto).iter().map(|v| format!("{}", v * 2.0)).collect();
    let saved = saved_upright(&props(&base.replace(auto, &scaled.join(", ")))).unwrap();
    assert!(saved.matrix.iter().zip(matrix(auto)).all(|(a, b)| (a - b).abs() < 1e-12), "{saved:?}");
}

/// A deterministic stream of hostile field values: none may panic, and whatever is accepted maps
/// to finite coordinates in every orientation.
#[test]
fn hostile_upright_fields_never_panic() {
    let pieces = ["1", "0", "-0", "1e308", "-1e308", "1e-320", "NaN", "inf", "", " ", ",", "0.5", "2", "-3", "x", "1e400", "١"];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..4000 {
        let n = (next() % 12) as usize;
        let m: Vec<&str> = (0..n).map(|_| pieces[(next() % pieces.len() as u64) as usize]).collect();
        let mode = ["1", "2", "3", "4", "0", "5", "-1", "1e9", "x"][(next() % 9) as usize];
        let count = ["6", "0", "5", "-1", "18446744073709551616", "x"][(next() % 6) as usize];
        let version = ["151388160", "0", "x", "-151388160"][(next() % 4) as usize];
        let xmp = packet(&format!(r#"crs:PerspectiveUpright="{mode}" crs:UprightVersion="{version}" crs:UprightTransformCount="{count}""#))
            .replace(SLOTS[1], &m.join(","))
            .replace(SLOTS[3], &m.join(","));
        let Ok(d) = lightcraft_meta::parse_xmp(&xmp) else { continue };
        if let Some(saved) = saved_upright(&d.properties) {
            for o in ORIENTATIONS {
                for (w, h) in [(3.0, 2.0), (1.0, 1e-300), (1e300, 1.0), (f64::NAN, 1.0), (0.0, 1.0)] {
                    if let Some(t) = upright_in_frame(&saved, o, w, h) {
                        assert!(t.0.iter().all(|v| v.is_finite()), "{saved:?} {o:?}");
                    }
                }
            }
        }
    }
}

/// Lightroom's own reading of a matrix (stored, normalized coordinates) and LightCraft's reading of
/// the converted one (oriented, centred coordinates) move every point of the photo to the same
/// place, in every orientation; converting back gives Lightroom's matrix.
#[test]
fn frame_conversion_agrees_with_lightroom_point_for_point() {
    let (sw, sh) = (6000.0, 4000.0);
    for slot in [1, 2, 3, 4] {
        let saved = SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[slot]) };
        for o in ORIENTATIONS {
            let h = upright_in_frame(&saved, o, sw, sh).unwrap();
            let (ow, oh) = if o.swaps_axes() { (sh, sw) } else { (sw, sh) };
            let l = ow.max(oh) / 2.0;
            let oriented = |p: Point| {
                let (x, y) = o.map(p.x * sw, p.y * sh, sw, sh);
                Point::new(x, y)
            };
            for i in 0..=10 {
                for j in 0..=10 {
                    let p = Point::new(i as f64 / 10.0, j as f64 / 10.0);
                    let want = oriented(apply(&saved.matrix, p));
                    let a = oriented(p);
                    let c = h.apply(Point::new((a.x - ow / 2.0) / l, (a.y - oh / 2.0) / l));
                    let got = Point::new(c.x * l + ow / 2.0, c.y * l + oh / 2.0);
                    assert!(got.dist(want) < 1e-6, "slot {slot} {o:?} {p:?}: {got:?} vs {want:?}");
                }
            }
            // and back: LightCraft's homography read in Lightroom's frame is the saved matrix
            let corners = [Point::new(0.0, 0.0), Point::new(1.0, 0.0), Point::new(1.0, 1.0), Point::new(0.0, 1.0)];
            let moved = corners.map(|p| {
                let a = oriented(p);
                let c = h.apply(Point::new((a.x - ow / 2.0) / l, (a.y - oh / 2.0) / l));
                stored_from_oriented(o, Point::new(c.x * l + ow / 2.0, c.y * l + oh / 2.0), sw, sh)
            });
            let back = Homography::from_quads(&corners, &moved).unwrap();
            assert!(back.0.iter().zip(saved.matrix).all(|(a, b)| (a - b).abs() < 1e-9), "slot {slot} {o:?}: {back:?}");
        }
    }
    // identity stays identity, in every orientation and frame
    let identity = SavedUpright { mode: Upright::Auto, matrix: matrix(IDENTITY) };
    for o in ORIENTATIONS {
        for (w, h) in [(6000.0, 4000.0), (40.0, 24.0), (1.0, 1.0), (24.0, 40.0)] {
            let t = upright_in_frame(&identity, o, w, h).unwrap();
            assert!(t.0.iter().zip(Homography::IDENTITY.0).all(|(a, b)| (a - b).abs() < 1e-12), "{o:?} {t:?}");
        }
    }
    for (w, h) in [(0.0, 1.0), (1.0, -1.0), (f64::NAN, 1.0), (1.0, f64::INFINITY)] {
        assert_eq!(upright_in_frame(&identity, Orientation::Normal, w, h), None);
    }
}

/// Oriented px (an oriented image of the stored `sw × sh`) → normalized stored coordinates.
fn stored_from_oriented(o: Orientation, p: Point, sw: f64, sh: f64) -> Point {
    // invert `Orientation::map` from three of its points (it is affine)
    let at = |x: f64, y: f64| {
        let (u, v) = o.map(x, y, 1.0, 1.0);
        Point::new(u, v)
    };
    let (z, ex, ey) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0));
    let fwd = Homography([ex.x - z.x, ey.x - z.x, z.x, ex.y - z.y, ey.y - z.y, z.y, 0.0, 0.0, 1.0]);
    let (ow, oh) = if o.swaps_axes() { (sh, sw) } else { (sw, sh) };
    fwd.inverse().unwrap().apply(Point::new(p.x / ow, p.y / oh))
}

/// Lightroom's Level matrix is a rotation about the centre of the stored image: in LightCraft's
/// isotropic coordinates it is a rotation about the origin, of the same angle.
#[test]
fn level_matrix_is_a_rotation_in_lightcraft_coordinates() {
    let saved = SavedUpright { mode: Upright::Level, matrix: matrix(SLOTS[3]) };
    let h = upright_in_frame(&saved, Orientation::Normal, 4800.0, 3200.0).unwrap().0;
    // (the saved matrix has nine decimals: its centre stays put to about 1e-9)
    assert!(h[2].abs() < 1e-8 && h[5].abs() < 1e-8 && h[6].abs() < 1e-12 && h[7].abs() < 1e-12, "{h:?}");
    assert!((h[0] - h[4]).abs() < 1e-9 && (h[1] + h[3]).abs() < 2e-6, "{h:?}");
    let degrees = h[3].atan2(h[0]).to_degrees();
    assert!((degrees - 0.581).abs() < 0.001, "{degrees}");
}

/// One Lightroom export of a public raw with Upright Auto, measured (SIFT matches between
/// LightCraft's render without geometry and Lightroom's export, RANSAC, then least squares on
/// the inliers): the fitted map from LightCraft's base pixels to export pixels.
struct Reference {
    name: &'static str,
    /// `crs:UprightTransform_1` of the export's own packet.
    auto: &'static str,
    orientation: Orientation,
    /// Lightroom's stored image (its default crop), before orientation.
    stored: (f64, f64),
    /// Where Lightroom's oriented image starts inside LightCraft's (LightCraft decodes these NEFs
    /// a little larger), in oriented pixels; measured on Upright Off exports to 0.5 px.
    inset: (f64, f64),
    crop: [f64; 4],
    export: (usize, usize),
    /// Base px → export px (OpenCV pixel coordinates: centres at whole numbers).
    fit: [f64; 9],
}

const REFERENCES: [Reference; 3] = [
    Reference {
        name: "DSC_1457 (landscape)",
        auto: SLOTS[1],
        orientation: Orientation::Normal,
        stored: (4800.0, 3200.0),
        inset: (6.0, 6.0),
        crop: [0.0, 0.0, 1.0, 1.0],
        export: (4800, 3200),
        fit: [
            1.0103827566515864,
            -0.012910769838480735,
            -35.18705406883105,
            0.009940071369263049,
            1.0117804655183038,
            -55.14972108764364,
            -1.5048134911065293e-06,
            -1.0989184648032297e-06,
            1.0,
        ],
    },
    Reference {
        name: "DSC_1316 (portrait, EXIF orientation 8)",
        auto: "1.014491182,0.006583939,-0.010537101,-0.014802884,1.014513584,0.000149262,0.000001442,0.000009357,0.999998558",
        orientation: Orientation::Rotate270,
        stored: (6000.0, 4000.0),
        inset: (10.0, 18.0),
        crop: [0.0, 0.0, 1.0, 1.0],
        export: (4000, 6000),
        fit: [
            1.0145773365814015,
            0.00989331650336279,
            -69.00371740408151,
            -0.009872550000906167,
            1.0145972704719037,
            -42.124240481845156,
            -1.2235571140649089e-09,
            1.328135644733238e-08,
            1.0,
        ],
    },
    Reference {
        name: "DSC_2051 (cropped, Constrain Crop on)",
        auto: "1.015255953,0.006622517,-0.010961914,-0.016162078,1.014735275,0.000170720,0.000065470,-0.001142520,1.000000000",
        orientation: Orientation::Normal,
        stored: (4800.0, 3200.0),
        inset: (6.0, 6.0),
        crop: [0.127635, 0.195079, 0.85347, 0.920914],
        export: (3484, 2323),
        fit: [
            1.0154458088182998,
            0.010299904328396522,
            -672.1052981190101,
            -0.01076867585799946,
            1.0153630727100376,
            -629.6318219157675,
            1.638771482679291e-08,
            -2.8365852308384464e-07,
            1.0,
        ],
    },
];

/// LightCraft's render mapping (`Frame`: crop, perspective) with the imported matrix agrees with
/// Lightroom's exports to about a pixel at full resolution; without it, by tens of pixels.
#[test]
fn imported_upright_renders_like_lightroom_exports() {
    for r in &REFERENCES {
        let (ow, oh) = if r.orientation.swaps_axes() { (r.stored.1, r.stored.0) } else { r.stored };
        let saved = SavedUpright { mode: Upright::Auto, matrix: matrix(r.auto) };
        let error = |transform: Option<Homography>| {
            let mut s = DevelopSettings::default();
            s.geometry.upright = Upright::Auto;
            s.geometry.upright_transform = transform;
            let [x0, y0, x1, y1] = r.crop;
            s.crop.geometry.rect = lightcraft_geom::Rect { x0, y0, x1, y1 };
            let f = lightcraft_pipeline::geometry::Frame::new(ow as usize, oh as usize, &s, true);
            let to_oriented = f.out_to_oriented(r.export.0, r.export.1);
            let fit = Homography(r.fit).inverse().unwrap();
            let (mut worst, mut sum, mut n) = (0.0f64, 0.0, 0.0);
            for i in 0..9 {
                for j in 0..9 {
                    let e = Point::new((0.05 + 0.1125 * i as f64) * r.export.0 as f64, (0.05 + 0.1125 * j as f64) * r.export.1 as f64);
                    // continuous coordinates (pixel centres at +0.5) through LightCraft's frame
                    let t = to_oriented.apply(Point::new(e.x + 0.5, e.y + 0.5));
                    let src = f.warp.as_ref().map_or(t, |w| w.to_corrected(t));
                    let predicted = Point::new(src.x - 0.5 + r.inset.0, src.y - 0.5 + r.inset.1);
                    let d = predicted.dist(fit.apply(e));
                    worst = worst.max(d);
                    sum += d;
                    n += 1.0;
                }
            }
            (sum / n, worst)
        };
        let (mean, worst) = error(upright_in_frame(&saved, r.orientation, r.stored.0, r.stored.1));
        assert!(mean < 0.75 && worst < 1.25, "{}: mean {mean:.3} px, worst {worst:.3} px", r.name);
        let (mean_without, _) = error(None);
        assert!(mean_without > 15.0, "{}: {mean_without:.1} px without the saved matrix", r.name);
    }
}

fn write_png(path: &Path) {
    let (w, h) = (40usize, 24usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 9) as u8, 90, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A 40 × 24 JPEG stored on its side: EXIF orientation 6 shows it 24 × 40.
fn write_turned_jpeg(path: &Path) {
    write_jpeg(path, 40, 24, 6);
}

/// A `w × h` JPEG (stored size) with EXIF orientation `exif`.
fn write_jpeg(path: &Path, w: usize, h: usize, exif: u16) {
    write_jpeg_seeded(path, w, h, exif, 30);
}

/// [`write_jpeg`] with its own pixels (`seed`), so two of them aren't duplicates.
fn write_jpeg_seeded(path: &Path, w: usize, h: usize, exif: u16, seed: u8) {
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 9) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let exif = lightcraft_codecs::exif::minimal_exif(exif);
    let meta = lightcraft_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() };
    let bytes =
        lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, lightcraft_codecs::ChromaSubsampling::S444, &meta).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn upright_warnings(report: &Value) -> Vec<Value> {
    report["warnings"].as_array().map(|w| w.iter().filter(|w| w["code"] == "uprightGeometry").cloned().collect()).unwrap_or_default()
}

fn geometry(s: &Session) -> lightcraft_develop::Geometry {
    s.catalog.photos().next().unwrap().develop.geometry.clone()
}

fn placed(slot: usize, o: Orientation, w: f64, h: f64) -> Homography {
    upright_in_frame(&SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[slot]) }, o, w, h).unwrap()
}

#[test]
fn import_keeps_lightroom_upright_and_rereads_follow_the_sidecar() {
    let src = temp_dir("upright-import");
    let photo = src.join("tower.png");
    write_png(&photo);
    let sidecar = src.join("tower.xmp");
    std::fs::write(&sidecar, lightroom(1)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert!(upright_warnings(&r).is_empty(), "{r}");
    // the analysis record is not an adjustment that was lost
    let warnings = r["warnings"].as_array().cloned().unwrap_or_default();
    let mut unmapped = warnings.iter().filter(|w| w["code"] == "unmappedXmp").flat_map(|w| w["fields"].as_array().cloned().unwrap_or_default());
    assert!(unmapped.all(|f| !f.as_str().unwrap_or_default().starts_with("Upright")), "{r}");
    let g = geometry(&s);
    assert_eq!(g.upright, Upright::Auto);
    assert_eq!(g.upright_transform, Some(placed(1, Orientation::Normal, 40.0, 24.0)));
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();

    // re-reading follows a changed mode to its own slot
    std::fs::write(&sidecar, lightroom(4)).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert!(upright_warnings(&reread).is_empty(), "{reread}");
    let g = geometry(&s);
    assert_eq!((g.upright, g.upright_transform), (Upright::Vertical, Some(placed(4, Orientation::Normal, 40.0, 24.0))));

    // Guided: Lightroom keeps no result to import, so it is solved here and says so
    std::fs::write(&sidecar, lightroom(5)).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(upright_warnings(&reread).first().map(|w| w["mode"].clone()), Some(json!("guided")), "{reread}");
    assert_eq!((geometry(&s).upright, geometry(&s).upright_transform), (Upright::Guided, None));

    // an Upright version whose convention is unknown: solved again, with the warning
    std::fs::write(&sidecar, lightroom(3).replace("151388160", "151388161")).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(upright_warnings(&reread).first().map(|w| w["mode"].clone()), Some(json!("level")), "{reread}");
    assert_eq!((geometry(&s).upright, geometry(&s).upright_transform), (Upright::Level, None));

    // Upright Off: Lightroom applies none of its saved matrices, so nothing is lost
    std::fs::write(&sidecar, lightroom(0)).unwrap();
    let reread = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert!(upright_warnings(&reread).is_empty(), "{reread}");
    assert_eq!((geometry(&s).upright, geometry(&s).upright_transform), (Upright::Off, None));
    let _ = std::fs::remove_dir_all(&src);
}

/// The matrix is in the stored image's frame: a photo shown on its side gets it turned with it.
#[test]
fn import_turns_lightroom_upright_with_the_file_orientation() {
    let src = temp_dir("upright-turned");
    let photo = src.join("turned.jpg");
    write_turned_jpeg(&photo);
    std::fs::write(src.join("turned.xmp"), lightroom(1)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert!(upright_warnings(&r).is_empty(), "{r}");
    let p = s.catalog.photos().next().unwrap().clone();
    assert_eq!((p.width, p.height), (24, 40));
    let turned = placed(1, Orientation::Rotate90, 40.0, 24.0);
    assert_eq!(p.develop.geometry.upright_transform, Some(turned));
    assert_ne!(Some(turned), Some(placed(1, Orientation::Normal, 24.0, 40.0)));
    let _ = std::fs::remove_dir_all(&src);
}

/// LightCraft's own sidecar keeps the imported correction exactly (round trip through
/// `lc:settings`), and another library importing it gets the same geometry.
#[test]
fn imported_upright_round_trips_through_lightcraft_sidecars() {
    let src = temp_dir("upright-roundtrip");
    let photo = src.join("hall.png");
    write_png(&photo);
    std::fs::write(src.join("hall.xmp"), lightroom(2)).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    let first = geometry(&s);
    assert_eq!((first.upright, first.upright_transform), (Upright::Full, Some(placed(2, Orientation::Normal, 40.0, 24.0))));
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let saved = std::fs::read_to_string(src.join("hall.xmp")).unwrap();
    assert!(saved.contains("crs:UprightTransform_2"), "Lightroom's fields are kept byte for byte");
    let mut again = Session::new().with_fs();
    let r = again.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert!(upright_warnings(&r).is_empty(), "{r}");
    assert_eq!(geometry(&again), first);
    let _ = std::fs::remove_dir_all(&src);
}

/// Lightroom's matrix placed in the frame the file's orientation gives, then turned with the
/// photo's own Rotate Left / Right, is the matrix placed in the composed frame: for every pair.
/// Not turning it would leave DSC_1457's correction tens of pixels off after a Rotate Right.
#[test]
fn a_kept_upright_turns_with_the_photo_orientation() {
    let saved = SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[1]) };
    let (sw, sh) = (4800.0, 3200.0);
    for file in ORIENTATIONS {
        for user in ORIENTATIONS {
            let turned = lightcraft_pipeline::upright::turned(&upright_in_frame(&saved, file, sw, sh).unwrap(), user);
            let direct = upright_in_frame(&saved, file.then(user), sw, sh).unwrap();
            assert!(turned.0.iter().zip(direct.0).all(|(a, b)| (a - b).abs() < 1e-12), "{file:?} then {user:?}");
        }
    }
    // the unturned matrix after Rotate Right, in pixels of the 3200 × 4800 frame
    let (right, stale) =
        (upright_in_frame(&saved, Orientation::Rotate90, sw, sh).unwrap(), upright_in_frame(&saved, Orientation::Normal, sw, sh).unwrap());
    let worst = (0..=8)
        .flat_map(|i| (0..=8).map(move |j| Point::new(i as f64 / 8.0 * 2.0 / 3.0 * 2.0 - 2.0 / 3.0, j as f64 / 8.0 * 2.0 - 1.0)))
        .map(|c| right.apply(c).dist(stale.apply(c)) * 2400.0)
        .fold(0.0, f64::max);
    assert!(worst > 15.0, "{worst:.1} px");
}

/// Rotate Right, then Read Metadata from File; and Read Metadata, then Rotate Right: the same
/// correction, in the turned frame. Two more turns bring it back.
#[test]
fn rotating_and_rereading_keep_lightroom_upright_in_the_photo_frame() {
    let src = temp_dir("upright-rotate");
    let photo = src.join("arch.png");
    write_png(&photo);
    std::fs::write(src.join("arch.xmp"), lightroom(1)).unwrap();
    let want = placed(1, Orientation::Rotate90, 40.0, 24.0);
    let close = |a: Option<Homography>, b: Homography| a.is_some_and(|a| a.0.iter().zip(b.0).all(|(x, y)| (x - y).abs() < 1e-12));
    for read_first in [false, true] {
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        if read_first {
            s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
            s.execute("photo.rotateRight", &json!({})).unwrap();
        } else {
            s.execute("photo.rotateRight", &json!({})).unwrap();
            let r = s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
            assert!(upright_warnings(&r).is_empty(), "{r}");
        }
        let g = geometry(&s);
        assert!(close(g.upright_transform, want), "read first: {read_first}: {g:?}");
        s.execute("photo.rotateLeft", &json!({})).unwrap();
        assert!(close(geometry(&s).upright_transform, placed(1, Orientation::Normal, 40.0, 24.0)), "back");
        s.execute("photo.flipHorizontal", &json!({})).unwrap();
        assert!(close(geometry(&s).upright_transform, placed(1, Orientation::Normal, 40.0, 24.0)), "a flip of the output leaves it");
    }
    let _ = std::fs::remove_dir_all(&src);
}

fn crop_packet(fields: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:HasCrop="True" {fields}/>
    </rdf:RDF></x:xmpmeta>"#
    )
}

fn crop_of(s: &Session) -> lightcraft_geom::CropGeometry {
    s.catalog.photos().next().unwrap().develop.crop.geometry
}

/// DSC_1486.NEF (zjqc/photographing-art) is stored 3:2 and shown in portrait (EXIF 8); its
/// Lightroom crop cuts the stored image's left 20.7 %, which is the bottom of the portrait, and
/// Lightroom exports 3200 × 3808. The sidecar crop used to cut the portrait's left instead.
#[test]
fn a_sidecar_crop_on_a_turned_photo_lands_where_lightroom_puts_it() {
    let src = temp_dir("crop-portrait");
    let photo = src.join("portrait.jpg");
    write_jpeg(&photo, 60, 40, 8);
    let sidecar = src.join("portrait.xmp");
    std::fs::write(&sidecar, crop_packet(r#"crs:CropLeft="0.206655" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#))
        .unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    let check = |s: &Session, what: &str| {
        let g = crop_of(s);
        let r = [g.rect.x0, g.rect.y0, g.rect.x1, g.rect.y1].map(|v| (v * 1e6).round() / 1e6);
        assert_eq!(r, [0.0, 0.0, 1.0, 0.793345], "{what}");
        let (w, h) = s.catalog.photos().next().unwrap().shown_size();
        assert!((w / h - 3200.0 / 3808.0).abs() < 1e-3, "{what}: {w} × {h}");
    };
    check(&s, "import");
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    check(&s, "read metadata");
    let _ = std::fs::remove_dir_all(&src);
}

/// Straightened crops from a real Lightroom Classic catalog (stored 9504 × 6336, the sizes of the
/// JPEGs Lightroom exported from it, the same numbers as the catalog import's test): the same
/// fields in a sidecar give the same frame, upright and on its side.
#[test]
fn a_straightened_sidecar_crop_matches_lightroom_exports() {
    for (fields, exif, export) in [
        (
            r#"crs:CropAngle="-9.34444" crs:CropBottom="0.802034" crs:CropLeft="0.05499" crs:CropRight="0.94501" crs:CropTop="0.197966""#,
            1,
            (7725.0, 5150.0),
        ),
        (
            r#"crs:CropAngle="-1.48964" crs:CropBottom="0.962457" crs:CropLeft="0.010429" crs:CropRight="0.989571" crs:CropTop="0.037543""#,
            6,
            (6100.0, 9150.0),
        ),
    ] {
        let src = temp_dir(&format!("crop-straightened-{exif}"));
        let photo = src.join("hall.jpg");
        write_jpeg(&photo, 48, 32, exif);
        std::fs::write(src.join("hall.xmp"), crop_packet(fields)).unwrap();
        let mut s = Session::new().with_fs();
        s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
        let r = crop_of(&s).rect;
        let (ow, oh) = if exif == 6 { (6336.0, 9504.0) } else { (9504.0, 6336.0) };
        let (w, h) = (r.width() * ow, r.height() * oh);
        assert!((w - export.0).abs() <= 3.0 && (h - export.1).abs() <= 3.0, "EXIF {exif}: {w:.1} × {h:.1}, Lightroom exported {export:?}");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A straightened crop on top of a saved Upright isn't measured yet: the mode is solved again
/// and the warning says so.
#[test]
fn a_straightened_crop_on_a_saved_upright_is_solved_again_with_the_warning() {
    let src = temp_dir("upright-straightened");
    let photo = src.join("bridge.png");
    write_png(&photo);
    let xmp = lightroom(1).replace(r#"crs:UprightFourSegmentsCount="0""#, r#"crs:UprightFourSegmentsCount="0" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0.1" crs:CropRight="0.9" crs:CropBottom="0.9" crs:CropAngle="2.5""#);
    std::fs::write(src.join("bridge.xmp"), xmp).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert_eq!(upright_warnings(&r).first().map(|w| w["mode"].clone()), Some(json!("auto")), "{r}");
    assert_eq!((geometry(&s).upright, geometry(&s).upright_transform), (Upright::Auto, None));
    let _ = std::fs::remove_dir_all(&src);
}

/// Guided Upright's guides aren't read: a packet with guides and no automatic matrices still says
/// so; without guides or matrices nothing is lost.
#[test]
fn guided_upright_guides_that_are_not_read_are_reported() {
    let src = temp_dir("upright-guides");
    let photo = src.join("tower.png");
    write_png(&photo);
    let guides = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.30"
        crs:PerspectiveUpright="5" crs:UprightFourSegmentsCount="2"
        crs:UprightFourSegments_0="0.21, 0.12, 0.23, 0.88" crs:UprightFourSegments_1="0.78, 0.10, 0.76, 0.90"/>
    </rdf:RDF></x:xmpmeta>"#;
    std::fs::write(src.join("tower.xmp"), guides).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    assert_eq!(upright_warnings(&r).first().map(|w| w["mode"].clone()), Some(json!("guided")), "{r}");
    let none = props(
        &guides
            .replace(r#"crs:UprightFourSegmentsCount="2""#, r#"crs:UprightFourSegmentsCount="0""#)
            .replace(r#"crs:UprightFourSegments_0="0.21, 0.12, 0.23, 0.88" crs:UprightFourSegments_1="0.78, 0.10, 0.76, 0.90""#, ""),
    );
    assert_eq!(crate::crs::upright_at_risk(&none), None);
    let _ = std::fs::remove_dir_all(&src);
}

// ------------------------------------------------------------------ settings moved between photos

fn dev(s: &Session, id: lightcraft_catalog::PhotoId) -> DevelopSettings {
    (*s.catalog.photo(id).unwrap().develop).clone()
}

fn same(a: Option<Homography>, b: Homography) -> bool {
    a.is_some_and(|a| a.0.iter().zip(b.0).all(|(x, y)| (x - y).abs() < 1e-12))
}

/// A 60 × 40 JPEG `name` with EXIF `exif` (and, with `sidecar`, that XMP next to it), imported.
fn import_jpeg(s: &mut Session, dir: &Path, name: &str, exif: u16, sidecar: Option<&str>) -> lightcraft_catalog::PhotoId {
    let photo = dir.join(format!("{name}.jpg"));
    // each name its own pixels: identical files would be skipped as duplicates
    write_jpeg_seeded(&photo, 60, 40, exif, name.bytes().fold(17u8, |a, b| a.wrapping_mul(31).wrapping_add(b)));
    if let Some(x) = sidecar {
        std::fs::write(dir.join(format!("{name}.xmp")), x).unwrap();
    }
    s.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    s.catalog.photos().find(|p| p.file_name == format!("{name}.jpg")).unwrap().id
}

/// Paste, Sync and Paste from Previous carry an Upright correction into the target photo's frame:
/// a virtual copy turned a quarter right gets the original's correction turned with it (the
/// transferred matrix used to keep the original's frame: about 15 px off at 3200 × 4800).
#[test]
fn transferred_upright_lands_in_the_target_photo_frame() {
    for command in ["develop.paste", "develop.sync", "develop.pastePrevious"] {
        let src = temp_dir(&format!("upright-transfer-{}", command.replace('.', "-")));
        let mut s = Session::new().with_fs();
        let id = import_jpeg(&mut s, &src, "hall", 1, Some(&lightroom(1)));
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        let copy = lightcraft_catalog::PhotoId(s.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
        s.execute("library.select", &json!({"ids": [copy.0]})).unwrap();
        s.execute("photo.rotateRight", &json!({})).unwrap();
        let turned = placed(1, Orientation::Rotate90, 60.0, 40.0);
        assert!(same(dev(&s, copy).geometry.upright_transform, turned));
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("develop.copy", &json!({"groups": ["geometry"]})).unwrap();
        if command == "develop.sync" {
            s.selection = crate::Selection { ids: vec![id, copy], active: Some(id) };
        } else {
            s.execute("library.select", &json!({"ids": [copy.0]})).unwrap();
        }
        s.execute(command, &json!({"groups": ["geometry"]})).unwrap();
        assert!(same(dev(&s, copy).geometry.upright_transform, turned), "{command}: {:?}", dev(&s, copy).geometry.upright_transform);
        assert!(same(dev(&s, id).geometry.upright_transform, placed(1, Orientation::Normal, 60.0, 40.0)), "{command}: the source is untouched");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A saved Upright matrix is one photo's own analysis: between different files only the mode and
/// the Transform sliders move (as Lightroom does when it syncs the Upright mode). A target with the
/// same mode keeps its own correction; one whose mode changes solves its own. (Converting the
/// source's matrix into another file's frame was wrong whenever a frame wasn't recorded.)
#[test]
fn transfers_between_files_carry_the_upright_mode_not_the_matrix() {
    for command in ["develop.paste", "develop.sync", "develop.pastePrevious"] {
        let src = temp_dir(&format!("upright-between-files-{}", command.replace('.', "-")));
        let mut s = Session::new().with_fs();
        let a = import_jpeg(&mut s, &src, "a", 8, Some(&lightroom(1)));
        // b has its own saved Auto matrix (another one), c has Upright Off
        let b = import_jpeg(&mut s, &src, "b", 8, Some(&lightroom(1).replace(SLOTS[1], SLOTS[2])));
        let c = import_jpeg(&mut s, &src, "c", 6, None);
        let own_b = upright_in_frame(&SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[2]) }, Orientation::Rotate270, 60.0, 40.0).unwrap();
        assert!(same(dev(&s, b).geometry.upright_transform, own_b));
        s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
        s.execute("develop.copy", &json!({"groups": ["geometry"]})).unwrap();
        for target in [b, c] {
            if command == "develop.sync" {
                s.selection = crate::Selection { ids: vec![a, target], active: Some(a) };
            } else {
                s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
                s.execute("library.select", &json!({"ids": [target.0]})).unwrap();
            }
            s.execute(command, &json!({"groups": ["geometry"]})).unwrap();
        }
        let gb = dev(&s, b).geometry;
        assert!(gb.upright == Upright::Auto && same(gb.upright_transform, own_b), "{command}: b keeps its own: {gb:?}");
        let gc = dev(&s, c).geometry;
        assert_eq!((gc.upright, gc.upright_transform), (Upright::Auto, None), "{command}: c solves its own");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A crop paste carries the source's rotation: the target's own Upright correction turns with it.
#[test]
fn a_crop_paste_that_turns_the_photo_turns_its_upright() {
    let src = temp_dir("upright-crop-paste");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "tower", 1, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let copy = lightcraft_catalog::PhotoId(s.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [copy.0]})).unwrap();
    s.execute("photo.rotateRight", &json!({})).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["crop"]})).unwrap();
    s.execute("library.select", &json!({"ids": [copy.0]})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    let d = dev(&s, copy);
    assert_eq!(d.orientation, Orientation::Normal);
    assert!(same(d.geometry.upright_transform, placed(1, Orientation::Normal, 60.0, 40.0)), "{:?}", d.geometry);
    let _ = std::fs::remove_dir_all(&src);
}

/// Auto Sync carries an edit of the correction into each selected photo's own frame.
#[test]
fn auto_sync_carries_upright_into_each_photos_frame() {
    let src = temp_dir("upright-auto-sync");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "nave", 1, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let copy = lightcraft_catalog::PhotoId(s.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [copy.0]})).unwrap();
    s.execute("photo.rotateRight", &json!({})).unwrap();
    s.selection = crate::Selection { ids: vec![id, copy], active: Some(id) };
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let full = placed(2, Orientation::Normal, 60.0, 40.0);
    s.execute("develop.merge", &json!({"settings": {"geometry": {"upright": "full", "upright_transform": full}}})).unwrap();
    assert!(same(dev(&s, id).geometry.upright_transform, full));
    assert!(same(dev(&s, copy).geometry.upright_transform, placed(2, Orientation::Rotate90, 60.0, 40.0)), "{:?}", dev(&s, copy).geometry);
    let _ = std::fs::remove_dir_all(&src);
}

// ------------------------------------------------------------------ Synchronize Folder

fn bump(path: &Path, seconds: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(seconds)).unwrap();
}

/// Synchronize Folder reads a sidecar in the photo's frame, as import does: an unchanged packet
/// (only its time moved) changes nothing; a real change keeps the crop and the saved Upright where
/// import put them; what can't be kept is reported (it used to undo import's conversion silently).
#[test]
fn synchronize_folder_reads_sidecars_in_the_photo_frame() {
    let src = temp_dir("upright-folder-sync");
    let mut s = Session::new().with_fs();
    let crop = r#"crs:HasCrop="True" crs:CropLeft="0.206655" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#;
    let packet = |exposure: &str| lightroom(1).replace(r#"crs:Exposure2012="+0.30""#, &format!(r#"crs:Exposure2012="{exposure}" {crop}"#));
    let id = import_jpeg(&mut s, &src, "portrait", 8, Some(&packet("+0.30")));
    let before = dev(&s, id);
    let r = [before.crop.geometry.rect.x0, before.crop.geometry.rect.y0, before.crop.geometry.rect.x1, before.crop.geometry.rect.y1];
    assert_eq!(r.map(|v| (v * 1e6).round() / 1e6), [0.0, 0.0, 1.0, 0.793345]);
    assert!(same(before.geometry.upright_transform, placed(1, Orientation::Rotate270, 60.0, 40.0)));
    let dir = src.to_string_lossy().to_string();
    let sidecar = src.join("portrait.xmp");
    let sync = |s: &mut Session| s.execute("folder.synchronize", &json!({"path": dir, "readMetadata": true, "importNew": false})).unwrap();

    bump(&sidecar, 5);
    let r = sync(&mut s);
    assert_eq!(dev(&s, id), before, "an unchanged packet changed the photo: {r}");

    std::fs::write(&sidecar, packet("+0.80")).unwrap();
    bump(&sidecar, 10);
    let r = sync(&mut s);
    let after = dev(&s, id);
    assert_eq!(after.light.exposure, 0.8, "{r}");
    assert_eq!(after.crop.geometry, before.crop.geometry, "{r}");
    assert_eq!(after.geometry.upright_transform, before.geometry.upright_transform, "{r}");

    // Guided guides can't be read: the synchronize report says so
    std::fs::write(
        &sidecar,
        packet("+0.80").replace(r#"crs:PerspectiveUpright="1""#, r#"crs:PerspectiveUpright="5""#).replace(
            r#"crs:UprightFourSegmentsCount="0""#,
            r#"crs:UprightFourSegmentsCount="2" crs:UprightFourSegments_0="0.2, 0.1, 0.25, 0.9" crs:UprightFourSegments_1="0.8, 0.1, 0.75, 0.9""#,
        ),
    )
    .unwrap();
    bump(&sidecar, 15);
    let r = sync(&mut s);
    let warned = r["warnings"].as_array().into_iter().flatten().any(|w| w["code"] == "uprightGeometry" && w["mode"] == "guided");
    assert!(warned, "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

// ------------------------------------------------------------------ round three: presets, merge, stale frames

/// The catalog keeps its format: nothing about the file's frame is written into it (a recorded
/// frame would be dropped by an older build that reads format 7, and go stale when the file
/// changes).
#[test]
fn catalogs_keep_their_format() {
    let src = temp_dir("upright-catalog-format");
    let mut s = Session::new().with_fs();
    import_jpeg(&mut s, &src, "a", 8, Some(&lightroom(1)));
    assert!(!s.catalog.to_snapshot().contains("file_orientation"));
    let _ = std::fs::remove_dir_all(&src);
}

/// Sidecars are read in the frame the file has now: a file whose EXIF orientation changed (8 to 6)
/// gets its crop and saved Upright in the new frame on Read Metadata from File and Synchronize
/// Folder (a frame recorded at import used to stay, 19 px off, the crop on the wrong end).
#[test]
fn sidecars_are_read_in_the_files_current_frame() {
    let crop = r#"crs:HasCrop="True" crs:CropLeft="0.206655" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#;
    let packet = |exposure: &str| lightroom(1).replace(r#"crs:Exposure2012="+0.30""#, &format!(r#"crs:Exposure2012="{exposure}" {crop}"#));
    for via_sync in [false, true] {
        let src = temp_dir(&format!("upright-current-frame-{via_sync}"));
        let mut s = Session::new().with_fs();
        let id = import_jpeg(&mut s, &src, "turned", 8, Some(&packet("+0.30")));
        write_jpeg_seeded(&src.join("turned.jpg"), 60, 40, 6, b"turned".iter().fold(17u8, |a, b| a.wrapping_mul(31).wrapping_add(*b)));
        let sidecar = src.join("turned.xmp");
        std::fs::write(&sidecar, packet("+0.60")).unwrap();
        if via_sync {
            bump(&sidecar, 5);
            let r = s.execute("folder.synchronize", &json!({"path": src.to_string_lossy(), "readMetadata": true, "importNew": false})).unwrap();
            assert_eq!(r["read"], 1, "{r}");
        } else {
            s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
        }
        let d = dev(&s, id);
        assert_eq!(d.light.exposure, 0.6);
        let r = [d.crop.geometry.rect.x0, d.crop.geometry.rect.y0, d.crop.geometry.rect.x1, d.crop.geometry.rect.y1];
        assert_eq!(r.map(|v| (v * 1e6).round() / 1e6), [0.0, 0.206655, 1.0, 1.0], "sync: {via_sync}");
        assert!(same(d.geometry.upright_transform, placed(1, Orientation::Rotate90, 60.0, 40.0)), "sync: {via_sync}: {:?}", d.geometry);
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A preset keeps the Upright mode and the Transform sliders, never a saved correction: made from a
/// photo turned right and applied after it is turned back, the photo keeps its own correction (the
/// preset used to restore the quarter-turned one); a photo with another mode solves its own.
#[test]
fn presets_carry_the_upright_mode_not_a_correction() {
    let src = temp_dir("upright-presets");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 1, Some(&lightroom(1)));
    let other = import_jpeg(&mut s, &src, "b", 1, None);
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let pid = s.execute("preset.create", &json!({"name": "Upright", "groups": ["geometry"]})).unwrap()["id"].as_str().unwrap().to_string();
    let preset = s.presets.iter().find(|p| p.id == pid).unwrap().clone();
    assert!(preset.settings.pointer("/geometry/upright_transform").is_none_or(Value::is_null), "{}", preset.settings);
    s.execute("photo.rotateLeft", &json!({})).unwrap();
    s.execute("preset.apply", &json!({"id": pid})).unwrap();
    assert!(same(dev(&s, id).geometry.upright_transform, placed(1, Orientation::Normal, 60.0, 40.0)), "{:?}", dev(&s, id).geometry);
    s.execute("library.select", &json!({"ids": [other.0]})).unwrap();
    s.execute("preset.apply", &json!({"id": pid})).unwrap();
    assert_eq!((dev(&s, other).geometry.upright, dev(&s, other).geometry.upright_transform), (Upright::Auto, None));
    let _ = std::fs::remove_dir_all(&src);
}

/// Settings JSON (develop.merge: MCP `set_develop`, the CLI) that only turns the photo turns the
/// correction it keeps, as Rotate does; one that changes the mode drops it to be solved again; an
/// explicit correction is taken as given. Undo restores each step.
#[test]
fn merging_settings_json_keeps_upright_in_the_photo_frame() {
    let src = temp_dir("upright-merge");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 1, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let original = placed(1, Orientation::Normal, 60.0, 40.0);
    s.execute("photo.rotateRight", &json!({})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"orientation": "Normal"}})).unwrap();
    assert!(same(dev(&s, id).geometry.upright_transform, original), "{:?}", dev(&s, id).geometry);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(same(dev(&s, id).geometry.upright_transform, placed(1, Orientation::Rotate90, 60.0, 40.0)));
    s.execute("edit.redo", &json!({})).unwrap();
    let given = placed(2, Orientation::Rotate90, 60.0, 40.0);
    s.execute("develop.merge", &json!({"settings": {"orientation": "Rotate90", "geometry": {"upright_transform": given}}})).unwrap();
    assert!(same(dev(&s, id).geometry.upright_transform, given));
    s.execute("develop.merge", &json!({"settings": {"geometry": {"upright": "vertical"}}})).unwrap();
    assert_eq!((dev(&s, id).geometry.upright, dev(&s, id).geometry.upright_transform), (Upright::Vertical, None));
    let _ = std::fs::remove_dir_all(&src);
}

/// A sidecar whose only change is a crop the photo's file can't be read to place is still reported
/// by Synchronize Folder (holding the crop used to hide the change, and with it the warning).
#[test]
fn a_crop_only_change_that_cant_be_placed_is_reported() {
    let src = temp_dir("upright-held-crop");
    let mut s = Session::new().with_fs();
    let crop = |left: &str| crop_packet(&format!(r#"crs:CropLeft="{left}" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#));
    let id = import_jpeg(&mut s, &src, "gone", 8, Some(&crop("0.206655")));
    let before = dev(&s, id);
    std::fs::write(src.join("gone.jpg"), b"not a photo any more").unwrap();
    let sidecar = src.join("gone.xmp");
    std::fs::write(&sidecar, crop("0.3")).unwrap();
    bump(&sidecar, 5);
    let r = s.execute("folder.synchronize", &json!({"path": src.to_string_lossy(), "readMetadata": true, "importNew": false})).unwrap();
    assert_eq!(dev(&s, id).crop, before.crop, "{r}");
    let held = r["warnings"].as_array().into_iter().flatten().any(|w| w["code"] == "unmappedXmp" && w["fields"].to_string().contains("Crop"));
    assert!(held, "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

// ------------------------------------------------------------------ sweeps over every frame

/// The correction `m` (a matrix on the stored 60 × 40 image) in the frame of a photo whose file is
/// turned by `file` and then by the user's `user`, built from coordinate maps alone.
fn oracle(file: Orientation, user: Orientation, m: [f64; 9]) -> Homography {
    let (sw, sh) = if file.swaps_axes() { (40.0, 60.0) } else { (60.0, 40.0) };
    let (ow, oh) = if user.swaps_axes() { (sh, sw) } else { (sw, sh) };
    let l = f64::max(ow, oh) / 2.0;
    let at = |x: f64, y: f64| {
        let (u, v) = file.map(x * 60.0, y * 40.0, 60.0, 40.0);
        let (u, v) = user.map(u, v, sw, sh);
        Point::new((u - ow / 2.0) / l, (v - oh / 2.0) / l)
    };
    let (o, x, y) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0));
    let c = Homography([x.x - o.x, y.x - o.x, o.x, x.y - o.y, y.y - o.y, o.y, 0.0, 0.0, 1.0]);
    let h = c.mul(&Homography(m)).mul(&c.inverse().unwrap());
    Homography(h.0.map(|v| v / h.0[8]))
}

const TURNS: [Orientation; 4] = [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate180, Orientation::Rotate270];

/// A photo of a 60 × 40 file (`path`) with EXIF `file`, shown turned `user`, with saved Auto
/// correction `m` in its frame.
fn sweep_photo(id: u64, path: &str, file: Orientation, user: Orientation, m: [f64; 9]) -> lightcraft_catalog::Photo {
    let (w, h) = if file.swaps_axes() { (40, 60) } else { (60, 40) };
    let mut p = lightcraft_catalog::Photo::new(
        lightcraft_catalog::PhotoId(id),
        lightcraft_catalog::Source::File { path: path.into() },
        "a.jpg",
        "JPEG",
        w,
        h,
        "2026-01-01T00:00:00",
    );
    let mut d = DevelopSettings { orientation: user, ..Default::default() };
    d.geometry.upright = Upright::Auto;
    d.geometry.upright_transform = Some(oracle(file, user, m));
    p.develop = std::sync::Arc::new(d);
    p
}

fn sweep_transfer(s: &mut Session, command: &str, groups: &[&str]) {
    let (a, b) = (lightcraft_catalog::PhotoId(1), lightcraft_catalog::PhotoId(2));
    s.selection = crate::Selection::single(a);
    s.execute("develop.copy", &json!({"groups": groups})).unwrap();
    if command == "develop.sync" {
        s.selection = crate::Selection { ids: vec![a, b], active: Some(a) };
    } else {
        s.previous_active = Some(a);
        s.selection = crate::Selection::single(b);
    }
    s.execute(command, &json!({"groups": groups})).unwrap();
}

/// Paste, Sync and Paste from Previous, every source and target EXIF orientation and user turn and
/// the Geometry, Crop and both groups: between different files (9,216 cases, each with its own
/// scene) each target keeps its own correction, in the frame it ends up in; within one file
/// (original to copy, copy to original, copy to copy: 3,456 cases) the correction moves into the
/// target's frame. Undo restores every paste exactly.
#[test]
fn transfers_in_every_frame_and_group() {
    let (mine, theirs) = (matrix(SLOTS[1]), matrix(SLOTS[2]));
    let mut cases = 0;
    for command in ["develop.paste", "develop.sync", "develop.pastePrevious"] {
        for groups in [&["geometry"][..], &["crop"][..], &["geometry", "crop"][..]] {
            for fa in ORIENTATIONS {
                for fb in ORIENTATIONS {
                    for ua in TURNS {
                        for ub in TURNS {
                            let a = sweep_photo(1, "/sweep/a.jpg", fa, ua, mine);
                            let b = sweep_photo(2, "/sweep/b.jpg", fb, ub, theirs);
                            let mut s = Session::new();
                            s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(a) }).unwrap();
                            s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(b) }).unwrap();
                            let before = dev(&s, lightcraft_catalog::PhotoId(2));
                            sweep_transfer(&mut s, command, groups);
                            let got = dev(&s, lightcraft_catalog::PhotoId(2));
                            let user = if groups.contains(&"crop") { ua } else { ub };
                            assert_eq!(got.orientation, user);
                            assert!(
                                same(got.geometry.upright_transform, oracle(fb, user, theirs)),
                                "{command} {groups:?} {fa:?}/{ua:?} -> {fb:?}/{ub:?}"
                            );
                            if command == "develop.paste" {
                                s.execute("edit.undo", &json!({})).unwrap();
                                assert_eq!(dev(&s, lightcraft_catalog::PhotoId(2)), before);
                            }
                            cases += 1;
                        }
                    }
                }
                // the same file: original to copy, copy to original, copy to copy
                for (copy_a, copy_b) in [(None, Some(1)), (Some(2), None), (Some(9), Some(9))] {
                    for ua in TURNS {
                        for ub in TURNS {
                            let mut a = sweep_photo(1, "/sweep/a.jpg", fa, ua, mine);
                            let mut b = sweep_photo(2, "/sweep/a.jpg", fa, ub, mine);
                            a.copy_of = copy_a.map(lightcraft_catalog::PhotoId);
                            b.copy_of = copy_b.map(lightcraft_catalog::PhotoId);
                            let mut s = Session::new();
                            s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(a) }).unwrap();
                            s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(b) }).unwrap();
                            sweep_transfer(&mut s, command, groups);
                            let got = dev(&s, lightcraft_catalog::PhotoId(2));
                            let user = if groups.contains(&"crop") { ua } else { ub };
                            assert!(
                                same(got.geometry.upright_transform, oracle(fa, user, mine)),
                                "same file {copy_a:?}/{copy_b:?} {command} {groups:?} {fa:?} {ua:?} -> {ub:?}"
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 9216 + 3 * 1152);
}

/// Auto Sync of an edited correction (develop.merge on the active photo) for every source and
/// target EXIF orientation and user turn (1,024 cases): a virtual copy of the same file gets it in
/// its own frame, another file keeps its own.
#[test]
fn auto_sync_in_every_frame() {
    let (mine, theirs, edited) = (matrix(SLOTS[1]), matrix(SLOTS[2]), matrix(SLOTS[4]));
    let mut cases = 0;
    for fa in ORIENTATIONS {
        for fb in ORIENTATIONS {
            for ua in TURNS {
                for ub in TURNS {
                    let same_file = fa == fb && cases % 2 == 0;
                    let a = sweep_photo(1, "/sweep/a.jpg", fa, ua, mine);
                    let mut b = if same_file { sweep_photo(2, "/sweep/a.jpg", fb, ub, mine) } else { sweep_photo(2, "/sweep/b.jpg", fb, ub, theirs) };
                    if same_file {
                        b.copy_of = Some(lightcraft_catalog::PhotoId(1));
                    }
                    let mut s = Session::new();
                    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(a) }).unwrap();
                    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(b) }).unwrap();
                    s.selection = crate::Selection {
                        ids: vec![lightcraft_catalog::PhotoId(1), lightcraft_catalog::PhotoId(2)],
                        active: Some(lightcraft_catalog::PhotoId(1)),
                    };
                    s.auto_sync = true;
                    s.execute("develop.merge", &json!({"settings": {"geometry": {"upright_transform": oracle(fa, ua, edited)}}})).unwrap();
                    let got = dev(&s, lightcraft_catalog::PhotoId(2)).geometry.upright_transform;
                    let want = if same_file { oracle(fb, ub, edited) } else { oracle(fb, ub, theirs) };
                    assert!(same(got, want), "same file {same_file}: {fa:?}/{ua:?} -> {fb:?}/{ub:?}");
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 1024);
}

// ------------------------------------------------------------------ round four

/// Settings JSON sent back whole (`develop.get`, the rotation changed, `develop.merge`) states the
/// photo's unchanged correction: it is turned with the photo all the same (it used to stay in the
/// old frame, 15.5 px off), on the photo and, through Auto Sync, on its virtual copy.
#[test]
fn whole_settings_json_round_trip_turns_the_correction() {
    let src = temp_dir("upright-round-trip");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 6, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let copy = lightcraft_catalog::PhotoId(s.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
    let original = placed(1, Orientation::Rotate90, 60.0, 40.0);
    s.selection = crate::Selection { ids: vec![id, copy], active: Some(id) };
    s.execute("photo.rotateRight", &json!({})).unwrap();
    s.selection = crate::Selection { ids: vec![id, copy], active: Some(id) };
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let mut whole = s.execute("develop.get", &json!({"id": id.0})).unwrap();
    whole["orientation"] = json!("Normal");
    s.execute("develop.merge", &json!({"settings": whole})).unwrap();
    for x in [id, copy] {
        let d = dev(&s, x);
        assert_eq!(d.orientation, Orientation::Normal);
        assert!(same(d.geometry.upright_transform, original), "{x:?}: {:?}", d.geometry);
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// The matrix in settings JSON `v` with each number rounded to `digits` decimals, as a program that
/// writes numbers its own way would send it.
fn rounded(mut v: Value, digits: i32) -> Value {
    let k = 10f64.powi(digits);
    for x in v.pointer_mut("/geometry/upright_transform").and_then(Value::as_array_mut).into_iter().flatten() {
        *x = json!((x.as_f64().unwrap() * k).round() / k);
    }
    v
}

/// An agent reads the settings once and sends its copy back whole after each change: the rotation
/// set back to Normal, the same copy again, then with Exposure changed. After the first send the
/// copy's correction is no longer the photo's current one; it is still the photo's own, in the
/// frame it was read in, so every send leaves the correction in the photo's frame (the second and
/// third used to put it back in the old frame: 15.5 px mean, 28.2 px max at 3200 x 4800).
#[test]
fn a_local_copy_of_the_settings_sent_whole_again_keeps_the_correction_in_frame() {
    let src = temp_dir("upright-local-copy");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 6, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let original = placed(1, Orientation::Rotate90, 60.0, 40.0);
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let mut local = s.execute("develop.get", &json!({"id": id.0})).unwrap();
    local["orientation"] = json!("Normal");
    for send in ["first", "the same again", "with Exposure"] {
        if send == "with Exposure" {
            local["light"]["exposure"] = json!(0.5);
        }
        s.execute("develop.merge", &json!({"settings": local})).unwrap();
        let d = dev(&s, id);
        assert_eq!(d.orientation, Orientation::Normal, "{send}");
        assert!(same(d.geometry.upright_transform, original), "{send}: {:?}", d.geometry.upright_transform);
    }
    assert_eq!(dev(&s, id).light.exposure, 0.5);
    let _ = std::fs::remove_dir_all(&src);
}

/// Settings JSON whose matrix another program wrote with fewer decimals (12, 9, 6) still names the
/// photo's own correction, so a change of rotation turns it (it used to be stored as given, in the
/// old frame). A correction that differs by more than rounding (1e-4 in one element), or another
/// analysis's, is taken as given.
#[test]
fn settings_json_with_re_rounded_numbers_names_the_photos_own_correction() {
    let src = temp_dir("upright-re-rounded");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 6, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let original = placed(1, Orientation::Rotate90, 60.0, 40.0);
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let mut read = s.execute("develop.get", &json!({"id": id.0})).unwrap();
    read["orientation"] = json!("Normal");
    for digits in [12, 9, 6] {
        s.execute("develop.merge", &json!({"settings": rounded(read.clone(), digits)})).unwrap();
        let d = dev(&s, id);
        assert!(same(d.geometry.upright_transform, original), "{digits}: {:?}", d.geometry);
        s.execute("edit.undo", &json!({})).unwrap();
    }
    let mut nudged = placed(1, Orientation::Rotate90, 60.0, 40.0);
    nudged.0[1] += 1e-4;
    let other = placed(2, Orientation::Rotate90, 60.0, 40.0);
    for given in [nudged, other] {
        let mut sent = read.clone();
        sent["geometry"]["upright_transform"] = serde_json::to_value(given).unwrap();
        s.execute("develop.merge", &json!({"settings": sent})).unwrap();
        assert_eq!(dev(&s, id).geometry.upright_transform, Some(given));
        s.execute("edit.undo", &json!({})).unwrap();
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// Every file orientation, every orientation the settings were read at and two later ones: the
/// copy read once is sent whole with the first, the same copy again, then with the second and an
/// Exposure change, exact and with its numbers rounded to 6 decimals (24,576 sends). After each,
/// the correction is the photo's own in the stated orientation's frame, from coordinate maps alone.
#[test]
fn settings_json_sent_whole_in_every_frame() {
    let m = matrix(SLOTS[1]);
    let mut sends = 0;
    for file in ORIENTATIONS {
        for read_at in ORIENTATIONS {
            for digits in [None, Some(6)] {
                for first in ORIENTATIONS {
                    for then in ORIENTATIONS {
                        let mut s = Session::new();
                        s.catalog
                            .apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(sweep_photo(1, "/sweep/a.jpg", file, read_at, m)) })
                            .unwrap();
                        let id = lightcraft_catalog::PhotoId(1);
                        s.selection = crate::Selection::single(id);
                        let read = s.execute("develop.get", &json!({"id": 1})).unwrap();
                        let mut local = digits.map_or(read.clone(), |n| rounded(read, n));
                        for (i, o) in [first, first, then].into_iter().enumerate() {
                            local["orientation"] = serde_json::to_value(o).unwrap();
                            if i == 2 {
                                local["light"]["exposure"] = json!(0.5);
                            }
                            s.execute("develop.merge", &json!({"settings": local})).unwrap();
                            let d = dev(&s, id);
                            assert_eq!(d.orientation, o);
                            assert!(
                                same(d.geometry.upright_transform, oracle(file, o, m)),
                                "file {file:?}, read at {read_at:?}, {digits:?} decimals, send {i} to {o:?}: {:?}",
                                d.geometry.upright_transform
                            );
                            sends += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(sends, 24_576);
}

/// Copy, open another library, Paste: photo ids start again in each library, so a photo there with
/// the copied photo's id is another file and keeps its own correction (it used to get the copied
/// one).
#[test]
fn paste_after_opening_another_library_keeps_each_photos_own() {
    let (lib_a, lib_b, src) = (temp_dir("upright-lib-a"), temp_dir("upright-lib-b"), temp_dir("upright-lib-src"));
    let mut s = Session::new().with_fs();
    s.open_library(&lib_a, false).unwrap();
    let a = import_jpeg(&mut s, &src, "a", 1, Some(&lightroom(1)));
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["geometry"]})).unwrap();
    s.open_library(&lib_b, false).unwrap();
    let b = import_jpeg(&mut s, &src, "b", 1, Some(&lightroom(1).replace(SLOTS[1], SLOTS[2])));
    assert_eq!(a, b, "the regression needs the same id in both libraries");
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    let own = upright_in_frame(&SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[2]) }, Orientation::Normal, 60.0, 40.0).unwrap();
    assert!(same(dev(&s, b).geometry.upright_transform, own), "{:?}", dev(&s, b).geometry);
    for d in [lib_a, lib_b, src] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// "No crop" needs no frame: on a photo whose file can't be read, a sidecar saying HasCrop="False"
/// removes the crop without a warning (it used to be held, warned about, and listed again on every
/// Synchronize Folder).
#[test]
fn a_sidecar_without_a_crop_needs_no_frame() {
    let src = temp_dir("upright-no-crop");
    let mut s = Session::new().with_fs();
    let crop = crop_packet(r#"crs:CropLeft="0.206655" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#);
    let id = import_jpeg(&mut s, &src, "gone", 8, Some(&crop));
    assert!(!dev(&s, id).crop.geometry.is_identity());
    std::fs::write(src.join("gone.jpg"), b"not a photo any more").unwrap();
    let sidecar = src.join("gone.xmp");
    std::fs::write(&sidecar, crop.replace(r#"crs:HasCrop="True""#, r#"crs:HasCrop="False""#)).unwrap();
    let dir = src.to_string_lossy().to_string();
    let sync = |s: &mut Session| s.execute("folder.synchronize", &json!({"path": dir, "readMetadata": true, "importNew": false})).unwrap();
    bump(&sidecar, 5);
    let r = sync(&mut s);
    assert!(dev(&s, id).crop.geometry.is_identity(), "{r}");
    assert!(r.get("warnings").is_none(), "{r}");
    bump(&sidecar, 10);
    let r = sync(&mut s);
    assert_eq!(r["read"], 0, "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

/// Synchronize Folder learns a file's orientation through the session's file probe (the one import
/// uses, inside its memory budget), not by reading the file on its own.
#[test]
fn synchronize_folder_probes_files_through_the_session() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let src = temp_dir("upright-sync-probe");
    let mut s = Session::new().with_fs();
    let (_, fs_probe) = crate::files::fs_hooks();
    let probes = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = probes.clone();
    s.media.file_probe = Some(std::sync::Arc::new(move |p: &str| {
        counted.fetch_add(1, Ordering::Relaxed);
        fs_probe(p)
    }));
    let crop = r#"crs:HasCrop="True" crs:CropLeft="0.206655" crs:CropTop="0" crs:CropRight="1" crs:CropBottom="1" crs:CropAngle="0""#;
    let packet = |exposure: &str| lightroom(1).replace(r#"crs:Exposure2012="+0.30""#, &format!(r#"crs:Exposure2012="{exposure}" {crop}"#));
    let id = import_jpeg(&mut s, &src, "portrait", 8, Some(&packet("+0.30")));
    let before = dev(&s, id);
    probes.store(0, Ordering::Relaxed);
    let sidecar = src.join("portrait.xmp");
    std::fs::write(&sidecar, packet("+0.70")).unwrap();
    bump(&sidecar, 5);
    let r = s.execute("folder.synchronize", &json!({"path": src.to_string_lossy(), "readMetadata": true, "importNew": false})).unwrap();
    assert_eq!(dev(&s, id).light.exposure, 0.7, "{r}");
    assert_eq!((dev(&s, id).crop, dev(&s, id).geometry), (before.crop, before.geometry));
    assert!(probes.load(Ordering::Relaxed) > 0, "the file's orientation was read around the session's probe");
    let _ = std::fs::remove_dir_all(&src);
}

/// A preset saved before presets stopped holding a correction still has one: it is ignored at any
/// amount and as an import preset; a photo keeps its own, or gets the mode and solves its own.
#[test]
fn older_presets_holding_a_correction_are_ignored() {
    let src = temp_dir("upright-old-preset");
    let mut s = Session::new().with_fs();
    let theirs = upright_in_frame(&SavedUpright { mode: Upright::Auto, matrix: matrix(SLOTS[2]) }, Orientation::Normal, 60.0, 40.0).unwrap();
    s.presets.push(lightcraft_develop::Preset {
        id: "user.old-upright".into(),
        name: "Old Upright".into(),
        group: "User Presets".into(),
        settings: json!({"geometry": {"upright": "auto", "upright_transform": theirs}}),
        favorite: false,
        builtin: false,
    });
    let own = import_jpeg(&mut s, &src, "own", 1, Some(&lightroom(1)));
    let off = import_jpeg(&mut s, &src, "off", 1, None);
    for amount in [50.0, 100.0, 150.0] {
        s.execute("library.select", &json!({"ids": [own.0, off.0]})).unwrap();
        s.execute("preset.apply", &json!({"id": "user.old-upright", "amount": amount})).unwrap();
        assert!(same(dev(&s, own).geometry.upright_transform, placed(1, Orientation::Normal, 60.0, 40.0)), "{amount}");
        assert_eq!((dev(&s, off).geometry.upright, dev(&s, off).geometry.upright_transform), (Upright::Auto, None), "{amount}");
        s.execute("edit.undo", &json!({})).unwrap();
    }
    // as an import preset, after the sidecar
    let photo = src.join("late.jpg");
    write_jpeg_seeded(&photo, 60, 40, 1, 211);
    std::fs::write(src.join("late.xmp"), lightroom(1)).unwrap();
    s.execute("library.import", &json!({"paths": [photo.to_string_lossy()], "preset": "user.old-upright"})).unwrap();
    let late = s.catalog.photos().find(|p| p.file_name == "late.jpg").unwrap().id;
    assert!(same(dev(&s, late).geometry.upright_transform, placed(1, Orientation::Normal, 60.0, 40.0)));
    let _ = std::fs::remove_dir_all(&src);
}

/// What the Presets panel previews (`Session::preset_look`) is what Apply stores: a preset that
/// changes the mode leaves no correction, one that turns the photo turns it (the panel used to
/// preview `Preset::apply` alone: up to 15.4 px off).
#[test]
fn preset_previews_match_what_apply_stores() {
    let src = temp_dir("upright-preset-preview");
    let mut s = Session::new().with_fs();
    let id = import_jpeg(&mut s, &src, "a", 1, Some(&lightroom(1)));
    let preset = |pid: &str, settings: Value| lightcraft_develop::Preset {
        id: pid.into(),
        name: pid.into(),
        group: "User Presets".into(),
        settings,
        favorite: false,
        builtin: false,
    };
    for p in [preset("vertical", json!({"geometry": {"upright": "vertical"}})), preset("turned", json!({"orientation": "Rotate90"}))] {
        s.presets.push(p.clone());
        let preview = s.preset_look(id, &p, 1.0).unwrap();
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("preset.apply", &json!({"id": p.id})).unwrap();
        assert_eq!(dev(&s, id), preview, "{}", p.id);
        s.execute("edit.undo", &json!({})).unwrap();
    }
    let turned = s.preset_look(id, s.presets.iter().find(|p| p.id == "turned").unwrap(), 1.0).unwrap();
    assert!(same(turned.geometry.upright_transform, placed(1, Orientation::Rotate90, 60.0, 40.0)));
    let _ = std::fs::remove_dir_all(&src);
}
