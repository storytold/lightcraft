use super::files::{edit_in, open_log_folder, open_original_in, show_in_finder, show_in_folders};
use super::ui_commands::{UI_COMMANDS, language_from_command};
use super::{BeforeAfter, Dialog, LightcraftApp, PickRequest, Picked, RightPanel, Value, ViewMode, Zoom, json, thumb_size_view};

fn panel(app: &mut LightcraftApp, ctx: &egui::Context, p: RightPanel, name: &str) {
    if app.ui.right == p && crate::docking::exposed(app, crate::docking::Panel::Tools) {
        app.ui.right = RightPanel::None;
        app.toast(ctx, crate::i18n::tr_format!("{name} Off", name = crate::i18n::tr(name)));
    } else {
        app.ui.right = p;
        app.toast(ctx, crate::i18n::tr_format!("{name} On", name = crate::i18n::tr(name)));
        if p.is_edit_tool() && !matches!(app.ui.view, ViewMode::Detail) {
            app.ui.view = ViewMode::Detail;
        }
    }
    if p != RightPanel::Masking && app.ui.tool != "wbPicker" {
        app.ui.tool.clear();
    }
    let _ = app.session.end_interaction();
}

/// `[` / `]` (size ×`k`) and ⇧`[` / ⇧`]` (feather +`df`) for the brush in use: the Remove tool's
/// (and its selected spot's) or the Masking brush's.
fn adjust_brush(app: &mut LightcraftApp, k: f32, df: f32) -> Value {
    if app.ui.right == RightPanel::Remove {
        app.ui.remove_size = (app.ui.remove_size * k).clamp(0.001, 0.25);
        app.ui.remove_feather = (app.ui.remove_feather + df).clamp(0.0, 100.0);
        if app.session.active_spot.is_some() {
            let mut p = json!({});
            if k != 1.0 {
                p["size"] = json!(app.ui.remove_size);
            }
            if df != 0.0 {
                p["feather"] = json!(app.ui.remove_feather);
            }
            let _ = app.run("spot.update", p);
        }
        json!({"size": app.ui.remove_size, "feather": app.ui.remove_feather})
    } else {
        app.ui.brush_size = (app.ui.brush_size * k).clamp(0.002, 0.5);
        app.ui.brush_feather = (app.ui.brush_feather + df).clamp(0.0, 100.0);
        json!({"size": app.ui.brush_size, "feather": app.ui.brush_feather})
    }
}

/// The `parent` folder id of a `dialog.new*` command (none: the top level).
fn parent_param(p: &Value) -> Option<u64> {
    p.get("parent").and_then(Value::as_u64)
}

/// `file.addPhotos` / `file.addFolder` / `file.addFromDevice` `{album?}`: the review of the scan
/// just started opens with that album chosen (an album's context menu ▸ Import ▸ …). An error
/// means another scan is running.
fn import_into_album(app: &mut LightcraftApp, p: &Value, r: Result<Value, String>) -> Result<Value, String> {
    if r.is_ok()
        && let Some(t) = &mut app.scan
    {
        t.album = p.get("album").and_then(Value::as_u64);
    }
    r
}

/// An sRGB colour from `"#rrggbb"` or `[r, g, b]` (0..255).
pub fn parse_rgb(v: &Value) -> Option<[u8; 3]> {
    if let Some(s) = v.as_str() {
        let h = s.strip_prefix('#').unwrap_or(s);
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        return Some([c(0)?, c(2)?, c(4)?]);
    }
    let a = v.as_array()?;
    let c = |i: usize| a.get(i)?.as_f64().map(|x| x.clamp(0.0, 255.0).round() as u8);
    Some([c(0)?, c(1)?, c(2)?])
}

/// The left-sidebar section a `view.sidebar*` command names (`section`), which this build must have.
fn sidebar_section_param(command: &str, p: &Value) -> Result<crate::sidebar::SidebarSection, String> {
    use crate::sidebar::SidebarSection;
    let Some(name) = p.get("section").and_then(Value::as_str) else {
        return Err(format!("{command}: `section` is needed ({})", SidebarSection::ids()));
    };
    let section = SidebarSection::from_id(name).ok_or_else(|| format!("{command}: unknown section `{name}` ({})", SidebarSection::ids()))?;
    if !section.available() {
        return Err(format!("{command}: there is no `{name}` section here"));
    }
    Ok(section)
}

/// An optional true / false parameter: absent or null is `None`, anything else is refused.
fn bool_param(command: &str, p: &Value, key: &str) -> Result<Option<bool>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(format!("{command}: `{key}` must be true or false, not {other}")),
    }
}

/// What Shift+H says it switched to (the pins themselves may be off screen).
fn tool_overlay_toast(mode: crate::state::ToolOverlay) -> String {
    use crate::state::ToolOverlay;
    crate::i18n::tr(match mode {
        ToolOverlay::Auto => "Tool Overlay: Auto",
        ToolOverlay::Always => "Tool Overlay: Always",
        ToolOverlay::Selected => "Tool Overlay: Selected",
        ToolOverlay::Never => "Tool Overlay: Never",
    })
    .to_string()
}

fn appearance_json(app: &LightcraftApp) -> Value {
    let s = &app.ui.settings;
    json!({"mode": s.appearance_mode, "darkTheme": s.dark_theme, "lightTheme": s.light_theme})
}

/// `view.appearance {mode?}`: set Appearance Mode, or step to the next one (Auto, Light, Dark).
/// The theme follows on the next frame ([`LightcraftApp::sync_theme`]).
fn appearance_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    use crate::state::AppearanceMode;
    let mode = match p.get("mode") {
        None | Some(Value::Null) => app.ui.settings.appearance_mode.next(),
        Some(v) => match v.as_str() {
            Some("auto") => AppearanceMode::Auto,
            Some("light") => AppearanceMode::Light,
            Some("dark") => AppearanceMode::Dark,
            _ => return Err(format!("view.appearance: unknown mode {v} (auto|light|dark)")),
        },
    };
    app.ui.settings.appearance_mode = mode;
    Ok(appearance_json(app))
}

/// `view.theme {theme}`: pick a theme; the mode becomes its family's (Dark or Light).
fn theme_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let ids = crate::theme::ThemeKind::ALL.map(|k| k.id()).join("|");
    let Some(id) = p.get("theme").and_then(Value::as_str) else {
        return Err(format!("view.theme: `theme` is required ({ids})"));
    };
    let Some(kind) = crate::theme::ThemeKind::ALL.into_iter().find(|k| k.id() == id) else {
        return Err(format!("view.theme: unknown theme `{id}` ({ids})"));
    };
    app.ui.settings.select_theme(kind);
    Ok(appearance_json(app))
}

/// Handle UI commands; `None` means "not a UI command — send it to the engine".
pub fn run_ui_command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "ui.dock" | "ui.dock.reset") {
        return Some(crate::docking::command(app, id == "ui.dock.reset", p));
    }
    if matches!(id, "library.inspectLightroom" | "library.importLightroom") {
        // the app's own context: a fresh one's repaints reach no window and its clock starts at zero
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        return Some(crate::lightroom_import::command(app, id, p, &ctx));
    }
    if id == "photo.reload" {
        // the files are read in the background (a raw's decode takes a second or more)
        return crate::reload::command(app, p);
    }
    if let Some(language) = language_from_command(id) {
        app.ui.language = language;
        // Immediately, not on the next frame: the reply and anything else run this frame
        // (menus rebuilt from it, toasts) are already in the new language.
        crate::i18n::set_language(app.ui.language);
        return Some(Ok(json!(app.ui.language)));
    }
    // the app's own context: a toast's time comes from its clock (a fresh context's clock is at 0,
    // and a toast set by it was long over by the app's clock: it never showed)
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    const PARTS: [fn(&mut LightcraftApp, &egui::Context, &str, &Value) -> Option<Result<Value, String>>; 12] = [
        ui_commands_1,
        ui_commands_2,
        ui_commands_3,
        ui_commands_4,
        ui_commands_5,
        ui_commands_6,
        ui_commands_7,
        ui_commands_8,
        ui_commands_9,
        ui_commands_10,
        ui_commands_11,
        ui_commands_12,
    ];
    PARTS.iter().find_map(|part| part(app, &ctx, id, p))
}

