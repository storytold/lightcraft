use super::fs::{MoveFs, is_respelling, move_one};
use super::{Path, PathBuf};

/// An unfinished sidecar operation. Keep its exact paths and any recorded copy requirement: the photo may
/// already have followed its new path, so a retry cannot infer this work from the catalog.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SidecarMove {
    pub from: PathBuf,
    pub to: PathBuf,
    pub(super) copied: bool,
    /// Observed packets in the partial move, including the separated source. Absent planned
    /// destinations are never owned; a completed repair advances these observations.
    pub(super) companions: Vec<RetainedPacket>,
}

/// A content version, not an assertion that a pathname still names the same packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PacketVersion {
    pub(super) len: u64,
    pub(super) hash: lightcraft_preview::Hash128,
}

impl PacketVersion {
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self { len: bytes.len() as u64, hash: lightcraft_preview::hash_bytes(bytes) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RetainedPacket {
    pub(super) path: PathBuf,
    // An unreadable observation remains untrusted even if a later read succeeds.
    pub(super) version: Option<PacketVersion>,
}

impl RetainedPacket {
    pub(super) fn validate(&self, fs: &dyn MoveFs) -> std::result::Result<(), String> {
        if self.version.is_none() {
            return Err(format!("can't verify retained XMP {}: its content was unreadable when the partial move was recorded", self.path.display()));
        }
        let current = fs.packet_version(&self.path).map_err(|e| format!("can't verify retained XMP {}: {e}", self.path.display()))?;
        if self.version != Some(current) {
            return Err(format!(
                "retained XMP {} changed since the partial move; packet ownership must be resolved before retrying",
                self.path.display()
            ));
        }
        Ok(())
    }
}

/// Advance only packets whose operation actually succeeded. A copy retains its original;
/// a move retires vanished source aliases and adds the observed destination.
pub(super) fn advance_packets(packets: &mut Vec<RetainedPacket>, aliases: &[PathBuf], to: &Path, copied: bool, fs: &dyn MoveFs) {
    let Some(mut packet) = packets.iter().find(|p| aliases.contains(&p.path)).cloned() else { return };
    if !copied {
        packets.retain(|p| !aliases.contains(&p.path) || fs.is_file(&p.path));
    }
    packet.path = to.to_path_buf();
    if !packets.iter().any(|p| p.path == packet.path) {
        packets.push(packet);
    }
}

pub(super) fn packet_aliases(packets: &[RetainedPacket], fs: &dyn MoveFs, from: &Path) -> Vec<PathBuf> {
    packets.iter().filter(|p| p.path == from || fs.same_file(&p.path, from)).map(|p| p.path.clone()).collect()
}

impl SidecarMove {
    fn validate_packets(&self, fs: &dyn MoveFs) -> std::result::Result<(), String> {
        for packet in &self.companions {
            packet.validate(fs)?;
        }
        Ok(())
    }

    /// Validate a queued retry and return the copy decision for this physical operation.
    pub(super) fn validate_repair(&self, fs: &dyn MoveFs) -> std::result::Result<bool, String> {
        self.validate_packets(fs)?;
        let respelling = is_respelling(fs, &self.from, &self.to);
        // Batch rollback may have restored another photo using this source packet.
        let copied = self.copied || (!respelling && fs.stem_sibling(&self.from).is_some());
        if fs.exists(&self.to) && (copied || !respelling) {
            return Err(format!("{} already exists while repairing {} → {}", self.to.display(), self.from.display(), self.to.display()));
        }
        Ok(copied)
    }

    pub(super) fn needed(&self, fs: &dyn MoveFs) -> bool {
        fs.is_file(&self.from) && !(fs.exists(&self.to) && (self.copied || !is_respelling(fs, &self.from, &self.to)))
    }

    pub(super) fn touches(&self, fs: &dyn MoveFs, path: &Path) -> bool {
        let same_path = |endpoint: &Path| {
            endpoint == path
                || fs.same_file(endpoint, path)
                || (endpoint.file_name() == path.file_name()
                    && endpoint.parent().zip(path.parent()).is_some_and(|(a, b)| a == b || fs.same_file(a, b)))
        };
        same_path(&self.from) || same_path(&self.to)
    }

    pub(super) fn apply(&self, fs: &dyn MoveFs) -> std::io::Result<()> {
        if self.copied { fs.copy_no_replace(&self.from, &self.to) } else { move_one(fs, &self.from, &self.to) }
    }
}

/// Complete known unfinished sidecars before moving any more photos. Completed operations
/// leave the queue; a failure retains that operation and the rest, without replacing a file.
pub(crate) fn retry_sidecars(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>) -> std::result::Result<(), String> {
    validate_sidecars(fs, pending)?;
    while !pending.is_empty() {
        apply_repair(fs, pending, 0)?;
    }
    Ok(())
}

/// Preflight a detached history entry before global queues are allowed to retire. The
/// caller can validate without mutating it; retry revalidates before each physical operation.
pub(crate) fn validate_sidecars(fs: &dyn MoveFs, pending: &[SidecarMove]) -> std::result::Result<(), String> {
    for repair in pending {
        repair.validate_repair(fs)?;
    }
    Ok(())
}

/// Finish one operation and transfer its observed content to the remaining repairs. Do not
/// infer ownership by reading an occupied destination or by retaining a planned pathname.
fn apply_repair(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>, i: usize) -> std::result::Result<(), String> {
    let Some(mut repair) = pending.get(i).cloned() else { return Ok(()) };
    repair.copied = repair.validate_repair(fs)?;
    let aliases = packet_aliases(&repair.companions, fs, &repair.from);
    repair.apply(fs).map_err(|e| format!("rename {} → {}: {e}", repair.from.display(), repair.to.display()))?;
    for remaining in pending.iter_mut() {
        advance_packets(&mut remaining.companions, &aliases, &repair.to, repair.copied, fs);
    }
    pending.remove(i);
    Ok(())
}

/// Repair only operations that affect a sidecar being saved. Unrelated unavailable originals
/// must not block healthy metadata saves. Completed repairs retire; failures stay queued.
pub(crate) fn retry_sidecars_for_paths(fs: &dyn MoveFs, pending: &mut Vec<SidecarMove>, paths: &[PathBuf]) -> std::result::Result<(), String> {
    for repair in pending.iter().filter(|m| paths.iter().any(|path| m.touches(fs, path))) {
        repair.validate_repair(fs)?;
    }
    let mut i = 0;
    while let Some(m) = pending.get(i) {
        if paths.iter().any(|path| m.touches(fs, path)) {
            // Path/alias equality only locates an observation; validation above proves its
            // content. A newly occupied, previously absent peer destination is a collision.
            if let Some(path) = paths.iter().find(|path| {
                fs.exists(path)
                    && !fs.same_file(path, &m.from)
                    && !fs.same_file(path, &m.to)
                    && !m.companions.iter().any(|known| fs.same_file(path, &known.path))
            }) {
                return Err(format!("{} already exists while repairing {} → {}", path.display(), m.from.display(), m.to.display()));
            }
            apply_repair(fs, pending, i)?;
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Every record came from an actual packet observed before this move. Successful physical
/// operations have already advanced its path; missing planned destinations are not recorded.
pub(super) fn note_companions(pending: &mut [SidecarMove], packets: Vec<RetainedPacket>) {
    for repair in pending {
        repair.companions.clone_from(&packets);
    }
}
