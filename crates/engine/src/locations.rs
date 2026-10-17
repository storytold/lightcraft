//! Saved locations (LRC-MAP-LOCATIONS, as Lightroom Classic's Saved Locations): named circles on
//! the map, a centre and a radius, each optionally *private* (photos inside it are exported
//! without their location, as if "Remove location info" were on for them; see
//! [`crate::export::prepare_export`]).
//!
//! They are a library setting, not catalog records: kept in `locations.json` next to the catalog
//! (written atomically on every change, like `prefs.json`), so the catalog's journal format is
//! untouched. Edits take effect at once and are not undo steps (Lightroom Classic's saved
//! location edits aren't part of a photo's history either). The file is hostile input: one that
//! doesn't parse is set aside as `locations.json.corrupt-<time>` and the library opens without
//! saved locations; entries that aren't valid (a NaN, a latitude of 300, an empty name, a radius
//! of 0, a duplicate id, past [`MAX_LOCATIONS`]) are left out and the file is set aside the same
//! way before it is next written, with a warning naming how many were dropped.
//!
//! Commands: `map.locations`, `map.location.add`, `.update`, `.remove`, `.photos` (`cmd/map.rs`).
//! Counting photos inside each location uses a latitude-sorted index ([`PositionIndex`]): a
//! binary search for the circle's latitude band, then the exact distance for those, so thousands
//! of locations over tens of thousands of photos stay cheap.

use lightcraft_catalog::rules::{GpsCircle, MAX_RADIUS_M, MIN_RADIUS_M, valid_gps};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The file next to the catalog that holds them.
pub const FILE: &str = "locations.json";
/// At most this many saved locations per library (a file with more keeps the first ones).
pub const MAX_LOCATIONS: usize = 10_000;
/// A name is 1…this many characters (after trimming).
pub const MAX_NAME_CHARS: usize = 200;
/// The file format written (a newer one is read the same way: unknown fields are ignored).
const VERSION: u32 = 1;

/// One saved location: a circle of `radius` metres around `lat, lon` (degrees).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLocation {
    pub id: u64,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// Metres, 1 m…1000 km.
    pub radius: f64,
    /// Photos inside are exported without location metadata, and the Info panel's map hides.
    #[serde(default)]
    pub private: bool,
}

impl SavedLocation {
    /// Its circle (always valid: every location is checked when made, changed or loaded).
    pub fn circle(&self) -> GpsCircle {
        GpsCircle { lat: self.lat, lon: self.lon, radius: self.radius }
    }

    /// Whether the GPS position `gps` (as `Meta.gps` stores it) is inside; never for a position
    /// that isn't one.
    pub fn contains(&self, gps: Option<(f64, f64)>) -> bool {
        valid_gps(gps).is_some_and(|(lat, lon)| self.circle().contains(lat, lon))
    }

    /// `{id, name, lat, lon, radius, private}`.
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "lat": self.lat, "lon": self.lon, "radius": self.radius, "private": self.private})
    }
}

/// A name as a location keeps it: trimmed, control characters out, 1…[`MAX_NAME_CHARS`].
pub fn check_name(name: &str) -> Result<String, String> {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return Err("the name is empty".into());
    }
    if clean.chars().count() > MAX_NAME_CHARS {
        return Err(format!("the name is longer than {MAX_NAME_CHARS} characters"));
    }
    Ok(clean.to_string())
}

/// The centre and radius checked: finite, latitude −90…90, longitude −180…180, radius 1 m…1000 km.
pub fn check_circle(lat: f64, lon: f64, radius: f64) -> Result<(), String> {
    if !(lat.is_finite() && (-90.0..=90.0).contains(&lat)) {
        return Err(format!("lat {lat} isn't a latitude (−90…90)"));
    }
    if !(lon.is_finite() && (-180.0..=180.0).contains(&lon)) {
        return Err(format!("lon {lon} isn't a longitude (−180…180)"));
    }
    if !(radius.is_finite() && (MIN_RADIUS_M..=MAX_RADIUS_M).contains(&radius)) {
        return Err(format!("radius {radius} isn't 1 m to 1000 km (in metres)"));
    }
    Ok(())
}

/// What `map.location.update` changes (what is `None` stays).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocationChange {
    pub name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius: Option<f64>,
    pub private: Option<bool>,
}

/// The library's saved locations, in the order they were made.
#[derive(Clone, Debug, Default)]
pub struct SavedLocations {
    list: Vec<SavedLocation>,
    /// Changes so far this session (what the UI caches its counts under).
    revision: u64,
}