/// Part 1 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_1(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "view.photoGrid" => {
            app.ui.view = ViewMode::PhotoGrid;
            Ok(Value::Null)
        }
        "view.squareGrid" => {
            app.ui.view = ViewMode::SquareGrid;
            Ok(Value::Null)
        }
        "view.gridInfo" => {
            // {info?: filename | exposure | date} (cycles when omitted)
            let next = match p.get("info").and_then(Value::as_str) {
                Some(i @ ("filename" | "exposure" | "date")) => i.to_string(),
                Some(other) => return Some(Err(format!("view.gridInfo: unknown `{other}` (filename|exposure|date)"))),
                None => match app.ui.grid_info.as_str() {
                    "filename" => "exposure".into(),
                    "exposure" => "date".into(),
                    _ => "filename".into(),
                },
            };
            app.ui.grid_info = next;
            app.ui.show_filenames = true;
            // the caption is drawn on Square Grid cells only: from Photo Grid, show it there
            if app.ui.view == ViewMode::PhotoGrid {
                app.ui.view = ViewMode::SquareGrid;
            }
            Ok(json!({"info": app.ui.grid_info}))
        }
        "tool.keywordPainter" => {
            // {keyword?}: paint that keyword onto photos in the grid by clicking them; no keyword stops
            app.ui.keyword_painter = p.get("keyword").and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
            if app.ui.keyword_painter.is_some() && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::PhotoGrid;
            }
            if let Some(k) = app.ui.keyword_painter.clone() {
                app.ui.keyword_painter_text = k.clone();
                app.toast(ctx, crate::i18n::tr_format!("Painting “{k}”: click photos to add or remove it · Esc stops", k = k));
            }
            Ok(json!({"keyword": app.ui.keyword_painter}))
        }
        "tool.enablePainting" => {
            let k = app.ui.keyword_painter_text.trim().to_string();
            if app.ui.keyword_painter.is_some() {
                app.ui.keyword_painter = None;
                Ok(json!({"keyword": null}))
            } else {
                // the full-screen preview covers both the grid and the Keywords panel
                if app.ui.fullscreen {
                    crate::panels::slideshow::end(app);
                }
                if k.is_empty() {
                    app.ui.right = RightPanel::Keywords;
                    if app.ui.tool != "wbPicker" {
                        app.ui.tool.clear();
                    }
                    let _ = app.session.end_interaction();
                    if app.ui.section_folded("keywording") {
                        app.ui.toggle_folded("keywording");
                    }
                    app.toast(ctx, crate::i18n::tr("Type a keyword to paint in the Keywords panel"));
                    Ok(json!({"keyword": null}))
                } else {
                    run_ui_command(app, "tool.keywordPainter", &json!({"keyword": k})).unwrap_or(Ok(Value::Null))
                }
            }
        }
        "view.secondWindow" => {
            app.ui.second_window = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.second_window);
            Ok(json!({"show": app.ui.second_window}))
        }
        "view.photoCounts" => {
            app.ui.show_counts = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.show_counts);
            Ok(json!({"show": app.ui.show_counts}))
        }
        "view.sidebarSection" => {
            // {section, show?, collapsed?}: show or hide a section of the left sidebar, fold or
            // unfold it; with neither, a shown section is hidden and a hidden one shown
            let args = sidebar_section_param(id, p).and_then(|s| Ok((s, bool_param(id, p, "show")?, bool_param(id, p, "collapsed")?)));
            let (section, show, collapsed) = match args {
                Ok(args) => args,
                Err(e) => return Some(Err(e)),
            };
            let layout = &mut app.ui.sidebar;
            match (show, collapsed) {
                (None, None) => layout.set_hidden(section, !layout.is_hidden(section)),
                _ => {
                    if let Some(show) = show {
                        layout.set_hidden(section, !show);
                    }
                    if let Some(collapsed) = collapsed {
                        layout.set_collapsed(section, collapsed);
                    }
                }
            }
            Ok(json!({"section": section.id(), "show": !layout.is_hidden(section), "collapsed": layout.is_collapsed(section)}))
        }
        "view.sidebarMoveSection" => {
            // {section, direction: up | down} one place among the shown sections, or
            // {section, before: <section> | null} just above another section (null: last)
            let section = match sidebar_section_param(id, p) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let layout = &mut app.ui.sidebar;
            let moved = match (p.get("direction"), p.get("before")) {
                (Some(d), None) => match d.as_str() {
                    Some("up") => layout.move_by(section, true),
                    Some("down") => layout.move_by(section, false),
                    _ => return Some(Err(format!("{id}: `direction` must be \"up\" or \"down\", not {d}"))),
                },
                (None, Some(Value::Null)) => layout.move_before(section, None),
                (None, Some(b)) => {
                    let before = b.as_str().and_then(crate::sidebar::SidebarSection::from_id);
                    let Some(before) = before else {
                        return Some(Err(format!("{id}: `before` must be a section ({}) or null, not {b}", crate::sidebar::SidebarSection::ids())));
                    };
                    layout.move_before(section, Some(before))
                }
                _ => return Some(Err(format!("{id}: give either `direction` (up|down) or `before` (a section, or null for last)"))),
            };
            let order: Vec<&str> = layout.sections().iter().map(|s| s.id.id()).collect();
            Ok(json!({"section": section.id(), "moved": moved, "order": order}))
        }
        "view.sidebarReset" => {
            // every section shown and open, in the usual order
            app.ui.sidebar = Default::default();
            Ok(json!({"sidebar": app.ui.sidebar}))
        }
        "view.appearance" => appearance_command(app, p),
        "view.theme" => theme_command(app, p),
        "library.selectOnlyActive" => {
            let Some(active) = app.session.active() else { return Some(Err("no active photo".into())) };
            app.run("library.select", json!({"ids": [active.0], "active": active.0}))
        }
        "view.gridToggle" => {
            app.ui.view = if app.ui.view == ViewMode::PhotoGrid { ViewMode::SquareGrid } else { ViewMode::PhotoGrid };
            Ok(Value::Null)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 2 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_2(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "view.detail" => {
            app.ui.view = ViewMode::Detail;
            Ok(Value::Null)
        }
        "view.compare" => crate::panels::compare::enter_compare(app),
        "view.focusPoint" => {
            // {on?}; no params toggles. The camera's focus point (Sony, from the file) over the photo.
            app.ui.settings.focus_point = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.focus_point);
            Ok(json!({"on": app.ui.settings.focus_point}))
        }
        "view.zoomToFocus" => {
            // {on?}; no params toggles. Toggle Zoom (Z) centres on the focus point when there is one.
            app.ui.settings.zoom_to_focus = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.settings.zoom_to_focus);
            Ok(json!({"on": app.ui.settings.zoom_to_focus}))
        }
        "view.faceBoxes" => {
            app.ui.face_boxes = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.face_boxes);
            Ok(json!({"show": app.ui.face_boxes}))
        }
        "view.people" => {
            // Everyone; the page just left stays one click away (the chip next to the title)
            if let Some(name) = app.ui.person_page.take() {
                app.ui.last_person = Some(name);
            }
            app.ui.person_from = None;
            app.ui.view = ViewMode::People;
            Ok(json!({"people": app.session.catalog.people().len()}))
        }
        "view.person" => {
            // {name}: one person's page in the People view: their faces, and the faces that look like them
            let Some(name) = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) else {
                return Some(Err("view.person: missing `name`".into()));
            };
            app.ui.view = ViewMode::People;
            app.ui.person_from = None;
            app.ui.person_page = Some(name.to_string());
            Ok(json!({"person": name}))
        }
        "view.survey" => {
            app.ui.view = ViewMode::Survey;
            Ok(json!({"photos": crate::panels::compare::survey_photos(app).len()}))
        }
        "view.reference" => {
            // the reference: the one set before, else the active photo (the next one becomes active)
            let vis = app.session.visible_cloned();
            let live = |id: u64| crate::panels::compare::live(app, lightcraft_catalog::PhotoId(id));
            let active = app.session.active().map(|a| a.0);
            let reference = app.ui.reference.filter(|r| live(*r)).or_else(|| active.filter(|a| live(*a)));
            let Some(r) = reference else {
                return Some(Err(match active {
                    Some(a) => format!("photo {a} is in Recently Deleted: restore it to use it as the reference"),
                    None => "select a photo to use as the reference".into(),
                }));
            };
            app.ui.reference = Some(r);
            if app.session.active().is_none_or(|a| a.0 == r)
                && let Some(i) = vis.iter().position(|x| x.0 == r)
                && let Some(next) = vis.get(i + 1).or(i.checked_sub(1).and_then(|j| vis.get(j)))
            {
                let _ = app.session.execute("library.select", &json!({"ids": [next.0]}));
            }
            app.ui.view = ViewMode::Reference;
            Ok(json!({"reference": r, "active": app.session.active().map(|a| a.0)}))
        }
        "photo.setReference" => {
            // only an absent (or null) `id` means the active photo; any other non-id is refused
            let id = match p.get("id").filter(|v| !v.is_null()) {
                Some(v) => match v.as_u64() {
                    Some(id) => Some(id),
                    None => return Some(Err(format!("photo.setReference: `id` must be a photo id, not {v}"))),
                },
                None => app.session.active().map(|a| a.0),
            };
            let Some(id) = id else { return Some(Err("select a photo to use as the reference".into())) };
            match app.session.catalog.photo(lightcraft_catalog::PhotoId(id)) {
                None => return Some(Err(format!("no photo {id}"))),
                Some(p) if p.deleted => return Some(Err(format!("photo {id} is in Recently Deleted: restore it to use it as the reference"))),
                Some(_) => {}
            }
            app.ui.reference = Some(id);
            Ok(json!({"reference": id}))
        }
        "compare.swap" => crate::panels::compare::swap(app),
        "compare.set" => {
            let Some(id) = p.get("id").and_then(Value::as_u64) else { return Some(Err("compare.set needs `id`".into())) };
            crate::panels::compare::set_photo(app, lightcraft_catalog::PhotoId(id), p.get("side").and_then(Value::as_str))
        }
        "compare.makeSelect" => crate::panels::compare::make_select(app),
        "view.autoAdvance" => {
            app.ui.auto_advance = !app.ui.auto_advance;
            app.toast(ctx, crate::i18n::tr(if app.ui.auto_advance { "Auto Advance On" } else { "Auto Advance Off" }));
            Ok(json!({"autoAdvance": app.ui.auto_advance}))
        }
        "view.back" => view_back_command(app, ctx),
        "view.slideshow" => {
            // the photos in view, full screen, with the playback settings (see panels::slideshow)
            let now = app.tick_now();
            let r = crate::panels::slideshow::start(app, p, now);
            if r.is_ok() {
                app.toast(ctx, crate::i18n::tr("Slideshow · Space pauses · Esc ends"));
            }
            r
        }
        "slideshow.togglePause" | "slideshow.next" | "slideshow.previous" | "slideshow.end" => {
            let now = app.tick_now();
            crate::panels::slideshow::command(app, id, now)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 3 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_3(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "view.slideshowOptions" => {
            app.ui.dialog = Some(Dialog::Settings { tab: "slideshow".into() });
            Ok(Value::Null)
        }
        "view.fullScreenPreview" => {
            if app.ui.fullscreen {
                // Through `slideshow::end`: it also releases the held slide textures. Clearing the
                // two fields directly left every rendered slide in the renderer.
                crate::panels::slideshow::end(app);
            } else {
                app.ui.fullscreen = true;
                app.ui.zoom = Zoom::Fit;
                app.ui.tool.clear();
                let _ = app.session.end_interaction();
            }
            Ok(json!({"fullscreen": app.ui.fullscreen}))
        }
        "view.lightsOut" => {
            // {mode?: on | dim | off} (cycles when omitted)
            match p.get("mode").and_then(Value::as_str) {
                Some(m) => serde_json::from_value(json!(m)).map_err(|_| format!("unknown lights-out mode: {m} (on|dim|off)")),
                None => Ok(app.ui.lights_out.next()),
            }
            .map(|mode| {
                app.ui.lights_out = mode;
                json!({"lightsOut": mode})
            })
        }
        "view.enterFullScreen" => {
            // applied by the host's frame logic (it knows the window's current state)
            let on = p.get("on").and_then(Value::as_bool);
            app.ui.window_fullscreen = Some(on.unwrap_or(!app.window_is_fullscreen));
            Ok(json!({"windowFullscreen": app.ui.window_fullscreen}))
        }
        "view.infoOverlay" => {
            app.ui.info_overlay = match p.get("mode").and_then(Value::as_str) {
                Some(m) => match serde_json::from_value(json!(m)) {
                    Ok(v) => v,
                    Err(_) => return Some(Err(format!("unknown info overlay `{m}` (off|basic|exposure)"))),
                },
                None => app.ui.info_overlay.next(),
            };
            let label = match app.ui.info_overlay {
                crate::state::InfoOverlay::Off => "Info Overlay Off",
                crate::state::InfoOverlay::Basic => "Info Overlay: File",
                crate::state::InfoOverlay::Exposure => "Info Overlay: Exposure",
            };
            app.toast(ctx, crate::i18n::tr(label));
            Ok(json!({"infoOverlay": app.ui.info_overlay}))
        }
        "view.viewOptions" => view_view_options_command(app, p),
        "view.chooseLayoutImage" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(path) => path.trim().to_string(),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Choose Layout Image…"), "PNG", &["png"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => match paths.first() {
                            Some(path) => path.trim().to_string(),
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform; supply path".into())),
                    }
                }
            };
            if path.is_empty() {
                return Some(Err("view.chooseLayoutImage: path is empty".into()));
            }
            let li = &mut app.ui.layout_image;
            li.path = path;
            li.show = true;
            li.revision = li.revision.wrapping_add(1);
            app.toast(ctx, "Layout Image On");
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.layoutImage" => {
            let show = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.layout_image.show);
            // nothing chosen yet: showing it chooses one first
            if show && app.ui.layout_image.path.trim().is_empty() {
                return run_ui_command(app, "view.chooseLayoutImage", &json!({}));
            }
            app.ui.layout_image.show = show;
            app.toast(ctx, if show { "Layout Image On" } else { "Layout Image Off" });
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.layoutImageOpacity" | "view.layoutImageMatte" => {
            let key = if id == "view.layoutImageOpacity" { "opacity" } else { "matte" };
            let Some(v) = p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()) else {
                return Some(Err(format!("{id}: {key} (0..1) is required")));
            };
            let v = v.clamp(0.0, 1.0) as f32;
            if key == "opacity" {
                app.ui.layout_image.opacity = v;
            } else {
                app.ui.layout_image.matte = v;
            }
            Ok(json!({"layoutImage": app.ui.layout_image}))
        }
        "view.navigator" => {
            app.ui.navigator = !app.ui.navigator;
            Ok(json!({"navigator": app.ui.navigator}))
        }
        _ => return None,
    };
    Some(r)
}

