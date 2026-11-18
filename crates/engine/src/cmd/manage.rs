//! Photo management commands: batch rename, capture time, colour label names and sets.

use serde_json::{Value, json};

use lcc_catalog::{ColorLabel, Op};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_or, has_selection, str_param};
use crate::Result;

/// The photos, template and first number of a rename. The template's `{text:…}` tags take `texts`
/// (`strict`: each must be given; the preview leaves a missing one empty).
fn rename_args(s: &mut crate::Session, p: &Value, c: &str, strict: bool) -> Result<(Vec<lcc_catalog::PhotoId>, String, usize)> {
    let template = str_param(p, "template").or_else(|| str_param(p, "name")).ok_or_else(|| bad(c, "missing `template`"))?.to_string();
    let template = if strict {
        let texts = crate::rename::texts_param(&template, p.get("texts")).map_err(|e| bad(c, e))?;
        crate::rename::fill_texts(&template, &|l| texts.get(l).cloned())
    } else {
        let texts = p.get("texts").and_then(Value::as_object);
        crate::rename::fill_texts(&template, &|l| texts.and_then(|t| t.get(l)).and_then(Value::as_str).map(str::to_string))
    };
    let start = p.get("start").and_then(Value::as_u64).unwrap_or(1);
    let mut ids = s.targets(p)?;
    if p.get("ids").is_none() && p.get("id").is_none() {
        // the selection is numbered (`{seq}`) in the order the grid shows it (Custom Order too)
        let rank: std::collections::HashMap<_, _> = s.visible().iter().enumerate().map(|(i, id)| (*id, i)).collect();
        ids.sort_by_key(|id| rank.get(id).copied().unwrap_or(usize::MAX));
    }
    // every number of the sequence `start..start + n` must fit
    let last = start.checked_add((ids.len() as u64).saturating_sub(1));
    let start = last.and_then(|l| usize::try_from(l).ok()).and_then(|_| usize::try_from(start).ok());
    let start = start.ok_or_else(|| bad(c, "start out of range"))?;
    Ok((ids, template, start))
}

/// A file-name template saved by name (Lightroom's Filename Template Editor presets), shared by
/// Rename Photos and Export. `{text:Label}` tags in it are asked each time.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NamingPreset {
    pub name: String,
    pub template: String,
}

/// Most saved naming presets (a hand-edited file can't grow the list without end).
pub const MAX_NAMING_PRESETS: usize = 500;

/// The app's own naming presets (not deletable), then the user's.
pub fn builtin_naming_presets() -> Vec<NamingPreset> {
    let p = |name: &str, template: &str| NamingPreset { name: name.into(), template: template.into() };
    vec![
        p("Original name", "{name}"),
        p("Date - Name", "{date}_{name}"),
        p("Title - Number", "{title}-{seq:3}"),
        p("Custom Text - Number", "{text:Name}-{seq:3}"),
    ]
}

fn naming_presets_json(s: &crate::Session) -> Value {
    let b = builtin_naming_presets().into_iter().map(|p| json!({"name": p.name, "template": p.template, "builtin": true}));
    let u = s.naming_presets.iter().map(|p| json!({"name": p.name, "template": p.template, "builtin": false}));
    Value::Array(b.chain(u).collect())
}

/// A named set of colour-label names (red, yellow, green, blue, purple; empty = the colour's name).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LabelSet {
    pub name: String,
    pub names: [String; 5],
}

/// Our built-in sets.
pub fn builtin_label_sets() -> Vec<LabelSet> {
    let set = |name: &str, n: [&str; 5]| LabelSet { name: name.into(), names: n.map(str::to_string) };
    vec![set("Colors", ["", "", "", "", ""]), set("Review", ["Reject", "Needs Work", "Approved", "Retouch", "Print"])]
}

fn all_label_sets(s: &crate::Session) -> Vec<LabelSet> {
    builtin_label_sets().into_iter().chain(s.label_sets.iter().cloned()).collect()
}

/// The set whose names are the catalog's current ones.
fn current_label_set(s: &crate::Session) -> Option<String> {
    let cur: [String; 5] = ColorLabel::ALL.map(|l| s.catalog.custom_label_name(l).unwrap_or("").to_string());
    all_label_sets(s)
        .into_iter()
        .find(|x| x.names.iter().zip(&cur).all(|(a, b)| a.trim() == b.trim() || (a.trim().is_empty() && b.is_empty())))
        .map(|x| x.name)
}

