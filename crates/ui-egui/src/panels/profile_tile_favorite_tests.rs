//! Authored in-memory integration tests; execution requires later admitted Rust QA.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton};

const PROFILE: &str = "lc.cine.teal-amber";
const NAME: &str = "Teal & Amber";
#[derive(Clone, Copy)]
enum Pass {
    Real,
    EguiSizing,
    SdkSizing,
}

struct Frames {
    app: LightcraftApp,
    ctx: egui::Context,
    photo: PhotoId,
    now: f64,
    enabled: bool,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
    selections: Vec<(egui::Id, Option<(Option<bool>, Option<String>)>)>,
    visible_focus: Vec<(egui::Id, bool)>,
    intents: Vec<ProfileCellIntent>,
    shapes: Vec<egui::epaint::ClippedShape>,
    probe_guard: bool,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.install_framework(&ctx);
        let photo = app.session.visible_cloned()[0];
        app.run("library.select", json!({"ids": [photo.0], "active": photo.0})).unwrap();
        // Deliberately use a non-default amount to catch accidental star application/reset.
        app.run("develop.profile", json!({"id": "lc.color", "amount": 37})).unwrap();
        Self {
            app,
            ctx,
            photo,
            now: 0.0,
            enabled: true,
            rects: vec![],
            hits: vec![],
            elements: vec![],
            selections: vec![],
            visible_focus: vec![],
            intents: vec![],
            shapes: vec![],
            probe_guard: false,
        }
    }
    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        let _ = take_registries(&self.ctx);
        self.now += 0.02;
        let raw = HeadlessView::raw_input(vec2(460.0, 340.0), 1.0, self.now, events);
        let app = &mut self.app;
        let photo = self.photo;
        let enabled = self.enabled;
        let probe_guard = self.probe_guard;
        let mut elements = vec![];
        let mut selections = vec![];
        let mut visible_focus = vec![];
        let mut intents = vec![];
        let mut out = self.ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui| {
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("profile-tile-fixture"), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        for (index, tag) in ["", "fav"].into_iter().enumerate() {
                            let cell = Rect::from_min_size(pos2(20.0 + index as f32 * 200.0, 20.0), vec2(160.0, 144.0));
                            let thumb = Rect::from_min_size(cell.min, vec2(160.0, 120.0));
                            let star = Rect::from_center_size(pos2(thumb.right() - 13.0, thumb.top() + 13.0), vec2(20.0, 20.0));
                            let d = app.session.develop_of(photo).unwrap();
                            let fav = app.session.profile_favorites.iter().any(|id| id == PROFILE);
                            let controls = profile_cell_controls(ui, tag, PROFILE, NAME, cell, star, d.profile.id == PROFILE, fav);
                            visible_focus.push((controls.star.id, controls.star_focus_visible));
                            for response in [&controls.tile, &controls.star] {
                                // button-role toggles report their state as pressed (toggled), not selected
                                selections.push((
                                    response.id,
                                    ui.ctx().accesskit_node_builder(response.id, |n| {
                                        (
                                            n.toggled().and_then(|t| (t == egui::accesskit::Toggled::True).then_some(true)),
                                            n.description().map(str::to_owned),
                                        )
                                    }),
                                ));
                            }
                            if controls.intent != ProfileCellIntent::None {
                                intents.push(controls.intent);
                            }
                            profile_cell_apply(app, ui, PROFILE, controls.intent);
                        }
                        if probe_guard {
                            // Directly challenge the final side-effect guard in each speculative pass.
                            profile_cell_apply(app, ui, PROFILE, ProfileCellIntent::Favorite);
                            profile_cell_apply(app, ui, PROFILE, ProfileCellIntent::Apply);
                        }
                    });
                });
                if let Some(record) = record {
                    elements = record.elements.clone();
                }
            };
            match pass {
                Pass::Real => draw(ui),
                Pass::EguiSizing => {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible(), |ui| draw(ui));
                }
                Pass::SdkSizing => {
                    craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui));
                }
            }
        });
        out.textures_delta.clear();
        self.shapes = out.shapes;
        (self.rects, self.hits) = take_registries(&self.ctx);
        self.elements = elements;
        self.selections = selections;
        self.visible_focus = visible_focus;
        self.intents = intents;
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![], Pass::Real);
        }
    }
    fn target(&self, key: &str) -> (Rect, WidgetHit) {
        assert_eq!(self.rects.iter().filter(|(id, _)| id == key).count(), 1);
        assert_eq!(self.hits.iter().filter(|(id, _)| id == key).count(), 1);
        let r = self.rects.iter().find(|(id, _)| id == key).unwrap().1;
        let hit = self.hits.iter().find(|(id, _)| id == key).unwrap().1;
        let element =
            self.elements.iter().find(|e| e.id == hit.id && e.part.is_none()).expect("locator points at actual SDK component, not a hover probe");
        assert_eq!(element.component, "ProfileCellAction");
        assert_eq!(element.rect, r);
        assert_eq!(hit.interact_rect, r);
        if self.enabled {
            assert!(element.stop.is_some());
            assert!(element.facts.as_ref().is_some_and(|f| f.role == "Button" && f.interactive));
        } else {
            assert!(element.facts.as_ref().is_some_and(|f| f.role == "Button" && f.disabled));
        }
        (r, hit)
    }
    fn click(&mut self, key: &str) {
        let at = self.target(key).0.center();
        self.frame(vec![Event::PointerMoved(at)], Pass::Real);
        self.frame(vec![pointer(at, PointerButton::Primary, true)], Pass::Real);
        self.frame(vec![pointer(at, PointerButton::Primary, false)], Pass::Real);
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Modifiers::NONE };
        for pressed in [true, false] {
            self.frame(
                vec![Event::ModifiersChanged(modifiers), Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }],
                Pass::Real,
            );
        }
    }
    fn assert_state(&self, owner: egui::Id, on: bool) {
        let node = self.selections.iter().find(|(id, _)| *id == owner).expect("current SDK response").1.as_ref().expect("actual AX button node");
        // For Button the SDK reports the state as pressed (toggled); only On counts here. The explicit
        // localized description must remain available in *both* states.
        assert_eq!(node.0, on.then_some(true));
        assert_eq!(node.1.as_deref(), Some(crate::i18n::tr(if on { "On" } else { "Off" })));
    }
    fn settings(&self) -> serde_json::Value {
        serde_json::to_value(self.app.session.develop_of(self.photo).unwrap().as_ref()).unwrap()
    }
    fn snapshot(&self) -> serde_json::Value {
        json!({"settings":self.settings(), "undo":self.app.session.undo.len(), "redo":self.app.session.redo.len(),
               "revision":self.app.session.catalog.revision, "favorites":self.app.session.profile_favorites, "recent":self.app.session.profile_recent})
    }
}
fn pointer(pos: egui::Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}
fn tile(tag: &str) -> String {
    if tag.is_empty() { format!("profileCell:{PROFILE}") } else { format!("profileCell:fav:{PROFILE}") }
}
fn star(tag: &str) -> String {
    if tag.is_empty() { format!("profileStar:{PROFILE}") } else { format!("profileStar:fav:{PROFILE}") }
}

