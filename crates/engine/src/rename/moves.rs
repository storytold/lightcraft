use super::fs::{MoveFs, RealFs, folded_path, is_respelling, move_one, sidecars};
use super::sidecar::{RetainedPacket, SidecarMove, advance_packets, note_companions, packet_aliases};
use super::{HashMap, HashSet, Op, Path, PathBuf, PhotoId, SidecarNaming, Source};

/// Why [`move_file`] failed, and what it left behind.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveError {
    /// What went wrong, including every step that could not be undone and where that file is now.
    pub message: String,
    /// The file itself ended up at the new path: it moved, a sidecar didn't, and moving the file
    /// back failed too. Its sidecars that did move stay with it.
    pub moved: bool,
    pub(crate) pending_sidecars: Vec<SidecarMove>,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Move `from` to `to` (never over an existing file) together with its sidecars. On error the
/// moves are undone; whatever can't be undone is named in the error (see [`MoveError`]).
pub fn move_file(from: &str, to: &str) -> std::result::Result<(), MoveError> {
    move_file_with(&RealFs, from, to)
}

/// [`move_file`] on the file system `fs`.
pub(crate) fn move_file_with(fs: &dyn MoveFs, from: &str, to: &str) -> std::result::Result<(), MoveError> {
    if Path::new(from) == Path::new(to) {
        return Ok(());
    }
    let fail = |message: String| MoveError { message, moved: false, pending_sidecars: Vec::new() };
    let (f, t) = (Path::new(from), Path::new(to));
    // `to` may be this very file spelled differently (case-insensitive volume); any other file
    // there — e.g. `img.jpg` next to `IMG.JPG` on a case-sensitive volume — is never replaced
    if fs.exists(t) && !is_respelling(fs, f, t) {
        return Err(fail(format!("{to} already exists")));
    }
    // An upper-case `.XMP` spelling that only answers through a case-insensitive volume is the
    // lower-case sidecar: track and move it under its own name, so `a.xmp` doesn't become `b.XMP`.
    let present = |(sc, _, upper): &(PathBuf, SidecarNaming, bool)| fs.is_file(sc) && (!*upper || fs.listed(sc));
    let mut packets: Vec<RetainedPacket> =
        sidecars(from).into_iter().filter(present).map(|(path, _, _)| RetainedPacket { version: fs.packet_version(&path).ok(), path }).collect();
    move_one(fs, f, t).map_err(|e| fail(format!("rename {from}: {e}")))?;
    let mut done: Vec<SidecarMove> = Vec::new();
    let mut remaining = sidecars(from).into_iter().filter(present).map(|(sc, naming, upper)| {
        let mut dst = crate::sidecar::sidecar_path(to, naming);
        if upper {
            dst = dst.with_extension("XMP");
        }
        // A stem sidecar another photo still uses is copied, not taken away (issue #92).
        let copied = naming == SidecarNaming::Stem && fs.stem_sibling(&sc).is_some();
        SidecarMove { from: sc, to: dst, copied, companions: Vec::new() }
    });
    while let Some(m) = remaining.next() {
        if !m.needed(fs) {
            continue; // never overwrite: the old sidecar stays where it was
        }
        let aliases = packet_aliases(&packets, fs, &m.from);
        if let Err(e) = m.apply(fs) {
            let mut message = format!("rename {}: {e}", m.from.display());
            // The file first: if it can't go back, sidecars that moved stay with it.
            if let Err(e) = move_one(fs, t, f) {
                message.push_str(&format!("; {from} could not be moved back ({e}): it is now {to}"));
                if !done.is_empty() {
                    let names: Vec<String> = done.iter().map(|m| m.to.display().to_string()).collect();
                    message.push_str(&format!(" with its sidecar {}", names.join(", ")));
                }
                let mut pending_sidecars = vec![m];
                for m in remaining.filter(|m| m.needed(fs)) {
                    // The two case spellings can name one file. Keep one operation for
                    // those aliases, while retaining distinct sidecars on sensitive volumes.
                    let alias = pending_sidecars.iter().any(|p| {
                        folded_path(&p.from.to_string_lossy()) == folded_path(&m.from.to_string_lossy())
                            && folded_path(&p.to.to_string_lossy()) == folded_path(&m.to.to_string_lossy())
                            && fs.same_file(&p.from, &m.from)
                    });
                    if !alias {
                        pending_sidecars.push(m);
                    }
                }
                note_companions(&mut pending_sidecars, packets);
                return Err(MoveError { message, moved: true, pending_sidecars });
            }
            let mut pending_sidecars = Vec::new();
            for m in done.iter().rev() {
                if m.copied {
                    // The original is still there: a leftover copy loses nothing.
                    if fs.remove_file(&m.to).is_ok() {
                        packets.retain(|p| p.path != m.to);
                    }
                } else {
                    let aliases = packet_aliases(&packets, fs, &m.to);
                    if let Err(e) = move_one(fs, &m.to, &m.from) {
                        message.push_str(&format!("; sidecar {} could not be moved back ({e}): it is now {}", m.from.display(), m.to.display()));
                        pending_sidecars.push(SidecarMove { from: m.to.clone(), to: m.from.clone(), copied: false, companions: Vec::new() });
                    } else {
                        advance_packets(&mut packets, &aliases, &m.from, false, fs);
                    }
                }
            }
            note_companions(&mut pending_sidecars, packets);
            return Err(MoveError { message, moved: false, pending_sidecars });
        }
        advance_packets(&mut packets, &aliases, &m.to, m.copied, fs);
        done.push(m);
    }
    Ok(())
}