/// Part 4 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_4(app: &mut LightcraftApp, _ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "app.settings" => {
            let tab = p.get("tab").and_then(Value::as_str).unwrap_or("general");
            if !crate::panels::settings::TABS.iter().any(|(id, _)| *id == tab) {
                let tabs: Vec<&str> = crate::panels::settings::TABS.iter().map(|(id, _)| *id).collect();
                return Some(Err(format!("unknown settings tab `{tab}` ({})", tabs.join("|"))));
            }
            app.ui.dialog = Some(Dialog::Settings { tab: tab.into() });
            Ok(Value::Null)
        }
        "app.openLibrary" => crate::panels::settings::open_library(app, p),
        "app.newLibrary" => crate::panels::settings::new_library(app, p),
        "app.recentLibraries" => crate::panels::settings::recent_libraries(app, p),
        "app.clearRecentLibraries" => crate::panels::settings::recent_libraries(app, &json!({"clear": true})),
        "app.displayProfile" => crate::panels::settings::display_profile(app, p),
        "file.backupLibrary" | "file.restoreLibrary" => {
            let action = if id == "file.backupLibrary" { app.services.backup_library.as_mut() } else { app.services.restore_library.as_mut() };
            match action {
                Some(f) => f(&mut app.session),
                None => Err("not available here: on the desktop the library is a folder; back it up with your other files".into()),
            }
        }
        "view.filmstrip" => {
            app.ui.filmstrip = !app.ui.filmstrip;
            Ok(Value::Null)
        }
        "view.frameRate" => {
            // {show?: bool} (toggles when omitted)
            app.ui.frame_rate = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.frame_rate);
            Ok(json!({"show": app.ui.frame_rate}))
        }
        "view.leftPanel" => {
            app.ui.left_panel = !app.ui.left_panel;
            Ok(Value::Null)
        }
        "view.rightPanel" => {
            // Classic's F8: hide the right panel, and bring back the same one next time (Edit when
            // none was hidden). Hiding a tool ends it, as closing its panel does.
            if app.ui.right != RightPanel::None && crate::docking::exposed(app, crate::docking::Panel::Tools) {
                app.ui.right_hidden = app.ui.right;
                app.ui.right = RightPanel::None;
                if app.ui.tool != "wbPicker" {
                    app.ui.tool.clear();
                }
                let _ = app.session.end_interaction();
            } else if app.ui.right == RightPanel::None {
                let p = match std::mem::take(&mut app.ui.right_hidden) {
                    RightPanel::None => RightPanel::Edit,
                    p => p,
                };
                app.ui.right = p;
                if p.is_edit_tool() && !matches!(app.ui.view, ViewMode::Detail) {
                    app.ui.view = ViewMode::Detail;
                }
                if p == RightPanel::Remove && app.ui.tool.is_empty() {
                    app.ui.tool = "remove".into();
                }
            }
            // A right panel that is open but not showing (minimized, or behind another tab)
            // keeps its mode; reveal_command brings it to the front.
            Ok(json!({"open": app.ui.right != RightPanel::None}))
        }
        "view.beforeAfter" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::SideBySide { BeforeAfter::Off } else { BeforeAfter::SideBySide };
            Ok(Value::Null)
        }
        "view.beforeAfterSplit" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::Split { BeforeAfter::Off } else { BeforeAfter::Split };
            Ok(Value::Null)
        }
        "view.beforeAfterTopBottom" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::TopBottom { BeforeAfter::Off } else { BeforeAfter::TopBottom };
            Ok(Value::Null)
        }
        "view.beforeAfterSplitTopBottom" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::SplitTopBottom { BeforeAfter::Off } else { BeforeAfter::SplitTopBottom };
            Ok(Value::Null)
        }
        "view.showOriginal" => {
            app.ui.before_after = if app.ui.before_after == BeforeAfter::Original { BeforeAfter::Off } else { BeforeAfter::Original };
            Ok(Value::Null)
        }
        "view.zoomFit" => {
            app.ui.zoom = Zoom::Fit;
            Ok(Value::Null)
        }
        "view.zoom100" => {
            app.ui.zoom = Zoom::Percent(100.0);
            Ok(Value::Null)
        }
        "view.zoomToggle" => {
            // the same ratio a click on the photo zooms to; zooming in centres on the camera's focus
            // point when the photo has one (View ▸ Zoom to Focus Point)
            let zoom_in = app.ui.zoom == Zoom::Fit;
            app.ui.zoom = if zoom_in { Zoom::Percent(app.ui.click_zoom as f32) } else { Zoom::Fit };
            app.ui.zoom_anim = true;
            if zoom_in
                && app.ui.settings.zoom_to_focus
                && let Some(pan) = app.session.active().and_then(|id| crate::panels::detail::focus_pan(app, id))
            {
                app.ui.pan = pan;
                return Some(Ok(json!({"focus": [pan.0, pan.1]})));
            }
            Ok(Value::Null)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 5 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_5(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "view.clickZoom" => {
            // {ratio?: 1|2|3|4|8} → {ratio}
            if let Some(r) = p.get("ratio").and_then(Value::as_f64) {
                let pct = (r * 100.0).round() as u32;
                if !crate::state::CLICK_ZOOMS.contains(&pct) {
                    return Some(Err(format!("view.clickZoom: ratio {r} (1, 2, 3, 4 or 8)")));
                }
                app.ui.click_zoom = pct;
            }
            Ok(json!({"ratio": app.ui.click_zoom / 100}))
        }
        "view.zoomIn" | "view.zoomOut" => {
            let steps = [25.0, 50.0, 100.0, 200.0, 400.0, 800.0];
            let cur = match app.ui.zoom {
                Zoom::Percent(p) => p,
                _ => 25.0,
            };
            let next = if id == "view.zoomIn" {
                steps.iter().find(|s| **s > cur).copied().unwrap_or(800.0)
            } else {
                steps.iter().rev().find(|s| **s < cur).copied().unwrap_or(0.0)
            };
            app.ui.zoom = if next == 0.0 { Zoom::Fit } else { Zoom::Percent(next) };
            Ok(Value::Null)
        }
        "view.thumbSizeUp" | "view.thumbSizeDown" => {
            // the bottom-bar slider's range, one `THUMB_SIZE_STEP` per call; {} → {size}
            if !thumb_size_view(app) {
                return Some(Err(format!("{id}: only in Photo Grid, Square Grid or People")));
            }
            let step = if id == "view.thumbSizeUp" { crate::state::THUMB_SIZE_STEP } else { -crate::state::THUMB_SIZE_STEP };
            app.ui.thumb_size = (app.ui.thumb_size + step).clamp(crate::state::THUMB_SIZE_MIN, crate::state::THUMB_SIZE_MAX);
            Ok(json!({"size": app.ui.thumb_size}))
        }
        "view.importThumbSize" => {
            // {size?: points}; without a size, read the current preference.
            let size = match p.get("size") {
                None => app.ui.import_thumb_size,
                Some(value) => match value.as_f64().map(|n| n as f32).filter(|n| n.is_finite()) {
                    Some(size) => size,
                    None => return Some(Err("view.importThumbSize: size must be a finite number".into())),
                },
            };
            app.ui.import_thumb_size = crate::import::thumb_size(size);
            Ok(json!({"size": app.ui.import_thumb_size}))
        }
        "view.navigate" => {
            // {zoom?: "fit"|"fill"|{percent: number}, pan?: [x, y]} (normalized image centre).
            // Validate the complete request before changing either part of the viewport.
            let zoom = match p.get("zoom") {
                Some(v) => match serde_json::from_value::<Zoom>(v.clone()) {
                    Ok(Zoom::Percent(p)) if !p.is_finite() || p <= 0.0 || p > 800.0 => {
                        return Some(Err("view.navigate: zoom percent must be greater than 0 and at most 800".into()));
                    }
                    Ok(z) => z,
                    Err(e) => return Some(Err(format!("view.navigate: {e}"))),
                },
                None => app.ui.zoom,
            };
            let pan = match p.get("pan") {
                Some(v) => match serde_json::from_value::<(f32, f32)>(v.clone()) {
                    Ok((x, y)) if x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) => (x, y),
                    _ => return Some(Err("view.navigate: pan must be [x, y] with finite coordinates from 0 to 1".into())),
                },
                None => app.ui.pan,
            };
            app.ui.zoom = zoom;
            app.ui.pan = pan;
            app.ui.zoom_anim = false;
            Ok(json!({"zoom": zoom, "pan": pan}))
        }
        "view.clipping" => {
            app.ui.show_clipping = !app.ui.show_clipping;
            Ok(Value::Null)
        }
        "view.visualizeHdr" => {
            // {on?}; no params toggles. Shown on photos edited in HDR (see `develop.hdr`).
            app.ui.hdr_visualize = p.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.hdr_visualize);
            Ok(json!({"on": app.ui.hdr_visualize}))
        }
        "view.softProof" => view_soft_proof_command(app, id, p),
        "view.histogram" => {
            app.ui.histogram = !app.ui.histogram;
            Ok(Value::Null)
        }
        "view.maskOverlay" => {
            app.ui.mask_overlay = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_overlay);
            Ok(json!({"maskOverlay": app.ui.mask_overlay}))
        }
        "view.maskOverlayAll" => {
            app.ui.mask_overlay_all = !app.ui.mask_overlay_all;
            Ok(json!({"maskOverlayAll": app.ui.mask_overlay_all}))
        }
        "view.maskOverlayMode" => {
            use lightcraft_engine::pipeline::MaskView;
            let cur = MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default();
            let next = match p.get("mode").and_then(Value::as_str) {
                Some(m) => match MaskView::parse(m) {
                    Some(v) => v,
                    None => {
                        let names: Vec<&str> = MaskView::ALL.iter().map(|v| v.name()).collect();
                        return Some(Err(format!("view.maskOverlayMode: unknown mode `{m}` ({})", names.join("|"))));
                    }
                },
                None => cur.next(),
            };
            app.ui.mask_overlay_mode = next.name().into();
            app.ui.mask_overlay = true;
            app.toast(ctx, next.label());
            Ok(json!({"mode": next.name()}))
        }
        _ => return None,
    };
    Some(r)
}

