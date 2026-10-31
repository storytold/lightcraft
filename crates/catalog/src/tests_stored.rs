//! What a library's missing develop settings read as ([`crate::stored`]): a saved library states
//! them, and a build whose own differ reads the library over them.

use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use serde::Serialize;
use serde_json::Value;

use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::*;

/// Settings with nothing stated, as every saved library reads them: every value its default,
/// on the first rendering process (a missing process never means "the default one").
pub(crate) fn base() -> DevelopSettings {
    serde_json::from_str("{}").unwrap()
}

fn plain(id: u64) -> Photo {
    let mut p = Photo::new(
        PhotoId(id),
        Source::File { path: format!("/pictures/2019/trip/IMG_{id:04}.jpg") },
        &format!("IMG_{id:04}.jpg"),
        "JPEG",
        4032,
        3024,
        "2026-10-10T12:00:00",
    );
    p.develop = Arc::new(base());
    p
}

// ---- fixtures (also used, as they are, to write `fixtures/v<N>-catalog.*` with the build of format N)

pub(crate) fn edited(d: impl FnOnce(&mut DevelopSettings)) -> Arc<DevelopSettings> {
    let mut s = base();
    d(&mut s);
    Arc::new(s)
}

/// Settings with lists, choices and optional values: masks of several kinds, curve points,
/// guides, an Upright transform, a locked crop aspect, spots, red eye, point colours.
pub(crate) fn rich() -> DevelopSettings {
    use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape, PointColor, RedEye, SegMask, Spot};
    use lightcraft_geom::{Homography, Point};
    let at = |x: f64, y: f64| Point { x, y };
    let seg = |side: u32| SegMask { side, data: "eJxjYAAAAAEAAQ==".into(), rect: Some([0.1, 0.2, 0.6, 0.7]) };
    let component = |shape: MaskShape| MaskComponent { name: None, op: MaskOp::default(), invert: false, shape };
    let mut s = DevelopSettings { treatment: lightcraft_develop::Treatment::Bw, ..base() };
    s.profile.id = "lc.mono".into();
    s.profile.amount = 150.0;
    s.profile.camera_local_tone = true;
    s.wb.tint = 12.0;
    s.curve.master = vec![at(0.0, 0.0), at(0.4, 0.55), at(1.0, 1.0)];
    s.curve.red = vec![at(0.0, 0.1), at(1.0, 0.9)];
    s.point_colors = vec![PointColor { hue_shift: 20.0, ..Default::default() }];
    s.geometry.guides = vec![(at(0.1, 0.1), at(0.1, 0.9)), (at(0.9, 0.1), at(0.8, 0.9))];
    s.geometry.upright_transform = Some(Homography([1.0, 0.01, 0.0, -0.01, 1.0, 0.0, 0.0, 0.002, 1.0]));
    s.crop.aspect = Some((3, 2));
    s.crop.flip_h = true;
    s.crop.geometry.angle = 1.5;
    s.spots = vec![Spot { points: vec![at(0.3, 0.3), at(0.32, 0.31)], source_offset: Some(at(0.05, 0.0)), ..Default::default() }];
    s.red_eye = vec![RedEye { center: at(0.5, 0.4), catchlight: Some(at(0.1, -0.1)), ..Default::default() }];
    s.masks = vec![
        Mask {
            id: 1,
            name: "Sky".into(),
            components: vec![
                component(MaskShape::Linear { start: at(0.5, 0.0), end: at(0.5, 0.4) }),
                MaskComponent {
                    name: Some("Sun".into()),
                    invert: true,
                    ..component(MaskShape::Radial { center: at(0.7, 0.2), rx: 0.1, ry: 0.1, angle: 0.0, feather: 50.0, invert: false })
                },
                component(MaskShape::LuminanceRange { lo: 0.6, hi: 1.0, lo_feather: 0.1, hi_feather: 0.0 }),
            ],
            refine: 30.0,
            ..Default::default()
        },
        Mask {
            id: 2,
            name: "Boat".into(),
            components: vec![
                component(MaskShape::Object {
                    hint: vec![at(0.4, 0.6)],
                    exclude: vec![at(0.1, 0.1)],
                    seg: Some(seg(256)),
                    detail: vec![seg(128), seg(64)],
                    edge: 2.0,
                }),
                component(MaskShape::Prompt { text: "the red boat".into(), seg: None, detail: Vec::new(), edge: 0.0 }),
                component(MaskShape::ColorRange { samples: vec![[0.5, 0.1, 0.2]], refine: 40.0 }),
                component(MaskShape::People { person: 1, parts: vec!["face".into()] }),
                component(MaskShape::Subject),
            ],
            ..Default::default()
        },
    ];
    s.disabled_sections = vec!["detail".into(), "light".into()];
    s
}

