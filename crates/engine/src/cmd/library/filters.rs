use super::{AlbumId, LibrarySource, PhotoId, Result, Session, Value, bad};

/// `base` with a partial Filter (JSON) merged on top.
/// `patch` merged onto `base`, its rules brought up to date (`RuleSet::upgrade`), unchecked (see
/// [`merge_rules`]).
fn merge_filter(base: &lightcraft_catalog::Filter, patch: &Value, c: &str) -> Result<lightcraft_catalog::Filter> {
    let mut v = serde_json::to_value(base).unwrap_or_default();
    lightcraft_develop::presets::deep_merge(&mut v, patch);
    let mut f: lightcraft_catalog::Filter = serde_json::from_value(v).map_err(|e| bad(c, e.to_string()))?;
    if let Some(rules) = f.rule_set.as_mut() {
        rules.upgrade();
    }
    Ok(f)
}

/// `patch` merged onto `base`, refused when the rule set it sets has problems (`RuleSet::check_for`;
/// `owner` is the smart album the rules are for, so testing an album that leads back to it is a
/// problem too).
pub(super) fn merge_rules(
    base: &lightcraft_catalog::Filter,
    patch: &Value,
    c: &str,
    cat: &lightcraft_catalog::Catalog,
    owner: Option<AlbumId>,
) -> Result<lightcraft_catalog::Filter> {
    let f = merge_filter(base, patch, c)?;
    // only rules this change sets are checked: one that stopped checking since (its album was
    // deleted) doesn't block editing the album's other settings
    let sets_rules = patch.get("ruleSet").is_some_and(|r| !r.is_null());
    let mut problems = f.rule_set.as_ref().filter(|_| sets_rules).map(|r| r.check_for(cat, owner)).unwrap_or_default();
    // and a loop through the album field the change sets
    let sets = |key: &str| patch.get(key).is_some_and(|a| !a.is_null());
    if sets("album") || sets("albums") {
        problems.extend(cat.album_filter_problem(&f, owner));
    }
    if !problems.is_empty() {
        return Err(bad(c, problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")));
    }
    Ok(f)
}

/// Whether `path` lies above a disk: some of its `ids` photos are on a disk (not the startup
/// disk) that the folder does not lie in. Such a folder (`/Volumes`, `/mnt`…) is not a folder of
/// one disk but a way to reach several.
pub(crate) fn covers_other_disks(s: &Session, path: &str, ids: &[PhotoId]) -> bool {
    ids.iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter_map(|p| lightcraft_catalog::folders::volume_of(p))
        .any(|v| v != "/" && !lightcraft_catalog::query::folder_within(path, &v))
}

/// A view of several albums can't be saved when one of them is a smart album (smart album rules
/// can't test another smart album): refuse up front, naming it, instead of letting the rules'
/// check say so without the cause.
pub(super) fn several_smart_refused(s: &Session, cmd: &str) -> Result<()> {
    let shown = s.source_albums();
    if shown.len() < 2 {
        return Ok(());
    }
    match shown.iter().find_map(|a| s.catalog.album(*a).filter(|a| a.is_smart())) {
        Some(smart) => Err(bad(
            cmd,
            format!(
                "the view shows several albums and \"{}\" is a smart album: smart album rules can't test another smart album. Show only albums that aren't smart, or save the smart album's own view",
                smart.name
            ),
        )),
        None => Ok(()),
    }
}

/// The current view (source + filter) as smart-album rules. Viewing a smart album starts from
/// its rules with the filter bar's settings on top.
pub(super) fn view_rules(s: &Session) -> lightcraft_catalog::Filter {
    use lightcraft_catalog::Filter;
    let smart = match s.source {
        // (one smart album alone: among several albums shown it is one more album)
        LibrarySource::Album(a) if s.also_albums.is_empty() => s.catalog.album(a).and_then(|a| a.smart.as_deref().cloned()),
        _ => None,
    };
    match smart {
        Some(base) => {
            // overlay the fields the filter bar changed
            let cur = serde_json::to_value(&s.filter).unwrap_or_default();
            let def = serde_json::to_value(Filter::default()).unwrap_or_default();
            let mut patch = serde_json::Map::new();
            if let (Some(c), Some(d)) = (cur.as_object(), def.as_object()) {
                for (k, v) in c {
                    if d.get(k) != Some(v) {
                        patch.insert(k.clone(), v.clone());
                    }
                }
            }
            // the view as it is: a rule that no longer checks (its album was deleted) just matches
            // nothing there, and mustn't cost the filter bar's settings
            merge_filter(&base, &Value::Object(patch), "").unwrap_or(base)
        }
        None => {
            let mut f = s.source.to_filter(&s.filter, &s.catalog);
            f.deleted = false;
            match s.source {
                LibrarySource::LibraryFolder => {
                    f.library_folder = s.library_folder.clone();
                    f.library_folders = s.also_library_folders.clone();
                }
                // every album shown, not the filter's own
                LibrarySource::Album(_) => f.albums = s.also_albums.clone(),
                _ => {}
            }
            f
        }
    }
}

impl Session {
    /// The current view (source + filter) as smart-album rules (see `album.createSmart`).
    pub fn view_rules(&self) -> lightcraft_catalog::Filter {
        view_rules(self)
    }
}
