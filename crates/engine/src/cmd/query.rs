//! Read-only queries (not journaled): catalog, photos, develop state, controls, presets, albums.

use std::collections::HashMap;

use lcc_catalog::{Album, AlbumId, Photo, PhotoId};
use lcc_develop::{CONTROLS, controls};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active};
use crate::{LibrarySource, Session};

pub fn photo_summary(p: &Photo) -> Value {
    json!({
        "id": p.id.0,
        "fileName": p.file_name,
        "format": p.format,
        "kind": p.kind,
        "width": p.width,
        "height": p.height,
        "captured": p.captured,
        "rating": p.rating,
        "flag": p.flag,
        "label": p.label,
        "edited": p.is_edited(),
        "title": p.meta.title,
        "keywords": p.meta.keywords,
        "camera": p.meta.camera,
        "deleted": p.deleted,
        "copyOf": p.copy_of.map(|c| c.0),
        "copyName": p.copy_name,
        // an undecodable raw variant, shown and edited from its embedded JPEG: why
        "previewOnly": p.preview_only,
    })
}

/// Current catalog values, which can differ from embedded file metadata until explicitly written.
fn catalog_metadata(photo: &Photo) -> Vec<Value> {
    let m = &photo.meta;
    let mut rows: Vec<Value> = [
        ("Title", &m.title),
        ("Caption", &m.caption),
        ("Alt Text", &m.alt_text),
        ("Extended Description", &m.extended_description),
        ("Creator", &m.creator),
        ("Copyright", &m.copyright),
        ("Rights Usage Terms", &m.usage_terms),
        ("Copyright Info URL", &m.copyright_url),
        ("Location", &m.location),
        ("City", &m.city),
        ("State / Province", &m.state),
        ("Country", &m.country),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| json!({"name": name, "value": value}))
    .collect();
    if let Some((lat, lon)) = m.gps {
        rows.push(json!({"name": "GPS", "value": format!("{lat:.6}, {lon:.6}")}));
    }
    if m.copyright_status != lcc_catalog::CopyrightStatus::Unknown {
        rows.push(json!({"name": "Copyright Status", "value": m.copyright_status.label()}));
    }
    if !m.keywords.is_empty() {
        rows.push(json!({"name": "Keywords", "value": m.keywords.join("; ")}));
    }
    rows
}

/// How deep [`album_json`] lists folders inside folders: a catalog that loops (an album inside
/// itself, which [`lcc_catalog::Catalog::repair`] puts right on load) lists no further
/// instead of overflowing the stack.
const MAX_ALBUM_DEPTH: usize = 64;

fn album_json(a: &Album, all: &[Album], counts: &HashMap<AlbumId, usize>, cat: &lcc_catalog::Catalog, depth: usize) -> Value {
    let children: Vec<Value> = if depth < MAX_ALBUM_DEPTH {
        all.iter().filter(|c| c.parent == Some(a.id)).map(|c| album_json(c, all, counts, cat, depth + 1)).collect()
    } else {
        Vec::new()
    };
    let mut v = json!({
        "id": a.id.0,
        "name": a.name,
        "folder": a.folder,
        "count": counts.get(&a.id).copied().unwrap_or(0),
        "cover": a.cover.map(|c| c.0),
        "children": children,
    });
    if let Some(rules) = &a.smart {
        v["smart"] = json!(true);
        v["rules"] = serde_json::to_value(rules).unwrap_or_default();
        v["rulesText"] = json!(rules.describe_with(cat));
        // rules that no longer check (an album they test was deleted…), as `rule 1: …`
        v["problems"] = json!(cat.smart_album_problems(a.id).iter().map(ToString::to_string).collect::<Vec<_>>());
    }
    v
}

/// The photo a query is about: `id` when given (which must be in the library), else the active one.
fn photo_arg(s: &Session, p: &Value, c: &str) -> crate::Result<PhotoId> {
    match super::photo_id(p, c)? {
        Some(id) if s.catalog.photo(id).is_some() => Ok(id),
        Some(id) => Err(bad(c, format!("no such photo {}", id.0))),
        None => s.active().ok_or_else(|| bad(c, "no photo")),
    }
}