/// A photo with every field set to something other than what [`plain`] has, but for the few that
/// formats after 6 added (the focus point, the History cursor, the preview-only recheck).
fn full() -> Photo {
    let look = edited(|s| s.light.contrast = 20.0);
    let now = Arc::new(DevelopSettings { light: lightcraft_develop::Light { exposure: 0.3, ..Default::default() }, ..rich() });
    let mut p = plain(7);
    p.kind = MediaKind::Raw;
    p.file_size = 25_000_000;
    p.captured = Some("2019-07-14T15:21:07".into());
    p.meta = Meta {
        camera: "Camera Model X".into(),
        lens: "24-70mm f/2.8".into(),
        focal_mm: Some(35.0),
        aperture: Some(4.0),
        shutter: "1/250".into(),
        iso: Some(400),
        location: "Old harbour".into(),
        city: "Lisbon".into(),
        state: "Lisboa".into(),
        country: "Portugal".into(),
        gps: Some((38.7, -9.1)),
        title: "Harbour".into(),
        caption: "Boats at dusk".into(),
        alt_text: "Three boats".into(),
        extended_description: "Three boats moored at dusk".into(),
        copyright: "(c) Someone".into(),
        copyright_status: CopyrightStatus::Copyrighted,
        usage_terms: "All rights reserved".into(),
        copyright_url: "https://example.org/rights".into(),
        creator: "Someone".into(),
        keywords: vec!["Places|Lisbon".into(), "boats".into()],
        regions: vec![lightcraft_meta::Region {
            rect: lightcraft_meta::Rect { x0: 0.1, y0: 0.2, x1: 0.3, y1: 0.4 },
            kind: lightcraft_meta::RegionKind::Face,
            name: Some("Ana".into()),
            description: None,
        }],
        // (no format with a fixture had it)
        focus: None,
    };
    p.rating = 4;
    p.flag = Flag::Pick;
    p.label = Some(ColorLabel::Green);
    p.develop = now.clone();
    p.edited = Some("2026-10-10T13:00:00".into());
    p.versions = vec![Version { name: "Warm".into(), created: "2026-10-10T12:30:00".into(), settings: look.clone(), auto: true }];
    p.history = vec![HistoryStep { label: "Contrast".into(), settings: look.clone() }, HistoryStep { label: "Exposure".into(), settings: now }];
    p.deleted = true;
    p.local = true;
    p.duration = Some(12.5);
    p.as_shot_wb = Some((5200.0, 3.0));
    p.content_hash = Some("00000000000000009e3779b97f4a7c15".into());
    p.embedded_lens = Some(lightcraft_develop::EmbeddedLens { vignette: Some(Default::default()), ..Default::default() });
    p.copy_of = Some(PhotoId(3));
    p.copy_name = Some("Copy 1".into());
    p.import_look = Some(look);
    p.analysis = Some(Analysis { sharpness: 71.0, clipped: 0.02, group: Some(4), best: true });
    p.preview_only = Some("unsupported raw variant".into());
    p.local_baseline = Some(0xdead_beef);
    p
}

