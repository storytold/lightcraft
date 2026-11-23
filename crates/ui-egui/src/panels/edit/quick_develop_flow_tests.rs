//! Native Quick Develop baseline through the real right/Edit parent. No image/frame/service execution.
use crate::{
    LightcraftApp, Services,
    headless::HeadlessView,
    state::{RightPanel, ViewMode},
};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Response};
use lightcraft_catalog::{Flag, HistoryStep, MediaKind, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::{DevelopSettings, WbMode, WbScale, controls};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
const VIEWPORT: egui::Vec2 = egui::Vec2::new(1200.0, 900.0);
const CLOCK: &str = "2026-11-21T14:00:00";
const ROWS: [(&str, &str, f64, f64); 9] = [
    ("Exposure", "light.exposure", 1.0 / 3.0, 1.0),
    ("Contrast", "light.contrast", 5.0, 20.0),
    ("Highlights", "light.highlights", 5.0, 20.0),
    ("Shadows", "light.shadows", 5.0, 20.0),
    ("Whites", "light.whites", 5.0, 20.0),
    ("Blacks", "light.blacks", 5.0, 20.0),
    ("Clarity", "effects.clarity", 5.0, 20.0),
    ("Vibrance", "color.vibrance", 5.0, 20.0),
    ("Temp", "wb.temp", 100.0, 500.0),
];
const ARROWS: [&str; 4] = ["◀◀", "◀", "▶", "▶▶"];
fn alias(row: usize, direction: usize) -> String {
    format!("button:quick-{}-{}", ROWS[row].1, ARROWS[direction])
}
fn delta(row: usize, direction: usize) -> f64 {
    let (_, _, small, big) = ROWS[row];
    [-big, -small, small, big][direction]
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Real,
    Disabled,
    Measure,
    Sizing,
}
struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
struct Review {
    app: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    pass: Pass,
    raw_wb: bool,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    responses: Vec<Response>,
    focused: Option<Response>,
    clicked: Option<Id>,
    owners: BTreeMap<String, Id>,
    painted: Vec<(String, Rect)>,
    access_labels: BTreeSet<String>,
    initial: BTreeMap<u64, Photo>,
    stable: Value,
    pending: Option<Value>,
    inverse: Option<Op>,
    last_label: String,
    old_language: crate::i18n::Locale,
}
impl Review {
    fn new(square: bool) -> Self {
        Self::fixture(square, false, None, false)
    }
    fn fixture(square: bool, raw_wb: bool, bound: Option<(usize, bool)>, auto_sync: bool) -> Self {
        Self::scene(if square { "view.squareGrid" } else { "view.photoGrid" }, raw_wb, bound, auto_sync, None)
    }
    fn scene(view: &str, raw_wb: bool, bound: Option<(usize, bool)>, auto_sync: bool, selection: Option<&[u64]>) -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        session.clock = Box::new(|| CLOCK.into());
        let mut initial = BTreeMap::new();
        for id in 101..=105 {
            let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("quick-{id}.jpg"), "JPEG", 64, 48, CLOCK);
            photo.flag = if id % 2 == 0 { Flag::Reject } else { Flag::None };
            photo.rating = (id % 5) as u8;
            photo.meta.title = format!("Untouched title {id}");
            photo.meta.caption = format!("Untouched caption {id}");
            photo.meta.keywords = vec![format!("untouched-{id}")];
            let mut d = DevelopSettings::default();
            for (row, (_, ctl, _, _)) in ROWS.iter().enumerate() {
                let v = if *ctl == "wb.temp" { 4500.0 + (id - 100) as f64 * 500.0 } else { (id as f64 - 103.0) * (row + 1) as f64 * 0.25 };
                assert!(controls::set(&mut d, ctl, v));
            }
            d.wb.mode = WbMode::Custom;
            d.wb.scale = WbScale::Kelvin;
            d.wb.tint = (id - 100) as f64;
            if let Some((row, upper)) = bound.filter(|_| id <= 102) {
                let spec = controls::find(ROWS[row].1).unwrap();
                let distance = ROWS[row].2 * (id - 100) as f64 / 4.0;
                assert!(controls::set(&mut d, ROWS[row].1, if upper { spec.max - distance } else { spec.min + distance }));
            }
            if raw_wb && id <= 103 {
                // These are inert catalog strings, never opened. Header facts suffice for source_info.
                photo.source = Source::File { path: format!("/catalog-only-no-files/quick-{id}.arw") };
                photo.kind = MediaKind::Raw;
                photo.format = "ARW".into();
                photo.file_name = format!("quick-{id}.arw");
                photo.meta.camera = if id == 103 { "SONY ILCE-7M2" } else { "SONY ILCE-7M3" }.into();
                photo.as_shot_wb = Some(if id == 101 {
                    (4986.0, -2.0)
                } else if id == 102 {
                    (6200.0, 8.0)
                } else {
                    (6500.0, 0.0)
                });
                d.wb.mode = if id == 102 { WbMode::Custom } else { WbMode::AsShot };
                d.wb.scale = WbScale::Legacy;
                d.wb.temp = if id == 102 { 5800.0 } else { 6500.0 };
                d.wb.tint = if id == 102 { 3.0 } else { 0.0 };
            }
            photo.develop = Arc::new(d);
            photo.edited = Some(format!("2026-11-20T10:00:0{}", id - 100));
            photo.history = vec![HistoryStep { label: format!("Existing {id}"), settings: photo.develop.clone() }];
            photo.versions = vec![Version { name: format!("Saved {id}"), created: CLOCK.into(), settings: photo.develop.clone(), auto: false }];
            initial.insert(id, photo.clone());
            session.commit("Quick Develop fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let ids = selection.map(<[u64]>::to_vec).unwrap_or_else(|| if raw_wb { vec![101, 102, 103] } else { vec![101, 102] });
        session.execute("library.select", &json!({"ids":ids,"active":ids.first().copied()})).unwrap();
        session.auto_sync = auto_sync;
        let mut app = LightcraftApp::new(session, Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.set_simulated_clock(Some(0.0));
        app.ui.auto_advance = false;
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        // Ordinary persisted panel preferences only: no focus, hit, scroll or component-state injection.
        app.ui.open_sections.clear();
        app.ui.open_flyouts.clear();
        app.ui.right_width = crate::state::RIGHT_WIDTH.default;
        app.run(view, json!({})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        let mut r = Self {
            app,
            ctx,
            tick: 0,
            pass: Pass::Real,
            raw_wb,
            rects: BTreeMap::new(),
            hits: BTreeMap::new(),
            responses: vec![],
            focused: None,
            clicked: None,
            owners: BTreeMap::new(),
            painted: vec![],
            access_labels: BTreeSet::new(),
            initial,
            stable: Value::Null,
            pending: None,
            inverse: None,
            last_label: String::new(),
            old_language,
        };
        r.stable = r.snapshot();
        r.idle();
        if r.app.session.selection.ids.len() > 1 && matches!(r.app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
            r.assert_grid();
        } else {
            assert!(!r.rects.keys().any(|name| name.starts_with("button:quick-")));
        }
        r.observe("opened-native-actual-right-edit-parent");
        r
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
    }
    fn snapshot(&self) -> Value {
        let s = &self.app.session;
        // UiState intentionally skips transient fields in saved preferences. Observe the
        // actual dialog/toast/Compare state explicitly, and guard every remaining UiState
        // field with a Debug projection of a CLONE. No application state is overwritten.
        let mut ui = json!(self.app.ui);
        ui["dialog"] = json!(self.app.ui.dialog);
        ui["toast"] = json!(self.app.ui.toast);
        ui["compare"] = json!(self.app.ui.compare);
        let mut ui_rest = self.app.ui.clone();
        ui_rest.dialog = None;
        ui_rest.toast = None;
        ui_rest.compare = None;
        ui_rest.view = ViewMode::PhotoGrid;
        json!({"uiRest":format!("{ui_rest:?}"),"catalog":s.catalog,"revision":s.catalog.revision,"undo":Self::history(&s.undo),"redo":Self::history(&s.redo),
            "commits":s.commits(),"journal":s.journal,"recent":s.recent_keywords,"ui":ui,
            "selection":s.selection,"previousActive":s.previous_active,"clipboard":s.clipboard,"copyGroups":s.copy_groups,
            "source":s.source,"filter":s.filter,"sort":s.sort,"autoSync":s.auto_sync,
            "activeMask":s.active_mask,"activeSpot":s.active_spot,"toolEffects":s.mask_tool_effects,
            "lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,"printTemplates":s.print_templates,
            "effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,"interaction":s.interaction.is_some()})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 5);
        assert!(s.catalog.photos().all(|p| matches!(&p.source, Source::Demo { .. })
            || (self.raw_wb && matches!(&p.source,Source::File { path } if path.starts_with("/catalog-only-no-files/")))));
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.services.save_preset_file.is_none() && self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty() && self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(self.app.merge.preview_task.is_none() && self.app.merge.final_task.is_none());
        assert!(self.app.merge.preview.is_none() && self.app.merge.preview_pixels.is_none());
        assert!(!s.segmenter.analyzing() && !s.segmenter.busy());
        assert!(s.activity.list().is_empty());
        let actual = self.snapshot();
        assert!(
            actual == self.stable || self.pending.as_ref() == Some(&actual),
            "unpredicted Quick Develop/parent action or replay in {:?}, tick {}: actual={actual}; stable={}; pending={:?}",
            self.pass,
            self.tick,
            self.stable,
            self.pending
        );
    }
    fn text_shapes(shape: &egui::epaint::Shape, clip: Rect, painted: &mut Vec<(String, Rect)>) {
        match shape {
            egui::epaint::Shape::Vec(parts) => {
                for part in parts {
                    Self::text_shapes(part, clip, painted);
                }
            }
            egui::epaint::Shape::Text(text) => {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                if clip.contains_rect(rect) {
                    painted.push((text.galley.job.text.clone(), rect));
                }
            }
            _ => {}
        }
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let input = events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { pos, button, pressed, .. } => {
                    Some(json!({"kind":"pointer","point":[pos.x,pos.y],"button":format!("{button:?}"),"pressed":pressed}))
                }
                Event::Key { key, pressed, modifiers, .. } => {
                    Some(json!({"kind":"key","key":format!("{key:?}"),"pressed":pressed,"shift":modifiers.shift}))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(VIEWPORT, 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let pass = self.pass;
        let known = self.owners.values().copied().collect::<Vec<_>>();
        let mut last = None;
        let output = Output(ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(root, |ui| {
                ui.set_max_width(crate::state::RIGHT_WIDTH.default);
                let panel = Rect::from_min_size(ui.cursor().min, egui::vec2(crate::state::RIGHT_WIDTH.default, VIEWPORT.y));
                ui.set_clip_rect(ui.clip_rect().intersect(panel));
                let mut draw = |body: &mut egui::Ui| crate::panels::right::show(app, body);
                match pass {
                    Pass::Real => draw(ui),
                    Pass::Disabled => {
                        ui.disable();
                        draw(ui);
                    }
                    Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass());
                        draw(&mut child);
                    }
                }
            });
            // Query genuine responses in the callback. Native register() aliases are hover
            // probes, not command owners. Current register_response() IDs also cover the
            // unchanged SDK Pick/dialog footer; no owner is constructed or forced here.
            let registry = crate::widgets::take_registries(&ctx);
            let (clicked, mut ids) = ctx.interaction_snapshot(|s| (s.clicked, s.contains_pointer.iter().copied().collect::<Vec<_>>()));
            ids.extend(known.iter().copied());
            ids.extend(registry.1.iter().map(|(_, hit)| hit.id));
            if let Some(id) = clicked {
                ids.push(id);
            }
            let focused = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
            if let Some(r) = &focused {
                ids.push(r.id);
            }
            ids.sort_by_key(Id::value);
            ids.dedup();
            let responses = ids.into_iter().filter_map(|id| ctx.read_response(id)).collect::<Vec<_>>();
            last = Some((registry, responses, focused, clicked));
        }));
        let ((rects, hits), responses, focused, clicked) = last.unwrap();
        self.rects = rects.into_iter().collect();
        self.hits = hits.into_iter().collect();
        self.responses = responses;
        let native_focus = focused.as_ref().map(|r| json!({"id":format!("{:?}",r.id),"rect":[r.rect.min.x,r.rect.min.y,r.rect.max.x,r.rect.max.y],"enabled":r.enabled(),"hasFocus":r.has_focus()}));
        // SDK focus is a real stop under egui's focused arena root. Keep the actual
        // leaf response for geometry/owner checks, not that much larger root response.
        self.focused = match craft::ui::runtime::focus::focused_rec(&self.ctx) {
            Some(record) => self.ctx.read_response(record.stop.id),
            None => focused,
        };
        self.clicked = clicked;
        self.painted.clear();
        for shape in &output.0.shapes {
            Self::text_shapes(&shape.shape, shape.clip_rect, &mut self.painted);
        }
        if let Some(update) = &output.0.platform_output.accesskit_update {
            for (_, node) in &update.nodes {
                if let Some(label) = node.label() {
                    self.access_labels.insert(label.to_owned());
                }
            }
        }
        drop(output);
        if !input.is_empty() {
            println!(
                "QUICK_NATIVE_INPUT {}",
                json!({"tick":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"pass":format!("{:?}",self.pass),
                "view":self.app.ui.view,"events":input,"focus":self.focus_name(),"clicked":format!("{:?}",self.clicked),
                "implementation":"sdk-port-1","nativeFocus":native_focus,
                "sdkFocusedStop":craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec|format!("{:?}",rec.stop.id)),
                "observedFocusResponse":self.focused.as_ref().map(|r|json!({"id":format!("{:?}",r.id),"rect":[r.rect.min.x,r.rect.min.y,r.rect.max.x,r.rect.max.y],"nativeHasFocus":r.has_focus(),"enabled":r.enabled()})),
                "owners":self.responses.iter().filter(|r|r.is_pointer_button_down_on()||r.clicked()).map(|r|json!({"id":format!("{:?}",r.id),"enabled":r.enabled(),"down":r.is_pointer_button_down_on(),"clicked":r.clicked()})).collect::<Vec<_>>(),
                "selection":self.app.session.selection,"dialog":self.app.ui.dialog,"commits":self.app.session.commits(),"journalLast":self.app.session.journal.last()})
            );
        }
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, alias: &str) -> Rect {
        *self.rects.get(alias).unwrap_or_else(|| panic!("missing actual parent locator {alias}: {:?}", self.rects.keys()))
    }
    fn clipped(&self, alias: &str) -> Rect {
        self.rect(alias).intersect(self.hits.get(alias).unwrap().interact_rect).intersect(Rect::from_min_size(Pos2::ZERO, VIEWPORT))
    }
    fn actual_at(&self, alias: &str, point: Pos2) -> Option<&Response> {
        let hit = self.hits.get(alias)?;
        let mut candidates = self
            .responses
            .iter()
            .filter(|r| r.sense.senses_click() && r.rect == self.rect(alias) && r.layer_id == hit.layer_id && r.interact_rect.contains(point));
        let first = candidates.next()?;
        assert!(candidates.next().is_none(), "ambiguous actual click owner at {alias}");
        Some(first)
    }
    fn ready(&mut self, alias: &str) -> (Pos2, Id) {
        assert_eq!(self.pass, Pass::Real);
        let mut point = self.clipped(alias).center();
        let mut previous = None;
        let mut consecutive = 0;
        for attempt in 0..32 {
            self.frame(vec![Event::PointerMoved(point)]);
            let clipped = self.clipped(alias);
            let inside = point.x > clipped.left() && point.x < clipped.right() && point.y > clipped.top() && point.y < clipped.bottom();
            let owner = self.actual_at(alias, point).map(|r| (r.id, r.layer_id, r.enabled()));
            let eligible = inside && owner.is_some_and(|(_, layer, enabled)| enabled && self.ctx.layer_id_at(point) == Some(layer));
            consecutive = if eligible && owner == previous { consecutive + 1 } else { usize::from(eligible) };
            previous = owner;
            if consecutive >= 3 {
                let id = owner.unwrap().0;
                self.owners.insert(alias.into(), id);
                println!(
                    "QUICK_NATIVE_READY {}",
                    json!({"alias":alias,"attempt":attempt,"owner":format!("{id:?}"),"registered":format!("{:?}",self.hits[alias].id),
                    "point":[point.x,point.y],"clip":[clipped.left(),clipped.top(),clipped.right(),clipped.bottom()]})
                );
                return (point, id);
            }
            if !inside {
                point = clipped.center();
                consecutive = 0;
            }
        }
        panic!("actual native parent never obtains a stable interior hit: {alias}");
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Default::default() };
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn expect(&mut self, next: Value) {
        assert!(self.pending.is_none());
        self.safety();
        self.pending = Some(next);
    }
    fn accept(&mut self, stage: &str) {
        let expected = self.pending.take().unwrap();
        assert_eq!(self.snapshot(), expected, "{stage}: exact full-state prediction must hold");
        self.stable = expected;
        self.idle();
        self.observe(stage);
    }
    fn click_expected(&mut self, alias: &str, next: Value, stage: &str) {
        let (point, id) = self.ready(alias);
        self.pointer(point, true);
        let response = self.actual_at(alias, point).expect("actual owner remains on press");
        assert_eq!(response.id, id);
        assert!(response.is_pointer_button_down_on());
        assert_eq!(self.snapshot(), self.stable, "press alone cannot dispatch");
        self.expect(next);
        self.pointer(point, false);
        assert_eq!(self.clicked, Some(id), "release must belong to the actual control, not its locator probe");
        self.accept(stage);
    }
    fn focus_name(&self) -> String {
        if let Some(record) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            return self
                .hits
                .iter()
                .find(|(_, hit)| hit.id == record.stop.id)
                .map_or_else(|| format!("unregistered-sdk:{:?}", record.stop.id), |(name, _)| name.clone());
        }
        self.focused.as_ref().map_or_else(
            || "none".into(),
            |r| self.rects.iter().find(|(_, rect)| **rect == r.rect).map_or_else(|| format!("unregistered:{:?}", r.id), |(name, _)| name.clone()),
        )
    }

    fn assert_grid(&self) {
        assert_eq!(self.rects.keys().filter(|name| name.starts_with("button:quick-")).count(), 36, "all real rows and directions must be present");
        for row in 0..9 {
            for direction in 0..4 {
                let name = alias(row, direction);
                let clipped = self.clipped(&name);
                assert!(clipped.is_positive(), "ordinary target must have genuine visible area: {name}");
            }
        }
    }
    fn observe(&self, stage: &str) {
        let s = &self.app.session;
        println!(
            "QUICK_NATIVE_FLOW {}",
            json!({"stage":stage,"tick":self.tick,"pass":format!("{:?}",self.pass),"view":self.app.ui.view,
          "selection":s.selection,"focus":self.focus_name(),"commits":s.commits(),"undo":s.undo.len(),"redo":s.redo.len(),"journalLast":s.journal.last(),
          "photos":s.catalog.photos().map(|p|json!({"id":p.id,"wb":p.develop.wb,"values":ROWS.iter().map(|(_,ctl,_,_)|json!([ctl,controls::get(&p.develop,ctl)])).collect::<Vec<_>>(),"history":p.history.len(),"versions":p.versions.len(),"edited":p.edited})).collect::<Vec<_>>() })
        );
    }
    fn numeric(settings: &DevelopSettings, ctl: &str) -> f64 {
        match ctl {
            "light.exposure" => settings.light.exposure,
            "light.contrast" => settings.light.contrast,
            "light.highlights" => settings.light.highlights,
            "light.shadows" => settings.light.shadows,
            "light.whites" => settings.light.whites,
            "light.blacks" => settings.light.blacks,
            "effects.clarity" => settings.effects.clarity,
            "color.vibrance" => settings.color.vibrance,
            "wb.temp" => settings.wb.temp,
            _ => panic!("unexpected baseline control"),
        }
    }
    fn expected_settings(&self, id: u64, ctl: &str, amount: f64) -> DevelopSettings {
        let mut d = (*self.app.session.develop_of(PhotoId(id)).unwrap()).clone();
        if ctl == "wb.temp" && self.raw_wb {
            match id {
                101 if d.wb.mode == WbMode::AsShot => {
                    d.wb.temp = 4986.0;
                    d.wb.tint = -2.0;
                    d.wb.scale = WbScale::Kelvin;
                }
                102 if d.wb.scale == WbScale::Legacy => {
                    // Pure color math, not normalize_wb or a second command-running Session.
                    let (temp, tint) = lightcraft_engine::pipeline::local::legacy_to_kelvin(d.wb.temp, d.wb.tint, (6200.0, 8.0));
                    d.wb.temp = temp;
                    d.wb.tint = tint;
                    d.wb.scale = WbScale::Kelvin;
                }
                103 if d.wb.mode == WbMode::AsShot => {
                    d.wb.temp = 6500.0;
                    d.wb.tint = 0.0;
                    d.wb.scale = WbScale::Legacy;
                }
                _ => {}
            }
        }
        let spec = controls::find(ctl).unwrap();
        let value = (Self::numeric(&d, ctl) + amount).clamp(spec.min, spec.max);
        match ctl {
            "light.exposure" => d.light.exposure = value,
            "light.contrast" => d.light.contrast = value,
            "light.highlights" => d.light.highlights = value,
            "light.shadows" => d.light.shadows = value,
            "light.whites" => d.light.whites = value,
            "light.blacks" => d.light.blacks = value,
            "effects.clarity" => d.effects.clarity = value,
            "color.vibrance" => d.color.vibrance = value,
            "wb.temp" => {
                d.wb.temp = value;
                d.wb.mode = WbMode::Custom;
            }
            _ => unreachable!(),
        }
        assert!(d.masks.is_empty());
        d
    }
    fn predict_nudge(&mut self, row: usize, direction: usize) -> Value {
        let ctl = ROWS[row].1;
        let amount = delta(row, direction);
        let label = format!("Quick Develop: {}", controls::find(ctl).unwrap().label);
        let mut catalog = self.app.session.catalog.clone();
        let ops = self
            .app
            .session
            .selection
            .ids
            .iter()
            .map(|id| {
                let settings = Arc::new(self.expected_settings(id.0, ctl, amount));
                Op::Batch {
                    ops: vec![
                        Op::SetDevelop { id: *id, settings: settings.clone(), label: label.clone(), edited: Some(CLOCK.into()) },
                        Op::PushHistory { id: *id, step: HistoryStep { label: label.clone(), settings } },
                    ],
                }
            })
            .collect();
        let inverse = catalog.apply(Op::Batch { ops }).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().push(json!({"label":label,"op":inverse,"folder":"None"}));
        next["redo"] = json!([]);
        next["commits"] = json!(self.app.session.commits() + 1);
        next["journal"].as_array_mut().unwrap().push(json!(["develop.quickAdjust",{"control":ctl,"delta":amount}]));
        self.inverse = Some(inverse);
        self.last_label = label;
        next
    }
    fn nudge(&mut self, row: usize, direction: usize) {
        let next = self.predict_nudge(row, direction);
        self.click_expected(&alias(row, direction), next, "one-real-relative-batch");
        self.untouched();
        let text = crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = delta(row, direction), label = crate::i18n::tr(ROWS[row].0));
        assert!(self.access_labels.contains(&text), "real native arrow accessibility label must be emitted: {text}");
    }
    fn untouched(&self) {
        for id in 101..=105 {
            if !self.app.session.selection.ids.contains(&PhotoId(id)) {
                assert_eq!(
                    self.app.session.catalog.photo(PhotoId(id)).unwrap().as_ref(),
                    &self.initial[&id],
                    "whole unselected photo remains untouched"
                );
            }
        }
    }
    fn undo(&mut self) {
        let mut catalog = self.app.session.catalog.clone();
        let forward = catalog.apply(self.inverse.clone().unwrap()).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().pop().unwrap();
        next["redo"].as_array_mut().unwrap().push(json!({"label":self.last_label,"op":forward,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.undo", {}]));
        self.expect(next);
        self.app.run("edit.undo", json!({})).unwrap();
        self.accept("genuine-relative-batch-undo");
    }
    fn redo(&mut self) {
        let mut catalog = self.app.session.catalog.clone();
        let forward = self.app.session.redo.last().unwrap().op.clone();
        let inverse = catalog.apply(forward).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["redo"].as_array_mut().unwrap().pop().unwrap();
        next["undo"].as_array_mut().unwrap().push(json!({"label":self.last_label,"op":inverse,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.redo", {}]));
        self.expect(next);
        self.app.run("edit.redo", json!({})).unwrap();
        self.accept("genuine-relative-batch-redo");
        self.inverse = Some(inverse);
    }
    fn undo_redo(&mut self) {
        self.undo();
        self.redo();
        self.untouched();
    }
    fn select(&mut self, ids: &[u64], active: u64) {
        let before = self.app.session.active();
        let params = json!({"ids":ids,"active":active});
        let mut next = self.stable.clone();
        next["selection"] = json!({"ids":ids,"active":active});
        if before != Some(PhotoId(active)) && before.is_some() {
            next["previousActive"] = json!(before);
        }
        next["journal"].as_array_mut().unwrap().push(json!(["library.select", params]));
        self.expect(next);
        self.app.run("library.select", params).unwrap();
        self.accept("ordinary-selection-handoff");
    }
    fn geometry(&self, stage: &str, name: &str, point: Pos2) {
        println!(
            "QUICK_NATIVE_GEOMETRY {}",
            json!({"stage":stage,"alias":name,"point":[point.x,point.y],"pass":format!("{:?}",self.pass),
          "registered":self.rects.get(name).map(|r|format!("{r:?}")),"hit":self.hits.get(name).map(|h|json!({"id":format!("{:?}",h.id),"interact":format!("{:?}",h.interact_rect)})),
          "actual":self.actual_at(name,point).map(|r|json!({"id":format!("{:?}",r.id),"rect":format!("{:?}",r.rect),"interact":format!("{:?}",r.interact_rect),"enabled":r.enabled(),"down":r.is_pointer_button_down_on(),"clicked":r.clicked()}))})
        );
    }
    fn non_real(&mut self, pass: Pass, row: usize, direction: usize) {
        let name = alias(row, direction);
        let (point, _) = self.ready(&name);
        self.geometry("ordinary-before-blocked-press", &name, point);
        self.pass = pass;
        self.idle();
        self.geometry("actual-non-real-output-before-physical-press", &name, point);
        self.pointer(point, true);
        self.pass = Pass::Real;
        self.pointer(point, false);
        self.idle();
        self.observe("blocked-press-real-release-inert");
        let (point, _) = self.ready(&name);
        self.pass = pass;
        self.pointer(point, true);
        self.pointer(point, false);
        self.pass = Pass::Real;
        self.idle();
        self.observe("complete-blocked-click-no-replay");
        self.nudge(row, direction);
        self.undo_redo();
        self.observe("fresh-positive-after-blocked-input");
    }
    fn tab_to(&mut self, name: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..160 {
            if self.focus_name() == name {
                let response = self.focused.as_ref().expect("genuine focused stop response");
                assert!(response.enabled() && response.sense.senses_click());
                if let Some(record) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
                    assert_eq!(record.stop.id, response.id, "the real arena focus resolves to this leaf");
                    assert_eq!(record.stop.id, self.hits.get(name).unwrap().id, "the current registered target is that same leaf");
                } else {
                    assert!(response.has_focus(), "a foreign/native widget must actually own egui focus");
                }
                assert_eq!(response.rect, self.rect(name));
                assert!(response.interact_rect.intersects(self.clipped(name)));
                return;
            }
            self.key(Key::Tab, false);
        }
        panic!("ordinary traversal never reached {name}; stopped at {}", self.focus_name());
    }
    fn focus_cycle(&mut self, reverse: bool) -> Vec<String> {
        let anchor = alias(0, 0);
        assert_eq!(self.focus_name(), anchor);
        let mut cycle = vec![];
        for _ in 0..160 {
            self.key(Key::Tab, reverse);
            let name = self.focus_name();
            cycle.push(name.clone());
            if name == anchor {
                return cycle;
            }
        }
        panic!("natural focus cycle did not return to its actual anchor: {cycle:?}");
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
    }
}

fn directions(row: usize) {
    for square in [false, true] {
        let mut r = Review::new(square);
        for direction in 0..4 {
            r.nudge(row, direction);
            r.undo_redo();
        }
    }
}
fn clamp_repeat(row: usize) {
    for upper in [false, true] {
        let mut r = Review::fixture(false, false, Some((row, upper)), false);
        let direction = if upper { 3 } else { 0 };
        let spec = controls::find(ROWS[row].1).unwrap();
        let limit = if upper { spec.max } else { spec.min };
        for repeat in 0..3 {
            let before = r.app.session.selection.ids.iter().map(|id| r.app.session.catalog.photo(*id).unwrap().history.len()).collect::<Vec<_>>();
            let commits = r.app.session.commits();
            r.nudge(row, direction);
            for (index, id) in r.app.session.selection.ids.iter().enumerate() {
                let photo = r.app.session.catalog.photo(*id).unwrap();
                assert_eq!(Review::numeric(&photo.develop, ROWS[row].1), limit);
                assert_eq!(photo.history.len(), before[index] + 1, "even a clamped unchanged repeat retains native history");
            }
            assert_eq!(r.app.session.commits(), commits + 1);
            r.undo_redo();
            r.observe(&format!("clamp-repeat-{repeat}-upper-{upper}"));
        }
    }
}
#[test]
fn quick_develop_native_exposure_all_four_directions_both_grids() {
    directions(0);
}
#[test]
fn quick_develop_native_exposure_clamp_repeat_preserves_batch_history() {
    clamp_repeat(0);
}
#[test]
fn quick_develop_native_contrast_all_four_directions_both_grids() {
    directions(1);
}
#[test]
fn quick_develop_native_contrast_clamp_repeat_preserves_batch_history() {
    clamp_repeat(1);
}
#[test]
fn quick_develop_native_highlights_all_four_directions_both_grids() {
    directions(2);
}
#[test]
fn quick_develop_native_highlights_clamp_repeat_preserves_batch_history() {
    clamp_repeat(2);
}
#[test]
fn quick_develop_native_shadows_all_four_directions_both_grids() {
    directions(3);
}
#[test]
fn quick_develop_native_shadows_clamp_repeat_preserves_batch_history() {
    clamp_repeat(3);
}
#[test]
fn quick_develop_native_whites_all_four_directions_both_grids() {
    directions(4);
}
#[test]
fn quick_develop_native_whites_clamp_repeat_preserves_batch_history() {
    clamp_repeat(4);
}
#[test]
fn quick_develop_native_blacks_all_four_directions_both_grids() {
    directions(5);
}
#[test]
fn quick_develop_native_blacks_clamp_repeat_preserves_batch_history() {
    clamp_repeat(5);
}
#[test]
fn quick_develop_native_clarity_all_four_directions_both_grids() {
    directions(6);
}
#[test]
fn quick_develop_native_clarity_clamp_repeat_preserves_batch_history() {
    clamp_repeat(6);
}
#[test]
fn quick_develop_native_vibrance_all_four_directions_both_grids() {
    directions(7);
}
#[test]
fn quick_develop_native_vibrance_clamp_repeat_preserves_batch_history() {
    clamp_repeat(7);
}
#[test]
fn quick_develop_native_temperature_all_four_directions_both_grids() {
    directions(8);
}
#[test]
fn quick_develop_native_temperature_clamp_repeat_preserves_batch_history() {
    clamp_repeat(8);
}

#[test]
fn quick_develop_native_mixed_photo_white_balance_all_four_directions() {
    for direction in 0..4 {
        let mut r = Review::fixture(false, true, None, false);
        for (id, relative, as_shot) in [(101, false, (4986.0, -2.0)), (102, false, (6200.0, 8.0)), (103, true, (6500.0, 0.0))] {
            let info = r.app.session.source_info(PhotoId(id));
            println!(
                "QUICK_NATIVE_WB_FACT {}",
                json!({"id":id,"raw":info.raw,"relative":info.relative_wb,"legacy":info.legacy_relative_wb,"asShot":[info.as_shot_temp,info.as_shot_tint]})
            );
            assert!(info.raw);
            assert_eq!(info.relative_wb, relative);
            assert_eq!((info.as_shot_temp, info.as_shot_tint), as_shot);
            assert_eq!(info.legacy_relative_wb, !relative);
        }
        r.nudge(8, direction);
        r.undo_redo();
        for id in 101..=103 {
            assert_eq!(r.app.session.develop_of(PhotoId(id)).unwrap().wb.mode, WbMode::Custom);
        }
    }
}
#[test]
fn quick_develop_native_relative_selection_is_independent_of_auto_sync() {
    for square in [false, true] {
        for sync in [false, true] {
            let mut r = Review::fixture(square, false, None, sync);
            r.nudge(0, 2);
            r.undo_redo();
            r.nudge(7, 0);
            r.undo_redo();
            assert_eq!(r.app.session.auto_sync, sync);
        }
    }
}
#[test]
fn quick_develop_native_fresh_adjust_clears_existing_redo() {
    let mut r = Review::new(false);
    r.nudge(0, 2);
    r.undo();
    assert!(!r.app.session.redo.is_empty());
    r.nudge(1, 3);
    assert!(r.app.session.redo.is_empty());
    r.undo_redo();
}
#[test]
fn quick_develop_native_enter_and_space_every_arrow_both_grids() {
    for square in [false, true] {
        let mut r = Review::new(square);
        for row in 0..9 {
            for direction in 0..4 {
                let name = alias(row, direction);
                r.tab_to(&name);
                for key in [Key::Enter, Key::Space] {
                    let next = r.predict_nudge(row, direction);
                    r.expect(next);
                    r.key(key, false);
                    r.accept("natural-focused-key-single-relative-batch");
                    println!(
                        "QUICK_NATIVE_KEYBOARD {}",
                        json!({"view":r.app.ui.view,"row":row,"direction":direction,"key":format!("{key:?}"),"alias":name,"focus":r.focus_name(),"actualOwner":format!("{:?}",r.focused.as_ref().unwrap().id)})
                    );
                    r.undo_redo();
                }
            }
        }
    }
}
#[test]
fn quick_develop_native_records_complete_natural_forward_reverse_cycles() {
    for square in [false, true] {
        let mut r = Review::new(square);
        r.tab_to(&alias(0, 0));
        let forward = r.focus_cycle(false);
        let forward_again = r.focus_cycle(false);
        assert_eq!(forward_again, forward);
        let reverse = r.focus_cycle(true);
        let reverse_again = r.focus_cycle(true);
        assert_eq!(reverse_again, reverse);
        let mut expected_reverse = forward[..forward.len() - 1].iter().rev().cloned().collect::<Vec<_>>();
        expected_reverse.push(alias(0, 0));
        assert_eq!(reverse, expected_reverse, "observe every stop, including native siblings; never filter unknowns");
        let actual_grid = forward.iter().filter(|name| name.starts_with("button:quick-")).cloned().collect::<BTreeSet<_>>();
        let expected_grid = (0..9).flat_map(|row| (0..4).map(move |direction| alias(row, direction))).collect::<BTreeSet<_>>();
        assert_eq!(actual_grid, expected_grid);
        assert_eq!(forward.len(), forward.iter().collect::<BTreeSet<_>>().len(), "unknown native stops must also have distinct observed identities");
        println!("QUICK_NATIVE_FOCUS_ORDER {}", json!({"view":r.app.ui.view,"forward":[forward,forward_again],"reverse":[reverse,reverse_again]}));
    }
}
#[test]
fn quick_develop_native_absent_in_detail_compare_single_and_empty_selection() {
    for view in ["view.detail", "view.compare"] {
        let mut r = Review::scene(view, false, None, false, None);
        r.idle();
        assert!(!r.rects.keys().any(|name| name.starts_with("button:quick-")));
    }
    for ids in [vec![101], vec![]] {
        let mut r = Review::scene("view.photoGrid", false, None, false, Some(&ids));
        r.idle();
        assert!(!r.rects.keys().any(|name| name.starts_with("button:quick-")));
    }
}
#[test]
fn quick_develop_native_interrupted_release_on_other_real_arrow_is_inert() {
    let mut r = Review::new(false);
    let original = alias(0, 0);
    let other = alias(8, 3);
    let (outside, _) = r.ready(&other);
    let (point, _) = r.ready(&original);
    r.pointer(point, true);
    r.pointer(outside, false);
    r.idle();
    r.observe("cancelled-release-on-another-visible-real-control");
    r.nudge(0, 0);
    r.undo_redo();
    r.nudge(8, 3);
    r.undo_redo();
}
#[test]
fn quick_develop_native_held_pointer_selection_membership_handoff_is_inert() {
    let mut r = Review::new(false);
    let name = alias(0, 3);
    let (point, _) = r.ready(&name);
    r.pointer(point, true);
    r.select(&[101, 103], 101);
    r.geometry("selection-membership-changed-before-release", &name, point);
    r.pointer(point, false);
    r.idle();
    r.observe("old-selection-gesture-not-retargeted");
    r.nudge(0, 3);
    r.undo_redo();
}
#[test]
fn quick_develop_native_held_pointer_active_photo_handoff_is_inert() {
    let mut r = Review::new(false);
    let name = alias(8, 2);
    let (point, _) = r.ready(&name);
    r.pointer(point, true);
    r.select(&[101, 102], 102);
    r.geometry("active-owner-changed-before-release", &name, point);
    r.pointer(point, false);
    r.idle();
    r.observe("old-active-owner-gesture-not-retargeted");
    r.nudge(8, 2);
    r.undo_redo();
}
#[test]
fn quick_develop_native_held_pointer_grid_disappearance_and_return_is_inert() {
    let mut r = Review::new(false);
    let name = alias(4, 1);
    let (point, _) = r.ready(&name);
    r.pointer(point, true);
    r.select(&[101], 101);
    assert!(!r.rects.keys().any(|name| name.starts_with("button:quick-")));
    r.pointer(point, false);
    r.idle();
    r.select(&[101, 102], 101);
    r.idle();
    r.assert_grid();
    r.observe("absent-grid-did-not-replay-on-return");
    r.nudge(4, 1);
    r.undo_redo();
}
#[test]
fn quick_develop_native_disabled_exposure_large_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Disabled, 0, 0);
}
#[test]
fn quick_develop_native_disabled_exposure_small_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Disabled, 0, 2);
}
#[test]
fn quick_develop_native_disabled_whites_small_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Disabled, 4, 1);
}
#[test]
fn quick_develop_native_disabled_temperature_large_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Disabled, 8, 3);
}
#[test]
fn quick_develop_native_measurement_exposure_large_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Measure, 0, 0);
}
#[test]
fn quick_develop_native_measurement_exposure_small_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Measure, 0, 2);
}
#[test]
fn quick_develop_native_measurement_whites_small_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Measure, 4, 1);
}
#[test]
fn quick_develop_native_measurement_temperature_large_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Measure, 8, 3);
}
#[test]
fn quick_develop_native_sizing_exposure_large_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Sizing, 0, 0);
}
#[test]
fn quick_develop_native_sizing_exposure_small_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Sizing, 0, 2);
}
#[test]
fn quick_develop_native_sizing_whites_small_decrease_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Sizing, 4, 1);
}
#[test]
fn quick_develop_native_sizing_temperature_large_increase_blocked_press_release_and_fresh_positive() {
    Review::new(false).non_real(Pass::Sizing, 8, 3);
}

