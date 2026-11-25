//! Actual Save Effect parent, in-memory only. Native baseline before Name/Group migration.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::cmd::mask_effects::BrushSettings;

const SDK_NAMING: bool = true;
// Public SDK overlays do not populate egui's legacy Popup memory. Current
// liveness comes from this completed draw's real PopoverOutput, never the
// SDK family/entry record documented as shown last frame. Keep that historical
// record below for explicit provenance, not as an altered native expectation.
fn save_current_popup_output(ctx: &egui::Context) -> Option<serde_json::Value> {
    let frame = ctx.cumulative_frame_nr();
    let pass = ctx.cumulative_pass_nr();
    let output = ctx.data(|data| data.get_temp::<serde_json::Value>(egui::Id::unique("save-effect-popup-frame-provenance")))?;
    // The public counters advance after ctx.run; actual press/release/quiet
    // receipts establish +1 for the final real show, even with discard passes.
    let current = output.get("frameAtShow").and_then(serde_json::Value::as_u64).and_then(|value| value.checked_add(1)) == Some(frame)
        && output.get("passAtShow").and_then(serde_json::Value::as_u64).and_then(|value| value.checked_add(1)) == Some(pass);
    current.then_some(output)
}
fn save_popup_open(ctx: &egui::Context) -> bool {
    egui::Popup::is_any_open(ctx)
        || save_current_popup_output(ctx).and_then(|output| output.get("outputOpen").and_then(serde_json::Value::as_bool)).unwrap_or(false)
}
fn save_historical_popup_open(ctx: &egui::Context) -> bool {
    egui::Popup::is_any_open(ctx)
        || craft::ui::runtime::overlay::family(ctx, egui::Id::unique("lightcraft.save-effect"))
            .into_iter()
            .filter_map(|id| craft::ui::runtime::overlay::entry(ctx, id))
            .any(|entry| !entry.exiting && matches!(entry.rank, craft::ui::runtime::overlay::Rank::Popup))
}
const NATIVE_FOCUS: &[(&str, &str, bool, bool)] = &[
    ("opened-name-autofocus", "field:effectName", false, true),
    ("group-return-does-not-save", "none", false, true),
    ("actual-create-exact-subset", "none", false, false),
    ("name-return-exact-all-fields", "none", false, false),
    ("empty-name-refused", "field:effectName", false, true),
    // a refused Create leaves the focus where it was: in the name being corrected (as for the empty name above)
    ("whitespace-name-refused", "field:effectName", false, true),
    // (the click on the checkbox and on Create leaves the focus in the name field, as above)
    ("no-fields-refused", "field:effectName", false, true),
    ("group-return-with-invalid-fields-does-not-confirm", "none", false, true),
    ("actual-cancel-typed-unsaved", "none", false, false),
    ("group-tab-to-picker", "button:effectGroupPick", false, true),
    ("picker-keyboard-open", "button:effectGroupPick", true, true),
    ("picker-forward-00", "popup:None", true, true),
    ("picker-forward-01", "popup:Existing Group", true, true),
    ("picker-forward-02", "effectInclude:temp", true, true),
    ("picker-forward-03", "effectInclude:tint", true, true),
    ("picker-forward-04", "effectInclude:exposure", true, true),
    ("picker-forward-05", "effectInclude:contrast", true, true),
    ("picker-forward-06", "effectInclude:highlights", true, true),
    ("picker-forward-07", "effectInclude:shadows", true, true),
    ("picker-forward-08", "effectInclude:whites", true, true),
    ("picker-forward-09", "effectInclude:blacks", true, true),
    ("picker-forward-10", "effectInclude:texture", true, true),
    ("picker-forward-11", "effectInclude:clarity", true, true),
    ("picker-forward-12", "effectInclude:dehaze", true, true),
    ("picker-forward-13", "effectInclude:hue", true, true),
    ("picker-forward-14", "effectInclude:saturation", true, true),
    ("picker-forward-15", "effectInclude:sharpness", true, true),
    ("picker-forward-16", "effectInclude:noise", true, true),
    ("picker-forward-17", "effectInclude:moire", true, true),
    ("picker-forward-18", "effectInclude:defringe", true, true),
    ("picker-forward-19", "effectInclude:lens_blur", true, true),
    ("picker-forward-20", "effectInclude:color_sat", true, true),
    ("picker-forward-21", "effectInclude:all", true, true),
    ("picker-forward-22", "effectInclude:none", true, true),
    ("picker-forward-23", "effectInclude:changed", true, true),
    ("picker-forward-24", "check:effectAmount", true, true),
    ("picker-forward-25", "check:effectBrush", true, true),
    ("picker-forward-26", "button:dialogCancel", true, true),
    ("picker-forward-27", "button:dialogOk", true, true),
    ("picker-forward-28", "field:effectName", true, true),
    ("picker-forward-29", "field:effectGroup", true, true),
    ("picker-forward-30", "button:effectGroupPick", true, true),
    ("picker-forward-31", "popup:None", true, true),
    ("picker-forward-32", "popup:Existing Group", true, true),
    ("picker-forward-33", "effectInclude:temp", true, true),
    ("picker-forward-34", "effectInclude:tint", true, true),
    ("picker-forward-35", "effectInclude:exposure", true, true),
    ("picker-reverse-00", "effectInclude:tint", true, true),
    ("picker-reverse-01", "effectInclude:temp", true, true),
    ("picker-reverse-02", "popup:Existing Group", true, true),
    ("picker-reverse-03", "popup:None", true, true),
    ("picker-reverse-04", "button:effectGroupPick", true, true),
    ("picker-reverse-05", "field:effectGroup", true, true),
    ("picker-reverse-06", "field:effectName", true, true),
    ("picker-reverse-07", "button:dialogOk", true, true),
    ("picker-reverse-08", "button:dialogCancel", true, true),
    ("picker-reverse-09", "check:effectBrush", true, true),
    ("picker-reverse-10", "check:effectAmount", true, true),
    ("picker-reverse-11", "effectInclude:changed", true, true),
    ("picker-reverse-12", "effectInclude:none", true, true),
    ("picker-reverse-13", "effectInclude:all", true, true),
    ("picker-reverse-14", "effectInclude:color_sat", true, true),
    ("picker-reverse-15", "effectInclude:lens_blur", true, true),
    ("picker-reverse-16", "effectInclude:defringe", true, true),
    ("picker-reverse-17", "effectInclude:moire", true, true),
    ("picker-reverse-18", "effectInclude:noise", true, true),
    ("picker-reverse-19", "effectInclude:sharpness", true, true),
    ("picker-reverse-20", "effectInclude:saturation", true, true),
    ("picker-reverse-21", "effectInclude:hue", true, true),
    ("picker-reverse-22", "effectInclude:dehaze", true, true),
    ("picker-reverse-23", "effectInclude:clarity", true, true),
    ("picker-reverse-24", "effectInclude:texture", true, true),
    ("picker-reverse-25", "effectInclude:blacks", true, true),
    ("picker-reverse-26", "effectInclude:whites", true, true),
    ("picker-reverse-27", "effectInclude:shadows", true, true),
    ("picker-reverse-28", "effectInclude:highlights", true, true),
    ("picker-reverse-29", "effectInclude:contrast", true, true),
    ("picker-reverse-30", "effectInclude:exposure", true, true),
    ("picker-reverse-31", "effectInclude:tint", true, true),
    ("picker-reverse-32", "effectInclude:temp", true, true),
    ("picker-reverse-33", "popup:Existing Group", true, true),
    ("picker-reverse-34", "popup:None", true, true),
    ("picker-reverse-35", "button:effectGroupPick", true, true),
    ("picker-escape-press", "none", false, true),
    ("picker-escape-release", "none", false, true),
    ("picker-escape-quiet", "none", false, true),
    ("clean-escape-parent", "none", false, false),
];