/// Changes made after the snapshot of the format 6 fixture (they are its log).
fn later_ops() -> Vec<Op> {
    let warm = edited(|s| s.wb.temp += 500.0);
    vec![
        Op::SetDevelop { id: PhotoId(8), settings: warm.clone(), label: "Temperature".into(), edited: Some("2026-10-10T14:00:00".into()) },
        Op::PushHistory { id: PhotoId(8), step: HistoryStep { label: "Temperature".into(), settings: warm } },
        Op::SetMeta { id: PhotoId(8), meta: Box::new(Meta { title: "Harbour".into(), keywords: vec!["boats".into()], ..Default::default() }) },
        Op::SetRating { id: PhotoId(9), rating: 3 },
        Op::SetVersions {
            id: PhotoId(9),
            versions: vec![Version { name: "Rich".into(), created: "2026-10-10T14:05:00".into(), settings: Arc::new(rich()), auto: false }],
        },
    ]
}

/// What a fixture of format 7 on holds besides: a camera's as-shot white balance read later, and
/// settings on the Kelvin scale (the format's `Op::SetAsShotWb` and `WhiteBalance.scale`).
fn kelvin_ops() -> Vec<Op> {
    let kelvin = edited(|s| {
        s.wb.scale = lightcraft_develop::WbScale::Kelvin;
        s.wb.temp = 5200.0;
        s.wb.tint = 8.0;
    });
    vec![
        Op::SetAsShotWb { id: PhotoId(8), wb: Some((5200.0, 8.0)) },
        Op::SetDevelop { id: PhotoId(8), settings: kelvin, label: "White Balance".into(), edited: Some("2026-10-11T09:00:00".into()) },
    ]
}

/// The changes a fixture's log holds: [`later_ops`], and from format 7 on [`kelvin_ops`]. (The
/// fixtures of formats 8 and 9 hold the same library as format 7's: what those formats added, the
/// History cursor and the preview-only recheck, is not in them.)
fn fixture_ops(version: u32) -> Vec<Op> {
    let mut ops = later_ops();
    if version >= 7 {
        ops.extend(kelvin_ops());
    }
    ops
}

// ---- end of fixtures

fn catalog_of(photos: impl IntoIterator<Item = Photo>) -> Catalog {
    let mut c = Catalog::new();
    for p in photos {
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    c
}

/// The stored record of the catalog's only photo, as JSON.
fn record(c: &Catalog) -> Value {
    let v: Value = serde_json::from_str(&c.to_snapshot()).unwrap();
    v["photos"].as_object().unwrap().values().next().unwrap().clone()
}

/// A record may leave a setting out, so a library depends on what a missing value reads as: if
/// that changed, every photo relying on it would silently change.
/// `develop_defaults.json` is what settings with nothing stated (`{}`) read as.
///
/// If this fails because a setting was **added**, add it to the file (the test prints what it
/// reads now): libraries that never heard of it take its default. If what a missing value reads
/// as **changed**: saved libraries keep the old value, because each states its own defaults and
/// is read over those (`a_library_written_with_other_defaults_reads_as_it_was_written`); change
/// the file with it. A setting that was removed or renamed needs a loader of its own. (A new
/// default rendering process changes nothing here: settings without a process stay on the first
/// one, and photos on the default one state it.)
#[test]
fn what_missing_develop_settings_read_as_is_unchanged() {
    let pinned: Value = serde_json::from_str(include_str!("develop_defaults.json")).unwrap();
    // (what a library states: every setting, also the ones left out of the settings' own JSON)
    let now = stored::base().1.clone();
    // (as text too: `Value`'s own comparison takes -0.0 for 0.0)
    assert_eq!(now.to_string(), pinned.to_string(), "see this test's comment");
    assert_eq!(
        now,
        pinned,
        "what missing develop settings read as changed; see this test's comment. They read as:\n{}",
        serde_json::to_string_pretty(&now).unwrap()
    );
}

// ---- a library says what its missing values read as

/// `catalog.snap` as a build with these defaults would have written it: `photos` are stored
/// records by id.
fn snapshot_stating(defaults: &Value, photos: Value) -> Vec<u8> {
    let mut catalog: Value = serde_json::from_str(&Catalog::new().to_snapshot()).unwrap();
    catalog["photos"] = photos;
    catalog["next_photo"] = 100.into();
    format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"defaults\":{defaults},\"catalog\":{catalog}}}\n").into_bytes()
}