// Catalog queries also run in the 32-bit web build. A narrowing cast wraps
// an out-of-range offset to zero, returning the first page instead of no photos.
fn pagination_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "catalog.query", "Query Photos", [], None, "{filter?: Filter, sort?: Sort, offset?, limit?}: omit filter to list the current view", always, |s, p| {
            let off = pagination_usize(p.get("offset").and_then(Value::as_u64).unwrap_or(0));
            let lim = pagination_usize(p.get("limit").and_then(Value::as_u64).unwrap_or(200));
            let (total, ids) = if p.get("filter").is_some() || p.get("sort").is_some() {
                let f = p.get("filter").map(|f| serde_json::from_value(f.clone())).transpose().map_err(|e| bad("catalog.query", e.to_string()))?.unwrap_or_default();
                let so = p.get("sort").map(|f| serde_json::from_value(f.clone())).transpose().map_err(|e| bad("catalog.query", e.to_string()))?.unwrap_or_default();
                let ids = s.catalog.query(&f, &so);
                (ids.len(), ids.into_iter().skip(off).take(lim).collect())
            } else {
                // the current view: only the page is copied
                s.visible_page(off, lim)
            };
            let items: Vec<Value> = ids.iter().filter_map(|id| s.catalog.photo(*id)).map(|p| photo_summary(p)).collect();
            Ok(json!({"total": total, "photos": items}))
        }),
        cmd!(query "catalog.stats", "Catalog Statistics", [], None, "{}", always, |s, _| {
            let all: Vec<_> = s.catalog.photos().filter(|p| p.in_library()).collect();
            Ok(json!({
                "photos": all.len(),
                "edited": all.iter().filter(|p| p.is_edited()).count(),
                "picks": all.iter().filter(|p| p.flag == lcc_catalog::Flag::Pick).count(),
                "rejects": all.iter().filter(|p| p.flag == lcc_catalog::Flag::Reject).count(),
                "deleted": s.catalog.photos().filter(|p| p.deleted).count(),
                "albums": s.catalog.albums().filter(|a| !a.folder).count(),
                "byDate": s.catalog.date_groups(),
                "keywords": s.catalog.keywords(),
            }))
        }),
        cmd!(query "library.state", "Library State", [], None, "{}", always, |s, _| {
            let label = match (s.source, s.library_folder.as_deref()) {
                (LibrarySource::LibraryFolder, Some(path)) => lcc_catalog::folders::folder_label(path),
                _ => s.source.label(&s.catalog),
            };
            let n = s.visible().len();
            Ok(json!({
                "source": s.source,
                "libraryFolder": s.library_folder.as_ref().filter(|_| s.source == LibrarySource::LibraryFolder),
                "sourceLabel": label,
                "filter": s.filter,
                "sort": s.sort,
                "selection": s.selection,
                "visibleCount": n,
                "activeMask": s.active_mask,
                "activeSpot": s.active_spot,
                "undo": s.undo.last().map(|u| u.label.clone()),
                "redo": s.redo.last().map(|u| u.label.clone()),
                "clipboard": s.clipboard.is_some(),
            }))
        }),
        cmd!(query "photo.allMetadata", "All Metadata", [], None, "{id?} → {catalog: [{name, value}], exif: [{group, tag, name, value}], xmp: [{name, value}], note?}: the values saved in the catalog, every EXIF / TIFF / GPS tag of the file and its XMP properties; a file that can't be read gives the catalog values and a note", always, |s, p| {
            let id = photo_arg(s, p, "photo.allMetadata")?;
            let ph = s.catalog.photo(id).ok_or_else(|| super::bad("photo.allMetadata", "no such photo"))?.clone();
            let catalog = catalog_metadata(&ph);
            let lcc_catalog::Source::File { path } = &ph.source else {
                return Ok(json!({"catalog": catalog, "exif": [], "xmp": [], "note": "a generated demo photo has no file"}));
            };
            let read = match &s.media.file_bytes {
                Some(r) => r(path),
                None => std::fs::read(path).map_err(|e| format!("{path}: {e}")),
            };
            let bytes = match read {
                Ok(bytes) => bytes,
                // what was saved in the catalog is still worth showing when the original is offline or gone
                Err(e) => return Ok(json!({"catalog": catalog, "exif": [], "xmp": [], "note": format!("the file can't be read: {e}")})),
            };
            let exif: Vec<Value> = lcc_meta::file_tag_rows(&bytes)
                .into_iter()
                .map(|r| json!({"group": r.group, "tag": r.tag, "name": r.name, "value": r.value}))
                .collect();
            // XMP: the sidecar if there is one, else the packet in the bytes already loaded, with the
            // JPEG's Extended XMP (what did not fit in the main packet; the main packet wins)
            let (packet, extended) = match crate::sidecar::find_sidecar(path, s.sidecar_naming(ph.id)) {
                Some(sc) => (std::fs::read_to_string(sc).ok(), None),
                None => {
                    let e = lcc_meta::embedded(&bytes);
                    (e.xmp, e.xmp_extended)
                }
            };
            let mut properties = packet.and_then(|x| lcc_meta::parse_xmp(&x).ok()).map(|d| d.properties).unwrap_or_default();
            if let Some(ext) = extended.and_then(|x| lcc_meta::parse_xmp(&x).ok()) {
                for (k, v) in ext.properties {
                    properties.entry(k).or_insert(v);
                }
            }
            let xmp: Vec<Value> =
                properties.into_iter().filter(|(k, _)| !k.starts_with("lc:")).map(|(k, v)| json!({"name": k, "value": v.join("; ")})).collect();
            Ok(json!({"catalog": catalog, "exif": exif, "xmp": xmp}))
        }),
        cmd!(query "photo.fujiRecipe", "Fujifilm Recipe", [], None, "{id?} → {present, text, lines: [{tag, label, value}]}: in-camera film settings from a Fujifilm MakerNote, or present: false", always, |s, p| {
            let id = photo_arg(s, p, "photo.fujiRecipe")?;
            let ph = s.catalog.photo(id).ok_or_else(|| super::bad("photo.fujiRecipe", "no such photo"))?.clone();
            let lcc_catalog::Source::File { path } = &ph.source else {
                return Ok(json!({"present": false}));
            };
            let recipe = if let Some(read) = &s.media.file_bytes {
                let bytes = read(path).map_err(|e| super::bad("photo.fujiRecipe", e))?;
                lcc_meta::recipe_from_bytes(&bytes)
            } else {
                lcc_meta::read_recipe_file(std::path::Path::new(path)).map_err(|e| super::bad("photo.fujiRecipe", e))?
            };
            let Some(recipe) = recipe else { return Ok(json!({"present": false})) };
            let lines: Vec<Value> = recipe.rows.iter().map(|r| json!({"tag": r.tag, "label": r.label, "value": r.value})).collect();
            Ok(json!({"present": true, "text": recipe.text(), "lines": lines}))
        }),
        cmd!(query "albums.list", "List Collections", [], None, "{}", always, |s, _| {
            let all: Vec<Album> = s.catalog.albums().cloned().collect();
            // every album counted in one pass, so a smart album tested by others is worked out once
            let counts = lcc_catalog::tally::AlbumTally::of(&s.catalog);
            Ok(Value::Array(all.iter().filter(|a| a.parent.is_none()).map(|a| album_json(a, &all, counts.counts(), &s.catalog, 0)).collect()))
        }),
        cmd!(query "photo.inspect", "Inspect Photo", [], None, "{id?}", always, |s, p| {
            let id = photo_arg(s, p, "photo.inspect")?;
            let ph = s.catalog.photo(id).ok_or_else(|| bad("photo.inspect", "no such photo"))?;
            let mut v = serde_json::to_value(ph.as_ref()).unwrap_or_default();
            v["albums"] = json!(s.catalog.albums_of(id).iter().map(|a| a.0).collect::<Vec<_>>());
            v["history"] = json!(ph.history.iter().map(|h| h.label.clone()).collect::<Vec<_>>());
            if let Some(st) = s.catalog.stack_of(id) {
                v["stack"] = json!({
                    "id": st.id.0,
                    "photos": st.photos.iter().map(|p| p.0).collect::<Vec<_>>(),
                    "position": st.position(id),
                    "collapsed": st.collapsed,
                });
            }
            Ok(v)
        }),
        cmd!(query "develop.get", "Get Develop Settings", [], None, "{id?} → the photo's develop settings; `process` is the rendering process they are interpreted by (see develop.updateProcess); a flat-field map they reference comes with them at `optics.vignetting_calibration_map` (develop.merge takes it back)", always, |s, p| {
            let id = photo_arg(s, p, "develop.get")?;
            let mut v = s
                .develop_of(id)
                .map(|d| {
                    let mut v = d.to_json();
                    // saved settings leave V1 out (`ProcessVersion::is_legacy`); here it is always stated
                    if let Some(o) = v.as_object_mut() {
                        o.insert("process".into(), json!(d.process));
                    }
                    v
                })
                .unwrap_or(Value::Null);
            // portable: settings an agent takes elsewhere carry their flat-field map
            s.portable_settings(&mut v);
            Ok(v)
        }),
        cmd!(query "develop.controls", "List Develop Controls", [], None, "{section?}: every slider with range, default and current value", always, |s, p| {
            let d = s.active().and_then(|id| s.develop_of(id)).unwrap_or_default();
            let sec = p.get("section").and_then(|v| serde_json::from_value::<lcc_develop::Section>(v.clone()).ok());
            Ok(Value::Array(
                CONTROLS
                    .iter()
                    .filter(|c| sec.is_none_or(|x| x == c.section))
                    .map(|c| {
                        let mut v = serde_json::to_value(c).unwrap_or_default();
                        v["value"] = json!(controls::get(&d, c.id));
                        v
                    })
                    // indexed controls of the list elements that exist (`pointColor.0.hueShift`, …)
                    .chain(controls::indexed_instances(&d).into_iter().filter(|(_, c)| sec.is_none_or(|x| x == c.section)).map(|(id, c)| {
                        let mut v = serde_json::to_value(c).unwrap_or_default();
                        v["value"] = json!(controls::get(&d, &id));
                        v["id"] = json!(id);
                        v
                    }))
                    .collect(),
            ))
        }),
        cmd!(query "presets.list", "List Presets", [], None, "{}", always, |s, _| {
            Ok(Value::Array(s.presets.iter().map(|p| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": p.favorite, "builtin": p.builtin, "atImport": s.preset_at_import.contains(&p.id), "hidden": s.preset_hidden.contains(&p.id)})).collect()))
        }),
        cmd!(query "presets.folders", "List Preset Folders", [], None, "{}: all preset folders (user, built-in, and empty folders)", always, |s, _| {
            let mut set = std::collections::BTreeSet::new();
            for f in &s.preset_folders {
                set.insert(f.clone());
                let mut cur = f.as_str();
                while let Some((par, _)) = cur.rsplit_once('/') {
                    set.insert(par.to_string());
                    cur = par;
                }
            }
            for p in s.presets.iter() {
                set.insert(p.group.clone());
                let mut cur = p.group.as_str();
                while let Some((par, _)) = cur.rsplit_once('/') {
                    set.insert(par.to_string());
                    cur = par;
                }
            }
            Ok(Value::Array(set.into_iter().map(Value::String).collect()))
        }),
        cmd!(query "profiles.list", "List Profiles", [], None, "{all?: bool (also the profiles the Profile Manager hides)}: every profile (built-in, then imported LUTs marked `imported`) with its group and favourite flag", always, |s, p| {
            let favorite = |id: &str| s.profile_favorites.iter().any(|f| f == id);
            let all = p.get("all").and_then(Value::as_bool).unwrap_or(false);
            // the user's names, groups and order; imported LUT profiles marked as in `profiles.menu` (#618)
            Ok(Value::Array(
                s.all_profiles()
                    .into_iter()
                    .filter(|p| all || !p.hidden)
                    .map(|p| {
                        let mut v = json!({"id": p.id, "name": p.name, "group": p.group, "favorite": favorite(&p.id)});
                        if p.kind == "lut" {
                            v["imported"] = json!(true);
                        }
                        v
                    })
                    .collect(),
            ))
        }),
        cmd!(query "profiles.menu", "Profile Menu", [], None, "{}: favourites, recent (newest first) and groups", always, |s, _| Ok(s.profile_menu())),
        cmd!(query "history.list", "List History", [], None, "{id?}: returns {history: labels oldest first, batches: per step the id of the edit of several photos it belongs to (null: this photo's own edit), cursor: index of the step the settings are at (later steps can be returned to), versions}", has_active, |s, p| {
            let id = photo_arg(s, p, "history.list")?;
            let ph = s.catalog.photo(id).ok_or_else(|| bad("history.list", "no such photo"))?;
            Ok(json!({
                "history": ph.history.iter().map(|h| h.label.clone()).collect::<Vec<_>>(),
                "batches": ph.history.iter().map(|h| h.batch).collect::<Vec<_>>(),
                "cursor": ph.history_index(),
                "versions": ph.versions.iter().map(|v| json!({"name": v.name, "created": v.created})).collect::<Vec<_>>(),
            }))
        }),
        cmd!(query "history.groups", "List History Groups", [], None, "{id?}: returns {groups: [{section (null = several sections: a preset, paste, sync…; replays but can't be edited inside), start, end, steps: [{index, label}]}] oldest first, cursor, latest}", has_active, |s, p| {
            let id = photo_arg(s, p, "history.groups")?;
            let ph = s.catalog.photo(id).ok_or_else(|| bad("history.groups", "no such photo"))?;
            let groups: Vec<Value> = crate::history_groups::groups(&ph.history)
                .into_iter()
                .map(|g| {
                    let steps: Vec<Value> = (g.start..=g.end)
                        .filter_map(|i| ph.history.get(i).map(|h| json!({"index": i, "label": h.label})))
                        .collect();
                    json!({"section": g.section, "start": g.start, "end": g.end, "steps": steps})
                })
                .collect();
            Ok(json!({"groups": groups, "cursor": ph.history_index(), "latest": ph.history_cursor.is_none()}))
        }),
        cmd!(query "app.gpu", "GPU Rendering", [], None, "{enabled?: bool, card?: string, helper?: string}: allow/forbid GPU rendering (CPU fallback; `{ENV_PREFIX}_GPU=0` forbids it for the process); returns {enabled, available, adapter, reason (why the GPU is off), lastFallback (latest render redone on the CPU, and why), recoveries (lost devices replaced), events [{seq, kind: lost|recovered|gaveUp, text}] (what happened to the graphics card, in plain words), batch {jobs, bytes, waiting, limitBytes, limitJobs, waits, waitedMs, pauses, pausedMs (running batch renders that paused for the interface), cards (cards taking batch work now), rate {editing, helper} (each card's measured batch speed, bytes/s, null before any), renders {editingInteractive, editingBatch, helperInteractive, helperBatch} (GPU renders so far by card), interfaceBusy} (exports' and preview builds' turns at the cards), card {choice, inUse, switching} (the graphics card for editing: `card` takes a card's key or \"\" for Automatic), helper {state: off|starting|ready|failed|lost, detail, jobs, bytes, waiting} (the second graphics card: `helper` takes \"\" Automatic, \"off\" or a key), adapters [{key, name, kind, memory, backend, drivesScreen, usable, whyNot}]}", always, |_, p| {
            if let Some(on) = p.get("enabled").and_then(Value::as_bool) {
                lcc_gpu::set_enabled(on);
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                if let Some(card) = p.get("card").and_then(Value::as_str) {
                    if card.chars().count() > 512 {
                        return Err(bad("app.gpu", "card is too long"));
                    }
                    lcc_gpu::set_card(card);
                }
                if let Some(helper) = p.get("helper").and_then(Value::as_str) {
                    if helper.chars().count() > 512 {
                        return Err(bad("app.gpu", "helper is too long"));
                    }
                    lcc_gpu::set_helper(helper);
                }
            }
            let b = lcc_gpu::batch_usage();
            Ok(json!({
                "enabled": lcc_gpu::enabled(),
                "available": lcc_gpu::available(),
                "adapter": lcc_gpu::adapter_name(),
                "reason": lcc_gpu::unavailable_reason(),
                "lastFallback": lcc_gpu::last_fallback(),
                "recoveries": lcc_gpu::recoveries(),
                "events": gpu_events(),
                "batch": {
                    "jobs": b.jobs, "bytes": b.bytes, "waiting": b.waiting, "limitBytes": b.limit_bytes,
                    "limitJobs": b.limit_jobs, "waits": b.waits, "waitedMs": b.waited_ms,
                    "pauses": b.pauses, "pausedMs": b.paused_ms, "cards": lcc_gpu::batch_cards(),
                    "rate": {"editing": lcc_gpu::batch_rate_of(false), "helper": lcc_gpu::batch_rate_of(true)},
                    "renders": gpu_render_counts(), "interfaceBusy": lcc_gpu::interface_busy(),
                },
                "card": gpu_card(),
                "helper": gpu_helper(),
                "adapters": gpu_adapters(),
            }))
        }),
        cmd!(query "library.memory", "Memory Usage", [], None, "{}: bytes held by each cache (decoded sources, rendered previews, GPU buffers; heap when instrumented)", always, |s, _| {
            Ok(serde_json::to_value(s.memory_report()).unwrap_or_default())
        }),
        cmd!(query "app.memoryBudget", "Memory Budget", [], None, "{mb?: number (0 = automatic)}: set the memory budget shared by the caches (automatic: a quarter of the computer's memory, at most 16 GB; by hand at most three quarters of it, `maxBudget`); returns the report", always, |s, p| {
            // missing or null: the report only; anything else must be a number in range (upstream #3706)
            if let Some(v) = p.get("mb").filter(|v| !v.is_null()) {
                let Some(mb) = v.as_f64() else {
                    return Err(bad("app.memoryBudget", "`mb` must be a number of megabytes from 64 to 1048576, or 0 for automatic, or null"));
                };
                if mb == 0.0 {
                    s.set_memory_budget(crate::memory::default_budget());
                } else if !(64.0..=1_048_576.0).contains(&mb) {
                    return Err(bad("app.memoryBudget", "mb must be 64..1048576, or 0 for automatic"));
                } else {
                    s.set_memory_budget((mb * 1048576.0) as usize);
                }
            }
            Ok(serde_json::to_value(s.memory_report()).unwrap_or_default())
        }),
        cmd!(query "journal.list", "Command Journal", [], None, "{limit?}", always, |s, p| {
            let lim = p.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
            let start = s.journal.len().saturating_sub(lim);
            Ok(Value::Array(s.journal[start..].iter().map(|(c, p)| json!({"command": c, "params": p})).collect()))
        }),
    ]
}

/// GPU renders so far by card, for `app.gpu`.
fn gpu_render_counts() -> Value {
    let r = lcc_gpu::render_counts();
    json!({
        "editingInteractive": r.editing_interactive, "editingBatch": r.editing_batch,
        "helperInteractive": r.helper_interactive, "helperBatch": r.helper_batch,
    })
}

/// The graphics card for editing, for `app.gpu`.
fn gpu_card() -> Value {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (choice, in_use, switching) = lcc_gpu::card_status();
        json!({"choice": choice, "inUse": in_use, "switching": switching})
    }
    #[cfg(target_arch = "wasm32")]
    {
        Value::Null
    }
}