/// Part 6 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_6(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "view.maskOverlayColor" => {
            // no params: the next of the panel's swatch colours
            if p.get("color").is_none() && p.get("opacity").is_none() {
                let all = crate::panels::masking::OVERLAY_COLORS;
                let i = all.iter().position(|c| *c == app.ui.mask_overlay_color).map_or(0, |i| (i + 1) % all.len());
                app.ui.mask_overlay_color = all[i];
            }
            if let Some(c) = p.get("color") {
                match parse_rgb(c) {
                    Some(rgb) => app.ui.mask_overlay_color = rgb,
                    None => return Some(Err("view.maskOverlayColor: `color` is \"#rrggbb\" or [r, g, b]".into())),
                }
            }
            if let Some(o) = p.get("opacity").and_then(Value::as_f64) {
                app.ui.mask_overlay_opacity = o.clamp(0.0, 100.0) as f32;
            }
            let [r, g, b] = app.ui.mask_overlay_color;
            Ok(json!({"color": format!("#{r:02x}{g:02x}{b:02x}"), "opacity": app.ui.mask_overlay_opacity}))
        }
        "brush.smaller" | "brush.larger" | "brush.featherLess" | "brush.featherMore" => {
            let (k, df) = match id {
                "brush.smaller" => (1.0 / 1.2, 0.0),
                "brush.larger" => (1.2, 0.0),
                "brush.featherLess" => (1.0, -10.0),
                _ => (1.0, 10.0),
            };
            Ok(adjust_brush(app, k, df))
        }
        "view.maskPins" => {
            app.ui.mask_pins = p.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.mask_pins);
            Ok(json!({"maskPins": app.ui.mask_pins}))
        }
        "view.infoSelectedPhotos" => {
            // {on?}; no params toggles. On: the Info panel shows what the selected photos share.
            let on = match p.get("on") {
                None | Some(Value::Null) => !app.ui.settings.info_selected_photos,
                Some(Value::Bool(on)) => *on,
                Some(v) => return Some(Err(format!("view.infoSelectedPhotos: `on` must be true or false, not {v}"))),
            };
            app.ui.settings.info_selected_photos = on;
            Ok(json!({"on": on}))
        }
        "view.toolOverlay" => {
            use crate::state::ToolOverlay;
            let mode = match p.get("mode") {
                None | Some(Value::Null) => app.ui.tool_overlay.next(),
                Some(v) => match v.as_str().and_then(ToolOverlay::parse) {
                    Some(m) => m,
                    None => return Some(Err(format!("view.toolOverlay: unknown mode {v} (auto|always|selected|never)"))),
                },
            };
            app.ui.tool_overlay = mode;
            app.toast(ctx, tool_overlay_toast(mode));
            Ok(json!({"mode": mode.name()}))
        }
        "view.visualizeSpots" => {
            // like Lightroom's A: opens the Remove tool with the view on, or toggles it there
            if app.ui.right == RightPanel::Remove {
                app.ui.visualize_spots = !app.ui.visualize_spots;
            } else {
                app.ui.right = RightPanel::Remove;
                app.ui.visualize_spots = true;
            }
            Ok(Value::Null)
        }
        // A under the Crop panel: the aspect lock (the panel's button runs the same toggle)
        "crop.lockAspect" => app.run("crop.aspect", json!({"aspect": "toggle"})),
        "view.cropOverlay" => {
            use crate::state::CropOverlay::*;
            app.ui.crop_overlay = match app.ui.crop_overlay {
                Thirds => Grid,
                Grid => Golden,
                Golden => Diagonal,
                Diagonal => Triangle,
                Triangle => Spiral,
                Spiral => None,
                None => Thirds,
            };
            Ok(json!({"overlay": app.ui.crop_overlay}))
        }
        "view.cropOverlayOrientation" => {
            app.ui.crop_overlay_orient = (app.ui.crop_overlay_orient + 1) % 4;
            Ok(json!({"orientation": app.ui.crop_overlay_orient}))
        }
        "local.addRoot" => {
            // a folder kept in Local's sidebar (saved with the UI state); nothing on disk changes
            let Some(path) = p.get("path").and_then(Value::as_str) else { return Some(Err("local.addRoot needs a path".into())) };
            let abs = std::path::absolute(path).map(|a| a.to_string_lossy().trim_end_matches(['/', '\\']).to_string()).unwrap_or(path.into());
            let abs = if abs.is_empty() { path.to_string() } else { abs };
            if !std::path::Path::new(&abs).is_dir() {
                return Some(Err(format!("{path}: not a folder")));
            }
            use crate::panels::left::same_folder;
            if !app.ui.local_roots.iter().any(|r| same_folder(r, &abs)) {
                app.ui.local_roots.push(abs.clone());
            }
            app.ui.hidden_locations.retain(|h| !same_folder(h, &abs));
            Ok(json!({"roots": app.ui.local_roots}))
        }
        "local.hide" => match p.get("path").and_then(Value::as_str) {
            Some(path) => {
                if !app.ui.hidden_locations.iter().any(|h| crate::panels::left::same_folder(h, path)) {
                    app.ui.hidden_locations.push(path.to_string());
                }
                Ok(json!({"hidden": app.ui.hidden_locations}))
            }
            None => Err("local.hide needs a path".into()),
        },
        _ => return None,
    };
    Some(r)
}

/// Part 7 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_7(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "local.restoreHidden" => {
            // one path, or (no path) every hidden location
            match p.get("path").and_then(Value::as_str) {
                Some(path) => app.ui.hidden_locations.retain(|h| !crate::panels::left::same_folder(h, path)),
                None => app.ui.hidden_locations.clear(),
            }
            Ok(json!({"hidden": app.ui.hidden_locations}))
        }
        "view.soloSections" => bool_param(id, p, "on").map(|on| {
            app.ui.set_single_panel(on.unwrap_or(!app.ui.single_panel));
            json!({"soloSections": app.ui.single_panel})
        }),
        "view.filterBar" => {
            app.ui.filter_bar = !app.ui.filter_bar;
            if app.ui.filter_bar && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::PhotoGrid;
            }
            Ok(json!({"filterBar": app.ui.filter_bar}))
        }
        "panel.edit" => {
            panel(app, ctx, RightPanel::Edit, "Edit");
            Ok(Value::Null)
        }
        "panel.profiles" => {
            // toggles between the profile browser and the Edit panel it belongs to
            app.ui.right = if app.ui.right == RightPanel::Profiles && crate::docking::exposed(app, crate::docking::Panel::Tools) {
                RightPanel::Edit
            } else {
                RightPanel::Profiles
            };
            if !matches!(app.ui.view, ViewMode::Detail) {
                app.ui.view = ViewMode::Detail;
            }
            Ok(json!({"open": app.ui.right == RightPanel::Profiles}))
        }
        "panel.crop" => {
            panel(app, ctx, RightPanel::Crop, "Crop, Rotate, Geometry");
            Ok(Value::Null)
        }
        "panel.remove" => {
            panel(app, ctx, RightPanel::Remove, "Remove");
            if app.ui.right == RightPanel::Remove && app.ui.tool.is_empty() {
                app.ui.tool = "remove".into();
            }
            Ok(Value::Null)
        }
        "panel.masking" => {
            panel(app, ctx, RightPanel::Masking, "Masking");
            Ok(Value::Null)
        }
        "panel.redeye" => {
            panel(app, ctx, RightPanel::RedEye, "Red Eye");
            Ok(Value::Null)
        }
        "panel.info" => {
            panel(app, ctx, RightPanel::Info, "Info");
            Ok(Value::Null)
        }
        "panel.keywords" => {
            panel(app, ctx, RightPanel::Keywords, "Keywords");
            Ok(Value::Null)
        }
        "panel.addKeywords" => {
            // opens Keywording when it is folded shut; the panel stays open when pressed again
            if app.ui.right != RightPanel::Keywords {
                panel(app, ctx, RightPanel::Keywords, "Keywords");
            }
            app.ui.folded_sections.retain(|s| s != "keywording");
            app.ui.focus_keywords = Some(ctx.cumulative_pass_nr());
            Ok(Value::Null)
        }
        "panel.versions" => {
            panel(app, ctx, RightPanel::Versions, "Versions");
            Ok(Value::Null)
        }
        "panel.editKeywords" => {
            // the Keywords panel with its Keyword List open; the keyword picked there opens in
            // Edit Keyword Tag (with none picked, no dialog opens)
            if app.ui.right != RightPanel::Keywords {
                panel(app, ctx, RightPanel::Keywords, "Keywords");
            }
            app.ui.folded_sections.retain(|s| s != "keywordList");
            if let Some(k) = app.ui.keyword_list_selected.clone().filter(|k| app.session.catalog.has_keyword(k)) {
                app.ui.dialog = Some(crate::panels::keyword_list::edit_dialog(app, &k));
            }
            Ok(Value::Null)
        }
        "panel.activity" => {
            panel(app, ctx, RightPanel::Activity, "History");
            Ok(Value::Null)
        }
        "panel.presets" => {
            app.ui.presets = !app.ui.presets || !crate::docking::exposed(app, crate::docking::Panel::Presets);
            if app.ui.presets && app.ui.view != ViewMode::Detail {
                app.ui.view = ViewMode::Detail;
            }
            Ok(Value::Null)
        }
        "tool.done" => {
            // Return commits a tool panel (crop, remove, red eye, masking): back to Edit
            use RightPanel::*;
            if app.ui.dialog.is_none() && matches!(app.ui.right, Crop | Remove | RedEye | Masking) {
                if app.ui.right == Crop {
                    app.apply_crop_preview();
                }
                let _ = app.session.end_interaction();
                app.ui.tool.clear();
                app.ui.right = Edit;
            }
            Ok(Value::Null)
        }
        "panel.close" => {
            app.ui.right = RightPanel::None;
            app.ui.presets = false;
            Ok(Value::Null)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 8 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_8(app: &mut LightcraftApp, _ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        s if s.starts_with("section.") => bool_param(s, p, "solo").and_then(|solo| {
            // only the Edit sections listed above: an unknown one would close the real ones
            if !UI_COMMANDS.iter().any(|c| c.0 == s) {
                return Err(format!("{s}: no such Edit section"));
            }
            let sec = &s["section.".len()..];
            if app.ui.right != RightPanel::Edit {
                app.ui.right = RightPanel::Edit;
            }
            if solo == Some(true) {
                app.ui.solo_section(sec);
            } else {
                app.ui.toggle_section(sec);
            }
            Ok(Value::Null)
        }),
        s if s.starts_with("tool.") => {
            let tool = &s["tool.".len()..];
            match tool {
                "none" => app.ui.tool.clear(),
                "guidedUpright" => {
                    // Crop & Geometry with Guided Upright on, ready to draw guides
                    app.ui.right = RightPanel::Crop;
                    app.ui.view = ViewMode::Detail;
                    let guided = app
                        .session
                        .active()
                        .and_then(|id| app.session.develop_of(id))
                        .is_some_and(|d| d.geometry.upright == lightcraft_develop::Upright::Guided);
                    if !guided && let Err(e) = app.run("geometry.upright", json!({"mode": "guided"})) {
                        return Some(Err(e));
                    }
                    app.ui.tool = "guidedUpright".into();
                }
                "brush" => {
                    // A new mask and Add/Subtract are selection operations, not the brush's
                    // Erase mode. Paint positively into a component with the requested op.
                    let create = if p.get("new").and_then(Value::as_bool) == Some(true) {
                        Some(app.run("mask.add", json!({"kind": "brush"})))
                    } else {
                        p.get("op").map(|op| app.run("mask.addComponent", json!({"kind": "brush", "op": op})))
                    };
                    if let Some(result) = create {
                        if let Err(e) = result {
                            return Some(Err(e));
                        }
                        app.ui.brush_erase = false;
                        app.ui.mask_overlay = true;
                    }
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "brush".into();
                }
                "linear" | "radial" => {
                    app.ui.right = RightPanel::Masking;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = tool.into();
                    return Some(app.session.execute("mask.add", &json!({"kind": tool})).map_err(|e| e.to_string()));
                }
                "wbPicker" => {
                    app.ui.right = RightPanel::Edit;
                    app.ui.view = ViewMode::Detail;
                    app.ui.tool = "wbPicker".into();
                }
                other => return Some(Err(format!("unknown tool `{other}`"))),
            }
            Ok(Value::Null)
        }
        "dialog.newFolder" => {
            app.ui.dialog = Some(Dialog::NewAlbum {
                name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
                folder: true,
                parent: parent_param(p),
                include_selected: false,
            });
            Ok(Value::Null)
        }
        "dialog.newAlbum" => {
            // {name?, parent?, includeSelected? (true)}
            app.ui.dialog = Some(Dialog::NewAlbum {
                name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
                folder: false,
                parent: parent_param(p),
                include_selected: p.get("includeSelected").and_then(Value::as_bool).unwrap_or(true),
            });
            Ok(Value::Null)
        }
        "dialog.autoStack" => {
            app.ui.dialog = Some(Dialog::AutoStack { gap: p.get("gap").and_then(Value::as_f64).unwrap_or(60.0) as f32 });
            Ok(Value::Null)
        }
        "dialog.allMetadata" => {
            let r = match app.session.execute("photo.allMetadata", p) {
                Ok(r) => r,
                Err(e) => return Some(Err(e.to_string())),
            };
            let title = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.file_name.clone()).unwrap_or_default();
            app.ui.dialog = Some(Dialog::AllMetadata { title, rows: r, search: String::new() });
            Ok(Value::Null)
        }
        "dialog.smartAlbum" => dialog_smart_album_command(app, id, p),
        "dialog.newSmartAlbum" => {
            app.ui.dialog = Some(Dialog::NewSmartAlbum { name: p.get("name").and_then(Value::as_str).unwrap_or("").into(), parent: parent_param(p) });
            Ok(Value::Null)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 9 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_9(app: &mut LightcraftApp, _ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "dialog.captureTime" => {
            // a time that doesn't fully read isn't offered as the starting point (`10:0` would read as midnight)
            let time = app
                .session
                .active()
                .and_then(|id| app.session.catalog.photo(id))
                .map(|p| p.date())
                .filter(|d| lightcraft_catalog::dates::shift_iso(d, 0).is_some())
                .map(|d| d.replace('T', " "))
                .unwrap_or_default();
            let mode = p.get("mode").and_then(Value::as_str).unwrap_or("set").to_string();
            app.ui.dialog =
                Some(Dialog::CaptureTime { mode, time: time.get(..19).unwrap_or(&time).to_string(), days: 0, hours: 0, minutes: 0, zone: 0.0 });
            Ok(Value::Null)
        }
        "dialog.labelNames" => {
            let names =
                lightcraft_catalog::ColorLabel::ALL.iter().map(|l| app.session.catalog.custom_label_name(*l).unwrap_or("").to_string()).collect();
            app.ui.dialog = Some(Dialog::LabelNames { names, save_as: String::new() });
            Ok(Value::Null)
        }
        "dialog.rename" => {
            let template = p.get("template").and_then(Value::as_str).unwrap_or("{name}").to_string();
            app.ui.dialog = Some(Dialog::Rename { template, start: p.get("start").and_then(Value::as_u64).unwrap_or(1) as u32 });
            Ok(Value::Null)
        }
        "dialog.createPreset" => {
            app.ui.dialog = Some(Dialog::create_preset());
            Ok(Value::Null)
        }
        "dialog.pasteSettings" => {
            let groups =
                app.session.copy_groups.iter().filter_map(|g| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string))).collect();
            app.ui.dialog = Some(Dialog::PasteSettings { groups });
            Ok(Value::Null)
        }
        "view.focusSearch" => {
            // the search field lives in the top bar of the library and detail views alike
            app.ui.focus_search = true;
            Ok(Value::Null)
        }
        "dialog.copySettings" => {
            let groups =
                app.session.copy_groups.iter().filter_map(|g| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string))).collect();
            app.ui.dialog = Some(Dialog::CopySettings { groups });
            Ok(Value::Null)
        }
        "dialog.slideshowExport" => {
            let format = match p.get("format").and_then(Value::as_str) {
                Some("jpeg") => lightcraft_engine::slideshow_export::Format::Jpeg,
                Some("video") => lightcraft_engine::slideshow_export::Format::Video,
                _ => lightcraft_engine::slideshow_export::Format::Pdf,
            };
            app.ui.dialog = Some(Dialog::SlideshowExport { settings: lightcraft_engine::slideshow_export::ExportSettings::for_format(format) });
            Ok(Value::Null)
        }
        "app.slideshowExport" => crate::slideshow_export::start(app, id, p),
        "dialog.contactSheet" => {
            app.ui.dialog = Some(Dialog::ContactSheet { options: lightcraft_engine::contact_sheet::Options::default() });
            Ok(Value::Null)
        }
        "dialog.print" => {
            app.ui.print = Default::default();
            app.ui.dialog = Some(Dialog::Print);
            Ok(Value::Null)
        }
        "app.print" => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("path").is_none() {
                let req = PickRequest::save("Print to PDF", "PDF", &["pdf"], "Print.pdf");
                let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no save dialog on this platform; supply path".into())),
                };
                let Some(path) = paths.first() else { return Some(Ok(Value::Null)) };
                params["path"] = json!(path);
            }
            crate::export_task::start_print(app, &params)
        }
        "app.contactSheet" => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("path").is_none() {
                let req = PickRequest::save("Contact Sheet PDF", "PDF", &["pdf"], "Contact Sheet.pdf");
                let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no save dialog on this platform; supply path".into())),
                };
                let Some(path) = paths.first() else { return Some(Ok(Value::Null)) };
                params["path"] = json!(path);
            }
            crate::export_task::start_contact_sheet(app, &params)
        }
        _ => return None,
    };
    Some(r)
}