const NAME: &str = "field:effectName";
const GROUP: &str = "field:effectGroup";
const PICKER: &str = "button:effectGroupPick";
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
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    sdk_fields: Vec<(String, egui::Id)>,
    native_fields: Vec<(String, egui::Response)>,
    focused_response: Option<egui::Response>,
    painted: Vec<(String, Rect)>,
    document: Value,
    stored: Value,
    pending: Option<Value>,
    old_language: crate::i18n::Locale,
}
impl Review {
    fn new() -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut s = lightcraft_engine::Session::new();
        let id = s.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::Demo { scene: 0 }, "IMG_0042.JPG", "JPEG", 6000, 4000, "2026-01-14T05:58:48");
        s.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        s.execute("library.select", &json!({"ids":[id]})).unwrap();
        s.execute("mask.add", &json!({"kind":"radial"})).unwrap();
        for (name, group) in [("Warm", "Existing Group"), ("Plain", "")] {
            s.execute("mask.saveEffect", &json!({"name":name,"group":group,"fields":["clarity"]})).unwrap();
        }
        s.execute("mask.favoriteEffect", &json!({"name":"Warm","favorite":true})).unwrap();
        // Genuine command fixture data, before the immutable photo/history baseline. No scalar UI edits.
        s.execute("mask.adjust", &json!({"values":{"exposure":0.5,"shadows":30,"color_hue":210,"color_sat":15,"amount":70}})).unwrap();
        assert!(s.active_mask.is_some());
        let app = LightcraftApp::new(s, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut r = Self {
            app,
            ctx,
            tick: 0,
            rects: vec![],
            hits: vec![],
            sdk_fields: vec![],
            native_fields: vec![],
            focused_response: None,
            painted: vec![],
            document: Value::Null,
            stored: Value::Null,
            pending: None,
            old_language,
        };
        r.document = r.photo_document();
        r.stored = r.saved();
        r.open();
        r
    }
    fn photo_document(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),"commits":s.commits(),
            "selection":format!("{:?}",s.selection),"activeMask":s.active_mask,"toolEffects":s.mask_tool_effects,
            "settings":self.app.ui.settings,"uiTool":self.app.ui.tool,
            "brush":[json!(self.app.ui.brush_size),json!(self.app.ui.brush_feather),json!(self.app.ui.brush_flow),json!(self.app.ui.brush_auto_mask)],
            "lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,"printTemplates":s.print_templates})
    }
    fn saved(&self) -> Value {
        json!({"effects":self.app.session.mask_effects,"journal":self.app.session.journal})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(self.photo_document(), self.document, "Save dialogs must not alter photo/mask/Undo/redo/source/history");
        let actual = self.saved();
        assert!(
            actual == self.stored || self.pending.as_ref() == Some(&actual),
            "only the exact anticipated preset+journal transaction is allowed: {actual}"
        );
        assert_eq!(s.catalog.len(), 1);
        assert!(s.catalog.photos().all(|photo| matches!(photo.source, Source::Demo { .. })));
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.services.save_preset_file.is_none() && self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty() && self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(self.app.merge.preview_task.is_none() && self.app.merge.final_task.is_none());
        assert!(self.app.merge.preview.is_none() && self.app.merge.preview_pixels.is_none());
        assert!(s.activity.list().is_empty());
    }
    fn texts(shape: &egui::epaint::Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
        match shape {
            egui::epaint::Shape::Vec(items) => {
                for item in items {
                    Self::texts(item, clip, out);
                }
            }
            egui::epaint::Shape::Text(text) => {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                if clip.contains_rect(rect) {
                    out.push((text.galley.job.text.clone(), rect));
                }
            }
            _ => {}
        }
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        // Same ordinary viewport as the existing Save Effects tests, not a widened visibility workaround.
        let raw = HeadlessView::raw_input(vec2(1200.0, 900.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut last = None;
        let out = Output(ctx.run_ui(raw, |root| {
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("save-effect-naming-owner"), |ui| {
                crate::dialogs::show_root(app, ui, false);
            });
            // The registry contains a hover probe with a copy of TextField::show's returned
            // rectangle. Read the actual input owner in this SAME completed draw pass.
            let native_fields: Vec<_> = if SDK_NAMING {
                vec![]
            } else {
                [NAME, GROUP]
                    .into_iter()
                    .filter_map(|name| ctx.read_response(crate::text_field::id(name)).map(|response| (name.to_string(), response)))
                    .collect()
            };
            let focused_response = ctx.memory(|memory| memory.focused()).and_then(|id| ctx.read_response(id));
            last = Some((crate::widgets::take_registries(&ctx), recorded, native_fields, focused_response));
        }));
        let ((rects, hits), recorded, native_fields, focused_response) = last.unwrap();
        self.native_fields = native_fields;
        self.focused_response = focused_response;
        self.sdk_fields.clear();
        for field in [NAME, GROUP] {
            if let Some((_, hit)) = hits.iter().find(|(name, _)| name == field)
                && recorded
                    .iter()
                    .flat_map(|record| record.elements.iter())
                    .any(|element| element.id == hit.id && element.part.is_none() && element.component == "TextField")
            {
                self.sdk_fields.push((field.into(), hit.id));
            }
        }
        self.rects = rects;
        self.hits = hits;
        self.painted.clear();
        for shape in &out.0.shapes {
            Self::texts(&shape.shape, shape.clip_rect, &mut self.painted);
        }
        drop(out);
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn open(&mut self) {
        self.safety();
        self.app.run("dialog.saveMaskEffect", json!({})).unwrap();
        self.idle();
        assert!(matches!(self.app.ui.dialog, Some(Dialog::SaveEffect { .. })));
        assert_eq!(self.sdk_fields.len(), if SDK_NAMING { 2 } else { 0 });
        self.settle(NAME);
        self.focused_field(NAME);
        self.observe("opened-name-autofocus");
    }
    fn draft(&self) -> (String, String, Vec<String>, bool, bool) {
        let Some(Dialog::SaveEffect { name, group, fields, amount, brush }) = &self.app.ui.dialog else {
            panic!("Save Effect parent disappeared: {:?}", self.app.ui.dialog)
        };
        (name.clone(), group.clone(), fields.clone(), *amount, *brush)
    }
    fn rect(&self, id: &str) -> Rect {
        let rows: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(rows.len(), 1, "exactly one actual mounted {id}");
        rows[0].1
    }
    fn visible(&self, id: &str) -> Rect {
        let rect = self.rect(id);
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        let clipped = rect.intersect(hit.interact_rect).intersect(self.rect("dialog:window")).intersect(self.ctx.content_rect());
        assert!(clipped.is_finite() && clipped.is_positive(), "{id} has no real visible clipped region");
        clipped
    }
    fn input_rect(&self, id: &str) -> Rect {
        let rect = self.visible(id);
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        let mut target = rect.intersect(hit.interact_rect);
        if !SDK_NAMING && [NAME, GROUP].contains(&id) {
            let response = &self.native_fields.iter().find(|(name, _)| name == id).unwrap().1;
            assert_eq!(response.id, crate::text_field::id(id));
            assert_eq!(response.layer_id, hit.layer_id);
            target = target.intersect(response.rect).intersect(response.interact_rect);
        }
        assert!(target.is_finite() && target.is_positive(), "no common real clipped target for {id}");
        assert!(self.rect("dialog:window").contains_rect(target) && self.ctx.content_rect().contains_rect(target));
        target
    }
    fn point_inside(rect: Rect, point: egui::Pos2) -> bool {
        rect.min.x < point.x && point.x < rect.max.x && rect.min.y < point.y && point.y < rect.max.y
    }
    fn input_identity(&self, id: &str) -> (egui::Id, egui::LayerId, Option<egui::Id>) {
        let probe = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        let actual_field = if [NAME, GROUP].contains(&id) {
            if SDK_NAMING {
                Some(self.sdk_fields.iter().find(|(name, _)| name == id).unwrap().1)
            } else {
                Some(self.native_fields.iter().find(|(name, _)| name == id).unwrap().1.id)
            }
        } else {
            None
        };
        (probe.id, probe.layer_id, actual_field)
    }
    fn settle(&mut self, id: &str) -> egui::Pos2 {
        let mut candidate = None;
        let mut owner = None;
        let mut consecutive = 0;
        let mut trace = Vec::new();
        for attempt in 0..32 {
            self.frame(vec![]);
            let target = self.input_rect(id);
            let identity = self.input_identity(id);
            if candidate
                .is_some_and(|point| Self::point_inside(target, point) && owner == Some(identity) && self.ctx.layer_id_at(point) == Some(identity.1))
            {
                consecutive += 1;
            } else {
                let point = target.center();
                if Self::point_inside(target, point) && self.ctx.layer_id_at(point) == Some(identity.1) {
                    candidate = Some(point);
                    owner = Some(identity);
                    consecutive = 1;
                } else {
                    candidate = None;
                    owner = None;
                    consecutive = 0;
                }
            }
            trace.push(json!({"field":id,"attempt":attempt,"candidate":candidate.map(|point|[point.x,point.y]),
                "probe":format!("{:?}",identity.0),"layer":format!("{:?}",identity.1),"actualField":format!("{:?}",identity.2),
                "clippedTarget":[target.min.x,target.min.y,target.max.x,target.max.y],"consecutive":consecutive}));
            if consecutive >= 3 {
                println!(
                    "SAVE_EFFECT_READY {}",
                    json!({"field":id,"samples":trace,"invariant":"same interior point and real field/probe/layer, not stationary edges"})
                );
                return candidate.unwrap();
            }
        }
        println!("SAVE_EFFECT_READY_FAILED {}", json!({"field":id,"samples":trace}));
        panic!("Save Effect has no consistently owned clipped interior point: {id}");
    }
    fn click(&mut self, id: &str) {
        let at = self.settle(id);
        let expected_identity = self.input_identity(id);
        self.frame(vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            let target = self.input_rect(id);
            assert!(Self::point_inside(target, at));
            assert_eq!(self.input_identity(id), expected_identity);
            assert_eq!(self.ctx.layer_id_at(at), Some(expected_identity.1));
            let native = !SDK_NAMING && [NAME, GROUP].contains(&id);
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
            let (clicked, hovered) = self.ctx.interaction_snapshot(|snapshot| (snapshot.clicked, format!("{:?}", snapshot.hovered)));
            let native_down = if native {
                let response = &self.native_fields.iter().find(|(name, _)| name == id).unwrap().1;
                assert_eq!(Some(response.id), expected_identity.2);
                Some(response.is_pointer_button_down_on())
            } else {
                None
            };
            println!(
                "SAVE_EFFECT_INPUT {}",
                json!({"field":id,"phase":if pressed {"press"} else {"release"},"point":[at.x,at.y],
                "probe":format!("{:?}",expected_identity.0),"layer":format!("{:?}",expected_identity.1),
                "actualField":format!("{:?}",expected_identity.2),"clicked":format!("{clicked:?}"),"hovered":hovered,
                "nativePointerDown":native_down,"targetBeforeInput":[target.min.x,target.min.y,target.max.x,target.max.y]})
            );
            if native {
                if pressed {
                    assert_eq!(native_down, Some(true));
                } else {
                    assert_eq!(clicked, expected_identity.2, "the genuine native text input, never its hover probe, received the release click");
                }
            }
        }
        self.idle();
        if ["effectInclude:none", "effectInclude:temp"].contains(&id) {
            let hit = self.hits.iter().find(|(name, _)| name == id).map(|(_, hit)| hit);
            let response = hit.and_then(|hit| self.ctx.read_response(hit.id));
            println!(
                "SAVE_EFFECT_POINTER_STATE {}",
                json!({
                    "field":id,"phase":"after-original-click-and-quiet","dialogAfter":self.app.ui.dialog,
                    "focus":self.focus_name(),"rawFocus":format!("{:?}",self.ctx.memory(|memory|memory.focused())),
                    "sdkFocus":craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec|format!("{:?}/{:?}/{:?}",rec.stop.id,rec.stop.part,rec.stop.root)),
                    "actualHit":hit.map(|hit|format!("{:?}",hit.id)),
                    "actualResponse":response.map(|response|json!({"id":format!("{:?}",response.id),"hasFocus":response.has_focus(),"clicked":response.clicked()})),
                    "popupOpen":save_popup_open(&self.ctx),"saved":self.saved()
                })
            );
        }
    }
    fn focused_field(&self, id: &str) {
        if SDK_NAMING {
            let owner = self.sdk_fields.iter().find(|(name, _)| name == id).unwrap().1;
            let focus = craft::ui::runtime::focus::focused_rec(&self.ctx).unwrap();
            assert!(focus.stop.text);
            assert!(focus.stop.part.is_none(), "actual text stop, never the independent picker part");
            assert_eq!(focus.stop.id, owner);
            assert_eq!(self.ctx.memory(|memory| memory.focused()), Some(focus.stop.root));
        } else {
            // The native facade registers a hover probe, not its actual text focus ID. Read its public ID.
            let owner = crate::text_field::id(id);
            assert_eq!(self.ctx.memory(|memory| memory.focused()), Some(owner));
            let response = &self.native_fields.iter().find(|(name, _)| name == id).unwrap().1;
            assert_eq!(response.id, owner);
            let target = self.input_rect(id);
            println!(
                "SAVE_EFFECT_GEOMETRY {}",
                json!({"field": id, "actualOwner": format!("{:?}", response.id),
                "actualRect": format!("{:?}", response.rect), "actualClip": format!("{:?}", response.interact_rect),
                "probeOwner": format!("{:?}", self.hits.iter().find(|(name, _)| name == id).unwrap().1.id),
                "registeredRect": format!("{:?}", self.rect(id)), "sharedTarget": format!("{target:?}"), "sameDrawPass": true})
            );
        }
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.idle();
    }
    fn replace(&mut self, id: &str, text: &str) {
        self.click(id);
        self.focused_field(id);
        self.key(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
        self.frame(vec![Event::Text(text.into())]);
        self.idle();
        self.focused_field(id);
        let draft = self.draft();
        assert_eq!(if id == NAME { draft.0 } else { draft.1 }, text);
    }
    fn focus_name(&self) -> String {
        if let Some(focus) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            if focus.stop.part.is_none()
                && let Some((name, _)) = self.hits.iter().find(|(_, hit)| hit.id == focus.stop.id)
            {
                return name.clone();
            }
            return format!("unmapped-real-sdk-focus:{:?}/{:?}", focus.stop.id, focus.stop.part);
        }
        let Some(focused) = self.ctx.memory(|memory| memory.focused()) else { return "none".into() };
        for id in [NAME, GROUP] {
            if focused == crate::text_field::id(id) {
                return id.into();
            }
        }
        if let Some(response) = self.focused_response.as_ref().filter(|response| response.id == focused) {
            if let Some((name, _)) = self.rects.iter().find(|(_, rect)| *rect == response.rect) {
                return name.clone();
            }
            if save_popup_open(&self.ctx) {
                for label in ["None", "Existing Group"] {
                    if self.painted.iter().any(|(text, rect)| text == label && response.rect.contains_rect(*rect)) {
                        return format!("popup:{label}");
                    }
                }
            }
            return format!("unmapped-real-response:{focused:?}@{:?}", response.rect);
        }
        format!("unmapped-real-focus:{focused:?}")
    }
    fn observe(&self, stage: &str) {
        let family: Vec<_> = craft::ui::runtime::overlay::family(&self.ctx, egui::Id::unique("lightcraft.save-effect"))
            .into_iter()
            .filter_map(|id| craft::ui::runtime::overlay::entry(&self.ctx, id))
            .map(|entry| json!({"id":format!("{:?}",entry.id),"rank":format!("{:?}",entry.rank),"exiting":entry.exiting}))
            .collect();
        let bound_popup = self.ctx.data(|data| data.get_temp::<bool>(egui::Id::unique("lightcraft.save-effect.group-popup")));
        println!(
            "SAVE_EFFECT_FLOW {}",
            json!({"stage":stage,"dialogAfter":self.app.ui.dialog,"focus":self.focus_name(),
            "rawFocus":format!("{:?}",self.ctx.memory(|memory|memory.focused())),"popupOpen":save_popup_open(&self.ctx),"historicalPopupOpen":save_historical_popup_open(&self.ctx),"currentPopoverOutput":save_current_popup_output(&self.ctx),"sdkFamily":family,"boundGroupPopupOpen":bound_popup,"legacyPopupOpen":egui::Popup::is_any_open(&self.ctx),
            "sdkNaming":SDK_NAMING,"sdkDialogPopover":true,"sdkFocus":craft::ui::runtime::focus::focused_rec(&self.ctx).map(|rec| format!("{:?}/{:?}/{:?}",rec.stop.id,rec.stop.part,rec.stop.root)),"toast":self.app.ui.toast.as_ref().map(|toast|toast.0.as_str()),"saved":self.saved()})
        );
        if SDK_NAMING && matches!(&self.app.ui.dialog, Some(Dialog::SaveEffect { .. })) {
            let output = save_current_popup_output(&self.ctx).expect("same completed-frame real Popover output, never a stale closed snapshot");
            assert!(output.get("outputOpen").and_then(serde_json::Value::as_bool).is_some(), "real current output contains its public open result");
        }
        let expected = NATIVE_FOCUS.iter().find(|(native_stage, _, _, _)| *native_stage == stage).unwrap();
        assert_eq!(
            (self.focus_name(), save_popup_open(&self.ctx), self.app.ui.dialog.is_some()),
            (expected.1.to_string(), expected.2, expected.3),
            "first actual native focus/popup/parent divergence at {stage}"
        );
    }
    fn expect_save(&mut self, name: &str, group: &str, subset: bool) {
        assert!(self.pending.is_none());
        self.safety();
        let s = &self.app.session;
        let id = s.active().unwrap();
        let d = s.develop_of(id).unwrap();
        let mask = d.masks.iter().find(|mask| Some(mask.id) == s.active_mask).unwrap();
        let mut expected = MaskEffect { name: name.into(), group: group.into(), adjust: mask.adjust, ..Default::default() };
        let mut params =
            json!({"name":name,"group":group,"amount":subset,"fields":if subset {json!(["exposure","color_sat","color_hue"])} else {json!("all")}});
        if subset {
            expected.fields = vec!["exposure".into(), "color_hue".into(), "color_sat".into()];
            expected.with_amount = true;
            let brush = BrushSettings {
                size: f64::from(self.app.ui.brush_size),
                feather: f64::from(self.app.ui.brush_feather),
                flow: f64::from(self.app.ui.brush_flow),
                density: 100.0,
                auto_mask: self.app.ui.brush_auto_mask,
            };
            expected.brush = Some(brush);
            params["brush"] = json!({"size":self.app.ui.brush_size,"feather":self.app.ui.brush_feather,"flow":self.app.ui.brush_flow,"auto_mask":self.app.ui.brush_auto_mask});
        } else {
            expected.adjust.amount = 100.0;
        }
        let mut effects = s.mask_effects.clone();
        effects.push(expected);
        let mut journal = s.journal.clone();
        journal.push(("mask.saveEffect".into(), params));
        self.pending = Some(json!({"effects":effects,"journal":journal}));
    }
    fn saved_exactly(&mut self) {
        let expected = self.pending.take().unwrap();
        assert_eq!(self.saved(), expected);
        self.stored = expected;
        assert!(self.app.ui.dialog.is_none());
        self.safety();
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn save_effect_actual_create_and_name_return_preserve_exact_presets_photo_mask_and_history() {
    let mut r = Review::new();
    r.replace(NAME, "  Lift  ");
    r.key(Key::Tab, Modifiers::NONE);
    r.focused_field(GROUP);
    r.frame(vec![Event::Text("  Retouch  ".into())]);
    r.idle();
    r.key(Key::Enter, Modifiers::NONE);
    r.observe("group-return-does-not-save");
    assert_eq!((r.draft().0, r.draft().1), ("  Lift  ".into(), "  Retouch  ".into()));
    r.replace(GROUP, "  Portraits  ");
    r.click("effectInclude:none"); // genuine blur preserves Group; no Save.
    assert_eq!(r.draft().1, "  Portraits  ");
    r.click("effectInclude:exposure");
    r.click("effectInclude:color_sat");
    r.click("check:effectAmount");
    if !r.draft().4 {
        r.click("check:effectBrush");
    }
    assert_eq!((r.draft().2, r.draft().3, r.draft().4), (vec!["exposure".into(), "color_sat".into()], true, true));
    r.expect_save("Lift", "Portraits", true);
    r.click("button:dialogOk");
    r.saved_exactly();
    r.observe("actual-create-exact-subset");
    r.open();
    r.replace(GROUP, "  Return Group  ");
    r.key(Key::Enter, Modifiers::NONE);
    assert_eq!(r.draft().1, "  Return Group  ");
    r.replace(NAME, "  Return Lift  ");
    r.expect_save("Return Lift", "Return Group", false);
    r.key(Key::Enter, Modifiers::NONE);
    r.saved_exactly();
    r.observe("name-return-exact-all-fields");
}

#[test]
fn save_effect_actual_empty_whitespace_and_empty_fields_refuse_and_typed_cancel_never_saves() {
    let mut r = Review::new();
    r.click("button:dialogOk");
    r.observe("empty-name-refused");
    assert_eq!(r.app.ui.toast.as_ref().unwrap().0, "Name the preset");
    assert_eq!(r.draft().0, "");
    r.replace(NAME, "   ");
    r.click("button:dialogOk");
    r.observe("whitespace-name-refused");
    assert_eq!(r.app.ui.toast.as_ref().unwrap().0, "Name the preset");
    assert_eq!(r.draft().0, "   ");
    r.replace(NAME, "  Kept draft  ");
    r.click("effectInclude:none");
    r.click("button:dialogOk");
    r.observe("no-fields-refused");
    assert_eq!(r.app.ui.toast.as_ref().unwrap().0, "Choose at least one setting to include");
    assert!(r.draft().2.is_empty());
    r.replace(GROUP, "  Group draft  ");
    r.key(Key::Enter, Modifiers::NONE);
    r.observe("group-return-with-invalid-fields-does-not-confirm");
    assert_eq!(r.draft().1, "  Group draft  ");
    r.click("button:dialogCancel");
    r.observe("actual-cancel-typed-unsaved");
    assert!(r.app.ui.dialog.is_none());
    r.safety();
}

#[test]
fn save_effect_actual_group_picker_natural_tab_reverse_tab_and_escape_preserve_draft_and_presets() {
    let mut r = Review::new();
    r.replace(NAME, "Picker draft");
    r.replace(GROUP, "  Group draft  ");
    let unchanged = r.draft();
    r.key(Key::Tab, Modifiers::NONE);
    r.observe("group-tab-to-picker");
    assert_eq!(r.focus_name(), PICKER);
    r.key(Key::Enter, Modifiers::NONE);
    r.observe("picker-keyboard-open");
    assert!(save_popup_open(&r.ctx));
    assert_eq!(r.draft(), unchanged);
    let start = r.focus_name();
    // Baseline receipts freeze the real installed order, not a guessed SDK/native cycle.
    // Each step is ordinary physical input; no key bridge, dummy stop or focus request.
    for (direction, mods) in [("forward", Modifiers::NONE), ("reverse", Modifiers::SHIFT)] {
        for step in 0..36 {
            r.key(Key::Tab, mods);
            r.observe(&format!("picker-{direction}-{step:02}"));
            assert!(save_popup_open(&r.ctx));
            assert_eq!(r.draft(), unchanged);
        }
    }
    assert_eq!(r.focus_name(), start, "natural forward/reverse Tab must roundtrip the same actual stop");
    for (phase, pressed) in [("press", true), ("release", false)] {
        r.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        r.observe(&format!("picker-escape-{phase}"));
        assert_eq!(r.draft(), unchanged);
    }
    r.idle();
    r.observe("picker-escape-quiet");
    assert!(!save_popup_open(&r.ctx));
    assert_eq!(r.draft(), unchanged);
    r.key(Key::Escape, Modifiers::NONE);
    r.observe("clean-escape-parent");
    assert!(r.app.ui.dialog.is_none());
    r.safety();
}

#[test]
fn save_effect_actual_include_controls_physical_selection_bulk_and_flags_keep_draft_and_presets() {
    let mut r = Review::new();
    r.replace(NAME, "Include draft");
    r.replace(GROUP, "  Group draft  ");
    let initial = r.draft();
    let unchanged_store = r.saved();
    let all: Vec<String> = [
        "temp",
        "tint",
        "exposure",
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "texture",
        "clarity",
        "dehaze",
        "hue",
        "saturation",
        "sharpness",
        "noise",
        "moire",
        "defringe",
        "lens_blur",
        "color_sat",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    r.click("effectInclude:none");
    assert!(r.draft().2.is_empty());
    r.click("effectInclude:temp");
    // Keep the actual pointer receipt: the real release selected Temperature,
    // but CLICK|FOCUS promises a Tab stop, not pointer TAKE_FOCUS.
    assert_eq!(r.draft().2, vec!["temp".to_owned()]);
    let pointer_selected = r.draft();
    r.click(NAME);
    r.focused_field(NAME);
    for target in [GROUP, PICKER, "effectInclude:temp"] {
        r.key(Key::Tab, Modifiers::NONE);
        assert_eq!(r.focus_name(), target, "natural acquisition after the pointer value check");
        assert_eq!(r.draft(), pointer_selected);
    }
    r.key(Key::Space, Modifiers::NONE);
    assert!(r.draft().2.is_empty());
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().2, vec!["temp".to_owned()]);
    r.key(Key::Tab, Modifiers::NONE);
    assert_eq!(r.focus_name(), "effectInclude:tint");
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().2, vec!["temp".to_owned(), "tint".to_owned()]);
    r.key(Key::Tab, Modifiers::SHIFT);
    assert_eq!(r.focus_name(), "effectInclude:temp");
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().2, vec!["tint".to_owned()]);
    // Keep pointer checks for all three bulk commands, then independently reach
    // their actual SDK stops with ordinary Tab and activate each with Space.
    r.click("effectInclude:all");
    assert_eq!(r.draft().2, all);
    r.click("effectInclude:none");
    assert!(r.draft().2.is_empty());
    r.click("effectInclude:changed");
    let changed = vec!["exposure".to_owned(), "shadows".to_owned(), "color_sat".to_owned()];
    assert_eq!(r.draft().2, changed);
    let before_bulk_keys = r.draft();
    r.click(NAME);
    r.focused_field(NAME);
    let path = [GROUP.to_owned(), PICKER.to_owned()]
        .into_iter()
        .chain(all.iter().map(|key| format!("effectInclude:{key}")))
        .chain(["effectInclude:all".to_owned()]);
    for target in path {
        r.key(Key::Tab, Modifiers::NONE);
        assert_eq!(r.focus_name(), target, "every real stop is exposed during keyboard acquisition");
        assert_eq!(r.draft(), before_bulk_keys);
    }
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().2, all);
    r.key(Key::Tab, Modifiers::NONE);
    assert_eq!(r.focus_name(), "effectInclude:none");
    r.key(Key::Space, Modifiers::NONE);
    assert!(r.draft().2.is_empty());
    r.key(Key::Tab, Modifiers::NONE);
    assert_eq!(r.focus_name(), "effectInclude:changed");
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().2, changed);
    r.key(Key::Tab, Modifiers::NONE);
    assert_eq!(r.focus_name(), "check:effectAmount");
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().3, !initial.3);
    r.key(Key::Tab, Modifiers::NONE);
    assert_eq!(r.focus_name(), "check:effectBrush");
    r.key(Key::Space, Modifiers::NONE);
    assert_eq!(r.draft().4, !initial.4);
    r.click("check:effectAmount");
    assert_eq!(r.draft().3, initial.3);
    r.click("check:effectBrush");
    assert_eq!(r.draft().4, initial.4);
    let end = r.draft();
    assert_eq!((end.0, end.1, end.2, end.3, end.4), (initial.0, initial.1, changed, initial.3, initial.4));
    assert_eq!(r.saved(), unchanged_store);
    assert!(!save_popup_open(&r.ctx));
    r.safety();
    println!(
        "SAVE_EFFECT_INCLUDE_FLOW {}",
        json!({"stage":"physical-checklist-and-bulk-complete","draft":r.draft(),"saved":r.saved(),"focus":r.focus_name()})
    );
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.saved(), unchanged_store);
    r.safety();
}

