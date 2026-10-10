//! Collection export / import: a collection, smart collection or collection set (with what is
//! inside it) written as a small JSON definition, and read back into this or another library.
//! Photos are matched by file path, else by file name and capture time.

use dac_catalog::{Album, AlbumId, Filter, Op, PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// The format tag of a definition file.
const FORMAT: &str = "collection-definition";
/// Limits (hostile files stay bounded).
const MAX_DEPTH: usize = 16;
const MAX_FILE: u64 = 64 << 20;
const MAX_ALBUMS: usize = 10_000;
const MAX_PHOTOS: usize = 1_000_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PhotoRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    captured: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Def {
    name: String,
    /// "set" (holds collections), "smart" (rules) or "collection" (photos).
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rules: Option<Filter>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    photos: Vec<PhotoRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<Def>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct File {
    format: String,
    version: u32,
    collections: Vec<Def>,
}

fn def_of(s: &Session, id: AlbumId, depth: usize) -> Option<Def> {
    let a = s.catalog.album(id)?;
    let mut d = Def { name: a.name.clone(), ..Default::default() };
    if a.folder {
        d.kind = "set".into();
        if depth < MAX_DEPTH {
            let kids: Vec<AlbumId> = s.catalog.albums().filter(|c| c.parent == Some(id)).map(|c| c.id).collect();
            d.children = kids.into_iter().filter_map(|k| def_of(s, k, depth + 1)).collect();
        }
    } else if let Some(rules) = &a.smart {
        d.kind = "smart".into();
        d.rules = Some((**rules).clone());
    } else {
        d.kind = "collection".into();
        d.photos = a
            .photos
            .iter()
            .filter_map(|p| s.catalog.photo(*p))
            .map(|p| PhotoRef {
                path: match &p.source {
                    Source::File { path } => Some(path.clone()),
                    Source::Demo { .. } => None,
                },
                file_name: p.file_name.clone(),
                captured: p.captured.clone(),
            })
            .collect();
    }
    Some(d)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "album.exportDefinition";
    let id = p.get("id").and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(c, "missing id"))?;
    let d = def_of(s, id, 0).ok_or_else(|| bad(c, format!("no album {}", id.0)))?;
    let file = File { format: FORMAT.into(), version: 1, collections: vec![d] };
    let text = serde_json::to_string_pretty(&file).map_err(|e| bad(c, e.to_string()))?;
    match str_param(p, "path").filter(|x| !x.is_empty()) {
        Some(path) => {
            std::fs::write(path, &text).map_err(|e| bad(c, format!("{path}: {e}")))?;
            Ok(json!({"path": path}))
        }
        None => Ok(serde_json::from_str(&text).unwrap_or(Value::Null)),
    }
}

struct Importer<'a> {
    s: &'a Session,
    ops: Vec<Op>,
    next: u64,
    created: usize,
    matched: usize,
    unmatched: usize,
}

impl Importer<'_> {
    fn find(&self, r: &PhotoRef) -> Option<PhotoId> {
        let photos = || self.s.catalog.photos().filter(|p| !p.deleted);
        if let Some(path) = &r.path
            && let Some(p) = photos().find(|p| matches!(&p.source, Source::File { path: q } if q == path))
        {
            return Some(p.id);
        }
        // by name and capture time (the same file elsewhere), name alone only when it's unique
        let named: Vec<&std::sync::Arc<dac_catalog::Photo>> = photos().filter(|p| p.file_name.eq_ignore_ascii_case(&r.file_name)).take(64).collect();
        if let Some(cap) = &r.captured
            && let Some(p) = named.iter().find(|p| p.captured.as_deref() == Some(cap.as_str()))
        {
            return Some(p.id);
        }
        named.first().filter(|_| named.len() == 1 && r.captured.is_none()).map(|p| p.id)
    }

    fn add(&mut self, d: &Def, parent: Option<AlbumId>, depth: usize) -> std::result::Result<(), String> {
        if depth > MAX_DEPTH || self.created >= MAX_ALBUMS {
            return Ok(());
        }
        let name = d.name.trim();
        let name = if name.is_empty() { "Imported Collection" } else { name };
        let id = AlbumId(self.next);
        self.next += 1;
        self.created += 1;
        let mut album = Album { parent, ..Album::new(id, name) };
        match d.kind.as_str() {
            "set" => album.folder = true,
            "smart" => album.smart = Some(Box::new(d.rules.clone().ok_or_else(|| format!("“{name}”: a smart collection without rules"))?)),
            _ => {
                for r in d.photos.iter().take(MAX_PHOTOS) {
                    match self.find(r) {
                        Some(pid) if !album.photos.contains(&pid) => {
                            album.photos.push(pid);
                            self.matched += 1;
                        }
                        Some(_) => {}
                        None => self.unmatched += 1,
                    }
                }
                album.cover = album.photos.first().copied();
            }
        }
        let is_set = album.folder;
        self.ops.push(Op::AddAlbum { album });
        if is_set {
            for c in &d.children {
                self.add(c, Some(id), depth + 1)?;
            }
        }
        Ok(())
    }
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "album.importDefinition";
    let text = match (str_param(p, "path").filter(|x| !x.is_empty()), p.get("definition")) {
        (Some(path), _) => {
            let len = std::fs::metadata(path).map_err(|e| bad(c, format!("{path}: {e}")))?.len();
            if len > MAX_FILE {
                return Err(bad(c, format!("{path} is larger than 64 MB: not a collection definition")));
            }
            std::fs::read_to_string(path).map_err(|e| bad(c, format!("{path}: {e}")))?
        }
        (None, Some(v)) => v.to_string(),
        (None, None) => return Err(bad(c, "give path or definition")),
    };
    let file: File = serde_json::from_str(&text).map_err(|e| bad(c, format!("not a collection definition: {e}")))?;
    if file.format != FORMAT {
        return Err(bad(c, format!("not a collection definition (format `{}`)", file.format)));
    }
    let parent = p.get("parent").and_then(Value::as_u64).map(AlbumId);
    if let Some(pa) = parent
        && !s.catalog.album(pa).is_some_and(|a| a.folder)
    {
        return Err(bad(c, "parent must be a collection set"));
    }
    let first = s.catalog.alloc_album_id().0;
    let mut im = Importer { s, ops: Vec::new(), next: first, created: 0, matched: 0, unmatched: 0 };
    for d in file.collections.iter().take(MAX_ALBUMS) {
        im.add(d, parent, 0).map_err(|e| bad(c, e))?;
    }
    let (ops, created, matched, unmatched) = (im.ops, im.created, im.matched, im.unmatched);
    if ops.is_empty() {
        return Err(bad(c, "the definition holds no collection"));
    }
    // later ids must not collide with the ones used here
    for _ in 1..created {
        s.catalog.alloc_album_id();
    }
    s.commit("Import Collection", Op::Batch { ops })?;
    Ok(json!({"created": created, "photos": matched, "unmatched": unmatched, "id": first}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "album.exportDefinition",
            "Export Collection Definition",
            [],
            None,
            "{id, path?} — a collection, smart collection or collection set (with everything inside) as JSON: written to `path`, else returned",
            always,
            export
        ),
        cmd!(
            "album.importDefinition",
            "Import Collection Definition",
            [],
            None,
            "{path | definition, parent?: set id} — recreate collections from a definition; photos are matched by path, else by file name and capture time; one undo step → {created, photos, unmatched, id}",
            always,
            import
        ),
    ]
}