/// Part 10 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_10(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "dialog.export" => {
            let prev = app.session.last_export.clone().unwrap_or_default();
            let u = |k: &str, d: u64| prev.get(k).and_then(Value::as_u64).unwrap_or(d);
            let dir = prev.get("dir").and_then(Value::as_str).map(str::to_string).unwrap_or_else(crate::control::default_export_dir);
            // stored settings: a depth the format lacks (saved before #1952) opens at the format's own
            let opts = lightcraft_engine::export::ExportOptions::from_json(&lightcraft_engine::export::ExportOptions::known_keys_only(&prev));
            // no previous export: 2048 px long edge; a previous full-size export: full size
            let full_size = opts.resize.is_none() && lightcraft_engine::export::ExportOptions::has_size_param(&prev);
            let resize = opts.resize.unwrap_or_default();
            app.ui.dialog = Some(Dialog::Export { opts, full_size, resize, preset_name: String::new(), limit_kb: u("limitKb", 0) as u32, dir });
            Ok(Value::Null)
        }
        "merge.hdrLast" => crate::merge::start_last(app, "merge.hdr"),
        "merge.hdrBatch" => crate::merge::start_batch(app, p),
        "merge.panoramaLast" => crate::merge::start_last(app, "merge.panorama"),
        "merge.hdrPanoramaLast" => crate::merge::start_last(app, "merge.hdrPanorama"),
        "dialog.mergeHdr" => crate::merge::open(app, "merge.hdr"),
        "dialog.faceModel" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => app.services.pick_model_file.as_mut().and_then(|f| f().into_iter().next()),
            };
            match path {
                Some(path) => crate::panels::faces::open_dialog(app, &path),
                None => Ok(Value::Null),
            }
        }
        "dialog.mergePanorama" => crate::merge::open(app, "merge.panorama"),
        "dialog.mergeHdrPanorama" => crate::merge::open(app, "merge.hdrPanorama"),
        "app.openSamModelFolder" => crate::panels::dialogs::show_model_folder(app),
        "app.about" => {
            app.ui.dialog = Some(Dialog::About);
            Ok(Value::Null)
        }
        "app.checkForUpdates" => crate::updates_ui::check(app),
        "photo.editInPhotoCraft" => edit_in(app, ctx, p, true),
        "photo.editInExternal" => edit_in(app, ctx, p, false),
        "photo.openOriginalInExternal" => open_original_in(app, ctx, p),
        "dialog.cull" => {
            let (burst_gap, similarity) = app.ui.cull_burst.unwrap_or((10.0, 0.93));
            app.ui.dialog = Some(Dialog::Cull { reject_below: 0.0, pick_best: true, burst_gap, similarity });
            Ok(Value::Null)
        }
        "app.whatsNew" => {
            app.ui.dialog = Some(Dialog::WhatsNew);
            Ok(json!({"text": crate::panels::dialogs::WHATS_NEW}))
        }
        "app.systemInfo" => app_system_info_command(app, p),
        "app.shortcuts" => {
            app.ui.dialog = Some(Dialog::Shortcuts);
            Ok(Value::Null)
        }
        "app.setShortcut" => crate::shortcuts::set_shortcut(app, p),
        "app.resetShortcuts" => {
            app.ui.settings.keymap.clear();
            Ok(Value::Null)
        }
        "library.browse" if !cfg!(target_arch = "wasm32") => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before browsing folders".into()));
            }
            // listed and read in the background (see `import::browse`)
            let path = p.get("path").and_then(Value::as_str)?;
            crate::import::browse(app, path, p.get("subfolders").and_then(Value::as_bool))
        }
        "file.addPhotos" => {
            if crate::lightroom_import::is_running(app) {
                return Some(Err("finish Lightroom catalog import before adding photos".into()));
            }
            let paths = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                None => {
                    let req = PickRequest::files(None, crate::i18n::tr("Photos"), lightcraft_engine::import::EXTENSIONS);
                    match crate::pick::ask(app, id, p, "paths", req, |s| s.pick_files.as_mut().map(|f| f())) {
                        Picked::Now(paths) => paths,
                        Picked::Later | Picked::Unavailable => return Some(Ok(Value::Null)),
                    }
                }
            };
            if paths.is_empty() {
                return Some(Ok(Value::Null));
            }
            // review first: the import dialog lists what was found
            let r = crate::import::open(app, paths);
            import_into_album(app, p, r)
        }
        "file.importLightroom" => {
            // where the catalog goes (#2672): checked before a file dialog opens
            let req = match crate::lightroom_destination::Request::parse(p) {
                Ok(req) => req,
                Err(e) => return Some(Err(e)),
            };
            let path = match p.get("path").and_then(Value::as_str).map(str::to_string) {
                Some(path) => Some(path),
                None => {
                    let req =
                        PickRequest::file(crate::i18n::tr("Import Lightroom Catalog"), crate::i18n::tr("Lightroom Classic Catalog"), &["lrcat"]);
                    match crate::pick::ask(app, id, p, "path", req, |s| s.pick_lightroom_catalog.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later | Picked::Unavailable => None,
                    }
                }
            };
            match path {
                Some(path) => crate::lightroom_destination::import(app, &path, &req),
                None => Ok(Value::Null),
            }
        }
        _ => return None,
    };
    Some(r)
}

