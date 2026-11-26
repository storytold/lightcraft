use super::fs::{MoveFs, RealFs, folded_path, is_respelling, move_one, sidecars};
use super::sidecar::{RetainedPacket, SidecarMove, advance_packets, note_companions, packet_aliases};
use super::{HashMap, HashSet, Op, Path, PathBuf, SidecarNaming, Source};

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

/// How a moving file stands to the stem-named sidecar (`IMG_1.xmp`) of its stem, as the library
/// decides it ([`crate::sidecar::StemOwners`]: a raw first, then the file name).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StemShare {
    /// No other catalogued file has this stem: a stem sidecar that another file on disk still
    /// shares is copied, not taken away (issue #92).
    #[default]
    Alone,
    /// Another catalogued file has this stem, and this file owns the stem sidecar: it moves with
    /// this file, so the file left behind doesn't read a copy of it (issue #4713).
    Owner,
    /// Another catalogued file has this stem and owns the stem sidecar: it stays, and no copy of
    /// it goes along under the new name (issue #4713).
    Sibling,
}

/// [`StemShare`] of each move, by the pair of its paths in either direction ([`share_key`]): the
/// move back after a failure, and the undo or redo of the move, treat the stem sidecar as the
/// move did. Keyed by both paths, so a move whose destination is another move's source keeps its
/// own share. A move that isn't listed is [`StemShare::Alone`].
pub(crate) type StemShares = HashMap<(String, String), StemShare>;

/// The key of the move between `a` and `b` (either way round) in [`StemShares`].
pub(crate) fn share_key(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) }
}

/// Move `from` to `to` (never over an existing file) together with its sidecars. On error the
/// moves are undone; whatever can't be undone is named in the error (see [`MoveError`]).
pub fn move_file(from: &str, to: &str) -> std::result::Result<(), MoveError> {
    move_file_with(&RealFs, from, to)
}

/// [`move_file`] on the file system `fs`.
pub(crate) fn move_file_with(fs: &dyn MoveFs, from: &str, to: &str) -> std::result::Result<(), MoveError> {
    move_file_shared(fs, from, to, StemShare::Alone)
}

/// [`move_file_with`] for a file that stands to the sidecar of its stem as `share` says.
pub(crate) fn move_file_shared(fs: &dyn MoveFs, from: &str, to: &str, share: StemShare) -> std::result::Result<(), MoveError> {
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
    // A stem sidecar another catalogued file owns isn't this file's: not moved, copied or tracked.
    let own: Vec<(PathBuf, SidecarNaming, bool)> =
        sidecars(from).into_iter().filter(|(_, naming, _)| share != StemShare::Sibling || *naming != SidecarNaming::Stem).collect();
    let mut packets: Vec<RetainedPacket> = own
        .iter()
        .filter(|sc| present(sc))
        .map(|(path, _, _)| RetainedPacket { version: fs.packet_version(path).ok(), path: path.clone() })
        .collect();
    move_one(fs, f, t).map_err(|e| fail(format!("rename {from}: {e}")))?;
    let mut done: Vec<SidecarMove> = Vec::new();
    let mut remaining = own.into_iter().filter(present).map(|(sc, naming, upper)| {
        let mut dst = crate::sidecar::sidecar_path(to, naming);
        if upper {
            dst = dst.with_extension("XMP");
        }
        // A stem sidecar another photo still uses is copied, not taken away (issue #92), unless
        // the library says it is this file's (issue #4713).
        let owned_stem = naming == SidecarNaming::Stem && share == StemShare::Owner;
        let copied = naming == SidecarNaming::Stem && !owned_stem && fs.stem_sibling(&sc).is_some();
        SidecarMove { from: sc, to: dst, copied, owned_stem, companions: Vec::new() }
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
                        pending_sidecars.push(SidecarMove {
                            from: m.to.clone(),
                            to: m.from.clone(),
                            copied: false,
                            owned_stem: m.owned_stem,
                            companions: Vec::new(),
                        });
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

/// What a failed batch of moves ([`move_all_shared`]) left behind.
#[derive(Debug)]
pub(crate) struct BatchMoveError {
    /// The first failure and every move that could not be undone.
    pub message: String,
    /// Moves that stayed done, (from, to): those files are at `to` now.
    pub stuck: Vec<(String, String)>,
    /// Sidecars still separated from the path where their photo ended up, in either direction.
    pub pending_sidecars: Vec<SidecarMove>,
}

/// [`move_all_shared`] with every file [`StemShare::Alone`].
#[cfg(test)]
pub(crate) fn move_all(fs: &dyn MoveFs, moves: &[(String, String)]) -> std::result::Result<(), BatchMoveError> {
    move_all_shared(fs, moves, &StemShares::new())
}

/// Move the files `moves` names, all or nothing: after a failure the earlier moves are undone.
/// Moves that can't be undone (e.g. the network share went away) are reported, not ignored. Each
/// file stands to the sidecar of its stem as `shares` says.
pub(crate) fn move_all_shared(fs: &dyn MoveFs, moves: &[(String, String)], shares: &StemShares) -> std::result::Result<(), BatchMoveError> {
    let share = |a: &str, b: &str| shares.get(&share_key(a, b)).copied().unwrap_or_default();
    let mut done: Vec<&(String, String)> = Vec::new();
    for m in moves {
        let Err(e) = move_file_shared(fs, &m.0, &m.1, share(&m.0, &m.1)) else {
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
            if let Err(mut e) = move_file_shared(fs, &d.1, &d.0, share(&d.1, &d.0)) {
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