/// The file's layout. Entries are read one by one ([`SavedLocations::load`]), so a bad one costs
/// only itself.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LocationsFile {
    pub version: u32,
    pub locations: Vec<Value>,
}

impl SavedLocations {
    pub fn list(&self) -> &[SavedLocation] {
        &self.list
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Bumped by every change (and by loading a library's).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, id: u64) -> Option<&SavedLocation> {
        self.list.iter().find(|l| l.id == id)
    }

    /// The first private location the position `gps` is inside.
    pub fn private_at(&self, gps: Option<(f64, f64)>) -> Option<&SavedLocation> {
        valid_gps(gps)?;
        self.list.iter().find(|l| l.private && l.contains(gps))
    }

    fn next_id(&self) -> u64 {
        self.list.iter().map(|l| l.id).max().unwrap_or(0).saturating_add(1)
    }

    /// Add a location (checked); returns it with its new id.
    pub fn add(&mut self, name: &str, lat: f64, lon: f64, radius: f64, private: bool) -> Result<SavedLocation, String> {
        if self.list.len() >= MAX_LOCATIONS {
            return Err(format!("a library holds at most {MAX_LOCATIONS} saved locations"));
        }
        let name = check_name(name)?;
        check_circle(lat, lon, radius)?;
        let id = self.next_id();
        if id == u64::MAX {
            return Err("no location id left".into());
        }
        let l = SavedLocation { id, name, lat, lon, radius, private };
        self.list.push(l.clone());
        self.revision = self.revision.wrapping_add(1);
        Ok(l)
    }

    /// Change location `id` (checked as a whole: nothing changes when any part is refused).
    pub fn update(&mut self, id: u64, c: &LocationChange) -> Result<SavedLocation, String> {
        let Some(cur) = self.get(id) else { return Err(format!("no saved location {id}")) };
        let mut next = cur.clone();
        if let Some(n) = &c.name {
            next.name = check_name(n)?;
        }
        next.lat = c.lat.unwrap_or(next.lat);
        next.lon = c.lon.unwrap_or(next.lon);
        next.radius = c.radius.unwrap_or(next.radius);
        next.private = c.private.unwrap_or(next.private);
        check_circle(next.lat, next.lon, next.radius)?;
        if let Some(slot) = self.list.iter_mut().find(|l| l.id == id) {
            *slot = next.clone();
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(next)
    }

    /// Remove location `id`; `None` when there is none.
    pub fn remove(&mut self, id: u64) -> Option<SavedLocation> {
        let i = self.list.iter().position(|l| l.id == id)?;
        self.revision = self.revision.wrapping_add(1);
        Some(self.list.remove(i))
    }

    /// The locations of a file's entries, and how many entries were left out (not a valid
    /// location, a duplicate id, past [`MAX_LOCATIONS`]).
    pub(crate) fn load(file: LocationsFile, revision: u64) -> (SavedLocations, usize) {
        let mut list: Vec<SavedLocation> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut dropped = 0usize;
        for v in file.locations {
            let ok = serde_json::from_value::<SavedLocation>(v).ok().and_then(|mut l| {
                l.name = check_name(&l.name).ok()?;
                check_circle(l.lat, l.lon, l.radius).ok()?;
                (l.id > 0 && l.id < u64::MAX && list.len() < MAX_LOCATIONS && seen.insert(l.id)).then_some(l)
            });
            match ok {
                Some(l) => list.push(l),
                None => dropped += 1,
            }
        }
        (SavedLocations { list, revision: revision.wrapping_add(1) }, dropped)
    }

    /// The file's bytes for these locations.
    pub(crate) fn to_file(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Out<'a> {
            version: u32,
            locations: &'a [SavedLocation],
        }
        serde_json::to_vec_pretty(&Out { version: VERSION, locations: &self.list }).unwrap_or_default()
    }

    /// Replace them all (a library opened, or one with none).
    pub(crate) fn replace(&mut self, other: SavedLocations) {
        let revision = self.revision.max(other.revision).wrapping_add(1);
        *self = SavedLocations { list: other.list, revision };
    }
}

/// Photo positions sorted by latitude, to find those inside a circle quickly: a binary search for
/// the circle's latitude band ([`GpsCircle::lat_range`]), then the exact distance for each
/// position in it.
#[derive(Clone, Debug, Default)]
pub struct PositionIndex {
    /// `(lat, lon, id)`, by latitude.
    by_lat: Vec<(f64, f64, u64)>,
}

impl PositionIndex {
    /// From `(id, gps)` pairs; positions that aren't ones are left out.
    pub fn new<I: IntoIterator<Item = (u64, Option<(f64, f64)>)>>(items: I) -> PositionIndex {
        let mut by_lat: Vec<(f64, f64, u64)> = items.into_iter().filter_map(|(id, gps)| valid_gps(gps).map(|(lat, lon)| (lat, lon, id))).collect();
        by_lat.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        PositionIndex { by_lat }
    }