fn stated_defaults(m: &MemStore) -> Value {
    let head: Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    head["defaults"].clone()
}

/// This build's defaults with an Exposure of +1 and a red hue of 5: what a build with other
/// defaults states.
fn other_defaults() -> Value {
    let mut d = stored::base().1.clone();
    d["light"]["exposure"] = 1.0.into();
    d["mixer"]["red"]["hue"] = 5.0.into();
    d
}

/// The stored record of an untouched photo, with these develop settings.
fn stored_record(id: u64, develop: Value) -> Value {
    let mut r = record(&catalog_of([plain(id)]));
    r["id"] = id.into();
    r["develop"] = develop;
    r
}

/// Given a saved library, then its snapshot states what a record's missing develop settings read
/// as: the settings of a record with nothing stated.
#[test]
fn a_library_states_what_missing_settings_read_as() {
    let m = MemStore::new();
    let (mut j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
    let pinned: Value = serde_json::from_str(include_str!("develop_defaults.json")).unwrap();
    assert_eq!(stated_defaults(&m), pinned, "a new library");
    j.snapshot(&catalog_of([plain(1)])).unwrap();
    assert_eq!(stated_defaults(&m), pinned, "a saved one");
    // and they are what a record with nothing stated reads as
    assert_eq!(serde_json::from_value::<DevelopSettings>(pinned).unwrap(), base());
}

/// Given a library written by a build whose defaults were an Exposure of +1 and a red hue of 5,
/// when this build opens it, then its photos are as that build left them: a photo that states
/// nothing has that Exposure and hue (not this build's 0), in its settings and in the history
/// step its log added; a photo that states an Exposure of 0 has 0. And the library is rewritten
/// with this build's defaults, so it opens the same again.
#[test]
fn a_library_written_with_other_defaults_reads_as_it_was_written() {
    let m = MemStore::new();
    let photos = serde_json::json!({
        "1": stored_record(1, serde_json::json!({})),
        "2": stored_record(2, serde_json::json!({"light": {"exposure": 0.0}})),
    });
    m.set(SNAPSHOT, snapshot_stating(&other_defaults(), photos));
    let body = r#"{"op":"pushHistory","id":1,"step":{"label":"Import","settings":{}}}"#;
    m.set(LOG, format!("{{\"seq\":3,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).into_bytes());

    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.other_defaults && r.upgraded_from.is_none() && r.kept.is_empty(), "{r:?}");
    assert_eq!((r.replayed, r.failed), (1, 0));
    let (one, two) = (c.photo(PhotoId(1)).unwrap(), c.photo(PhotoId(2)).unwrap());
    assert_eq!((one.develop.light.exposure, one.develop.mixer.red.hue), (1.0, 5.0));
    assert_eq!((one.history[0].settings.light.exposure, one.history[0].settings.mixer.red.hue), (1.0, 5.0));
    assert_eq!((two.develop.light.exposure, two.develop.mixer.red.hue), (0.0, 5.0));
    // everything else is what both builds have
    let mut expected = base();
    expected.light.exposure = 1.0;
    expected.mixer.red.hue = 5.0;
    assert_eq!(*one.develop, expected);

    // rewritten: this build's defaults, no log
    assert_eq!(stated_defaults(&m), stored::base().1);
    assert!(m.get(LOG).unwrap().is_empty());
    let (_, mut again, r) = Journal::open(Box::new(m)).unwrap();
    again.revision = c.revision;
    assert!(!r.other_defaults);
    assert_eq!(again, c);
}

/// Given a library written before a setting existed (its defaults don't state it), when this
/// build opens it, then it is read as it is, the setting taking this build's default, and it is
/// not rewritten for that.
#[test]
fn a_library_that_only_lacks_newer_settings_is_read_as_it_is() {
    let mut older = stored::base().1.clone();
    older.as_object_mut().unwrap().remove("lens_blur").unwrap();
    older["light"].as_object_mut().unwrap().remove("blacks").unwrap();
    let m = MemStore::new();
    let snap = snapshot_stating(&older, serde_json::json!({"1": stored_record(1, serde_json::json!({}))}));
    m.set(SNAPSHOT, snap.clone());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(!r.other_defaults, "{r:?}");
    assert_eq!(*c.photo(PhotoId(1)).unwrap().develop, base());
    assert_eq!(m.get(SNAPSHOT), Some(snap), "not rewritten");
}

/// Given a library written with other defaults that can't be rewritten (the disk is full), when
/// it is opened, then it is refused and left as it is: appending to it would leave out values
/// that mean something else in the file.
#[test]
fn a_library_with_other_defaults_that_cannot_be_rewritten_is_refused() {
    struct ReadOnly(MemStore);
    impl Store for ReadOnly {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.0.read(name)
        }
        fn write_atomic(&mut self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("disk full"))
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.0.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.0.truncate(name, len)
        }
        fn describe(&self) -> String {
            "read-only".into()
        }
    }
    let m = MemStore::new();
    let snap = snapshot_stating(&other_defaults(), serde_json::json!({"1": stored_record(1, serde_json::json!({}))}));
    m.set(SNAPSHOT, snap.clone());
    let e = Journal::open(Box::new(ReadOnly(m.clone()))).err().expect("refused");
    assert!(matches!(e, CatalogError::Io(_)), "{e:?}");
    assert_eq!(m.files.lock().unwrap().clone(), [(SNAPSHOT.to_string(), snap)].into_iter().collect());
}

