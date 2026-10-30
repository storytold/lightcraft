//! Sorting the photos of a move to another folder (`photo.moveToFolder`) before anything moves:
//! which ones go, which are in that folder already, and which can't go (a name taken there, the
//! file missing or offline, or a photo that can't move as it is: a Local photo or one with no file,
//! and a library photo of the same file as one in the batch, since skipping is by file). `photo.checkMove` reports the sort so a confirmation can show the
//! problems up front; the move itself uses the same sort, and leaves out the photos it was told
//! to skip (`skip`) instead of refusing the batch for them.
//!
//! Reads the disk (one look at each file and each name) but writes nothing.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use crate::cmd::bad;
use crate::rename::RenamePlan;
use crate::{EngineError, Result, Session};

const C: &str = "photo.moveToFolder";

/// Why a photo can't be moved into the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A file of that name is in the folder already, or another photo of the batch brings one.
    Clash,
    /// The file isn't where the library has it: the disk is offline, or it was moved or deleted.
    Missing,
    /// A photo that can't move as it is: not in the library (Local, until it is added) or with no
    /// file on disk (a demo photo); or a library photo of the same file as such a photo in the batch
    /// (skipping is by file). It refuses a move unless it is skipped. Only `photo.checkMove` lists
    /// it, as a problem.
    Unmovable,
    /// Only for a photo told to be skipped that has no problem (any more).
    Asked,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Clash => "clash",
            Kind::Missing => "missing",
            Kind::Unmovable => "unmovable",
            Kind::Asked => "asked",
        }
    }
}

/// One photo that doesn't move: its problem, or that it was skipped.
#[derive(Clone, Debug)]
pub(crate) struct Left {
    pub id: PhotoId,
    pub file: String,
    pub kind: Kind,
    /// For a clash: the name is taken by another photo of the same move, not by a file in the folder.
    pub twin: bool,
}

impl Left {
    pub(crate) fn json(&self) -> Value {
        json!({"id": self.id.0, "name": self.file, "kind": self.kind.word()})
    }

    /// [`Left::json`] for `photo.checkMove`: a clash with another photo of the same move says so
    /// (`twin`), since the fix differs from a name taken in the folder.
    fn check_json(&self) -> Value {
        let mut v = self.json();
        if self.twin {
            v["twin"] = json!(true);
        }
        v
    }
}

/// How the photos of a move sort out.
#[derive(Debug, Default)]
pub(crate) struct Sorted {
    /// One per file that moves (a master and its virtual copies move once).
    pub plans: Vec<RenamePlan>,
    /// Photos that go (virtual copies counted, as the person sees them).
    pub movable: usize,
    /// Photos already in the folder.
    pub here: usize,
    /// Photos that can't go and weren't asked to be skipped: they refuse the move.
    pub problems: Vec<Left>,
    /// Photos the caller asked to leave out, with the problem each has (or [`Kind::Asked`]).
    pub skipped: Vec<Left>,
}

#[derive(Clone, Copy)]
enum Fate {
    Move,
    Here,
    /// A file of that name is in the folder.
    Clash,
    /// Another photo of the same move has the name.
    Twin,
    Missing,
}