#[test]
fn profile_tile_favorite_four_real_owners_preserve_duplicate_locators_and_geometry() {
    let mut f = Frames::new();
    let before = f.snapshot();
    f.settle();
    assert_eq!(f.snapshot(), before, "idle controls never apply or favorite");
    assert_eq!(f.rects.len(), 4);
    assert_eq!(f.hits.len(), 4);
    let mut ids = std::collections::HashSet::new();
    for tag in ["", "fav"] {
        let (cell, t) = f.target(&tile(tag));
        let (s, sh) = f.target(&star(tag));
        assert!(ids.insert(t.id) && ids.insert(sh.id));
        assert_eq!(cell.size(), vec2(160.0, 144.0));
        assert_eq!(s.size(), vec2(20.0, 20.0));
        assert_eq!(s.center(), pos2(cell.right() - 13.0, cell.top() + 13.0));
        f.assert_state(t.id, false);
        f.assert_state(sh.id, false);
    }
    assert_eq!(f.elements.iter().filter(|e| e.component == "ProfileCellAction").count(), 4, "no proxy nodes or extra focus stops");
}

#[test]
fn profile_tile_favorite_star_pointer_changes_only_favorites_never_applies_or_adds_history() {
    let mut f = Frames::new();
    f.settle();
    let settings = f.settings();
    let undo = f.app.session.undo.len();
    let redo = f.app.session.redo.len();
    let revision = f.app.session.catalog.revision;
    let recent = f.app.session.profile_recent.clone();
    let main_id = f.target(&star("")).1.id;
    let duplicate_id = f.target(&star("fav")).1.id;
    f.click(&star(""));
    assert_eq!(f.intents, [ProfileCellIntent::Favorite], "star overlap must not activate tile too");
    assert!(f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
    f.settle();
    for owner in [main_id, duplicate_id] {
        f.assert_state(owner, true);
    }
    f.click(&star("fav"));
    assert_eq!(f.intents, [ProfileCellIntent::Favorite]);
    assert!(!f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
    f.settle();
    for owner in [main_id, duplicate_id] {
        f.assert_state(owner, false);
    }
    assert_eq!(f.settings(), settings);
    assert_eq!(f.app.session.undo.len(), undo);
    assert_eq!(f.app.session.redo.len(), redo);
    assert_eq!(f.app.session.catalog.revision, revision);
    assert_eq!(f.app.session.profile_recent, recent);
}

#[test]
fn profile_tile_favorite_tile_pointer_applies_once_at_100_and_applied_tile_is_noop() {
    let mut f = Frames::new();
    f.settle();
    let undo = f.app.session.undo.len();
    let favorites = f.app.session.profile_favorites.clone();
    f.click(&tile(""));
    assert_eq!(f.intents, [ProfileCellIntent::Apply]);
    f.settle();
    let d = f.app.session.develop_of(f.photo).unwrap();
    assert_eq!(d.profile.id, PROFILE);
    assert_eq!(d.profile.amount, 100.0);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert_eq!(f.app.session.profile_favorites, favorites);
    for tag in ["", "fav"] {
        f.assert_state(f.target(&tile(tag)).1.id, true);
        f.assert_state(f.target(&star(tag)).1.id, false);
    }
    // Already-applied is a no-op even when its amount is not the tile's 100% default.
    f.app.run("develop.profile", json!({"id":PROFILE,"amount":61})).unwrap();
    f.settle();
    let before = f.snapshot();
    f.click(&tile("fav"));
    assert!(f.intents.is_empty());
    assert_eq!(f.snapshot(), before);
    f.click(&star("fav"));
    assert_eq!(f.intents, [ProfileCellIntent::Favorite]);
    assert_eq!(f.settings(), before["settings"]);
    assert_eq!(f.app.session.undo.len(), before["undo"].as_u64().unwrap() as usize);
}

#[test]
fn profile_tile_favorite_natural_tab_enter_space_visit_separate_tile_and_star_owners() {
    let mut f = Frames::new();
    f.settle();
    let ids = [f.target(&tile("")).1.id, f.target(&star("")).1.id, f.target(&tile("fav")).1.id, f.target(&star("fav")).1.id];
    assert_eq!(f.ctx.memory(|m| m.focused()), None);
    f.key(Key::Tab, false);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[0]));
    f.key(Key::Enter, false);
    assert_eq!(f.app.session.develop_of(f.photo).unwrap().profile.id, PROFILE);
    let settings = f.settings();
    let undo = f.app.session.undo.len();
    f.key(Key::Tab, false);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[1]));
    f.key(Key::Space, false);
    assert!(f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
    assert_eq!(f.settings(), settings);
    assert_eq!(f.app.session.undo.len(), undo);
    f.key(Key::Tab, false);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[2]));
    f.key(Key::Space, false);
    assert_eq!(f.app.session.undo.len(), undo, "applied tile keyboard activation is no-op");
    f.key(Key::Tab, false);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[3]));
    f.key(Key::Enter, false);
    assert!(!f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
    f.key(Key::Tab, true);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[2]));
}