/// A reader of one library's defaults leaves the next library read on the thread alone: a record
/// that leaves out the values the library states other defaults for reads as this build's, once
/// the open that read the library is over.
#[test]
fn reading_over_a_librarys_defaults_ends_with_that_library() {
    let m = MemStore::new();
    m.set(SNAPSHOT, snapshot_stating(&other_defaults(), serde_json::json!({"1": stored_record(1, serde_json::json!({}))})));
    let (_, c, r) = Journal::open(Box::new(m)).unwrap();
    assert!(r.other_defaults, "{r:?}");
    assert_eq!(c.photo(PhotoId(1)).unwrap().develop.light.exposure, 1.0, "read over the library's own");
    let plainly: Photo = serde_json::from_value(stored_record(2, serde_json::json!({}))).unwrap();
    assert_eq!(*plainly.develop, base());
}

/// Given a library written by a build whose default for a setting that is left out of the
/// settings' JSON at its default was another one (the HDR section, the camera's local tone,
/// Denoise on or off), when this build opens it, then a photo that states nothing has the
/// library's value, not this build's: the library states those settings too.
#[test]
fn a_default_that_is_never_written_is_stated_all_the_same() {
    let ours = stored::base().1.clone();
    assert!(ours["hdr"].is_object() && ours["profile"]["camera_local_tone"] == false && ours["enhance"]["denoise_on"].is_null(), "{ours}");
    let mut theirs = ours.clone();
    theirs["hdr"]["enabled"] = true.into();
    theirs["profile"]["camera_local_tone"] = true.into();
    theirs["enhance"]["denoise_on"] = true.into();
    let m = MemStore::new();
    m.set(SNAPSHOT, snapshot_stating(&theirs, serde_json::json!({"1": stored_record(1, serde_json::json!({}))})));
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.other_defaults, "{r:?}");
    let d = &c.photo(PhotoId(1)).unwrap().develop;
    assert_eq!((d.hdr.enabled, d.profile.camera_local_tone, d.enhance.denoise_on), (true, true, Some(true)));
    assert_eq!(**d, {
        let mut expected = base();
        expected.hdr.enabled = true;
        expected.profile.camera_local_tone = true;
        expected.enhance.denoise_on = Some(true);
        expected
    });
    // and it opens the same once rewritten with this build's defaults
    let (_, mut again, r) = Journal::open(Box::new(m)).unwrap();
    again.revision = c.revision;
    assert!(!r.other_defaults);
    assert_eq!(again, c);
}