/// Every set (built-ins first) and the one in use: `{sets: [{name, names, builtin}], current}`.
pub fn label_sets_json(s: &crate::Session) -> Value {
    let builtin = builtin_label_sets().len();
    let sets: Vec<Value> =
        all_label_sets(s).iter().enumerate().map(|(i, x)| json!({"name": x.name, "names": x.names, "builtin": i < builtin})).collect();
    json!({"sets": sets, "current": current_label_set(s)})
}

fn set_names_ops(s: &crate::Session, names: &[String; 5]) -> Vec<Op> {
    ColorLabel::ALL
        .iter()
        .zip(names)
        .filter_map(|(l, n)| {
            let n = n.trim();
            let name = (!n.is_empty() && !n.eq_ignore_ascii_case(&format!("{l:?}"))).then(|| n.to_string());
            (s.catalog.custom_label_name(*l) != name.as_deref()).then_some(Op::SetLabelName { label: *l, name })
        })
        .collect()
}

/// Geotag photos from a GPX track log by capture time (Lightroom Classic's Map ▸ Tracklog ▸
/// Auto-Tag Photos). GPX times are UTC; capture times are the camera's local clock, so a photo's
/// recorded zone (Exif `OffsetTimeOriginal`) or the `offset` parameter converts them.
fn auto_tag_tracklog(s: &mut crate::Session, p: &Value) -> Result<Value> {
    use lcc_meta::{DateTime, Match, parse_gpx};
    const C: &str = "photo.autoTagTracklog";
    let text = match (str_param(p, "gpx"), str_param(p, "path")) {
        (Some(t), _) => t.to_string(),
        (None, Some(path)) => crate::bounded_read::read_bounded(std::path::Path::new(path), 256 << 20)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| bad(C, format!("can't read {path}: {e}")))?,
        (None, None) => return Err(bad(C, "missing `path` (a .gpx file) or `gpx` (its text)")),
    };
    let log = parse_gpx(&text).map_err(|e| bad(C, e.to_string()))?;
    let (start, end) = log.span().ok_or_else(|| bad(C, "the track log has no timed track points"))?;
    // minutes east of UTC; `None` = not given (zone-less capture times are then read as UTC)
    let offset: Option<i64> = match p.get("offset") {
        None | Some(Value::Null) => None,
        Some(Value::String(o)) if o.trim().is_empty() => None,
        Some(Value::Number(h)) => {
            let h = h.as_f64().unwrap_or(0.0);
            if h.abs() > 18.0 {
                return Err(bad(C, "offset is at most ±18 hours"));
            }
            Some((h * 60.0).round() as i64)
        }
        Some(Value::String(o)) => {
            Some(DateTime::parse_offset(o).ok_or_else(|| bad(C, format!("can't read offset `{o}` (e.g. +02:00 or -5)")))? as i64)
        }
        Some(_) => return Err(bad(C, "offset is `+HH:MM` or a number of hours")),
    };
    let max_gap = f64_or(p, "maxGap", 600.0);
    if max_gap.is_nan() || max_gap < 0.0 {
        return Err(bad(C, "maxGap must be ≥ 0 seconds"));
    }
    let replace = bool_or(p, "replace", false);
    let (mut no_time, mut outside, mut has_gps, mut assumed_utc, mut interpolated) = (0, 0, 0, 0, 0);
    let mut ops = Vec::new();
    let mut photos = Vec::new();
    for id in s.targets(p)? {
        let Some(ph) = s.catalog.photo(id) else { continue };
        if ph.meta.gps.is_some() && !replace {
            has_gps += 1;
            continue;
        }
        // a year or a month alone has no time to place on the track
        let Some(dt) = ph.captured.as_deref().and_then(DateTime::parse_iso).filter(DateTime::is_full) else {
            no_time += 1;
            continue;
        };
        // UTC seconds: the recorded zone wins, else the caller's offset
        let utc = match dt.offset_minutes {
            Some(_) => dt.unix_seconds(),
            None => {
                if offset.is_none() {
                    assumed_utc += 1;
                }
                dt.unix_seconds() - offset.unwrap_or(0) * 60
            }
        } as f64
            + dt.millis as f64 / 1000.0;
        let Some((g, m)) = log.locate(utc, max_gap) else {
            outside += 1;
            continue;
        };
        if m == Match::Interpolated {
            interpolated += 1;
        }
        let pos = ((g.latitude * 1e7).round() / 1e7, (g.longitude * 1e7).round() / 1e7);
        photos.push(json!({"id": id.0, "gps": [pos.0, pos.1], "match": if m == Match::Interpolated { "interpolated" } else { "nearest" }}));
        if ph.meta.gps != Some(pos) {
            let mut meta = ph.meta.clone();
            meta.gps = Some(pos);
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    let tagged = photos.len();
    if !bool_or(p, "dryRun", false) && !ops.is_empty() {
        s.commit("Auto-Tag from Tracklog", Op::Batch { ops })?;
    }
    let iso = |t: f64| format!("{}Z", lcc_catalog::dates::civil(t.floor() as i64));
    Ok(json!({
        "tagged": tagged,
        "interpolated": interpolated,
        "nearest": tagged - interpolated,
        "skipped": {"noTime": no_time, "outside": outside, "hasGps": has_gps},
        "assumedUtc": assumed_utc,
        "points": log.points.len(),
        "start": iso(start),
        "end": iso(end),
        "photos": photos,
    }))
}

/// The capture time the original file's own Exif states (`DateTimeOriginal` with its `OffsetTimeOriginal` and
/// `SubSecTimeOriginal`) as an ISO time; `None` when the file is missing, unreadable, or states none (or only a
/// partial one).
fn exif_time(p: &lcc_catalog::Photo) -> Option<String> {
    use std::io::Read;
    /// Exif lives in the first part of every format we read; a hostile or huge file is not read beyond this.
    const MAX_READ: u64 = 256 << 20;
    let lcc_catalog::Source::File { path } = &p.source else { return None };
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_READ).read_to_end(&mut bytes).ok()?;
    let exif = lcc_meta::embedded(&bytes).exif?;
    lcc_meta::read_exif(&exif).capture_time.filter(lcc_meta::DateTime::is_full).map(|d| d.to_iso())
}

