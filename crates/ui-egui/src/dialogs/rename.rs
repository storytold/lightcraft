use super::LightcraftApp;

/// Rows the Rename dialog previews.
pub(super) const RENAME_PREVIEW_ROWS: usize = 6;

/// The Rename dialog's preview: its first rows (`None` while being planned) and how many files
/// are renamed. Which names are taken is checked on disk, which can block on a slow drive, so the
/// rows are planned on a worker thread, and only when the template, start number, photos or
/// catalog change — never once per frame.
pub(crate) fn rename_preview(
    app: &mut LightcraftApp,
    ctx: &egui::Context,
    template: &str,
    start: usize,
) -> (Option<Vec<lightcraft_engine::rename::RenamePlan>>, usize) {
    use std::hash::{Hash, Hasher};
    type Rows = std::sync::Arc<std::sync::Mutex<Option<Vec<lightcraft_engine::rename::RenamePlan>>>>;
    let ids = app.session.selected();
    let photos = lightcraft_engine::rename::rename_photos(&app.session.catalog, &ids);
    let total = photos.len();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (template, start, app.session.catalog.revision, &ids).hash(&mut h);
    let key = h.finish();
    let id = egui::Id::unique("rename-preview-rows");
    let read = |rows: &Rows| rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    if let Some((k, rows)) = ctx.data(|d| d.get_temp::<(u64, Rows)>(id))
        && k == key
    {
        return (read(&rows), total);
    }
    let rows: Rows = Default::default();
    let first: Vec<_> = photos.into_iter().take(RENAME_PREVIEW_ROWS).collect();
    let (out, template, repaint) = (rows.clone(), template.to_string(), ctx.clone());
    let exists = app.session.media.availability.probe();
    let work = move || {
        let plans = lightcraft_engine::rename::plan_rename_photos(&first, &template, start, &|f| exists(f));
        *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(plans);
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("lc-rename-preview".into()).spawn(work) {
        log::warn!("rename preview: {e}");
    }
    #[cfg(target_arch = "wasm32")]
    work();
    ctx.data_mut(|d| d.insert_temp(id, (key, rows.clone())));
    (read(&rows), total)
}

pub(super) struct PersonRenamePlan {
    pub(super) faces: usize,
    pub(super) needs_merge: bool,
    /// Why Rename stays disabled. `None` when the typed name can be applied.
    pub(super) block: Option<&'static str>,
}

/// What renaming `from` to `to` would do, using the same rules as `faces.renamePerson`.
pub(super) fn person_rename_plan(cat: &lightcraft_catalog::Catalog, from: &str, to: &str) -> PersonRenamePlan {
    let to_trim = to.trim();
    if to_trim.is_empty() {
        return PersonRenamePlan { faces: 0, needs_merge: false, block: Some("Type a name") };
    }
    if to_trim.chars().count() > 200 || to_trim.chars().any(char::is_control) {
        return PersonRenamePlan { faces: 0, needs_merge: false, block: Some("A name is at most 200 characters, without control characters") };
    }
    let from_key = from.trim().to_lowercase();
    let to_key = to_trim.to_lowercase();
    let mut matched = 0usize;
    let mut faces = 0usize;
    let mut needs_merge = false;
    if !from_key.is_empty() {
        for photo in cat.photos().filter(|p| p.in_library()) {
            for region in photo.meta.regions.iter().filter(|r| r.kind == lightcraft_meta::RegionKind::Face) {
                let Some(name) = region.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
                let key = name.to_lowercase();
                if key == from_key {
                    matched += 1;
                    if region.name.as_deref() != Some(to_trim) {
                        faces += 1;
                    }
                } else if key == to_key {
                    needs_merge = true;
                }
            }
        }
    }
    let block = if matched == 0 {
        Some("No person by that name")
    } else if faces == 0 {
        Some("That is already their name")
    } else {
        None
    };
    // a case-only change is the same person, not a merge
    PersonRenamePlan { faces, needs_merge: needs_merge && from_key != to_key, block }
}