/// The second graphics card, for `app.gpu`.
fn gpu_helper() -> Value {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (state, detail) = lcc_gpu::helper_status();
        let (jobs, bytes, waiting) = lcc_gpu::helper_usage();
        json!({"state": state, "detail": detail, "jobs": jobs, "bytes": bytes, "waiting": waiting})
    }
    #[cfg(target_arch = "wasm32")]
    {
        Value::Null
    }
}

/// The graphics cards listed, for `app.gpu`.
fn gpu_adapters() -> Value {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Value::Array(
            lcc_gpu::adapters()
                .into_iter()
                .map(|e| {
                    json!({"key": e.key.to_string(), "name": e.name, "kind": e.kind.id(), "memory": e.memory, "backend": e.backend,
                           "drivesScreen": e.drives_screen, "usable": e.usable, "whyNot": e.why_not})
                })
                .collect(),
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        Value::Array(Vec::new())
    }
}

/// What happened to the graphics card this session ([`lcc_gpu::events_after`]), for `app.gpu`.
fn gpu_events() -> Value {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let kind = |k: lcc_gpu::EventKind| match k {
            lcc_gpu::EventKind::Lost => "lost",
            lcc_gpu::EventKind::Recovered => "recovered",
            lcc_gpu::EventKind::GaveUp => "gaveUp",
        };
        Value::Array(lcc_gpu::events_after(0).into_iter().map(|e| json!({"seq": e.seq, "kind": kind(e.kind), "text": e.text})).collect())
    }
    #[cfg(target_arch = "wasm32")]
    {
        Value::Array(Vec::new())
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    #[test]
    fn offsets_and_limits_saturate_instead_of_wrapping_on_wasm() {
        assert_eq!(pagination_usize(0), 0);
        assert_eq!(pagination_usize(200), 200);
        assert_eq!(pagination_usize(u64::MAX), usize::MAX);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(pagination_usize(u64::from(u32::MAX) + 1), usize::MAX);
    }
}

