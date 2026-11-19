//! The user's albums, smart albums, projects and groups (`ZCOLLECTION`).
//!
//! Every row is a collection; its entity says which kind. The catalog's root is a
//! `ProjectCollection` that points at Capture One's own collections (all images, recent imports,
//! recent captures, trash). Those, the folders and everything below them stay out: names are
//! stored in the user interface's language, so a role is told from the entity and the root's
//! pointers, never from a name.

use std::collections::{HashMap, HashSet};

use super::photos::Read;
use super::tables::{Db, Row};
use super::{Collection, CollectionKind, ReadError};

/// The root's pointers at Capture One's own collections.
const SYSTEM_POINTERS: &[&str] = &["ZALLIMAGESCOLLECTION", "ZLASTCAPTURESCOLLECTION", "ZLASTIMPORTSCOLLECTION", "ZTRASHCOLLECTION"];
/// Deepest nesting followed; a chain longer than this is a loop or not a user's tree.
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Root,
    User(CollectionKind),
    /// Capture One's own, or a kind this reader doesn't import.
    Other,
}

/// Where a user collection goes.
enum Place {
    In(Option<i64>),
    /// Its parent row is gone: it goes to the top level, with a warning.
    Orphan,
    /// It is below one of Capture One's own collections.
    Out,
    /// Its parents form a loop.
    Loop,
}

struct Tree<'a> {
    rows: HashMap<i64, Row<'a>>,
    roles: HashMap<i64, Role>,
}

pub(super) fn read(db: &Db, photos: &Read, warnings: &mut Vec<String>) -> Result<Vec<Collection>, ReadError> {
    let table = db.optional("ZCOLLECTION", warnings)?;
    let entities = db.optional("ZENTITIES", warnings)?;
    let documents = db.optional("ZDOCUMENTCONTENT", warnings)?;
    let names: HashMap<i64, &str> = entities.rows().filter_map(|r| Some((r.id("Z_ENT")?, r.text("ZNAME")))).collect();
    let rows: HashMap<i64, Row<'_>> = table.rows().filter_map(|r| Some((r.id("Z_PK")?, r))).collect();

    let mut roots: HashSet<i64> = documents.rows().filter_map(|r| r.id("ZROOTCOLLECTION")).collect();
    roots.extend(rows.iter().filter(|(_, r)| SYSTEM_POINTERS.iter().any(|p| r.id(p).is_some())).map(|(id, _)| *id));
    let system: HashSet<i64> = roots.iter().filter_map(|id| rows.get(id)).flat_map(|r| SYSTEM_POINTERS.iter().filter_map(|p| r.id(p))).collect();
    let roles = rows
        .iter()
        .map(|(id, row)| {
            let role = if roots.contains(id) {
                Role::Root
            } else if system.contains(id) {
                Role::Other
            } else {
                role_of(row.id("Z_ENT").and_then(|e| names.get(&e).copied()).unwrap_or(""))
            };
            (*id, role)
        })
        .collect();
    let tree = Tree { rows, roles };

    let mut ordered: Vec<(i64, i64)> = tree.rows.iter().map(|(id, row)| (row.int("ZCOLLECTIONINDEX").unwrap_or(i64::MAX), *id)).collect();
    ordered.sort_unstable();
    let mut out = Vec::new();
    let (mut orphans, mut loops) = (0usize, 0usize);
    for (_, id) in ordered {
        let (Some(Role::User(kind)), Some(row)) = (tree.roles.get(&id).copied(), tree.rows.get(&id)) else { continue };
        let parent = match tree.place(id) {
            Place::In(parent) => parent,
            Place::Orphan => {
                orphans = orphans.saturating_add(1);
                None
            }
            Place::Out => continue,
            Place::Loop => {
                loops = loops.saturating_add(1);
                continue;
            }
        };
        out.push(Collection {
            id,
            uuid: row.text("ZUUID").trim().to_string(),
            name: row.text("ZNAME").trim().to_string(),
            kind,
            parent,
            photos: Vec::new(),
            rule: if kind == CollectionKind::SmartAlbum { row.text("ZSMARTCRITERIA").to_string() } else { String::new() },
        });
    }
    if orphans > 0 {
        warnings.push(format!("{orphans} collection(s) are inside a collection the catalog no longer has; they come in at the top level"));
    }
    if loops > 0 {
        warnings.push(format!("{loops} collection(s) are nested in a loop and were left out"));
    }
    members(db, photos, &mut out, warnings)?;
    Ok(out)
}

fn role_of(entity: &str) -> Role {
    match entity {
        "AlbumCollection" => Role::User(CollectionKind::Album),
        "SmartCollection" => Role::User(CollectionKind::SmartAlbum),
        "ProjectCollection" => Role::User(CollectionKind::Project),
        "VirtualFolderCollection" => Role::User(CollectionKind::Group),
        _ => Role::Other,
    }
}

impl Tree<'_> {
    /// Follow a user collection's parents up to the root. Every one has to be a user collection
    /// too; the direct parent, when it is one, is where the collection goes.
    fn place(&self, id: i64) -> Place {
        let parent_of = |id: i64| self.rows.get(&id).and_then(|row| row.id("ZPARENT"));
        let Some(direct) = parent_of(id) else { return Place::In(None) };
        let mut at = direct;
        for _ in 0..MAX_DEPTH {
            match self.roles.get(&at) {
                Some(Role::Root) => return Place::In((at != direct).then_some(direct)),
                Some(Role::Other) => return Place::Out,
                // a missing row further up is that ancestor's own problem
                None => return if at == direct { Place::Orphan } else { Place::In(Some(direct)) },
                Some(Role::User(_)) => match parent_of(at) {
                    Some(next) => at = next,
                    None => return Place::In(Some(direct)),
                },
            }
        }
        Place::Loop
    }
}

/// Fill the albums' photos: `ZIMAGEINCOLLECTION` lists an image (its first variant),
/// `ZVARIANTINCOLLECTION` a further variant. Capture One repeats an album's photos on the
/// project around it, so only albums take photos.
fn members(db: &Db, photos: &Read, collections: &mut [Collection], warnings: &mut Vec<String>) -> Result<(), ReadError> {
    let albums: HashMap<i64, usize> =
        collections.iter().enumerate().filter(|(_, c)| c.kind == CollectionKind::Album).map(|(at, c)| (c.id, at)).collect();
    if albums.is_empty() {
        return Ok(());
    }
    let mut listed: HashSet<(usize, usize)> = HashSet::new();
    let mut add = |collection: Option<i64>, photo: Option<&usize>| {
        let (Some(album), Some(photo)) = (collection.and_then(|id| albums.get(&id)), photo) else { return };
        if listed.insert((*album, *photo))
            && let Some(c) = collections.get_mut(*album)
        {
            c.photos.push(*photo);
        }
    };
    for row in db.optional("ZIMAGEINCOLLECTION", warnings)?.rows() {
        add(row.id("ZCOLLECTION"), row.id("ZIMAGE").and_then(|id| photos.first_of_image.get(&id)));
    }
    for row in db.optional("ZVARIANTINCOLLECTION", warnings)?.rows() {
        add(row.id("ZCOLLECTION"), row.id("ZVARIANT").and_then(|id| photos.by_variant.get(&id)));
    }
    Ok(())
}