/// Sort `ids` for a move into `dir`. A photo that isn't in the library or has no file refuses the
/// move as a whole, unless it is in `skip`; with `lenient` (the check) it is listed in `problems`
/// instead of refusing. A name clash or a missing file goes to `problems`, or to
/// `skipped` when its photo is in `skip`. A photo skipped never holds a name in the folder.
pub(crate) fn sort(s: &Session, dir: &Path, ids: &[PhotoId], skip: &HashSet<u64>, lenient: bool) -> Result<Sorted> {
    // skipping is by file: a virtual copy goes where its master goes
    let skip_paths: HashSet<String> = skip
        .iter()
        .filter_map(|id| match s.catalog.photo(PhotoId(*id)).map(|p| &p.source) {
            Some(Source::File { path }) => Some(path.clone()),
            _ => None,
        })
        .collect();
    let mut out = Sorted::default();
    let mut fates: HashMap<String, Fate> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    // skipped photos that would have moved: their names are checked against the moving ones at the end
    let mut pending: Vec<(usize, String)> = Vec::new();
    // the check (which skips nothing yet): the files of the Local photos listed as problems; a
    // library photo of the same file is listed as `unmovable` too, because skipping is by file and
    // the move would leave it out with them
    let local_files: HashSet<String> = if lenient {
        ids.iter()
            .filter_map(|id| s.catalog.photo(*id))
            .filter(|p| p.local)
            .filter_map(|p| match &p.source {
                Source::File { path } => Some(path.clone()),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    for &id in ids {
        // (no command named: `execute` puts in the one that was asked for)
        let ph = s.catalog.photo(id).ok_or_else(|| bad("", format!("no photo {}", id.0)))?;
        let asked = skip.contains(&id.0);
        // a photo that can't move as it is (Local, or no file) is refused, or left out when it was told to be skipped
        let unmovable = if ph.local {
            Some(format!("{} is not in the library (Local): add it to the library first", ph.file_name))
        } else if !matches!(ph.source, Source::File { .. }) {
            Some(format!("{} has no file on disk, so it can't be moved", ph.file_name))
        } else {
            None
        };
        if let Some(why) = unmovable {
            let left = Left { id, file: ph.file_name.clone(), kind: if asked { Kind::Asked } else { Kind::Unmovable }, twin: false };
            if asked {
                out.skipped.push(left);
            } else if lenient {
                out.problems.push(left);
            } else {
                return Err(EngineError::Other(why));
            }
            continue;
        }
        let Source::File { path } = &ph.source else { continue };
        if local_files.contains(path) {
            out.problems.push(Left { id, file: ph.file_name.clone(), kind: Kind::Unmovable, twin: false });
            continue;
        }
        let left_out = asked || skip_paths.contains(path);
        let fate = match fates.get(path) {
            Some(f) => *f,
            None => {
                let src = Path::new(path);
                let file = src.file_name().ok_or_else(|| EngineError::Other(format!("{path} has no file name")))?;
                let to = dir.join(file);
                let to_s = to.to_string_lossy().to_string();
                let fate = if !src.is_file() {
                    Fate::Missing
                } else if src.parent() == Some(dir) {
                    Fate::Here
                } else if to.exists() {
                    Fate::Clash
                } else if !left_out && !taken.insert(to_s.to_lowercase()) {
                    Fate::Twin
                } else {
                    Fate::Move
                };
                if matches!(fate, Fate::Move) && !left_out {
                    let name = ph.file_name.clone();
                    out.plans.push(RenamePlan { id: id.0, from: name.clone(), to: name, from_path: Some(path.clone()), to_path: Some(to_s) });
                }
                fates.insert(path.clone(), fate);
                fate
            }
        };
        let kind = match fate {
            Fate::Clash | Fate::Twin => Kind::Clash,
            Fate::Missing => Kind::Missing,
            Fate::Move | Fate::Here => Kind::Asked,
        };
        let left = Left { id, file: ph.file_name.clone(), kind, twin: matches!(fate, Fate::Twin) };
        match (left_out, fate) {
            (true, _) => {
                if matches!(fate, Fate::Move) {
                    pending.push((
                        out.skipped.len(),
                        Path::new(path).file_name().map(|f| dir.join(f).to_string_lossy().to_lowercase()).unwrap_or_default(),
                    ));
                }
                out.skipped.push(left);
            }
            (false, Fate::Move) => out.movable += 1,
            (false, Fate::Here) => out.here += 1,
            (false, Fate::Clash | Fate::Twin | Fate::Missing) => out.problems.push(left),
        }
    }
    // a skipped photo whose name a moving photo takes is a clash, whichever of the two came first
    for (i, name) in pending {
        if taken.contains(&name) {
            out.skipped[i].kind = Kind::Clash;
            out.skipped[i].twin = true;
        }
    }
    Ok(out)
}

/// The `skip` parameter: photo ids to leave out of the move.
pub(crate) fn skip_param(p: &Value) -> Result<HashSet<u64>> {
    match p.get("skip") {
        None | Some(Value::Null) => Ok(HashSet::new()),
        Some(Value::Array(a)) => a.iter().map(|v| v.as_u64().ok_or_else(|| bad(C, "`skip` must be photo ids (whole numbers)"))).collect(),
        Some(_) => Err(bad(C, "`skip` must be photo ids (whole numbers)")),
    }
}

/// The folder as a person names it: its last part.
pub(crate) fn folder_name(folder: &str) -> String {
    Path::new(folder).file_name().map_or_else(|| folder.to_string(), |f| f.to_string_lossy().to_string())
}

/// The file names of `of`, each once, in order.
fn unique_names(of: &[&Left]) -> Vec<String> {
    let mut seen = HashSet::new();
    of.iter().filter(|l| seen.insert(l.file.as_str())).map(|l| l.file.clone()).collect()
}

/// Up to three names, then "and N more".
fn names(of: &[String]) -> String {
    let shown = &of[..of.len().min(3)];
    match of.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// Why a move with `problems` was refused, in plain words with what to do about it.
pub(crate) fn refusal(folder: &str, problems: &[Left]) -> String {
    let of =
        |kind: Kind, twin: bool| -> Vec<String> { unique_names(&problems.iter().filter(|l| l.kind == kind && l.twin == twin).collect::<Vec<_>>()) };
    let (clashes, twins, missing) = (of(Kind::Clash, false), of(Kind::Clash, true), of(Kind::Missing, false));
    let verb = |n: usize| if n == 1 { "is" } else { "are" };
    let dest = folder_name(folder);
    let mut text = String::from("Nothing was moved. ");
    if !clashes.is_empty() {
        text += &format!("{} {} already in “{dest}”. ", names(&clashes), verb(clashes.len()));
    }
    if !twins.is_empty() {
        text += &format!("More than one photo being moved is named {}, and only one of each name can go into “{dest}”. ", names(&twins));
    }
    if !missing.is_empty() {
        text += &format!("{} {} missing or offline. ", names(&missing), verb(missing.len()));
    }
    let mut fixes = Vec::new();
    if !clashes.is_empty() || !twins.is_empty() {
        fixes.push("rename the clashing photos");
    }
    if !missing.is_empty() {
        fixes.push("reconnect the disk or use File → Find Missing Photos…");
    }
    text += &format!("To go on: {}, or leave these photos out of the move.", fixes.join(", "));
    text
}

/// `photo.checkMove`: how a move of the photos (default: the selection) into `folder` would go,
/// without moving anything.
pub(crate) fn check_command(s: &Session, p: &Value) -> Result<Value> {
    let folder = crate::cmd::browse::folder_param(p, "folder").ok_or_else(|| bad("photo.checkMove", "missing folder"))?;
    let dir = Path::new(folder);
    if !dir.is_dir() {
        return Err(EngineError::Other(format!("“{}” is not a folder (offline, moved or deleted?)", folder_name(folder))));
    }
    let ids = s.targets(p).map_err(|e| match e {
        EngineError::BadParams { cmd, msg } if cmd.is_empty() => bad("photo.checkMove", msg),
        e => e,
    })?;
    let sorted = sort(s, dir, &ids, &HashSet::new(), true)?;
    Ok(json!({
        "folder": folder,
        "photos": ids.len(),
        "movable": sorted.movable,
        "here": sorted.here,
        "problems": sorted.problems.iter().map(Left::check_json).collect::<Vec<_>>(),
    }))
}
