use super::cull::cull_params;
use super::export::export_dialog_params;
use super::{Dialog, LightcraftApp, json};

/// Runs a command that creates an album and asks the Albums tree to open the folders down to it.
fn created_in(app: &mut LightcraftApp, command: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let r = app.run(command, params)?;
    app.ui.reveal_album = r.get("id").and_then(serde_json::Value::as_u64);
    Ok(r)
}

/// The albums an Album rule in smart album `editing` picks from, in the sidebar's order: the album
/// itself and any smart album that tests it (testing it back would loop) are blocked, with why.
pub(crate) fn album_entries(cat: &lightcraft_catalog::Catalog, editing: Option<u64>) -> Vec<crate::album_picker::AlbumEntry> {
    let editing = editing.map(lightcraft_catalog::AlbumId);
    crate::album_picker::entries_from(cat, |a| match editing {
        Some(e) if a.id == e => Some(crate::i18n::tr("This is the album you're editing").to_string()),
        Some(e) if cat.album_reaches(a.id, e) => Some(crate::i18n::tr("It tests this album, so testing it back would loop").to_string()),
        _ => None,
    })
}

pub fn confirm_dialog(app: &mut LightcraftApp, dlg: &Dialog) -> Result<serde_json::Value, String> {
    match dlg {
        Dialog::LightroomResult { first, .. } => {
            if let Some(first) = first {
                app.run("library.source", json!({"kind": "all"}))?;
                let mut filter = serde_json::to_value(lightcraft_catalog::Filter::default()).map_err(|e| e.to_string())?;
                // The filter command merges a patch; these defaults are omitted by serde.
                filter["only"] = json!([]);
                filter["labels"] = json!([]);
                filter["flags"] = json!([]);
                filter["ruleSet"] = serde_json::Value::Null;
                app.run("library.filter", filter)?;
                app.run("library.select", json!({"ids": [first]}))?;
                app.ui.view = crate::state::ViewMode::PhotoGrid;
            }
            Ok(serde_json::Value::Null)
        }
        Dialog::SamModel { then, .. } => {
            if app.session.segmenter.installed() {
                // installed: start what the user was doing
                if let Some((kind, op)) = then {
                    crate::panels::masking::begin_ai(app, kind, op)?;
                }
                return Ok(serde_json::Value::Null);
            }
            let r = app.run("segment.model.download", json!({"acknowledged": true}));
            if r.is_ok() {
                app.ui.sam_downloading = true;
            }
            r
        }
        Dialog::NewAlbum { name, folder, parent, include_selected } => {
            created_in(app, "album.create", json!({"name": name, "folder": folder, "addSelected": !folder && *include_selected, "parent": parent}))
        }
        Dialog::RenameAlbum { id, name } => app.run("album.rename", json!({"id": id, "name": name})),
        Dialog::TextPrompt { value, command, params, key, .. } => {
            // Rename Folder opens with the folder's own name typed: confirming it unchanged renames nothing, as it always has
            // (the engine would refuse it as a name that "already exists"). A name with spaces at its ends is the same
            // folder when typed back as it is, so it is compared as typed as well as trimmed.
            let kept = command == "folder.rename"
                && params["path"].as_str().is_some_and(|path| {
                    std::path::Path::new(path.trim_end_matches(['/', '\\']))
                        .file_name()
                        .is_some_and(|f| f.to_string_lossy() == value.as_str() || f.to_string_lossy() == value.trim())
                });
            if kept {
                return Ok(serde_json::Value::Null);
            }
            let mut p = params.clone();
            p[key.as_str()] = json!(value.trim());
            app.run(command, p)
        }
        Dialog::CaptureTime { mode, time, days, hours, minutes, zone } => app.run(
            "photo.setCaptureTime",
            match mode.as_str() {
                "set" => json!({"time": time}),
                "shift" => json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60}),
                _ => json!({"hours": zone}),
            },
        ),
        Dialog::LabelNames { names, save_as } => {
            let mut m = serde_json::Map::new();
            for (l, n) in lightcraft_catalog::ColorLabel::ALL.iter().zip(names) {
                m.insert(format!("{l:?}").to_lowercase(), if n.trim().is_empty() { serde_json::Value::Null } else { json!(n.trim()) });
            }
            let r = app.run("label.setNames", json!({"names": m}));
            if r.is_ok() && !save_as.trim().is_empty() {
                return app.run("label.saveSet", json!({"name": save_as.trim()}));
            }
            r
        }
        Dialog::Rename { template, start } => app.run("photo.rename", json!({"template": template, "start": start})),
        Dialog::RenameKeyword { from, to } => app.run("keyword.rename", json!({"from": from, "to": to})),
        Dialog::RenamePerson { from, to, merge } => app.run("faces.renamePerson", json!({"from": from, "to": to.trim(), "merge": merge})),
        Dialog::KeywordTag {
            editing,
            name,
            parent,
            inside,
            synonyms,
            include_on_export,
            export_containing,
            export_synonyms,
            person,
            add_to_selected,
            reveal,
        } => {
            let mut params = json!({
                "synonyms": synonyms.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
                "includeOnExport": include_on_export,
                "exportContaining": export_containing,
                "exportSynonyms": export_synonyms,
                "person": person,
            });
            match editing {
                Some(keyword) => {
                    params["keyword"] = json!(keyword);
                    params["name"] = json!(name);
                    let r = app.run("keyword.edit", params);
                    if r.is_ok() {
                        let to = match keyword.rsplit_once('|') {
                            Some((parent, _)) => format!("{parent}|{}", name.trim()),
                            None => name.trim().to_string(),
                        };
                        crate::panels::keyword_list::follow(app, keyword, &to);
                    }
                    r
                }
                None => {
                    params["name"] = json!(name);
                    params["parent"] = if *inside { json!(parent) } else { serde_json::Value::Null };
                    params["addToSelected"] = json!(add_to_selected);
                    let r = app.run("keyword.create", params);
                    if *reveal && let Ok(made) = &r {
                        app.ui.reveal_keyword = made["keyword"].as_str().map(str::to_string);
                    }
                    r
                }
            }
        }
        Dialog::DeleteKeyword { keyword, .. } => app.run("keyword.delete", json!({"keyword": keyword})),
        Dialog::KeywordSet { replaces, name, slots, as_new } => {
            use lightcraft_catalog::keywords::same;
            let name = name.trim();
            if name.is_empty() {
                return Err(crate::i18n::tr("A keyword set needs a name").to_string());
            }
            if same(name, lightcraft_engine::cmd::keywords::RECENT) {
                return Err(crate::i18n::tr("Recent Keywords is built in: choose another name").to_string());
            }
            // the set it renames: none when saving as new, or when it was deleted meanwhile (then it is
            // saved anew rather than refused)
            let sets = &app.session.keyword_sets;
            let renames = replaces.as_deref().filter(|_| !as_new).filter(|old| sets.iter().any(|x| same(&x.name, old)));
            if sets.iter().any(|x| same(&x.name, name) && !renames.is_some_and(|old| same(&x.name, old))) {
                return Err(crate::i18n::tr_format!("There is a keyword set “{name}” already", name = name));
            }
            let mut params = json!({"name": name, "keywords": slots});
            match renames {
                Some(old) => params["replace"] = json!(old),
                None => params["new"] = json!(true),
            }
            app.run("keyword.saveSet", params)
        }
        Dialog::MoveKeyword { keyword, parent } => {
            let r = app.run("keyword.move", json!({"keyword": keyword, "parent": parent, "merge": true}));
            if r.is_ok() {
                let leaf = keyword.rsplit('|').next().unwrap_or(keyword);
                let to = parent.as_deref().map_or_else(|| leaf.to_string(), |p| format!("{p}|{leaf}"));
                crate::panels::keyword_list::follow(app, keyword, &to);
            }
            r
        }
        Dialog::MergeKeywords { from, into } => app.run("keyword.merge", json!({"from": from, "into": into})),
        Dialog::AutoStack { gap } => app.run("stack.auto", json!({"gap": gap})),
        Dialog::DenoiseModel { info, accepted } => crate::panels::denoise::install(app, info, *accepted),
        Dialog::FaceModel { path, info, accepted } => crate::panels::faces::install(app, path, info, *accepted),
        Dialog::AllMetadata { .. } | Dialog::SystemInfo { .. } | Dialog::WhatsNew => Ok(serde_json::Value::Null),
        Dialog::Cull { reject_below, pick_best, burst_gap, similarity } => {
            app.ui.cull_burst = Some((*burst_gap, *similarity));
            app.run("photo.analyze", cull_params(*reject_below, *pick_best, *burst_gap, *similarity))
        }
        Dialog::SmartRules { id, name, rules, parent } => {
            // refused before anything changes (not even the name)
            if let Some(problem) = rules.check_for(&app.session.catalog, id.map(lightcraft_catalog::AlbumId)).first() {
                return Err(problem.to_string());
            }
            let name = if name.trim().is_empty() { "Smart Album".to_string() } else { name.trim().to_string() };
            match id {
                // the name and the rules together: one undo step, both or neither
                Some(id) => app.run("album.setRules", json!({"id": id, "name": name, "replace": true, "rules": {"ruleSet": rules}})),
                None => created_in(app, "album.createSmart", json!({"name": name, "rules": {"ruleSet": rules}, "parent": parent})),
            }
        }
        Dialog::NewSmartAlbum { name, parent } => {
            created_in(app, "album.createSmart", json!({"name": if name.trim().is_empty() { "Smart Album" } else { name }, "parent": parent}))
        }
        Dialog::CreatePreset { name, group, groups } => app.run(
            "preset.create",
            json!({
                "name": if name.trim().is_empty() { "My Preset" } else { name },
                "group": if group.trim().is_empty() { "User Presets" } else { group },
                "groups": groups,
            }),
        ),
        Dialog::ContactSheet { options } => app.run("app.contactSheet", json!(options)),
        Dialog::SlideshowExport { settings } => app.run("app.slideshowExport", json!(settings)),
        Dialog::Print => app.run("app.print", json!({})),
        Dialog::CopySettings { groups } => app.run("develop.copy", json!({"groups": groups})),
        Dialog::PasteSettings { groups } => app.run("develop.paste", json!({"groups": groups})),
        Dialog::Export { opts, full_size, resize, limit_kb, dir, .. } => {
            let mut p = export_dialog_params(opts, *full_size, resize, *limit_kb);
            p["dir"] = json!(dir);
            p["background"] = json!(true);
            app.run("app.export", p)
        }
        Dialog::Merge { opts } => crate::merge::start_final(app, opts),
        Dialog::Import { opts } => crate::import::start(app, opts),
        Dialog::ConfirmDelete { .. } => app.run("photo.delete", json!({})),
        Dialog::MovePhotos { ids, folder, dont_ask, library, .. } => {
            // another library opened meanwhile (from the menu bar, an agent): these numbers are
            // other photos there
            if *library != app.session.library_generation() {
                return Err(crate::i18n::tr("Another library was opened since the photos were dropped: nothing was moved").to_string());
            }
            let moved = crate::panels::left::move_photos(app, Some(ids), folder)?;
            // only a move that began turns the question off: one refused, or that found the photos there already,
            // leaves it as it was
            if *dont_ask && moved > 0 {
                app.ui.settings.ask_before_move = false;
            }
            Ok(json!({"moved": moved}))
        }
        // the catalog found: no file dialog; none: File ▸ Import Lightroom Catalog… asks for it
        Dialog::LightroomCatalog { catalog, .. } => {
            app.run("file.importLightroom", catalog.as_ref().map_or_else(|| json!({}), |c| json!({"path": c})))
        }
        d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::confirm(app, d),
        Dialog::SaveEffect { name, group, fields, amount, brush } => {
            crate::panels::mask_effects::save_confirm(app, name, group, fields, *amount, *brush)
        }
        Dialog::DeleteEffects { names } => app.run("mask.deleteEffect", json!({"names": names})),
        Dialog::RemoveFolder { path, disk, .. } => app.run("library.removeFolder", json!({"path": path, "disk": disk})),
        d @ Dialog::SynchronizeFolder { .. } => crate::sync::confirm(app, d),
        Dialog::About | Dialog::Shortcuts | Dialog::Settings { .. } | Dialog::ManageEffects { .. } | Dialog::ViewOptions => {
            Ok(serde_json::Value::Null)
        }
    }
}