    /// Positions held.
    pub fn len(&self) -> usize {
        self.by_lat.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_lat.is_empty()
    }

    fn band(&self, c: &GpsCircle) -> &[(f64, f64, u64)] {
        let (south, north) = c.lat_range();
        let from = self.by_lat.partition_point(|p| p.0 < south);
        let to = self.by_lat.partition_point(|p| p.0 <= north);
        self.by_lat.get(from..to.max(from)).unwrap_or(&[])
    }

    /// How many positions are inside `c`.
    pub fn count(&self, c: &GpsCircle) -> usize {
        self.band(c).iter().filter(|p| c.contains(p.0, p.1)).count()
    }

    /// The ids of the positions inside `c` (by latitude, south first).
    pub fn inside(&self, c: &GpsCircle) -> Vec<u64> {
        self.band(c).iter().filter(|p| c.contains(p.0, p.1)).map(|p| p.2).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_update_remove_check_every_value() {
        let mut s = SavedLocations::default();
        let home = s.add("  Home ", 48.85, 2.29, 300.0, true).unwrap();
        assert_eq!((home.id, home.name.as_str(), home.private), (1, "Home", true));
        let r0 = s.revision();
        for (name, lat, lon, radius) in [
            ("", 0.0, 0.0, 100.0),
            ("   ", 0.0, 0.0, 100.0),
            ("x", 90.5, 0.0, 100.0),
            ("x", f64::NAN, 0.0, 100.0),
            ("x", 0.0, 180.01, 100.0),
            ("x", 0.0, 0.0, 0.5),
            ("x", 0.0, 0.0, 1_000_000.5),
            ("x", 0.0, 0.0, f64::INFINITY),
        ] {
            assert!(s.add(name, lat, lon, radius, false).is_err(), "{name:?} {lat} {lon} {radius}");
        }
        assert!(s.add(&"n".repeat(MAX_NAME_CHARS + 1), 0.0, 0.0, 10.0, false).is_err());
        assert_eq!(s.add(&"é".repeat(MAX_NAME_CHARS), 0.0, 0.0, 10.0, false).unwrap().name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(s.add("tab\there\n", 0.0, 0.0, 10.0, false).unwrap().name, "tabhere");
        assert!(s.revision() > r0);
        // an update is checked as a whole: a bad radius leaves the name alone too
        let c = LocationChange { name: Some("Flat".into()), radius: Some(-1.0), ..Default::default() };
        assert!(s.update(home.id, &c).is_err());
        assert_eq!(s.get(home.id).unwrap().name, "Home");
        let c = LocationChange { name: Some("Flat".into()), lon: Some(-179.5), private: Some(false), ..Default::default() };
        let u = s.update(home.id, &c).unwrap();
        assert_eq!((u.name.as_str(), u.lon, u.private, u.radius), ("Flat", -179.5, false, 300.0));
        assert!(s.update(999, &LocationChange::default()).is_err());
        assert_eq!(s.remove(home.id).map(|l| l.name), Some("Flat".into()));
        assert!(s.remove(home.id).is_none());
        // ids are never reused while a higher one exists
        let next = s.add("Next", 1.0, 1.0, 10.0, false).unwrap();
        assert!(s.list().iter().filter(|l| l.id == next.id).count() == 1);
    }

    #[test]
    fn loading_keeps_good_entries_and_counts_the_rest() {
        let file: LocationsFile = serde_json::from_value(json!({
            "version": 1,
            "locations": [
                {"id": 1, "name": "Home", "lat": 48.8, "lon": 2.3, "radius": 200.0, "private": true},
                {"id": 1, "name": "Duplicate", "lat": 1.0, "lon": 1.0, "radius": 10.0},
                {"id": 2, "name": "", "lat": 1.0, "lon": 1.0, "radius": 10.0},
                {"id": 3, "name": "Too far north", "lat": 300.0, "lon": 1.0, "radius": 10.0},
                {"id": 4, "name": "Zero", "lat": 1.0, "lon": 1.0, "radius": 0.0},
                {"id": 0, "name": "No id", "lat": 1.0, "lon": 1.0, "radius": 10.0},
                {"id": "5", "name": "Text id", "lat": 1.0, "lon": 1.0, "radius": 10.0},
                "not an object",
                null,
                {"id": 6, "name": "Fiji", "lat": -17.0, "lon": 179.99, "radius": 50000.0, "extra": [1, 2]},
            ]
        }))
        .unwrap();
        let (s, dropped) = SavedLocations::load(file, 0);
        assert_eq!(s.list().iter().map(|l| l.id).collect::<Vec<_>>(), vec![1, 6]);
        assert_eq!(dropped, 8);
        assert!(s.get(1).unwrap().private && !s.get(6).unwrap().private);
        // written back and read again: the same
        let again: LocationsFile = serde_json::from_slice(&s.to_file()).unwrap();
        assert_eq!(again.version, VERSION);
        let (t, d) = SavedLocations::load(again, 0);
        assert_eq!((t.list(), d), (s.list(), 0));
    }

    #[test]
    fn a_file_with_too_many_keeps_the_first_ones() {
        let locations: Vec<Value> = (1..=MAX_LOCATIONS as u64 + 5)
            .map(|i| json!({"id": i, "name": format!("L{i}"), "lat": (i % 170) as f64 - 85.0, "lon": (i % 360) as f64 - 180.0, "radius": 1000.0}))
            .collect();
        let (mut s, dropped) = SavedLocations::load(LocationsFile { version: 1, locations }, 0);
        assert_eq!((s.len(), dropped), (MAX_LOCATIONS, 5));
        assert!(s.add("One more", 0.0, 0.0, 10.0, false).is_err());
    }

    #[test]
    fn the_index_counts_like_checking_every_photo() {
        // a spread of positions, the poles and both sides of the antimeridian included
        let mut items = Vec::new();
        let mut id = 0;
        for lat in (-90..=90).step_by(3) {
            for lon in (-180..=180).step_by(7) {
                id += 1;
                items.push((id, Some((f64::from(lat), f64::from(lon)))));
            }
        }
        items.push((id + 1, Some((f64::NAN, 0.0))));
        items.push((id + 2, None));
        let idx = PositionIndex::new(items.clone());
        assert_eq!(idx.len(), items.len() - 2);
        let circles = [
            GpsCircle::new(48.0, 2.0, 400_000.0).unwrap(),
            GpsCircle::new(0.0, 179.5, 1_000_000.0).unwrap(),
            GpsCircle::new(0.0, -180.0, 900_000.0).unwrap(),
            GpsCircle::new(88.0, 45.0, 600_000.0).unwrap(),
            GpsCircle::new(-90.0, 0.0, 1_000_000.0).unwrap(),
            GpsCircle::new(10.0, 10.0, 1.0).unwrap(),
        ];
        for c in circles {
            let brute: Vec<u64> = items.iter().filter(|(_, g)| valid_gps(*g).is_some_and(|(a, b)| c.contains(a, b))).map(|(i, _)| *i).collect();
            let mut got = idx.inside(&c);
            got.sort();
            assert_eq!(got, brute, "{c:?}");
            assert_eq!(idx.count(&c), brute.len());
        }
        // the circle around the south pole holds every position at −90 whatever its longitude
        let south = GpsCircle::new(-90.0, 0.0, 1_000_000.0).unwrap();
        assert!(idx.count(&south) >= (-180..=180).step_by(7).count() * 3);
        // across the antimeridian: positions on both sides
        let fiji = GpsCircle::new(0.0, 179.5, 1_000_000.0).unwrap();
        let lons: Vec<f64> = idx.band(&fiji).iter().filter(|p| fiji.contains(p.0, p.1)).map(|p| p.1).collect();
        assert!(lons.iter().any(|l| *l > 170.0) && lons.iter().any(|l| *l < -170.0), "{lons:?}");
    }

    #[test]
    fn private_at_finds_the_private_location_holding_a_position() {
        let mut s = SavedLocations::default();
        s.add("Park", 48.85, 2.29, 2000.0, false).unwrap();
        let home = s.add("Home", 48.86, 2.33, 500.0, true).unwrap();
        assert_eq!(s.private_at(Some((48.8601, 2.3302))).map(|l| l.id), Some(home.id));
        assert!(s.private_at(Some((48.85, 2.29))).is_none(), "inside a public location only");
        assert!(s.private_at(None).is_none());
        assert!(s.private_at(Some((f64::NAN, 2.33))).is_none());
    }
}