/// The settings whose JSON is left out at their default are exactly the ones the library
/// states by hand (`stored::UNWRITTEN`), and the rendering process, which has no default to
/// state. A setting given `skip_serializing_if` later fails here until it is stated too.
#[test]
fn every_setting_left_out_of_its_json_is_stated() {
    let mut skipped = Skipped::default();
    base().serialize(&mut skipped).unwrap();
    let mut expected: Vec<String> = stored::UNWRITTEN.iter().map(|(path, _)| path.join(".")).collect();
    expected.push("process".into());
    expected.sort();
    skipped.found.sort();
    assert_eq!(skipped.found, expected);
    // and with them, the library states every leaf of the settings
    let stated = &stored::base().1;
    for (path, _) in stored::UNWRITTEN {
        assert!(path.iter().try_fold(stated, |v, k| v.get(*k)).is_some(), "{path:?}");
    }
}

/// Given a library written by a build that knew a setting this build doesn't, when this build
/// opens it, then it reads it as it is (the setting is not this build's to read, whatever it is
/// read over) and does not rewrite it for that.
#[test]
fn a_library_that_knows_more_settings_is_read_as_it_is() {
    let mut newer = stored::base().1.clone();
    newer["light"]["glow"] = 7.0.into();
    newer["aurora"] = serde_json::json!({"amount": 1.0});
    let m = MemStore::new();
    let snap = snapshot_stating(&newer, serde_json::json!({"1": stored_record(1, serde_json::json!({}))}));
    m.set(SNAPSHOT, snap.clone());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(!r.other_defaults, "{r:?}");
    assert_eq!(*c.photo(PhotoId(1)).unwrap().develop, base());
    assert_eq!(m.get(SNAPSHOT), Some(snap), "not rewritten");
}

/// Walks a value and collects the struct fields it leaves out (`skip_serializing_if`), by path.
#[derive(Default)]
struct Skipped {
    path: Vec<&'static str>,
    found: Vec<String>,
}

type Walked = std::result::Result<(), std::fmt::Error>;

impl serde::Serializer for &mut Skipped {
    type Ok = ();
    type Error = std::fmt::Error;
    type SerializeSeq = serde::ser::Impossible<(), std::fmt::Error>;
    type SerializeTuple = serde::ser::Impossible<(), std::fmt::Error>;
    type SerializeTupleStruct = serde::ser::Impossible<(), std::fmt::Error>;
    type SerializeTupleVariant = serde::ser::Impossible<(), std::fmt::Error>;
    type SerializeMap = serde::ser::Impossible<(), std::fmt::Error>;
    type SerializeStruct = Self;
    type SerializeStructVariant = serde::ser::Impossible<(), std::fmt::Error>;