impl Review {
    fn blocked_key(&mut self, pass: Pass, key: Key) {
        let name = alias(0, 2);
        self.tab_to(&name);
        self.observe("ordinary-natural-focus-before-blocked-key");
        self.pass = pass;
        self.idle();
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        self.pass = Pass::Real;
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
        self.idle();
        self.observe("blocked-key-press-real-release-inert");
        self.tab_to(&name);
        self.pass = pass;
        self.key(key, false);
        self.pass = Pass::Real;
        self.idle();
        self.observe("complete-blocked-key-no-replay");
        self.tab_to(&name);
        let next = self.predict_nudge(0, 2);
        self.expect(next);
        self.key(key, false);
        self.accept("fresh-natural-key-after-non-real-pass");
        self.undo_redo();
    }
}
#[test]
fn quick_develop_native_disabled_natural_enter_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Disabled, Key::Enter);
}
#[test]
fn quick_develop_native_disabled_natural_space_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Disabled, Key::Space);
}
#[test]
fn quick_develop_native_measurement_natural_enter_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Measure, Key::Enter);
}
#[test]
fn quick_develop_native_measurement_natural_space_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Measure, Key::Space);
}
#[test]
fn quick_develop_native_sizing_natural_enter_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Sizing, Key::Enter);
}
#[test]
fn quick_develop_native_sizing_natural_space_blocked_press_release_and_fresh_positive() {
    Review::new(false).blocked_key(Pass::Sizing, Key::Space);
}