#[cfg(test)]
mod gpu_card_tests {
    /// `app.gpu` reports the cards and takes hostile card choices without a panic: an over-long
    /// one is refused, text that is no card means Automatic.
    #[test]
    fn app_gpu_reports_cards_and_refuses_hostile_choices() {
        let mut s = crate::Session::new();
        let long = "x".repeat(10_000);
        assert!(s.execute("app.gpu", &serde_json::json!({"card": long})).is_err());
        assert!(s.execute("app.gpu", &serde_json::json!({"helper": long})).is_err());
        let r = s.execute("app.gpu", &serde_json::json!({"card": "not a card", "helper": "nonsense"})).expect("reply");
        assert!(r["card"]["choice"].is_null(), "text that is no card is Automatic: {r}");
        assert!(r["helper"]["state"].is_string());
        assert!(r["adapters"].is_array());
        let r = s.execute("app.gpu", &serde_json::json!({"card": "", "helper": ""})).expect("reply");
        assert!(r["card"]["choice"].is_null());
    }
}

#[cfg(test)]
mod memory_budget_tests {
    use serde_json::json;

    use crate::{EngineError, Session};

    #[test]
    fn a_memory_budget_that_is_not_a_number_is_refused_and_a_missing_one_changes_nothing() {
        // Upstream issue #3706: an `mb` that is there but isn't a number is refused, not taken as "report only".
        {
            let mut s = Session::new();
            let before = crate::memory::budget();
            for mb in [json!("512"), json!(true), json!([512]), json!({"mb": 512})] {
                let r = s.execute("app.memoryBudget", &json!({ "mb": mb }));
                assert!(matches!(&r, Err(EngineError::BadParams { cmd, .. }) if cmd == "app.memoryBudget"), "{mb}: {r:?}");
            }
            assert_eq!(crate::memory::budget(), before, "a refused call changes nothing");
        }
        // Missing or null `mb`: the report only.
        {
            let mut s = Session::new();
            let before = crate::memory::budget();
            for p in [json!({}), json!({"mb": null})] {
                let r = s.execute("app.memoryBudget", &p).unwrap();
                assert_eq!(r["budget"].as_u64(), Some(before as u64), "{p}: {r}");
            }
            assert_eq!(crate::memory::budget(), before, "nothing changed");
        }
    }
}