/// The original file's modification time as an ISO time (UTC), if it has a readable file.
fn file_time(p: &lcc_catalog::Photo) -> Option<String> {
    let lcc_catalog::Source::File { path } = &p.source else { return None };
    let t = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(lcc_catalog::dates::civil(secs))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        // ---- naming presets
        cmd!(query "naming.presets", "File Naming Presets", [], None, "{} → [{name, template, builtin}]: saved file-name templates shared by Rename Photos, Export and Import (the app's first)", always, |s, _| {
            Ok(naming_presets_json(s))
        }),
        cmd!(
            "naming.savePreset",
            "Save File Naming Preset",
            [],
            None,
            "{name, template}: adds or replaces a user naming preset (kept with the library); `{text:Label}` tags in it are asked each time it is used → presets",
            always,
            |s, p| {
                let c = "naming.savePreset";
                let name = str_param(p, "name")
                    .map(str::trim)
                    .filter(|n| !n.is_empty() && n.chars().count() <= 120)
                    .ok_or_else(|| bad(c, "missing `name`"))?;
                let template = str_param(p, "template").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(c, "missing `template`"))?;
                if template.chars().count() > 1000 {
                    return Err(bad(c, "the template is too long"));
                }
                if builtin_naming_presets().iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
                    return Err(bad(c, format!("“{name}” is one of the built-in presets; choose another name")));
                }
                let preset = NamingPreset { name: name.to_string(), template: template.to_string() };
                let full = s.naming_presets.len() >= MAX_NAMING_PRESETS;
                match s.naming_presets.iter_mut().find(|x| x.name.eq_ignore_ascii_case(name)) {
                    Some(x) => *x = preset,
                    None if full => return Err(bad(c, "too many naming presets")),
                    None => s.naming_presets.push(preset),
                }
                s.save_prefs()?;
                Ok(naming_presets_json(s))
            }
        ),
        cmd!("naming.deletePreset", "Delete File Naming Preset", [], None, "{name}: user presets only → presets", always, |s, p| {
            let c = "naming.deletePreset";
            let name = str_param(p, "name").ok_or_else(|| bad(c, "missing `name`"))?;
            let before = s.naming_presets.len();
            s.naming_presets.retain(|x| !x.name.eq_ignore_ascii_case(name.trim()));
            if s.naming_presets.len() == before {
                return Err(bad(c, format!("no user naming preset “{name}”")));
            }
            s.save_prefs()?;
            Ok(naming_presets_json(s))
        }),
        cmd!(query "photo.renameTokens", "Rename Template Tags", [], None, "{} → {tokens: [{tag, aliases, meaning, example}], dateDirectives: [{directive, meaning}], notes: [..], sample}: the file-name template tags shared by photo.rename, library.import (rename) and app.export (naming); examples are for a sample photo", always, |_, _| {
            Ok(crate::rename::token_help_json())
        }),
        cmd!(query "photo.renamePreview", "Rename Preview", [], None, "{template: e.g. `{date}_{name}`, `Trip-{seq:3}` (tokens: {name} {num} {seq} {seq:N} {date} {date:%Y%m%d} {folder} {camera} {lens} {iso} {rating} {title} {creator} {ext} {text:Label}; photo.renameTokens explains each), start?: first sequence number (1), texts?: {label: text} (what each {text:Label} becomes), ids?}: renames the files on disk (sidecars too, never overwriting: collisions get -1, -2…); undoable", has_selection, |s, p| {
            let (ids, template, start) = rename_args(s, p, "photo.renamePreview", false)?;
            Ok(serde_json::to_value(s.plan_rename(&ids, &template, start)).unwrap_or_default())
        }),
        cmd!(
            "photo.rename",
            "Rename Photos",
            [],
            None,
            "{template: e.g. `{date}_{name}`, `Trip-{seq:3}` (tokens: {name} {num} {seq} {seq:N} {date} {date:%Y%m%d} {folder} {camera} {lens} {iso} {rating} {title} {creator} {ext} {text:Label}; photo.renameTokens explains each), start?: first sequence number (1), texts?: {label: text} (what each {text:Label} becomes), ids?}: renames the files on disk (sidecars too, never overwriting: collisions get -1, -2…); undoable",
            has_selection,
            |s, p| {
                let (ids, template, start) = rename_args(s, p, "photo.rename", true)?;
                // a browsed photo's file is only looked at until it is imported (LR-IMP-NODELETE)
                if ids.iter().any(|id| s.catalog.photo(*id).is_some_and(|ph| ph.local)) {
                    return Err(bad("photo.rename", "browsed photos are not in the catalog: import them before renaming their files"));
                }
                let plans = s.plan_rename(&ids, &template, start);
                let n = s.apply_rename(&plans)?;
                Ok(json!({"renamed": n, "plans": plans}))
            }
        ),
        cmd!(
            "photo.setCaptureTime",
            "Edit Capture Time",
            [],
            None,
            "{ids?, time?: `2026-09-30T14:05:00` (the active photo gets it, the others shift by the same amount), or a year or month alone (`1983`, `1983-04`; kept as stated, for one photo or with `each`), each?: bool (every photo gets `time`), fromFile?: bool (each photo gets its original file's modification time, UTC), fromExif?: bool (each photo gets the capture time its original file's Exif states, DateTimeOriginal with its offset and sub-seconds: Revert Capture Time to Original; photos whose file states none are left alone), fromFilename?: bool (each photo gets the date and time its file name states, as in `IMG_20260921_143000.jpg` or `2026-09-21 14.30.00.heic`; a date alone is midnight), shift?: seconds, hours?: time-zone shift in hours (both also move fromFile / fromFilename times), dryRun?: bool (report, change nothing)} → {changed, captured: [..], noFile, noExif: photos whose file states no capture time, noDateInName: photos whose file name holds no date, unreadable: photos skipped because their capture time can't be read, noTime: photos skipped because their capture time is a year or month alone, with no time to shift}",
            has_selection,
            |s, p| {
                use lcc_catalog::dates::{iso_seconds, normalize_iso, partial_capture, shift_iso};
                let c = "photo.setCaptureTime";
                let targets: Vec<_> = s.targets(p)?.into_iter().filter(|id| s.catalog.photo(*id).is_some()).collect();
                if targets.is_empty() {
                    return Err(bad(c, "no photos"));
                }
                // photos without a capture time start from their import time
                let base = |s: &crate::Session, id| s.catalog.photo(id).map(|p| p.date().to_string()).unwrap_or_default();
                // seconds; a shift beyond 10,000 years is a mistake (and would overflow), not a date
                const MAX_SHIFT: f64 = 10_000.0 * 365.25 * 86_400.0;
                let shift = f64_or(p, "shift", 0.0) + f64_or(p, "hours", 0.0) * 3600.0;
                if shift.is_nan() || shift.abs() > MAX_SHIFT {
                    return Err(bad(c, "the shift is more than 10,000 years"));
                }
                let mut delta = shift.round() as i64;
                let mut each: Option<String> = None;
                // a year or a month alone (`1983`, `1983-04`: a scanned print's date) is kept as
                // stated; it has no time to move other photos by, nor one to shift
                let partial = str_param(p, "time").map(str::trim).filter(|t| partial_capture(t).is_some());
                if let Some(t) = partial {
                    if targets.len() > 1 && !bool_or(p, "each", false) {
                        return Err(bad(c, "a year or month alone can't move other photos by the same amount; give every photo it with `each`"));
                    }
                    if delta != 0 {
                        return Err(bad(c, "a year or month alone can't be shifted"));
                    }
                    each = Some(t.to_string());
                } else if let Some(t) = str_param(p, "time") {
                    // the zone typed (`…+01:00`) is kept, as an import from Exif keeps it
                    let t = lcc_catalog::dates::normalize_iso_keeping_zone(t)
                        .ok_or_else(|| bad(c, format!("`{t}` is not a date (YYYY-MM-DDTHH:MM:SS[±HH:MM], or YYYY / YYYY-MM)")))?;
                    // one photo simply gets the time as typed, zone included
                    if bool_or(p, "each", false) || targets.len() == 1 {
                        each = Some(t);
                    } else {
                        let anchor = s.active().filter(|a| targets.contains(a)).unwrap_or(targets[0]);
                        let anchor_date = base(s, anchor);
                        // An impossible calendar date cannot anchor changes to the other photos. A
                        // year or a month alone reads (as the start of its period) but has no
                        // time to move the others by: alone, it gets the time below.
                        let start = lcc_catalog::dates::capture_start(&anchor_date);
                        let from = normalize_iso(start.trim().get(..10).unwrap_or(""))
                            .and_then(|_| iso_seconds(&start))
                            .ok_or_else(|| bad(c, "the photo's date doesn't parse"))?;
                        // the others move by the anchor's change, so it must read in full, not just
                        // its leading digits (a Lightroom catalog's capture time is copied verbatim);
                        // alone, it simply gets the time, which repairs it
                        if shift_iso(&anchor_date, 0).is_some() {
                            delta += iso_seconds(&t).unwrap_or(from) - from;
                        } else if targets.len() == 1 {
                            each = Some(t);
                        } else if partial_capture(anchor_date.trim()).is_some() {
                            return Err(bad(
                                c,
                                "the active photo's capture time is a year or month alone, with no time to move the other photos by; set its time with it selected alone",
                            ));
                        } else {
                            return Err(bad(
                                c,
                                "the active photo's capture time can't be read, so the other photos can't move by the same amount; set its time with it selected alone",
                            ));
                        }
                    }
                }
                let from_file = bool_or(p, "fromFile", false);
                let from_name = bool_or(p, "fromFilename", false);
                let from_exif = bool_or(p, "fromExif", false);
                if [from_file, from_name, from_exif].iter().filter(|b| **b).count() > 1 {
                    return Err(bad(c, "choose one of fromFile, fromFilename and fromExif"));
                }
                let mut no_exif = 0usize;
                let dry_run = bool_or(p, "dryRun", false);
                let mut no_name = 0usize;
                let mut ops = Vec::new();
                let mut out = Vec::new();
                let mut no_file = 0usize;
                let mut out_of_range = 0usize;
                let mut unreadable = 0usize;
                let mut no_time = 0usize;
                // a year or a month alone typed is given as it is (a file's time replaces it)
                let as_stated = partial.is_some() && !from_file && !from_name && !from_exif;
                for id in &targets {
                    let from = if from_file {
                        // the original's modification time (UTC; `hours` moves it to the camera's zone)
                        let Some(t) = s.catalog.photo(*id).and_then(|p| file_time(p)) else {
                            no_file += 1;
                            continue;
                        };
                        t
                    } else if from_exif {
                        let Some(t) = s.catalog.photo(*id).and_then(|p| exif_time(p)) else {
                            no_exif += 1;
                            continue;
                        };
                        t
                    } else if from_name {
                        let Some(t) = s.catalog.photo(*id).and_then(|p| lcc_catalog::dates::date_from_filename(&p.file_name)) else {
                            no_name += 1;
                            continue;
                        };
                        t
                    } else {
                        match &each {
                            Some(t) => t.clone(),
                            None => base(s, *id),
                        }
                    };
                    let new = if as_stated { Some(from.clone()) } else { shift_iso(&from, delta) };
                    let Some(new) = new else {
                        // a date that reads but would leave years 0000–9999 is refused below; one
                        // that doesn't read is skipped and counted
                        if shift_iso(&from, 0).is_some() {
                            out_of_range += 1;
                        } else if partial_capture(from.trim()).is_some() {
                            // a year or a month alone reads, but has no time to shift
                            no_time += 1;
                        } else {
                            unreadable += 1;
                        }
                        continue;
                    };
                    out.push(json!(new));
                    if s.catalog.photo(*id).and_then(|p| p.captured.as_deref()) != Some(new.as_str()) {
                        ops.push(Op::SetCaptured { id: *id, captured: Some(new) });
                    }
                }
                if out_of_range > 0 {
                    return Err(bad(c, format!("the shift would take {out_of_range} photo(s) outside the years 0000–9999; nothing was changed")));
                }
                let n = ops.len();
                if from_file && out.is_empty() {
                    return Err(bad(c, "none of the photos has a readable original file"));
                }
                if from_exif && out.is_empty() {
                    return Err(bad(c, "none of the photos' files states a capture time"));
                }
                if from_name && out.is_empty() {
                    return Err(bad(c, "none of the photos' file names holds a date"));
                }
                if n > 0 && !dry_run {
                    s.commit("Edit Capture Time", Op::Batch { ops })?;
                }
                Ok(
                    json!({"changed": n, "captured": out, "noFile": no_file, "noExif": no_exif, "noDateInName": no_name, "unreadable": unreadable, "noTime": no_time, "dryRun": dry_run}),
                )
            }
        ),
        cmd!(
            "photo.autoTagTracklog",
            "Auto-Tag Photos from Tracklog",
            [],
            None,
            "{path?: GPX file | gpx?: GPX text, ids?, offset?: camera clock's UTC offset for photos whose capture time has no zone (`+02:00`, or hours; default UTC), maxGap?: seconds (600), replace?: bool (also photos that already have GPS), dryRun?: bool} → {tagged, interpolated, nearest, skipped: {noTime, outside, hasGps}, assumedUtc, points, start, end, photos: [{id, gps, match}]}: positions from the track log by capture time; one undo step",
            has_selection,
            auto_tag_tracklog
        ),
        cmd!(query "label.names", "Color Label Names", [], None, "{} → [{label, name, custom}]", always, |s, _| {
            Ok(json!(ColorLabel::ALL
                .iter()
                .map(|l| json!({"label": format!("{l:?}").to_lowercase(), "name": s.catalog.label_name(*l), "custom": s.catalog.custom_label_name(*l)}))
                .collect::<Vec<_>>()))
        }),
        cmd!(
            "label.setNames",
            "Edit Color Label Names",
            [],
            None,
            "{names: {red?: name|null, yellow?, green?, blue?, purple?}}: null or empty restores the colour's name",
            always,
            |s, p| {
                let names = p.get("names").and_then(Value::as_object).ok_or_else(|| bad("label.setNames", "missing `names`"))?;
                let mut ops = Vec::new();
                for (k, v) in names {
                    let label = ColorLabel::parse(k).ok_or_else(|| bad("label.setNames", format!("unknown label `{k}`")))?;
                    let name =
                        v.as_str().map(str::trim).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&format!("{label:?}"))).map(str::to_string);
                    if s.catalog.custom_label_name(label) != name.as_deref() {
                        ops.push(Op::SetLabelName { label, name });
                    }
                }
                let n = ops.len();
                if n > 0 {
                    s.commit("Edit Label Names", Op::Batch { ops })?;
                }
                Ok(json!({"changed": n}))
            }
        ),
        cmd!(query "label.sets", "Color Label Sets", [], None, "{} → {sets: [{name, names: [red, yellow, green, blue, purple], builtin}], current: name|null}", always, |s, _| Ok(label_sets_json(s))),
        cmd!("label.applySet", "Apply Color Label Set", [], None, "{name}: use a set's label names (undoable)", always, |s, p| {
            let name = str_param(p, "name").ok_or_else(|| bad("label.applySet", "missing `name`"))?;
            let set = all_label_sets(s)
                .into_iter()
                .find(|x| x.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| bad("label.applySet", format!("no label set `{name}`")))?;
            let ops = set_names_ops(s, &set.names);
            let n = ops.len();
            if n > 0 {
                s.commit(&format!("Label Set: {}", set.name), Op::Batch { ops })?;
            }
            Ok(json!({"changed": n}))
        }),
        cmd!(
            "label.saveSet",
            "Save Color Label Set",
            [],
            None,
            "{name}: save the current label names as a set (replaces a user set of that name)",
            always,
            |s, p| {
                let name =
                    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("label.saveSet", "missing `name`"))?.to_string();
                if builtin_label_sets().iter().any(|b| b.name.eq_ignore_ascii_case(&name)) {
                    return Err(bad("label.saveSet", format!("`{name}` is a built-in set")));
                }
                let set = LabelSet { name: name.clone(), names: ColorLabel::ALL.map(|l| s.catalog.custom_label_name(l).unwrap_or("").to_string()) };
                match s.label_sets.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&name)) {
                    Some(x) => *x = set,
                    None => s.label_sets.push(set),
                }
                s.save_prefs()?;
                Ok(json!({"name": name}))
            }
        ),
        cmd!("label.deleteSet", "Delete Color Label Set", [], None, "{name}: user sets only", always, |s, p| {
            let name = str_param(p, "name").ok_or_else(|| bad("label.deleteSet", "missing `name`"))?;
            let before = s.label_sets.len();
            s.label_sets.retain(|x| !x.name.eq_ignore_ascii_case(name));
            if s.label_sets.len() == before {
                return Err(bad("label.deleteSet", format!("no user label set `{name}`")));
            }
            s.save_prefs()?;
            Ok(json!({"deleted": name}))
        }),
        // ---- what dialogs remember
        cmd!(
            "dialog.remember",
            "Remember Dialog Choices",
            [],
            None,
            "{dialog: id (at most 64 characters), choices: any JSON (at most 64 KB)}: keep a dialog's last choices across restarts and libraries; null forgets them",
            always,
            |s, p| {
                let c = "dialog.remember";
                let dialog =
                    str_param(p, "dialog").map(str::trim).filter(|d| !d.is_empty() && d.len() <= 64).ok_or_else(|| bad(c, "missing `dialog`"))?;
                let choices = p.get("choices").cloned().unwrap_or(Value::Null);
                if choices.is_null() {
                    if s.dialogs.other.remove(dialog).is_some() {
                        s.save_prefs()?;
                    }
                    return Ok(json!({"dialog": dialog}));
                }
                if serde_json::to_vec(&choices).ok().is_none_or(|b| b.len() > crate::app_store::MAX_REMEMBERED_BYTES) {
                    return Err(bad(c, "the choices are too large to remember"));
                }
                if !s.dialogs.other.contains_key(dialog) && s.dialogs.other.len() >= crate::app_store::MAX_REMEMBERED_DIALOGS {
                    return Err(bad(c, "too many dialogs remembered"));
                }
                if s.dialogs.other.get(dialog) != Some(&choices) {
                    s.dialogs.other.insert(dialog.to_string(), choices);
                    s.save_prefs()?;
                }
                Ok(json!({"dialog": dialog}))
            }
        ),
        cmd!(query "dialog.recall", "Recall Dialog Choices", [], None, "{dialog} → the choices dialog.remember kept (null: none)", always, |s, p| {
            let dialog = str_param(p, "dialog").ok_or_else(|| bad("dialog.recall", "missing `dialog`"))?;
            Ok(s.dialogs.other.get(dialog).cloned().unwrap_or(Value::Null))
        }),
    ]
}