#[test]
fn profile_tile_favorite_each_keyboard_activation_key_works_on_each_owner_without_forced_focus() {
    for key in [Key::Enter, Key::Space] {
        for favorite in [false, true] {
            let mut f = Frames::new();
            f.settle();
            let undo = f.app.session.undo.len();
            f.key(Key::Tab, false);
            if favorite {
                f.key(Key::Tab, false);
            }
            let before = f.settings();
            f.key(key, false);
            if favorite {
                assert_eq!(f.settings(), before);
                assert_eq!(f.app.session.undo.len(), undo);
                assert!(f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
            } else {
                assert_eq!(f.app.session.develop_of(f.photo).unwrap().profile.amount, 100.0);
                assert_eq!(f.app.session.develop_of(f.photo).unwrap().profile.id, PROFILE);
                assert_eq!(f.app.session.undo.len(), undo + 1);
            }
        }
    }
}

#[test]
fn profile_tile_favorite_secondary_cancelled_and_disabled_owners_do_not_mutate() {
    for target in [tile(""), star("")] {
        let mut f = Frames::new();
        f.settle();
        let before = f.snapshot();
        let at = f.target(&target).0.center();
        f.frame(vec![Event::PointerMoved(at)], Pass::Real);
        for pressed in [true, false] {
            f.frame(vec![pointer(at, PointerButton::Secondary, pressed)], Pass::Real);
        }
        assert_eq!(f.snapshot(), before);
        f.frame(vec![pointer(at, PointerButton::Primary, true)], Pass::Real);
        let outside = pos2(430.0, 300.0);
        f.frame(vec![Event::PointerMoved(outside)], Pass::Real);
        f.frame(vec![pointer(outside, PointerButton::Primary, false)], Pass::Real);
        assert_eq!(f.snapshot(), before);
        f.enabled = false;
        f.settle();
        f.click(&target);
        assert_eq!(f.snapshot(), before);
    }
}

#[test]
fn profile_tile_favorite_independent_sizing_guards_reject_release_and_explicit_intents() {
    for pass in [Pass::EguiSizing, Pass::SdkSizing] {
        for target in [tile(""), star("")] {
            let mut f = Frames::new();
            f.settle();
            let before = f.snapshot();
            let at = f.target(&target).0.center();
            f.frame(vec![Event::PointerMoved(at)], Pass::Real);
            f.frame(vec![pointer(at, PointerButton::Primary, true)], Pass::Real);
            f.probe_guard = true;
            f.frame(vec![pointer(at, PointerButton::Primary, false)], pass);
            assert!(f.rects.is_empty() && f.hits.is_empty() && f.intents.is_empty());
            assert_eq!(f.snapshot(), before, "neither a release nor an explicit intent may mutate in sizing");
        }
    }
}

#[test]
fn profile_tile_favorite_idle_and_pointer_move_never_dispatch_or_clear_last_profile_hover() {
    let mut f = Frames::new();
    f.settle();
    let before = f.snapshot();
    let saved = (f.photo, PROFILE.to_string());
    f.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("last-profile-hover"), saved.clone()));
    for key in [tile(""), star(""), tile("fav"), star("fav")] {
        let at = f.target(&key).0.center();
        f.frame(vec![Event::PointerMoved(at)], Pass::Real);
        assert!(f.intents.is_empty());
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.ctx.data(|d| d.get_temp::<(PhotoId, String)>(egui::Id::unique("last-profile-hover"))), Some(saved.clone()));
    }
    f.click(&star(""));
    assert_eq!(f.ctx.data(|d| d.get_temp::<(PhotoId, String)>(egui::Id::unique("last-profile-hover"))), Some(saved));
    f.click(&tile(""));
    assert_eq!(f.ctx.data(|d| d.get_temp::<(PhotoId, String)>(egui::Id::unique("last-profile-hover"))), None);
}

