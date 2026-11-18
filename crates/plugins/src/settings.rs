//! A plug-in's settings and the values its dialogs were last run with, kept for the whole app (not
//! per library, unlike its namespace) in `<plugins>/settings/<id>.json`:
//! `{"format": 1, "settings": {key: value}, "dialogs": {"command/<id>" | "destination/<id>": {key: value}}}`.
//!
//! Values are checked against the manifest's fields when they are written and again when they are
//! read: a value a field no longer takes (the plug-in was updated) reads as the field's default. A
//! damaged file reads as empty (the caller logs why) and is moved aside to `<id>.json.bad` by the
//! first save, so nothing replaces it unseen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::manifest::{Field, Manifest};
use crate::{Error, Result};

/// Largest settings file (serialized) a plug-in may keep.
pub const MAX_SETTINGS_BYTES: usize = 1 << 20;

/// The on-disk format version written.
const FORMAT: u64 = 1;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct File {
    format: u64,
    #[serde(default)]
    settings: Map<String, Value>,
    #[serde(default)]
    dialogs: BTreeMap<String, Map<String, Value>>,
}

/// One plug-in's settings and remembered dialog values.
#[derive(Debug, Default)]
pub struct SettingsStore {
    path: Option<PathBuf>,
    settings: Map<String, Value>,
    dialogs: BTreeMap<String, Map<String, Value>>,
    /// The file couldn't be read: moved aside before the first save.
    damaged: bool,
    dirty: bool,
    /// Counts changes to the settings (not the dialogs' values), so a page showing them can tell
    /// it's out of date.
    revision: u64,
}

/// Where a dialog's remembered values are kept: `command/<id>`.
pub fn command_scope(command: &str) -> String {
    format!("command/{command}")
}

/// Where a dialog's remembered values are kept: `destination/<id>`.
pub fn destination_scope(destination: &str) -> String {
    format!("destination/{destination}")
}

impl SettingsStore {
    /// An empty store kept in memory only.
    pub fn in_memory() -> SettingsStore {
        SettingsStore::default()
    }

    /// Forgets the file (the plug-in is being removed): from now on the store lives in memory, so a
    /// call of the plug-in still running can't write the file back after it was deleted.
    pub fn forget(&mut self) {
        self.path = None;
    }

    /// The file plug-in `id`'s settings live in, below the plug-in folder `dir`.
    pub fn path_in(dir: &Path, id: &str) -> PathBuf {
        dir.join("settings").join(format!("{id}.json"))
    }