// Observation-only continuation: this is not a replacement for the unchanged
// Group journey's press-frame assertion and cannot establish that journey passed.
fn save_effect_trace_frame_snapshot(r: &Review, stage: &str) {
    let family: Vec<_> = craft::ui::runtime::overlay::family(&r.ctx, egui::Id::unique("lightcraft.save-effect"))
        .into_iter()
        .filter_map(|id| craft::ui::runtime::overlay::entry(&r.ctx, id))
        .map(|entry| json!({"id":format!("{:?}",entry.id),"rank":format!("{:?}",entry.rank),"exiting":entry.exiting}))
        .collect();
    let output = r.ctx.data(|data| data.get_temp::<serde_json::Value>(egui::Id::unique("save-effect-popup-frame-provenance")));
    let bound = r.ctx.data(|data| data.get_temp::<bool>(egui::Id::unique("lightcraft.save-effect.group-popup")));
    let targets: Vec<_> = r.hits.iter().filter(|(name,_)|name.starts_with("popup:")).map(|(name,hit)| {
        let response = r.ctx.read_response(hit.id);
        let point = response.as_ref().map(|response|response.rect.center());
        json!({"alias":name,"actualId":format!("{:?}",hit.id),
            "response":response.as_ref().map(|response|json!({"id":format!("{:?}",response.id),"enabled":response.enabled(),"sense":format!("{:?}",response.sense)})),
            "cachedLayerAtInterior":point.map(|point|format!("{:?}",r.ctx.layer_id_at(point))),
            "lastPassShapesAtInterior":point.map(|point|format!("{:?}",craft::ui::runtime::pointer::shapes_at(&r.ctx,point)))})
    }).collect();
    println!(
        "SAVE_EFFECT_FRAME_PROVENANCE {}",
        json!({"diagnosticOnly":true,"stage":stage,"tick":r.tick,
        "frameAfterRun":r.ctx.cumulative_frame_nr(),"passAfterRun":r.ctx.cumulative_pass_nr(),"bound":bound,"popoverOutput":output,
        "familyAfterRun":family,"liveObserverAfterRun":save_popup_open(&r.ctx),"actualRegisteredOptions":targets,
        "focus":r.focus_name(),"rawFocus":format!("{:?}",r.ctx.memory(|memory|memory.focused())),"dialogAfter":r.app.ui.dialog,"saved":r.saved()})
    );
}