#[test]
fn profile_tile_favorite_pointer_idle_stays_hidden_but_keyboard_star_focus_is_visible() {
    let mut f = Frames::new();
    f.settle();
    let star_id = f.target(&star("")).1.id;
    f.click(&star(""));
    f.click(&star(""));
    assert!(!f.app.session.profile_favorites.iter().any(|id| id == PROFILE));
    f.frame(vec![Event::PointerMoved(pos2(430.0, 300.0))], Pass::Real);
    assert_eq!(f.visible_focus.iter().find(|(id, _)| *id == star_id).unwrap().1, false);
    // Pointer clicks need not leave keyboard focus on their owner. Traverse naturally
    // from the observed post-pointer state; only the ordinary (not duplicate) star counts.
    let before = f.snapshot();
    let ids = [f.target(&tile("")).1.id, star_id, f.target(&tile("fav")).1.id, f.target(&star("fav")).1.id];
    let mut path = Vec::new();
    for _ in 0..ids.len() {
        f.key(Key::Tab, false);
        let id = f.ctx.memory(|m| m.focused()).expect("Tab focuses a real owner");
        assert!(ids.contains(&id) && !path.contains(&id), "unexpected Tab route: {path:?} then {id:?}");
        path.push(id);
        if id == star_id {
            break;
        }
    }
    assert_eq!(path.last(), Some(&star_id), "ordinary, unfavorited star must be reachable: {path:?}");
    assert!(f.visible_focus.iter().find(|(id, _)| *id == star_id).unwrap().1);
    f.key(Key::Tab, false);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(ids[2]));
    assert!(!f.visible_focus.iter().find(|(id, _)| *id == star_id).unwrap().1);
    assert_eq!(f.snapshot(), before);
}

