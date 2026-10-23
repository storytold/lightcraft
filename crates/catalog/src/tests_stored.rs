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
fn base() -> DevelopSettings {
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

/// A reader of one library's defaults leaves the next library read on the thread alone.
#[test]
fn reading_over_a_librarys_defaults_ends_with_that_library() {
    let m = MemStore::new();
    m.set(SNAPSHOT, snapshot_stating(&other_defaults(), serde_json::json!({"1": stored_record(1, serde_json::json!({}))})));
    Journal::open(Box::new(m)).unwrap();
    let plainly: DevelopSettings = serde_json::from_str::<Photo>(&record(&catalog_of([plain(1)])).to_string()).map(|p| (*p.develop).clone()).unwrap();
    assert_eq!(plainly, base());
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