/// Part 11 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_11(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "app.quit" => {
            app.ui.quit = true;
            Ok(Value::Null)
        }
        "dialog.saveMetadataPreset" => {
            // from the active photo's copyright, creator and place
            crate::panels::dialogs::prompt(app, "Save Metadata Preset", "Preset name", "", "metadata.savePreset", json!({}), "name");
            Ok(Value::Null)
        }
        "dialog.keywordShortcut" => {
            let now = app.session.catalog.keyword_shortcut().join(", ");
            crate::panels::dialogs::prompt(app, "Set Keyword Shortcut", "Separated by commas", &now, "keyword.setShortcut", json!({}), "keywords");
            Ok(Value::Null)
        }
        "file.findMissing" => file_find_missing_command(app, id, p),
        "photo.tagFromTracklog" => photo_tag_from_tracklog_command(app, ctx, id, p),
        "photo.locate" => photo_locate_command(app, p),
        "file.addFromDevice" => {
            // a camera / card: review its DCIM folder, copying into the library by default
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => match lightcraft_engine::devices::devices_now().into_iter().next() {
                    Some(d) => d.path,
                    None => return Some(Err("no camera or memory card found".into())),
                },
            };
            let r = crate::import::open(app, vec![path]);
            // only the scan just started (an error means another scan is running)
            if r.is_ok()
                && let Some(t) = &mut app.scan
            {
                t.copy = true;
            }
            import_into_album(app, p, r)
        }
        "file.addFolder" => {
            // a folder (searched recursively) into the import review
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => match crate::pick::ask(app, id, p, "path", PickRequest::folder(crate::i18n::tr("Import from Folder…")), |s| {
                    s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])
                }) {
                    Picked::Now(v) => v.into_iter().next(),
                    Picked::Later | Picked::Unavailable => None,
                },
            };
            match path {
                Some(path) => {
                    let r = crate::import::open(app, vec![path]);
                    import_into_album(app, p, r)
                }
                None => Ok(Value::Null),
            }
        }
        "file.importPresets" => file_import_presets_command(app, ctx, id, p),
        "file.importKeywords" => file_import_keywords_command(app, ctx, id, p),
        "file.exportKeywords" => file_export_keywords_command(app, ctx, id, p),
        "dialog.saveMaskEffect" => {
            if app.session.active_mask.is_none() {
                return Some(Err("select a mask first".into()));
            }
            crate::panels::mask_effects::open_save(app);
            Ok(Value::Null)
        }
        "dialog.manageMaskEffects" => {
            crate::panels::mask_effects::open_manage(app);
            Ok(Value::Null)
        }
        "file.exportMaskEffects" => file_export_mask_effects_command(app, ctx, id, p),
        "file.exportPresets" => {
            let group = p.get("group").and_then(Value::as_str).map(str::to_string);
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => Some(x.to_string()),
                None => {
                    let name = format!("{}.lcpreset", group.as_deref().unwrap_or("LightCraft Presets"));
                    let req = PickRequest::save(crate::i18n::tr("Export Presets"), crate::i18n::tr("LightCraft Preset"), &["lcpreset"], name.clone());
                    match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                        Picked::Now(v) => v.into_iter().next(),
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform".into())),
                    }
                }
            };
            let Some(path) = path else { return Some(Ok(Value::Null)) };
            let mut params = json!({"path": path});
            if let Some(g) = group {
                params["group"] = json!(g);
            }
            if let Some(ids) = p.get("ids") {
                params["ids"] = ids.clone();
            }
            let r = app.session.execute("preset.export", &params).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                app.toast(ctx, crate::i18n::tr_format!("Exported {} preset{}", v["count"], if v["count"] == 1 { "" } else { "s" }));
            }
            r
        }
        "file.importCurvePresets" => file_import_curve_presets_command(app, ctx, id, p),
        _ => return None,
    };
    Some(r)
}

/// Part 12 of 12 of the UI commands; `None` means the id is not one of this part's.
fn ui_commands_12(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r: Result<Value, String> = match id {
        "file.exportCurvePresets" => file_export_curve_presets_command(app, ctx, id, p),
        "app.export" => crate::control::export_active(app, p),
        "app.showInFinder" => show_in_finder(app),
        "app.showInFolders" => show_in_folders(app, p.get("id").and_then(Value::as_u64).map(lightcraft_catalog::PhotoId)),
        "app.openLogFolder" => open_log_folder(app),
        "app.discord" | "app.website" | "app.github" | "app.artcraft" | "app.help" | "app.feedback" => {
            let url = crate::links::url_of(id).unwrap_or(crate::links::WEBSITE);
            crate::links::open(app, url)
        }
        "app.exportPrevious" => match app.session.last_export.clone() {
            Some(prev) => crate::control::export_active(app, &lightcraft_engine::export::ExportOptions::known_keys_only(&prev)),
            None => Err("nothing exported yet — use Export…".into()),
        },
        _ => return None,
    };
    Some(r)
}

/// The `view.back` command.
fn view_back_command(app: &mut LightcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    if app.ui.dialog.is_some() {
        app.ui.dialog = None;
    } else if app.ui.lights_out != crate::state::LightsOut::On {
        app.ui.lights_out = crate::state::LightsOut::On;
    } else if app.ui.keyword_painter.is_some() {
        app.ui.keyword_painter = None;
    } else if app.ui.fullscreen {
        // Through `slideshow::end`: it also releases the held slide textures. Clearing the
        // two fields directly left every rendered slide in the renderer.
        crate::panels::slideshow::end(app);
    } else if !app.ui.tool.is_empty() {
        app.ui.tool.clear();
    } else if app.ui.right == RightPanel::Crop && app.ui.view == ViewMode::Detail {
        // Esc leaves the crop tool (the crop stays), as C does; the next Esc goes back
        panel(app, ctx, RightPanel::Crop, "Crop, Rotate, Geometry");
    } else if app.ui.view == ViewMode::People && app.ui.person_page.is_some() {
        app.ui.person_page = None;
    } else if matches!(app.ui.view, ViewMode::Compare | ViewMode::Survey | ViewMode::Reference) {
        app.ui.view = ViewMode::Detail;
    } else if app.ui.view == ViewMode::Detail {
        // a photo opened from a person's page (or the unnamed faces) goes back there, any other to the grid
        match app.ui.person_from.take() {
            Some((name, photo)) if app.session.active().is_some_and(|a| a.0 == photo) => {
                app.ui.view = ViewMode::People;
                app.ui.person_page = name;
            }
            _ => app.ui.view = ViewMode::PhotoGrid,
        }
    }
    Ok(Value::Null)
}

/// The `view.viewOptions` command.
fn view_view_options_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    // `info1` / `info2` set the lines, `briefly` the Info that Show Briefly When Photo Changes
    // shows and `loupeMessages` Show message when loading or rendering photos, for an agent;
    // without any of them the window opens
    let mut info = app.ui.settings.loupe_info;
    let mut set = false;
    // `loupeMessages` sets Show message when loading or rendering photos
    let messages = match p.get("loupeMessages") {
        None => app.ui.settings.loupe_messages,
        Some(v) => match v.as_bool() {
            Some(on) => {
                set = true;
                on
            }
            None => return Err("`loupeMessages`: true|false".into()),
        },
    };
    for (key, lines) in [("info1", &mut info.info1), ("info2", &mut info.info2)] {
        let Some(v) = p.get(key) else { continue };
        match serde_json::from_value(v.clone()) {
            Ok(v) => *lines = v,
            Err(_) => {
                return Err(format!("`{key}`: three of none|fileName|captureDate|dimensions|exposure|camera|lens|cameraAndLens"));
            }
        }
        set = true;
    }
    if let Some(v) = p.get("briefly") {
        match serde_json::from_value(v.clone()) {
            Ok(v) => info.briefly = v,
            Err(_) => return Err("`briefly`: off|basic|exposure (neither, Info 1 or Info 2)".into()),
        }
        set = true;
    }
    if set {
        app.ui.settings.loupe_info = info;
        app.ui.settings.loupe_messages = messages;
    } else {
        app.ui.dialog = Some(Dialog::ViewOptions);
    }
    Ok(json!({"loupeInfo": app.ui.settings.loupe_info, "loupeMessages": app.ui.settings.loupe_messages}))
}

/// The `view.softProof` command.
fn view_soft_proof_command(app: &mut LightcraftApp, _id: &str, p: &Value) -> Result<Value, String> {
    // {on?, space?, profile?: ICC path | null, intent?: perceptual|relative, paperInk?,
    // destWarning?, displayWarning?}; no params toggles
    let has = |k: &str| p.get(k).is_some();
    if let Some(s) = p.get("space").and_then(Value::as_str) {
        match lightcraft_engine::pipeline::OutputSpace::parse(s) {
            Some(sp) => {
                app.ui.proof.space = sp;
                // a standard space replaces a device profile (unless one is given too)
                app.ui.proof_profile = None;
            }
            None => return Err(format!("view.softProof: unknown space {s:?} (srgb|displayP3|adobeRgb|proPhoto|rec2020)")),
        }
    }
    if let Some(i) = p.get("intent") {
        match i.as_str().and_then(lightcraft_engine::proof::ProofIntent::parse) {
            Some(i) => app.ui.proof_intent = i,
            None => return Err("view.softProof: intent must be perceptual or relative".into()),
        }
    }
    if let Some(b) = p.get("paperInk").and_then(Value::as_bool) {
        app.ui.proof_paper_ink = b;
    }
    let path = match p.get("profile") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) => {
            app.ui.proof_profile = None;
            None
        }
        Some(_) => return Err("view.softProof: profile must be an ICC file path or null".into()),
        None => app.ui.proof_profile.as_ref().map(|(f, _)| f.clone()),
    };
    match path {
        Some(path) => match lightcraft_engine::proof::load(&path, app.ui.proof_intent, app.ui.proof_paper_ink) {
            Ok(pp) => {
                app.ui.proof.profile = pp.id;
                app.ui.proof_profile = Some((path, pp.name));
            }
            Err(e) => return Err(format!("view.softProof: {e}")),
        },
        None => app.ui.proof.profile = 0,
    }
    if let Some(b) = p.get("destWarning").and_then(Value::as_bool) {
        app.ui.proof.dest_warning = b;
    }
    if let Some(b) = p.get("displayWarning").and_then(Value::as_bool) {
        app.ui.proof.display_warning = b;
    }
    app.ui.soft_proof = match p.get("on").and_then(Value::as_bool) {
        Some(b) => b,
        None if ["space", "destWarning", "displayWarning", "profile", "intent", "paperInk"].iter().any(|k| has(k)) => app.ui.soft_proof,
        None => !app.ui.soft_proof,
    };
    if app.ui.soft_proof && !matches!(app.ui.view, ViewMode::Detail | ViewMode::Reference) {
        app.ui.view = ViewMode::Detail;
    }
    let pr = app.ui.proof;
    let profile = app.ui.proof_profile.as_ref().map(|(f, n)| json!({"path": f, "name": n}));
    Ok(json!({"on": app.ui.soft_proof, "space": pr.space, "profile": profile, "intent": app.ui.proof_intent.name(),
        "paperInk": app.ui.proof_paper_ink, "destWarning": pr.dest_warning, "displayWarning": pr.display_warning}))
}

/// The `dialog.smartAlbum` command.
fn dialog_smart_album_command(app: &mut LightcraftApp, _id: &str, p: &Value) -> Result<Value, String> {
    // {id?}: edit that smart album's rules; without: a new one starting at Rating ≥ 3
    let album = p
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(id)).filter(|a| a.is_smart()).cloned());
    app.ui.dialog = Some(match album {
        Some(a) => Dialog::SmartRules {
            id: Some(a.id.0),
            name: a.name.clone(),
            // older rules read as they always matched (RuleSet::upgrade)
            rules: a
                .smart
                .as_ref()
                .and_then(|f| f.rule_set.clone())
                .map(|mut r| {
                    r.upgrade();
                    r
                })
                .unwrap_or_default(),
            parent: None,
        },
        None => Dialog::SmartRules {
            id: None,
            name: p.get("name").and_then(Value::as_str).unwrap_or("").into(),
            rules: lightcraft_catalog::RuleSet { rules: vec![crate::panels::rules_editor::new_rule()], ..Default::default() },
            parent: parent_param(p),
        },
    });
    Ok(Value::Null)
}