    fn serialize_struct(self, _: &'static str, _: usize) -> std::result::Result<Self, Self::Error> {
        Ok(self)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Walked {
        v.serialize(self)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Walked {
        v.serialize(self)
    }
    // (lists and choices hold no setting with a default of its own: not walked)
    fn serialize_seq(self, _: Option<usize>) -> std::result::Result<Self::SerializeSeq, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_tuple(self, _: usize) -> std::result::Result<Self::SerializeTuple, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> std::result::Result<Self::SerializeTupleStruct, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> std::result::Result<Self::SerializeTupleVariant, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_map(self, _: Option<usize>) -> std::result::Result<Self::SerializeMap, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> std::result::Result<Self::SerializeStructVariant, Self::Error> {
        Err(std::fmt::Error)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, _: &'static str, _: &T) -> Walked {
        Ok(())
    }
    fn serialize_bool(self, _: bool) -> Walked {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Walked {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Walked {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Walked {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Walked {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Walked {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Walked {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Walked {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Walked {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Walked {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Walked {
        Ok(())
    }
    fn serialize_char(self, _: char) -> Walked {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> Walked {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Walked {
        Ok(())
    }
    fn serialize_none(self) -> Walked {
        Ok(())
    }
    fn serialize_unit(self) -> Walked {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Walked {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Walked {
        Ok(())
    }
}

impl serde::ser::SerializeStruct for &mut Skipped {
    type Ok = ();
    type Error = std::fmt::Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, v: &T) -> Walked {
        self.path.push(key);
        // (a list or a choice: nothing in it to walk)
        let _ = v.serialize(&mut **self);
        self.path.pop();
        Ok(())
    }
    fn skip_field(&mut self, key: &'static str) -> Walked {
        self.found.push(self.path.iter().chain([&key]).copied().collect::<Vec<_>>().join("."));
        Ok(())
    }
    fn end(self) -> Walked {
        Ok(())
    }
}

/// The library the fixture of format `version` holds (`fixtures/v<N>-catalog.*`): three photos
/// in the snapshot, with a log of later changes.
pub(crate) fn fixture_library(version: u32) -> Catalog {
    let mut library = fixture_snapshot();
    for op in fixture_ops(version) {
        library.apply(op).unwrap();
    }
    library
}

/// What the fixtures' snapshot holds: a photo with every field set, an untouched one, and a
/// Local record with rich develop settings.
fn fixture_snapshot() -> Catalog {
    let mut rich_photo = plain(9);
    rich_photo.develop = Arc::new(rich());
    rich_photo.local = true;
    rich_photo.set_local_baseline();
    catalog_of([full(), plain(8), rich_photo])
}

/// Writes this build's fixture, `fixtures/v<VERSION>-catalog.*`: the fixture library as this
/// build saves it, snapshot and log. Run it once, when a format is new: put two empty files there
/// (the tests include them), then
/// `cargo test -p lightcraft-catalog write_this_formats_fixture -- --ignored`. The files are then
/// what that build wrote, and are never written again.
#[test]
#[ignore = "writes fixtures/v<VERSION>-catalog.*"]
fn write_this_formats_fixture() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
    let (snap, log) = (dir.join(format!("v{VERSION}-catalog.snap.json")), dir.join(format!("v{VERSION}-catalog.log.jsonl")));
    let written = |f: &std::path::Path| std::fs::metadata(f).is_ok_and(|m| m.len() > 0);
    assert!(!written(&snap) && !written(&log), "format {VERSION} has its fixture: a fixture is what that build wrote");
    let m = MemStore::new();
    let (mut j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
    // the snapshot first, then the later changes as the log beside it
    let mut library = fixture_snapshot();
    let photos: Vec<Op> = library.photos().map(|p| Op::AddPhoto { photo: Box::new((**p).clone()) }).collect();
    j.append(&photos).unwrap();
    j.snapshot(&library).unwrap();
    for op in fixture_ops(VERSION) {
        library.apply(op.clone()).unwrap();
        j.append(&[op]).unwrap();
    }
    drop(j);
    std::fs::write(snap, m.get(SNAPSHOT).unwrap()).unwrap();
    std::fs::write(log, m.get(LOG).unwrap()).unwrap();
}