#[test]
fn save_effect_group_popup_close_frame_provenance_diagnostic() {
    let mut r = Review::new();
    r.replace(NAME, "Picker draft");
    r.replace(GROUP, "  Group draft  ");
    let unchanged = r.draft();
    let saved = r.saved();
    r.key(Key::Tab, Modifiers::NONE);
    r.observe("group-tab-to-picker");
    assert_eq!(r.focus_name(), PICKER);
    r.key(Key::Enter, Modifiers::NONE);
    r.observe("picker-keyboard-open");
    assert!(save_popup_open(&r.ctx));
    assert_eq!(r.draft(), unchanged);
    let start = r.focus_name();
    for (direction, mods) in [("forward", Modifiers::NONE), ("reverse", Modifiers::SHIFT)] {
        for step in 0..36 {
            r.key(Key::Tab, mods);
            r.observe(&format!("picker-{direction}-{step:02}"));
            assert!(save_popup_open(&r.ctx));
            assert_eq!(r.draft(), unchanged);
        }
    }
    assert_eq!(r.focus_name(), start);
    // Record a genuine currently visible option interior, within the parent and
    // ordinary viewport. This is not a fabricated/offscreen future target.
    let (former_label, former_id, former_point, former_rect) = ["popup:Existing Group", "popup:None"]
        .into_iter()
        .find_map(|label| {
            let hit = r.hits.iter().find(|(name, _)| name == label)?.1;
            let response = r.ctx.read_response(hit.id)?;
            let rect = response.rect.intersect(response.interact_rect).intersect(r.rect("dialog:window")).intersect(r.ctx.content_rect());
            let point = rect.center();
            (rect.is_finite()
                && rect.is_positive()
                && Review::point_inside(rect, point)
                && response.enabled()
                && r.ctx.layer_id_at(point) == Some(response.layer_id))
            .then_some((label, hit.id, point, rect))
        })
        .expect("a genuine visible option interior over the parent, not an offscreen point");
    println!(
        "SAVE_EFFECT_FORMER_OPTION {}",
        json!({"alias":former_label,"actualId":format!("{:?}",former_id),
        "point":[former_point.x,former_point.y],"visibleRect":[former_rect.min.x,former_rect.min.y,former_rect.max.x,former_rect.max.y]})
    );
    save_effect_trace_frame_snapshot(&r, "before-Escape");
    // The original native journey still asserts on this exact press frame and
    // remains failing if its observer differs. Only this separately named probe
    // continues without converting later-frame state into an acceptance result.
    for (phase, pressed) in [("press", true), ("release-next-frame", false)] {
        r.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE }]);
        save_effect_trace_frame_snapshot(&r, phase);
        assert_eq!(r.draft(), unchanged);
        assert_eq!(r.saved(), saved);
        r.safety();
    }
    for step in 0..3 {
        r.frame(vec![]);
        save_effect_trace_frame_snapshot(&r, &format!("original-quiet-frame-{step}"));
        assert_eq!(r.draft(), unchanged);
        assert_eq!(r.saved(), saved);
    }
    // A later real pointer event refreshes hit classification before the press.
    // Its old option coordinate may now legitimately address a parent control.
    r.frame(vec![Event::PointerMoved(former_point)]);
    save_effect_trace_frame_snapshot(&r, "former-option-pointer-move");
    assert_eq!(r.draft(), unchanged);
    let shapes = craft::ui::runtime::pointer::shapes_at(&r.ctx, former_point);
    let parent_targets: Vec<_> = r
        .hits
        .iter()
        .filter(|(name, hit)| {
            !name.starts_with("popup:")
                && (name == NAME
                    || name == GROUP
                    || name == PICKER
                    || name.starts_with("effectInclude:")
                    || name.starts_with("check:effect")
                    || name == "button:dialogCancel"
                    || name == "button:dialogOk")
                && shapes.iter().any(|(owner, _)| *owner == hit.id)
                && r.ctx.read_response(hit.id).is_some_and(|response| response.enabled())
        })
        .map(|(name, hit)| (name.clone(), hit.id))
        .collect();
    assert!(parent_targets.len() <= 1, "ambiguous real parent action; diagnostic cannot fabricate an expected winner: {parent_targets:?}");
    let mut expected = unchanged.clone();
    let all: Vec<String> = [
        "temp",
        "tint",
        "exposure",
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "texture",
        "clarity",
        "dehaze",
        "hue",
        "saturation",
        "sharpness",
        "noise",
        "moire",
        "defringe",
        "lens_blur",
        "color_sat",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some((name, _)) = parent_targets.first() {
        match name.as_str() {
            NAME | GROUP => {} // caret/focus is legitimate; there is no text event
            "effectInclude:all" => expected.2 = all,
            "effectInclude:none" => expected.2.clear(),
            "effectInclude:changed" => expected.2 = vec!["exposure".to_owned(), "shadows".to_owned(), "color_sat".to_owned()],
            "check:effectAmount" => expected.3 = !expected.3,
            "check:effectBrush" => expected.4 = !expected.4,
            action if action.starts_with("effectInclude:") => {
                let key = action.strip_prefix("effectInclude:").unwrap().to_owned();
                assert!(all.contains(&key), "independently known field key");
                if expected.2.contains(&key) {
                    expected.2.retain(|field| field != &key);
                } else {
                    expected.2.push(key);
                }
            }
            action => panic!(
                "former option lies over a legitimate open/close parent action {action}; this coordinate cannot establish the requested parent-survival probe"
            ),
        }
    }
    println!(
        "SAVE_EFFECT_FORMER_OPTION_EXPECTATION {}",
        json!({"formerAlias":former_label,"actualFormerId":format!("{:?}",former_id),
        "actualParentTargets":parent_targets.iter().map(|(name,id)|json!({"alias":name,"id":format!("{:?}",id)})).collect::<Vec<_>>(),
        "lastPassShapesBeforePress":format!("{:?}",shapes),"expectedDraftFromParentSemantics":expected})
    );
    for pressed in [true, false] {
        r.frame(vec![
            Event::PointerMoved(former_point),
            Event::PointerButton { pos: former_point, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
        let clicked = r.ctx.interaction_snapshot(|snapshot| snapshot.clicked);
        save_effect_trace_frame_snapshot(&r, if pressed { "former-option-real-press" } else { "former-option-real-release" });
        println!(
            "SAVE_EFFECT_FORMER_OPTION_INPUT {}",
            json!({"pressed":pressed,"clicked":format!("{:?}",clicked),"formerId":format!("{:?}",former_id)})
        );
        assert_ne!(clicked, Some(former_id), "dismissed option cannot receive the later genuine click");
        assert_eq!(r.saved(), saved);
        assert!(r.app.ui.dialog.is_some());
        r.safety();
    }
    assert_eq!(r.draft(), expected, "only the independently identified legitimate parent action may change the draft");
    assert_eq!(r.draft().0, unchanged.0);
    assert_eq!(r.draft().1, unchanged.1, "the old option must not select a group");
    assert_eq!(r.saved(), saved);
    let bound = r.ctx.data(|data| data.get_temp::<bool>(egui::Id::unique("lightcraft.save-effect.group-popup")));
    assert_eq!(bound, Some(false));
    r.safety();
}
