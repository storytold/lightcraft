//! XMP sidecars: see [`dac_engine_library::sidecar`]; the session-side half (save, read back,
//! auto-write) lives here.

use std::path::PathBuf;

use dac_catalog::{MediaKind, Op, PhotoId};

pub use dac_engine_library::sidecar::*;

use crate::{EngineError, Result, Session};

impl Session {
    /// The sidecar naming for photo `id`: the library's, except under Stem naming when another
    /// catalogued file shares the stem (`IMG_0001.CR3` + `IMG_0001.JPG`) and owns the stem
    /// sidecar — a raw first, else the first by file name. The others use Full naming
    /// (`IMG_0001.JPG.xmp`), so their metadata never overwrites each other.
    pub fn sidecar_naming(&self, id: PhotoId) -> SidecarNaming {
        match self.catalog.photo(id) {
            Some(p) if self.xmp.naming == SidecarNaming::Stem => StemOwners::of(&self.catalog).naming(p, self.xmp.naming),
            _ => self.xmp.naming,
        }
    }

    /// Write the photo's XMP sidecar, merged into an existing one (see the module docs).
    pub fn save_sidecar(&self, id: PhotoId) -> Result<SidecarSaved> {
        self.save_sidecar_with(id, &StemOwners::of(&self.catalog))
    }

    pub(crate) fn save_sidecar_with(&self, id: PhotoId, owners: &StemOwners) -> Result<SidecarSaved> {
        let p = self.catalog.photo(id).ok_or(dac_catalog::CatalogError::NoPhoto(id))?;
        if p.copy_of.is_some() {
            return Err(EngineError::Other(format!("{} is a virtual copy: its settings live only in the library", p.file_name)));
        }
        let orig = file_path(p).ok_or_else(|| EngineError::Other(format!("{} is not a file on disk", p.file_name)))?;
        let path = sidecar_path(orig, owners.naming(p, self.xmp.naming));
        let err = |e: std::io::Error| EngineError::Other(format!("{}: {e}", path.display()));
        let (data, merged, backup) = sidecar_contents(&path, sidecar_packet(p, &self.catalog), &(self.clock)()).map_err(err)?;
        write_atomic(&path, data.as_bytes()).map_err(err)?;
        Ok(SidecarSaved { path, merged, backup })
    }

    /// The op that applies a photo's sidecar (or embedded XMP) to the catalog, if there is one.
    pub fn read_sidecar_op(&self, id: PhotoId) -> Result<Option<(Op, PathBuf)>> {
        let p = self.catalog.photo(id).ok_or(dac_catalog::CatalogError::NoPhoto(id))?;
        let Some(orig) = file_path(p) else { return Ok(None) };
        let Some((packet, from)) = read_packet(orig, p.kind, self.sidecar_naming(id)) else { return Ok(None) };
        let sc = parse_sidecar(&packet, p.kind == MediaKind::Raw)
            .map_err(|e| EngineError::Other(format!("{}: {e}", from.display())))?
            .resolve_label(&self.catalog);
        let mut q = (**p).clone();
        let develop_changed = merge_into(&mut q, &sc, &(self.clock)());
        let mut ops = vec![
            Op::SetRating { id, rating: q.rating },
            Op::SetFlag { id, flag: q.flag },
            Op::SetLabel { id, label: q.label },
            Op::SetMeta { id, meta: Box::new(q.meta.clone()) },
        ];
        if q.captured != p.captured {
            ops.push(Op::SetCaptured { id, captured: q.captured.clone() });
        }
        if develop_changed {
            ops.extend(self.develop_op(id, (*q.develop).clone(), "Read Metadata from File"));
        }
        Ok(Some((Op::Batch { ops }, from)))
    }

    /// Auto-write: sidecars for photos changed by `ops` (errors are logged, not returned).
    pub(crate) fn auto_write_sidecars(&self, ops: &[Op]) {
        let mut ids = Vec::new();
        ops.iter().for_each(|o| op_photos(o, &mut ids));
        if ids.is_empty() {
            return;
        }
        let owners = StemOwners::of(&self.catalog);
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| file_path(p).is_some() && p.copy_of.is_none())
                && let Err(e) = self.save_sidecar_with(id, &owners)
            {
                log::warn!("auto-write XMP: {e}");
            }
        }
    }
}