/// The `app.systemInfo` command.
fn app_system_info_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let info = app.session.execute("library.info", &json!({})).unwrap_or_default();
    let gpu = (lightcraft_engine::gpu::ready() && lightcraft_engine::gpu::available()).then(lightcraft_engine::gpu::adapter_name).flatten();
    let mb = |b: u64| format!("{:.0} MB", b as f64 / (1u64 << 20) as f64);
    let mut rows = vec![
        ("Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ("System".to_string(), format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
        ("CPU threads".to_string(), std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or_else(|_| "?".into())),
        (
            "GPU".to_string(),
            gpu.unwrap_or_else(|| match lightcraft_engine::gpu::unavailable_reason() {
                Some(why) => crate::i18n::tr_format!("none (CPU rendering): {why}", why = why),
                None => "none (CPU rendering)".into(),
            }),
        ),
        ("GPU rendering".to_string(), if app.ui.settings.gpu { "on".into() } else { "off".into() }),
        ("Memory budget".to_string(), mb(lightcraft_engine::memory::default_budget() as u64)),
        (
            "Preview size".to_string(),
            match app.ui.settings.preview_limit {
                0 => "automatic".to_string(),
                n => format!("{n} px"),
            },
        ),
        ("Photos".to_string(), info["photos"].to_string()),
        ("Albums".to_string(), info["albums"].to_string()),
    ];
    if let Some(dir) = info["dir"].as_str().or(info["path"].as_str()) {
        rows.push(("Library".into(), dir.to_string()));
    }
    rows.push(("Frame time".into(), format!("{:.1} ms ({:.0} fps)", app.perf.frame_ms, app.perf.fps)));
    rows.push((
        "Frame update".into(),
        format!("{:.1} ms (logic {:.1} ms; slowest {:.0} ms)", app.perf.update_ms, app.perf.logic_ms, app.perf.max_update_ms),
    ));
    rows.push(("Last loupe render".into(), format!("{:.0} ms", app.renderer.last_main_ms)));
    if let Some(f) = lightcraft_engine::gpu::last_fallback() {
        rows.push(("Last GPU fallback".into(), f));
    }
    let r = json!(rows.iter().map(|(k, v)| json!({"label": k, "value": v})).collect::<Vec<_>>());
    if p.get("open").and_then(Value::as_bool).unwrap_or(true) {
        app.ui.dialog = Some(Dialog::SystemInfo { rows });
    }
    Ok(r)
}

/// `file.findMissing {within?}`: the folder the search is limited to, if any.
fn find_missing_within(p: &Value) -> Result<Option<String>, String> {
    match p.get("within") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(path)) if !path.is_empty() => Ok(Some(path.clone())),
        Some(Value::String(_)) => Err("file.findMissing: `within` must not be empty".into()),
        Some(value) => Err(format!("file.findMissing: `within` must be a folder path, not {value}")),
    }
}

/// `file.findMissing {timeout?}`: how long `wait` waits for the search (ten minutes when omitted).
fn find_missing_timeout(p: &Value) -> Result<std::time::Duration, String> {
    let n = match p.get("timeout") {
        None | Some(Value::Null) => return Ok(std::time::Duration::from_secs(600)),
        Some(Value::Number(n)) => n,
        Some(other) => return Err(format!("file.findMissing: `timeout` must be a number of seconds, not {other}")),
    };
    let s = n.as_f64().ok_or("file.findMissing: `timeout` must be a number of seconds")?;
    if !s.is_finite() || s < 0.0 {
        return Err("file.findMissing: `timeout` must be a non-negative number of seconds".into());
    }
    std::time::Duration::try_from_secs_f64(s).map_err(|_| "file.findMissing: `timeout` is too large".to_string())
}

/// The toast for a finished search, and the result it leaves for a caller that waits.
fn find_missing_done(app: &mut LightcraftApp, ctx: &egui::Context, plan: Result<lightcraft_engine::cmd::missing::FindPlan, String>) {
    // relinked under the session as it is now: photos relinked meanwhile and files
    // now in use are skipped
    let r = plan.and_then(|plan| app.run("library.findMissing", plan.to_json()));
    match r {
        Ok(v) => {
            let n = v["found"].as_array().map_or(0, Vec::len);
            let left = v["missing"].as_u64().unwrap_or(0);
            let unsure = v["ambiguous"].as_array().map_or(0, Vec::len);
            let unsure = if unsure > 0 {
                crate::i18n::tr_format!(" ({unsure} with several look-alike files: use Locate)", unsure = unsure)
            } else {
                String::new()
            };
            app.toast(
                ctx,
                crate::i18n::tr_format!(
                    "Found {n} missing photo{}; {left} still missing{unsure}",
                    if n == 1 { "" } else { "s" },
                    left = left,
                    n = n,
                    unsure = unsure
                ),
            );
            app.ui.last_find_missing = Some(v.clone());
            app.ui.last_find_missing_result = Some(Ok(v));
        }
        Err(e) => {
            app.toast(ctx, e.clone());
            app.ui.last_find_missing_result = Some(Err(e));
        }
    }
}

/// The `photo.locate` command.
fn photo_locate_command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let Some(id) = app.session.active() else { return Err("no photo selected".into()) };
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => Some(x.to_string()),
        None => {
            let req = PickRequest::file(crate::i18n::tr("Locate Missing File…"), crate::i18n::tr("Photos"), lightcraft_engine::import::EXTENSIONS);
            match crate::pick::ask(app, "photo.locate", p, "path", req, |s| s.pick_files.as_mut().map(|f| f())) {
                Picked::Now(v) => v.into_iter().next(),
                Picked::Later | Picked::Unavailable => None,
            }
        }
    };
    match path {
        Some(path) => app.run("photo.relink", json!({"id": id.0, "path": path})),
        None => Ok(Value::Null),
    }
}

/// The `file.findMissing` command.
fn file_find_missing_command(app: &mut LightcraftApp, id: &str, p: &Value) -> Result<Value, String> {
    let within = find_missing_within(p)?;
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let timeout = find_missing_timeout(p)?;
    let title = if within.is_some() { "Locate Missing Files…" } else { "Find Missing Photos…" };
    let folder = match p.get("folder").and_then(Value::as_str) {
        Some(f) => Some(f.to_string()),
        None => match crate::pick::ask(app, id, p, "folder", PickRequest::folder(crate::i18n::tr(title)), |s| {
            s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])
        }) {
            Picked::Now(v) => v.into_iter().next(),
            Picked::Later | Picked::Unavailable => None,
        },
    };
    let Some(folder) = folder else { return Ok(Value::Null) };
    // the search (checking every photo's file, walking the folder) runs on a worker thread;
    // the relinking happens back here, as one undo step
    const LABEL: &str = "Find Missing Photos";
    if app.tasks.is_running(LABEL) {
        return Err("Find Missing Photos is already searching".into());
    }
    let mut candidates = lightcraft_engine::cmd::missing::find_candidates(&app.session.catalog);
    if let Some(within) = within {
        candidates.retain(|candidate| lightcraft_catalog::query::folder_within(&candidate.path, &within));
    }
    #[cfg(test)]
    let test_delay = app.ui.find_missing_test_delay;
    let work = move || {
        #[cfg(test)]
        if let Some(d) = test_delay {
            std::thread::sleep(d);
        }
        lightcraft_engine::cmd::missing::plan_find_missing(&candidates, &folder)
    };
    app.ui.last_find_missing = None;
    app.ui.last_find_missing_result = None;
    crate::tasks::spawn(app, LABEL, Some("findMissing"), work, find_missing_done)?;
    if !wait {
        return Ok(json!({"background": true}));
    }
    let ctx = app.tasks.repaint.clone().unwrap_or_default();
    if !crate::tasks::wait_for(app, &ctx, LABEL, timeout) {
        return Err("Find Missing Photos: search timed out (search continues in the background)".into());
    }
    app.ui.last_find_missing_result.take().unwrap_or_else(|| Err("Find Missing Photos: worker terminated unexpectedly".into()))
}

/// The `photo.tagFromTracklog` command.
fn photo_tag_from_tracklog_command(app: &mut LightcraftApp, _ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    // a GPX file → GPS for the selected photos by capture time (one undo step)
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => Some(x.to_string()),
        None => {
            let req = PickRequest::file(crate::i18n::tr("Auto-Tag from Tracklog"), crate::i18n::tr("GPS Track Log"), &["gpx"]);
            match crate::pick::ask(app, id, p, "path", req, |s| s.pick_tracklog.as_mut().map(|f| f())) {
                Picked::Now(v) => v.into_iter().next(),
                Picked::Later | Picked::Unavailable => None,
            }
        }
    };
    let Some(path) = path else { return Ok(Value::Null) };
    let mut params = p.as_object().cloned().unwrap_or_default();
    params.insert("path".into(), json!(path));
    let ask_zone = !params.contains_key("offset");
    let params = Value::Object(params);
    if ask_zone {
        // GPX times are UTC, camera clocks are local: ask for the camera's zone, then come back here
        crate::panels::dialogs::prompt(
            app,
            "Auto-Tag from Tracklog",
            "Camera time zone, e.g. -07:00 (empty: UTC)",
            "",
            "photo.tagFromTracklog",
            params,
            "offset",
        );
        return Ok(Value::Null);
    }
    let r = app.run("photo.autoTagTracklog", params);
    if let Ok(v) = &r {
        let n = v["tagged"].as_u64().unwrap_or(0);
        let sk = &v["skipped"];
        let mut msg = crate::i18n::tr_format!("Tagged {n} photo{} from the tracklog", if n == 1 { "" } else { "s" }, n = n);
        let outside = sk["outside"].as_u64().unwrap_or(0);
        if outside > 0 {
            msg += &crate::i18n::tr_format!("; {outside} outside its time range", outside = outside);
        }
        let kept = sk["hasGps"].as_u64().unwrap_or(0);
        if kept > 0 {
            msg += &crate::i18n::tr_format!("; {kept} already had a location", kept = kept);
        }
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        app.toast(&ctx, msg);
    }
    r
}

/// The files a command names in `paths`, else the ones picked in a dialog. `Err` is the command's own answer when
/// there is nothing to import (the dialog is still open or was cancelled, or none is available).
fn paths_or_pick(
    app: &mut LightcraftApp,
    id: &str,
    p: &Value,
    request: PickRequest,
    now: impl FnOnce(&mut crate::services::Services) -> Option<Vec<String>>,
) -> Result<Vec<String>, Result<Value, String>> {
    let paths: Vec<String> = match p.get("paths").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => match crate::pick::ask(app, id, p, "paths", request, now) {
            Picked::Now(paths) => paths,
            Picked::Later => return Err(Ok(Value::Null)),
            Picked::Unavailable => return Err(Err("no file dialog on this platform".into())),
        },
    };
    if paths.is_empty() {
        return Err(Ok(Value::Null));
    }
    Ok(paths)
}

/// What an import of presets and profiles has to say: one toast for both halves (a second toast would replace the first).
#[derive(Default)]
struct ImportNews {
    parts: Vec<String>,
    /// `name: reason` for each file that couldn't be imported
    problems: Vec<String>,
}

/// `name: reason` for each `[name, reason]` of a `failed` list.
fn failed_files(v: &Value) -> impl Iterator<Item = String> + '_ {
    v["failed"].as_array().into_iter().flatten().map(|f| {
        let name = f.get(0).and_then(Value::as_str).unwrap_or_default();
        let reason = f.get(1).and_then(Value::as_str).unwrap_or_default();
        format!("{name}: {reason}")
    })
}

/// Import the `.cube` LUTs (and those inside zips / folders) as profiles; the answer to report back.
fn import_luts(app: &mut LightcraftApp, lut_paths: &[&String], news: &mut ImportNews) -> Value {
    match app.session.execute("profile.import", &json!({"paths": lut_paths})) {
        Ok(v) => {
            let profiles = v["imported"].as_array().map_or(0, Vec::len);
            if profiles > 0 {
                news.parts.push(crate::i18n::tr_format!(
                    "Imported {profiles} profile{} (Profile browser ▸ their groups)",
                    crate::i18n::plural_s(profiles),
                    profiles = profiles
                ));
            }
            news.problems.extend(failed_files(&v));
            v
        }
        Err(e) => {
            news.problems.push(e.to_string());
            json!({"error": e.to_string()})
        }
    }
}