    /// Loads the store at `path`; missing is empty. A file that can't be read or understood is
    /// empty too, with the reason as the second value (the file is kept until the first save
    /// moves it aside).
    pub fn open(path: PathBuf) -> (SettingsStore, Option<String>) {
        let mut store = SettingsStore { path: Some(path.clone()), ..SettingsStore::default() };
        let read = match crate::safe_io::read_capped(&path, MAX_SETTINGS_BYTES as u64) {
            Ok(b) => serde_json::from_slice::<File>(&b)
                .map_err(|e| e.to_string())
                .and_then(|f| if f.format > FORMAT { Err(format!("written by a newer version (format {})", f.format)) } else { Ok(f) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
            Err(e) => Err(e.to_string()),
        };
        match read {
            Ok(f) => {
                store.settings = f.settings;
                store.dialogs = f.dialogs;
                (store, None)
            }
            Err(e) => {
                store.damaged = true;
                (store, Some(format!("{}: {e}; using the defaults", path.display())))
            }
        }
    }

    /// The value of every setting in `fields`: the stored one when the field takes it, else its default.
    pub fn values(&self, fields: &[Field]) -> Map<String, Value> {
        fields
            .iter()
            .map(|f| (f.key.clone(), self.settings.get(&f.key).and_then(|v| f.coerce(v).ok()).unwrap_or_else(|| f.default_value())))
            .collect()
    }

    /// Goes up with every change to the settings: [`SettingsStore::set`], and an update dropping one.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Setting `key` as stored (`None`: never set, or back to its default).
    pub fn stored(&self, key: &str) -> Option<Value> {
        self.settings.get(key).cloned()
    }

    /// Puts the revision back (a change undone as a whole, see [`crate::Manager::set_settings`]).
    pub(crate) fn rewind(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// Sets setting `key` to `value`, already checked against its field (`None`: back to its default).
    pub fn set(&mut self, key: &str, value: Option<Value>) -> Result<()> {
        let same = self.settings.get(key) == value.as_ref();
        let old = match value {
            Some(v) => self.settings.insert(key.to_string(), v),
            None => self.settings.remove(key),
        };
        if self.too_big() {
            match old {
                Some(o) => self.settings.insert(key.to_string(), o),
                None => self.settings.remove(key),
            };
            return Err(Error::Params(format!("settings would exceed {MAX_SETTINGS_BYTES} bytes")));
        }
        self.dirty = true;
        if !same {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }

    /// A dialog's start values: per field, the value it was last run with when the field remembers
    /// and still takes it, else its default.
    pub fn dialog_values(&self, scope: &str, fields: &[Field]) -> Map<String, Value> {
        let last = self.dialogs.get(scope);
        fields
            .iter()
            .map(|f| {
                let v = last.filter(|_| f.remember).and_then(|l| l.get(&f.key)).and_then(|v| f.coerce(v).ok());
                (f.key.clone(), v.unwrap_or_else(|| f.default_value()))
            })
            .collect()
    }

    /// Keeps the values a dialog ran with (`args`, already checked), for the fields that remember.
    /// Returns whether anything changed.
    pub fn remember(&mut self, scope: &str, fields: &[Field], args: &Value) -> Result<bool> {
        let new: Map<String, Value> = fields.iter().filter(|f| f.remember).filter_map(|f| Some((f.key.clone(), args.get(&f.key)?.clone()))).collect();
        if self.dialogs.get(scope).map_or(new.is_empty(), |old| *old == new) {
            return Ok(false);
        }
        let old = if new.is_empty() { self.dialogs.remove(scope) } else { self.dialogs.insert(scope.to_string(), new) };
        if self.too_big() {
            match old {
                Some(o) => self.dialogs.insert(scope.to_string(), o),
                None => self.dialogs.remove(scope),
            };
            return Err(Error::Params(format!("settings would exceed {MAX_SETTINGS_BYTES} bytes")));
        }
        self.dirty = true;
        Ok(true)
    }

    /// Drops what `manifest` no longer declares: settings, commands and destinations, and fields
    /// of their dialogs (after an update). Returns whether anything was dropped.
    pub fn prune(&mut self, manifest: &Manifest) -> bool {
        let before = (self.settings.len(), self.dialogs.values().map(Map::len).sum::<usize>(), self.dialogs.len());
        self.settings.retain(|k, _| manifest.setting(k).is_some());
        if self.settings.len() != before.0 {
            self.revision = self.revision.wrapping_add(1);
        }
        self.dialogs.retain(|scope, values| {
            let fields = match scope.split_once('/') {
                Some(("command", id)) => manifest.command(id).map(|c| &c.dialog),
                Some(("destination", id)) => manifest.destination(id).map(|d| &d.dialog),
                _ => None,
            };
            let Some(fields) = fields else { return false };
            values.retain(|k, _| fields.iter().any(|f| f.key == *k && f.remember));
            !values.is_empty()
        });
        let after = (self.settings.len(), self.dialogs.values().map(Map::len).sum::<usize>(), self.dialogs.len());
        if after != before {
            self.dirty = true;
        }
        after != before
    }

    fn serialized(&self) -> serde_json::Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct FileRef<'a> {
            format: u64,
            settings: &'a Map<String, Value>,
            dialogs: &'a BTreeMap<String, Map<String, Value>>,
        }
        serde_json::to_vec_pretty(&FileRef { format: FORMAT, settings: &self.settings, dialogs: &self.dialogs })
    }

    /// The store as kept with its plug-in's previous version ([`crate::Manager::rollback`]).
    pub(crate) fn snapshot(&self) -> Value {
        serde_json::json!({"format": FORMAT, "settings": self.settings, "dialogs": self.dialogs})
    }

    /// The file `snapshot` ([`SettingsStore::snapshot`]) is written as, or why this version can't
    /// take it back.
    pub(crate) fn snapshot_file(snapshot: &Value) -> std::result::Result<Vec<u8>, String> {
        let f: File = serde_json::from_value(snapshot.clone()).map_err(|e| e.to_string())?;
        if f.format > FORMAT {
            return Err(format!("written by a newer version (format {})", f.format));
        }
        let bytes = serde_json::to_vec_pretty(&f).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(format!("over {MAX_SETTINGS_BYTES} bytes"));
        }
        Ok(bytes)
    }

    /// Takes `snapshot` back, already written to the store's file: a change of the settings.
    pub(crate) fn restored(&mut self, snapshot: &Value) {
        let f: File = serde_json::from_value(snapshot.clone()).unwrap_or_default();
        self.settings = f.settings;
        self.dialogs = f.dialogs;
        self.damaged = false;
        self.dirty = false;
        self.revision = self.revision.wrapping_add(1);
    }

    fn too_big(&self) -> bool {
        self.serialized().map_or(true, |b| b.len() > MAX_SETTINGS_BYTES)
    }

    /// Writes the store if it changed (atomically). A damaged file is first moved aside to `.bad`.
    pub fn save(&mut self) -> Result<()> {
        let (Some(path), true) = (&self.path, self.dirty) else { return Ok(()) };
        if self.damaged {
            let mut bad = path.clone().into_os_string();
            bad.push(".bad");
            match std::fs::rename(path, &bad) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
            }
            self.damaged = false;
        }
        let bytes = self.serialized().map_err(|e| Error::Io(e.to_string()))?;
        crate::store::write_atomic(path, &bytes)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(v: Value) -> Manifest {
        Manifest::parse(v.to_string().as_bytes()).unwrap()
    }

    /// `p.json` in an empty Temp folder of the test's own, removed when the test ends.
    fn tmp(name: &str) -> lcc_testkit::TempFile {
        lcc_testkit::TempFile::new(&format!("lcc-settings-{name}"), "p.json")
    }

    #[test]
    fn a_forgotten_store_never_writes_its_file_back() {
        let path = tmp("forget");
        let m =
            manifest(json!({"id": "a", "name": "A", "settings": [{"key": "n", "label": "N", "type": "number", "min": 0, "max": 9, "default": 1}]}));
        let (mut s, _) = SettingsStore::open(path.to_path_buf());
        s.forget();
        s.set("n", Some(json!(2.0))).unwrap();
        s.save().unwrap();
        assert!(!path.exists(), "removed plug-ins leave no file");
        assert_eq!(s.values(&m.settings)["n"], 2.0, "kept for the rest of the call");
    }

    #[test]
    fn values_come_back_and_read_as_defaults_when_they_no_longer_fit() {
        let m = manifest(json!({"id": "a", "name": "A", "settings": [
            {"key": "n", "label": "N", "type": "number", "min": 0, "max": 10, "default": 3},
            {"key": "c", "label": "C", "type": "choice", "options": ["x", "y"]}]}));
        let path = tmp("values");
        let (mut s, err) = SettingsStore::open(path.to_path_buf());
        assert!(err.is_none());
        assert_eq!(s.values(&m.settings), *json!({"n": 3.0, "c": "x"}).as_object().unwrap());
        s.set("n", Some(json!(7.0))).unwrap();
        s.set("c", Some(json!("y"))).unwrap();
        s.save().unwrap();
        let (s, _) = SettingsStore::open(path.to_path_buf());
        assert_eq!(s.values(&m.settings), *json!({"n": 7.0, "c": "y"}).as_object().unwrap());
        // the plug-in changed its fields: values they no longer take read as the new defaults
        let m2 = manifest(json!({"id": "a", "name": "A", "settings": [
            {"key": "n", "label": "N", "type": "text", "default": "t"},
            {"key": "c", "label": "C", "type": "choice", "options": ["z"]}]}));
        assert_eq!(s.values(&m2.settings), *json!({"n": "t", "c": "z"}).as_object().unwrap());
    }

    #[test]
    fn a_damaged_file_reads_as_defaults_and_is_moved_aside_on_save() {
        let path = tmp("damaged");
        std::fs::write(&path, b"{not json").unwrap();
        let (mut s, err) = SettingsStore::open(path.to_path_buf());
        assert!(err.unwrap().contains("using the defaults"));
        assert!(s.settings.is_empty());
        s.set("k", Some(json!(true))).unwrap();
        s.save().unwrap();
        assert_eq!(std::fs::read(format!("{}.bad", path.display())).unwrap(), b"{not json");
        assert_eq!(SettingsStore::open(path.to_path_buf()).0.settings["k"], true);
        std::fs::write(&path, br#"{"format": 99}"#).unwrap();
        assert!(SettingsStore::open(path.to_path_buf()).1.unwrap().contains("newer"));
    }

    #[test]
    fn dialogs_remember_only_fields_that_remember_and_prune_drops_what_is_gone() {
        let m = manifest(json!({"id": "a", "name": "A",
            "commands": [{"id": "go", "label": "Go", "dialog": [
                {"key": "keep", "label": "K", "type": "text"},
                {"key": "once", "label": "O", "type": "bool", "remember": false}]}],
            "exportDestinations": [{"id": "d", "name": "D", "dialog": [{"key": "album", "label": "Album", "type": "text"}]}]}));
        let go = &m.command("go").unwrap().dialog;
        let mut s = SettingsStore::in_memory();
        assert!(s.remember(&command_scope("go"), go, &json!({"keep": "k1", "once": true})).unwrap());
        assert!(!s.remember(&command_scope("go"), go, &json!({"keep": "k1", "once": false})).unwrap(), "unchanged");
        assert_eq!(s.dialog_values(&command_scope("go"), go), *json!({"keep": "k1", "once": false}).as_object().unwrap());
        s.remember(&destination_scope("d"), &m.destination("d").unwrap().dialog, &json!({"album": "Trip"})).unwrap();
        s.remember("command/old", go, &json!({"keep": "x"})).unwrap();
        s.set("gone", Some(json!(1))).unwrap();
        // an update without the destination and without `gone`
        let m2 = manifest(
            json!({"id": "a", "name": "A", "commands": [{"id": "go", "label": "Go", "dialog": [{"key": "keep", "label": "K", "type": "text"}]}]}),
        );
        assert!(s.prune(&m2));
        assert!(s.settings.is_empty());
        assert_eq!(s.dialogs.keys().collect::<Vec<_>>(), ["command/go"]);
        assert!(!s.prune(&m2));
    }

    #[test]
    fn the_size_cap_refuses_and_keeps_the_old_value() {
        let mut s = SettingsStore::in_memory();
        s.set("a", Some(json!("small"))).unwrap();
        for i in 0..20 {
            let _ = s.set(&format!("k{i}"), Some(json!("x".repeat(64 * 1024))));
        }
        assert!(s.set("big", Some(json!("x".repeat(MAX_SETTINGS_BYTES)))).is_err());
        assert!(!s.settings.contains_key("big"));
        assert_eq!(s.settings["a"], "small");
    }
}