// The real panel is drawn in every frame. Only delivery of thumbnail pixels is omitted:
// no file access, variant request, backend tick or renderer worker is needed for these controls.
fn browser_frame(f: &mut Frames, events: Vec<Event>) {
    f.now += 0.02;
    let raw = HeadlessView::raw_input(vec2(370.0, 7200.0), 1.0, f.now, events);
    let app = &mut f.app;
    app.hover_preview = None; // transient preview is rebuilt by the panel each frame
    let id = f.photo;
    let mut registry = Default::default();
    let mut out = f.ctx.run_ui(raw, |ui| {
        let _ = take_registries(ui.ctx());
        super::show_with_thumbnail(app, ui, id, |_, _, _, _, _| {});
        registry = take_registries(ui.ctx());
    });
    out.textures_delta.clear(); // headless frame has no texture consumer
    (f.rects, f.hits) = registry;
    assert_eq!(app.renderer.in_flight(), 0);
    assert_eq!(app.renderer.live_workers(), 0);
}
fn browser_click(f: &mut Frames, key: &str) {
    let hits = f.hits.iter().filter(|(k, _)| k == key).collect::<Vec<_>>();
    assert_eq!(hits.len(), 1, "one current owner for {key}");
    let at = hits[0].1.interact_rect.center();
    browser_frame(f, vec![Event::PointerMoved(at)]);
    for pressed in [true, false] {
        browser_frame(f, vec![pointer(at, PointerButton::Primary, pressed)]);
    }
    for _ in 0..2 {
        browser_frame(f, vec![]);
    }
}
#[test]
fn profile_browser_group_to_favorites_applies_once_without_star_edit() {
    let mut f = Frames::new();
    f.app.session.profile_favorites.clear(); // start with no Favorites group
    for _ in 0..3 {
        browser_frame(&mut f, vec![]);
    }
    let has = |f: &Frames, k: &str| f.hits.iter().any(|(key, _)| key == k);
    let start = f.snapshot();
    let baseline_journal = f.app.session.journal.len();
    let baseline_history = f.app.session.catalog.photo(f.photo).unwrap().history.len();
    assert!(!has(&f, "profileGroup:Favorites"));
    browser_click(&mut f, "profileGroup:Cinematic");
    assert!(!has(&f, &star("")));
    browser_click(&mut f, "profileGroup:Cinematic");
    assert!(has(&f, &tile("")));
    let at = f.hits.iter().find(|(k, _)| k == &tile("")).unwrap().1.interact_rect.center();
    browser_frame(&mut f, vec![Event::PointerMoved(at)]);
    assert_eq!(f.app.hover_preview.as_ref().unwrap().settings.profile.id, PROFILE);
    assert_eq!(f.snapshot(), start);
    browser_click(&mut f, &star(""));
    assert_eq!(f.app.session.profile_favorites.as_slice(), &[PROFILE.to_owned()]);
    assert_eq!(f.app.session.journal.len(), baseline_journal + 1);
    assert_eq!(f.app.session.journal.last(), Some(&("profile.favorite".into(), json!({"id":PROFILE}))));
    assert_eq!(f.settings(), start["settings"]);
    assert_eq!(f.app.session.catalog.photo(f.photo).unwrap().history.len(), baseline_history);
    let starred = f.snapshot();
    for k in ["undo", "redo", "revision", "recent"] {
        assert_eq!(starred[k], start[k]);
    }
    assert!(has(&f, &tile("fav")));
    browser_click(&mut f, "profileGroup:Cinematic");
    assert!(!has(&f, &tile("")));
    assert!(has(&f, &tile("fav")));
    browser_click(&mut f, "profileGroup:Favorites");
    assert!(!has(&f, &tile("fav")));
    browser_click(&mut f, "profileGroup:Favorites");
    browser_click(&mut f, &tile("fav"));
    let d = f.app.session.develop_of(f.photo).unwrap();
    assert_eq!(d.profile.id, PROFILE);
    assert_eq!(d.profile.amount, 100.0);
    assert_eq!(f.app.session.selection.active, Some(f.photo));
    assert_eq!(f.app.session.catalog.photo(f.photo).unwrap().history.len(), baseline_history + 1);
    assert_eq!(f.app.session.journal.len(), baseline_journal + 2);
    assert_eq!(f.app.session.journal.last(), Some(&("develop.profile".into(), json!({"id":PROFILE,"amount":100}))));
    let applied = f.snapshot();
    assert_eq!(applied["undo"].as_u64(), Some(start["undo"].as_u64().unwrap() + 1));
    assert!(applied["revision"].as_u64().unwrap() > start["revision"].as_u64().unwrap());
    assert_eq!(applied["favorites"], starred["favorites"]);
    browser_click(&mut f, &tile("fav"));
    assert_eq!(f.snapshot(), applied);
    assert_eq!(f.app.session.journal.len(), baseline_journal + 2);
    assert_eq!(f.app.session.catalog.photo(f.photo).unwrap().history.len(), baseline_history + 1);
}