/// What a failed batch of moves ([`move_all`]) left behind.
#[derive(Debug)]
pub(crate) struct BatchMoveError {
    /// The first failure and every move that could not be undone.
    pub message: String,
    /// Moves that stayed done, (from, to): those files are at `to` now.
    pub stuck: Vec<(String, String)>,
    /// Sidecars still separated from the path where their photo ended up, in either direction.
    pub pending_sidecars: Vec<SidecarMove>,
}

/// Move the files `moves` names, all or nothing: after a failure the earlier moves are undone.
/// Moves that can't be undone (e.g. the network share went away) are reported, not ignored.
pub(crate) fn move_all(fs: &dyn MoveFs, moves: &[(String, String)]) -> std::result::Result<(), BatchMoveError> {
    let mut done: Vec<&(String, String)> = Vec::new();
    for m in moves {
        let Err(e) = move_file_with(fs, &m.0, &m.1) else {
            done.push(m);
            continue;
        };
        let mut message = e.message;
        let mut pending_sidecars = e.pending_sidecars;
        let mut stuck = Vec::new();
        if e.moved {
            stuck.push(m.clone());
        }
        for d in done.iter().rev() {
            if let Err(mut e) = move_file_with(fs, &d.1, &d.0) {
                message.push_str(&format!("; {} could not be moved back to {}: {}", d.1, d.0, e.message));
                if !e.moved {
                    stuck.push((*d).clone());
                }
                pending_sidecars.append(&mut e.pending_sidecars);
            }
        }
        return Err(BatchMoveError { message, stuck, pending_sidecars });
    }
    Ok(())
}

/// `rest`, what is left of a rollback that failed part-way, once the files were moved back (`moved_back`, the result of
/// `move_all` on the moves back): the `SetFile`s of a photo whose file did go back become `Relink`s, the same change to the
/// catalog that moves no file; those of a photo whose file stayed where it went keep moving it. A photo is judged by the
/// path its last `SetFile` points to, as `Session::file_moves` plans its move.
pub(crate) fn rest_after_move_back(rest: Op, moved_back: &std::result::Result<(), BatchMoveError>) -> Op {
    fn targets<'a>(op: &'a Op, out: &mut HashMap<PhotoId, &'a str>) {
        match op {
            Op::Batch { ops } => ops.iter().for_each(|op| targets(op, out)),
            Op::SetFile { id, source: Source::File { path }, .. } => {
                out.insert(*id, path);
            }
            _ => {}
        }
    }
    fn rewrite(op: Op, back: &HashSet<PhotoId>) -> Op {
        match op {
            Op::Batch { ops } => Op::Batch { ops: ops.into_iter().map(|op| rewrite(op, back)).collect() },
            Op::SetFile { id, file_name, source } if back.contains(&id) => Op::Relink { id, file_name, source, format: None },
            op => op,
        }
    }
    let mut to = HashMap::new();
    targets(&rest, &mut to);
    // (on a failed move back, `stuck` holds the moves back that stayed done: those files are at their old path)
    let back: HashSet<PhotoId> = to
        .into_iter()
        .filter(|(_, path)| match moved_back {
            Ok(()) => true,
            Err(e) => e.stuck.iter().any(|m| Path::new(&m.1) == Path::new(path)),
        })
        .map(|(id, _)| id)
        .collect();
    rewrite(rest, &back)
}

/// Final `SetFile` per photo (nested batches included), filtered by destination only afterward.
pub(super) fn set_file_ops_to(op: &Op, to: &HashSet<&str>, out: &mut Vec<Op>) {
    let mut pending = vec![op];
    let mut order = Vec::new();
    let mut final_ops = HashMap::new();
    while let Some(op) = pending.pop() {
        match op {
            Op::Batch { ops } => pending.extend(ops.iter().rev()),
            Op::SetFile { id, .. } if final_ops.insert(*id, op).is_none() => order.push(*id),
            _ => {}
        }
    }
    out.extend(order.into_iter().filter_map(|id| {
        let op = final_ops.get(&id).copied()?;
        matches!(op, Op::SetFile { source: Source::File { path }, .. } if to.contains(path.as_str())).then(|| op.clone())
    }));
}