/// The settings of imported presets that have no counterpart here (the other editor's profiles, masks…), as a note.
fn not_carried_over(v: &Value) -> String {
    let mut skipped: Vec<&str> = v["imported"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|i| i["unmapped"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    skipped.sort_unstable();
    skipped.dedup();
    if skipped.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = skipped.iter().take(3).copied().collect();
    crate::i18n::tr_format!(" — not carried over: {}{}", names.join(", "), if skipped.len() > 3 { "…" } else { "" })
}

/// The sentence for the presets of an import (none when the other half already says what happened).
fn presets_sentence(v: &Value, profile_news: bool) -> Option<String> {
    let n = v["imported"].as_array().map_or(0, Vec::len);
    let failed = v["failed"].as_array().map_or(0, Vec::len);
    let msg = match (n, failed) {
        // a zip of LUTs (or only local presets): the other half already says what happened
        (0, 0) if profile_news => return None,
        (0, 0) => "No new presets".to_string(),
        (n, 0) => crate::i18n::tr_format!("Imported {n} preset{}", crate::i18n::plural_s(n), n = n),
        (n, f) => crate::i18n::tr_format!(
            "Imported {n} preset{}, {f} file{} not readable",
            crate::i18n::plural_s(n),
            crate::i18n::plural_s(f),
            f = f,
            n = n
        ),
    };
    Some(msg + &not_carried_over(v))
}

/// Import the preset files; adds to what `news` reports.
fn import_preset_files(app: &mut LightcraftApp, preset_paths: &[&String], news: &mut ImportNews) -> Result<Value, String> {
    if preset_paths.is_empty() {
        return Ok(json!({"imported": [], "failed": [], "skipped": 0}));
    }
    let r = app.session.execute("preset.import", &json!({"paths": preset_paths})).map_err(|e| e.to_string());
    if let Ok(v) = &r {
        // Lightroom local adjustment presets land in the Masking panel's Effect menu: part of the same toast
        if v["effects"].as_array().is_some_and(|e| !e.is_empty()) {
            news.parts.push(crate::panels::mask_effects::import_toast(v));
        }
        let profile_news = !news.parts.is_empty() || !news.problems.is_empty();
        news.parts.extend(presets_sentence(v, profile_news));
        if v["imported"].as_array().is_some_and(|i| !i.is_empty()) {
            app.ui.presets = true;
        }
    }
    r
}

/// Show what an import did: errors if any file failed, else the parts.
fn toast_import_news(app: &mut LightcraftApp, ctx: &egui::Context, news: &ImportNews) {
    let mut msg = news.parts.join(" · ");
    if news.problems.is_empty() {
        if !msg.is_empty() {
            app.toast(ctx, msg);
        }
        return;
    }
    let more = if news.problems.len() > 2 { "…" } else { "" };
    let detail = format!("{}{more}", news.problems.iter().take(2).cloned().collect::<Vec<_>>().join("; "));
    if msg.is_empty() {
        msg = crate::i18n::tr_format!("Import failed: {e}", e = detail);
    } else {
        msg += &crate::i18n::tr_format!(" · {} failed: {}", news.problems.len(), detail);
    }
    app.toast_error(ctx, msg);
}

/// The `file.importPresets` command.
fn file_import_presets_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    let request =
        PickRequest::files(Some(crate::i18n::tr("Import Presets").into()), crate::i18n::tr("Presets & Profiles"), crate::pick::PRESET_EXTENSIONS);
    let paths = match paths_or_pick(app, id, p, request, |s| s.pick_preset_files.as_mut().map(|f| f())) {
        Ok(paths) => paths,
        Err(answer) => return answer,
    };
    let lut_paths: Vec<&String> = paths
        .iter()
        .filter(|p| {
            let l = p.to_ascii_lowercase();
            l.ends_with(".cube") || l.ends_with(".zip") || std::path::Path::new(p.as_str()).is_dir()
        })
        .collect();
    let mut news = ImportNews::default();
    let profiles = if lut_paths.is_empty() { Value::Null } else { import_luts(app, &lut_paths, &mut news) };
    let preset_paths: Vec<&String> = paths.iter().filter(|p| !p.to_ascii_lowercase().ends_with(".cube")).collect();
    let r = import_preset_files(app, &preset_paths, &mut news);
    toast_import_news(app, ctx, &news);
    r.map(|mut v| {
        if !lut_paths.is_empty()
            && let Some(o) = v.as_object_mut()
        {
            o.insert("profiles".into(), profiles);
        }
        v
    })
}

/// The `file.importKeywords` command.
fn file_import_keywords_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    // a keyword list file: Lightroom Classic's, Capture One's, Photo Supreme's (.utf8)
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let req = PickRequest::file(crate::i18n::tr("Import Keywords"), crate::i18n::tr("Keyword Lists"), &["txt", "utf8"]);
            match crate::pick::ask(app, id, p, "path", req, |s| s.pick_keyword_list.as_mut().map(|f| f())) {
                Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                Picked::Later => return Ok(Value::Null),
                Picked::Unavailable => return Err("no file dialog on this platform".into()),
            }
        }
    };
    let r = app.session.execute("keyword.import", &json!({"path": path})).map_err(|e| e.to_string());
    match &r {
        Ok(v) => {
            let (added, updated) = (v["added"].as_u64().unwrap_or(0), v["updated"].as_u64().unwrap_or(0));
            let mut msg = crate::i18n::tr_format!("Added {n} keyword{}", if added == 1 { "" } else { "s" }, n = added);
            if updated > 0 {
                msg.push_str(&crate::i18n::tr_format!("; {n} gained synonyms", n = updated));
            }
            app.toast(ctx, msg);
        }
        Err(e) => app.toast(ctx, e.clone()),
    }
    r
}

/// The `file.exportKeywords` command.
fn file_export_keywords_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let name = "Keywords.txt";
            let req = PickRequest::save(crate::i18n::tr("Export Keywords"), crate::i18n::tr("Keyword Lists"), &["txt"], name);
            match crate::pick::ask(app, id, p, "path", req, |s| s.save_keyword_list.as_mut().map(|f| f(name).into_iter().collect())) {
                Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                Picked::Later => return Ok(Value::Null),
                Picked::Unavailable => return Err("no file dialog on this platform".into()),
            }
        }
    };
    let r = app.session.execute("keyword.export", &json!({"path": path})).map_err(|e| e.to_string());
    if let Err(e) = &r {
        app.toast(ctx, e.clone());
    }
    if let Ok(v) = &r {
        let n = v["keywords"].as_u64().unwrap_or(0);
        let mut msg = crate::i18n::tr_format!("Exported {n} keyword{}", if n == 1 { "" } else { "s" }, n = n);
        // Capture One's importer refuses ; , < > in a list: say which to rename first
        let refuses: Vec<&str> = v["captureOneRefuses"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if !refuses.is_empty() {
            msg.push_str(&crate::i18n::tr_format!(
                " — Capture One won't import a list with ; , < or > in a keyword: {names}",
                names = refuses.iter().take(5).copied().collect::<Vec<_>>().join(" · ")
            ));
        }
        // what the format can't hold (a name in brackets, a line break…) isn't in the file
        let left: Vec<&str> = v["unwritable"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if !left.is_empty() {
            msg.push_str(&crate::i18n::tr_format!(
                " — left out, as a keyword list can't hold them: {names}",
                // escaped: a line break in a name would break the toast
                names = left.iter().take(5).map(|n| n.escape_debug().to_string()).collect::<Vec<_>>().join(" · ")
            ));
        }
        app.toast(ctx, msg);
    }
    r
}

/// The `file.exportMaskEffects` command.
fn file_export_mask_effects_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => Some(x.to_string()),
        None => {
            let name = format!("{}.lceffect", p.get("group").and_then(Value::as_str).unwrap_or("Effect Presets"));
            let req = PickRequest::save(
                crate::i18n::tr("Export Effect Presets"),
                crate::i18n::tr("LightCraft Effect Presets"),
                &["lceffect"],
                name.clone(),
            );
            match crate::pick::ask(app, id, p, "path", req, |s| s.save_preset_file.as_mut().map(|f| f(&name).into_iter().collect())) {
                Picked::Now(v) => v.into_iter().next(),
                Picked::Later => return Ok(Value::Null),
                Picked::Unavailable => return Err("no file dialog on this platform".into()),
            }
        }
    };
    let Some(path) = path else { return Ok(Value::Null) };
    let mut params = json!({"path": path});
    for k in ["names", "group"] {
        if let Some(v) = p.get(k) {
            params[k] = v.clone();
        }
    }
    let r = app.session.execute("mask.exportEffects", &params).map_err(|e| e.to_string());
    match &r {
        Ok(v) => app.toast(ctx, crate::i18n::tr_format!("Exported {} effect preset{}", v["count"], if v["count"] == 1 { "" } else { "s" })),
        Err(e) => app.toast_error(ctx, e.clone()),
    }
    r
}

/// The `file.importCurvePresets` command.
fn file_import_curve_presets_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    let request = PickRequest::files(
        Some(crate::i18n::tr("Import Point Curve Presets").into()),
        crate::i18n::tr("Point Curve Presets"),
        crate::pick::CURVE_PRESET_EXTENSIONS,
    );
    let paths = match paths_or_pick(app, id, p, request, |s| s.pick_curve_preset_files.as_mut().map(|f| f())) {
        Ok(paths) => paths,
        Err(answer) => return answer,
    };
    let r = app.session.execute("curve.importPresets", &json!({"paths": paths})).map_err(|e| e.to_string());
    if let Ok(v) = &r {
        app.toast(ctx, curve_import_message(v));
    }
    r
}

/// What an import of point curve presets did.
fn curve_import_message(v: &Value) -> String {
    let n = v["imported"].as_array().map_or(0, Vec::len);
    let failed = v["failed"].as_array().map_or(0, Vec::len);
    let imported = crate::i18n::tr_format!("Imported {n} point curve preset{}", crate::i18n::plural_s(n), n = n);
    if failed == 0 {
        return imported;
    }
    imported + &crate::i18n::tr_format!(", {failed} file{} not readable", crate::i18n::plural_s(failed), failed = failed)
}

/// The `file.exportCurvePresets` command.
fn file_export_curve_presets_command(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, p: &Value) -> Result<Value, String> {
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => Some(x.to_string()),
        None => {
            let req = PickRequest::save(
                crate::i18n::tr("Export Point Curve Presets"),
                crate::i18n::tr("Point Curve Presets"),
                &["lccurve"],
                "Point Curves.lccurve",
            );
            match crate::pick::ask(app, id, p, "path", req, |s| {
                s.save_curve_preset_file.as_mut().map(|f| f("Point Curves.lccurve").into_iter().collect())
            }) {
                Picked::Now(v) => v.into_iter().next(),
                Picked::Later => return Ok(Value::Null),
                Picked::Unavailable => return Err("no file dialog on this platform".into()),
            }
        }
    };
    let Some(path) = path else { return Ok(Value::Null) };
    let mut params = json!({"path": path});
    if let Some(n) = p.get("names") {
        params["names"] = n.clone();
    }
    let r = app.session.execute("curve.exportPresets", &params).map_err(|e| e.to_string());
    if let Ok(v) = &r {
        app.toast(ctx, crate::i18n::tr_format!("Exported {} point curve preset{}", v["count"], if v["count"] == 1 { "" } else { "s" }));
    }
    r
}
