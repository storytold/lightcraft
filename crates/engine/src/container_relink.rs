//! iOS (and its simulator) can give an app a new data-container folder, `.../Containers/Data/Application/<UUID>/`,
//! when it is reinstalled or updated. Photos added in place store absolute paths, so every one of them then points
//! into a container that no longer exists and shows as an offline original. This finds such photos at launch and
//! relinks each whose file is in the same place inside the current container.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Source};

use crate::{Result, Session};

const MARKER: &str = "/Containers/Data/Application/";

/// `path` with its container folder (`…/Containers/Data/Application/<UUID>`) replaced by `container`, if it has one.
fn in_container(path: &str, container: &Path) -> Option<PathBuf> {
    let at = path.find(MARKER)?.checked_add(MARKER.len())?;
    let (_old_uuid, inside) = path.get(at..)?.split_once('/')?;
    if inside.is_empty() {
        return None;
    }
    Some(container.join(inside))
}

impl Session {
    /// Relink the photos whose file is gone from an old app-container path but sits at the same place inside
    /// `container` (the current one). Returns how many were relinked.
    pub fn relink_after_container_change(&mut self, container: &Path) -> Result<usize> {
        let mut ops = Vec::new();
        for photo in self.catalog.photos() {
            let Source::File { path } = &photo.source else { continue };
            if Path::new(path).exists() {
                continue;
            }
            let Some(candidate) = in_container(path, container) else { continue };
            if candidate.is_file() {
                ops.push(crate::cmd::missing::relink_op(photo.id, &candidate.to_string_lossy()));
            }
        }
        let n = ops.len();
        if n > 0 {
            self.commit("Relink Photos After Update", Op::Batch { ops })?;
        }
        Ok(n)
    }
}

impl Session {
    /// Remove the procedural demo photos a library was seeded with (the iPad's library holds only the user's own
    /// photos). Not undoable: this is housekeeping at launch. Returns how many were removed.
    pub fn remove_demo_photos(&mut self) -> Result<usize> {
        let ops: Vec<Op> = self.catalog.photos().filter(|p| matches!(p.source, Source::Demo { .. })).map(|p| Op::RemovePhoto { id: p.id }).collect();
        let n = ops.len();
        if n > 0 {
            self.commit("Remove Demo Photos", Op::Batch { ops })?;
            self.undo.clear();
            self.redo.clear();
            // the restored selection may name a demo photo that is now gone
            self.selection.ids.retain(|id| self.catalog.photo(*id).is_some());
            self.selection.active = self.selection.active.filter(|id| self.catalog.photo(*id).is_some());
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_container_paths_are_remapped_and_what_follows_the_uuid_is_kept() {
        let new = Path::new("/var/mobile/Containers/Data/Application/NEW");
        assert_eq!(
            in_container("/var/mobile/Containers/Data/Application/OLD/Documents/TestPhotos/a b.dng", new),
            Some(new.join("Documents/TestPhotos/a b.dng"))
        );
        assert_eq!(in_container("/Users/me/Pictures/a.jpg", new), None);
        assert_eq!(in_container("/var/mobile/Containers/Data/Application/OLD/", new), None);
        assert_eq!(in_container("/var/mobile/Containers/Data/Application/OLD", new), None);
    }
}
