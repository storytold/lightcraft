//! Wave155: no chooser, file-backed photos, ICC reads, or filesystem writes.
use super::*;
use crate::widgets::WidgetHit;
use lightcraft_engine::proof::ProofIntent;

const PERCEPTUAL: &str = "button:proofIntent-perceptual";
const RELATIVE: &str = "button:proofIntent-relative";
const PAPER: &str = "button:proofPaperInk";
const COPY: &str = "button:createProofCopy";
type Registries = (Vec<(String, Rect)>, Vec<(String, WidgetHit)>);

fn fixture() -> (egui::Context, LightcraftApp, PhotoId) {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new();
    let mut ids = Vec::new();
    for scene in 0..2 {
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::Demo { scene }, "memory.jpg", "JPEG", 8, 8, "2026-01-01T00:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        ids.push(id);
    }
    session.execute("library.select", &json!({"ids": [ids[0].0, ids[1].0], "active": ids[1].0})).unwrap();
    let mut app = LightcraftApp::new(session, crate::Services::default());
    app.ui.soft_proof = true;
    let ctx = egui::Context::default();
    app.install_framework(&ctx);
    crate::theme::apply(&ctx);
    (ctx, app, ids[1])
}

fn frame(ctx: &egui::Context, events: Vec<egui::Event>, mut body: impl FnMut(&mut egui::Ui)) -> Registries {
    let time = ctx.input(|i| i.time) + 1.0 / 60.0;
    let input = egui::RawInput {
        time: Some(time),
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 640.0))),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |root_ui| {
        crate::widgets::take_registries(root_ui.ctx());
        egui::CentralPanel::default().show(root_ui, |ui| body(ui));
    });
    // Headless control tests do not upload atlas textures.
    output.textures_delta.clear();
    crate::widgets::take_registries(ctx)
}

fn at(reg: &Registries, name: &str) -> egui::Pos2 {
    reg.0.iter().find(|(id, _)| id == name).unwrap_or_else(|| panic!("missing {name}")).1.center()
}

fn click(ctx: &egui::Context, pos: egui::Pos2, mut body: impl FnMut(&mut egui::Ui)) {
    frame(ctx, vec![egui::Event::PointerMoved(pos)], &mut body);
    for pressed in [true, false] {
        frame(
            ctx,
            vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }],
            &mut body,
        );
    }
    frame(ctx, Vec::new(), body);
}

#[test]
fn wave155_production_device_gate_and_real_sdk_hit_owners() {
    let (ctx, mut app, id) = fixture();
    for device in [false, true, false] {
        // This is UI metadata only. No profile command is executed, so no ICC is read.
        app.ui.proof_profile = device.then(|| (String::new(), "Memory Printer".into()));
        let mut observed = None;
        let mut reg = (Vec::new(), Vec::new());
        for _ in 0..4 {
            reg = frame(&ctx, Vec::new(), |ui| {
                let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("wave155-proof"), |ui| soft_proofing(&mut app, ui, id));
                observed = record;
            });
        }
        let record = observed.expect("SDK region recorded");
        for (name, label, role) in [
            (COPY, "Create Proof Copy", "Button"),
            (PERCEPTUAL, "Perceptual", "Button"),
            (RELATIVE, "Relative", "Button"),
            (PAPER, "Simulate Paper & Ink", "CheckBox"),
        ] {
            let expected = device || name == COPY;
            assert_eq!(reg.0.iter().filter(|(id, _)| id == name).count(), usize::from(expected), "{name} device={device}");
            if !expected {
                continue;
            }
            let element = record
                .elements
                .iter()
                .find(|e| e.facts.as_ref().is_some_and(|f| f.name.as_deref() == Some(label) && f.role == role))
                .unwrap_or_else(|| panic!("missing SDK {role} {label}"));
            let hit = reg.1.iter().find(|(id, _)| id == name).unwrap().1;
            assert_eq!(hit.id, element.id, "{name}: automation must own the real component, not a hover probe");
            assert!(hit.interact_rect.contains(at(&reg, name)), "{name}: click center outside hit");
        }
    }
}

#[test]
fn wave155_local_options_select_both_intents_and_toggle_paper_twice() {
    let (ctx, _app, _) = fixture();
    let (mut intent, mut paper) = (ProofIntent::Perceptual, false);
    let mut reg = (Vec::new(), Vec::new());
    for _ in 0..4 {
        reg = frame(&ctx, Vec::new(), |ui| proof_options(ui, &mut intent, &mut paper));
    }
    for (name, expected_intent, expected_paper) in [
        (RELATIVE, ProofIntent::Relative, false),
        (PERCEPTUAL, ProofIntent::Perceptual, false),
        (PERCEPTUAL, ProofIntent::Perceptual, false),
        (PAPER, ProofIntent::Perceptual, true),
        (PAPER, ProofIntent::Perceptual, false),
    ] {
        click(&ctx, at(&reg, name), |ui| proof_options(ui, &mut intent, &mut paper));
        assert_eq!((intent, paper), (expected_intent, expected_paper), "{name}");
        reg = frame(&ctx, Vec::new(), |ui| proof_options(ui, &mut intent, &mut paper));
    }
}

#[test]
fn wave155_migrated_controls_publish_and_commit_nothing_in_both_sizing_modes() {
    let (ctx, mut app, id) = fixture();
    app.ui.proof_profile = Some((String::new(), "Memory Printer".into()));
    let count = app.session.catalog.photos().count();
    let (mut intent, mut paper) = (ProofIntent::Relative, true);
    frame(&ctx, Vec::new(), |ui| {
        for native in [true, false] {
            crate::widgets::take_registries(&ctx);
            let mut body = |ui: &mut egui::Ui| {
                proof_options(ui, &mut intent, &mut paper);
                assert!(!proof_button(ui, COPY, "Create Proof Copy", false));
            };
            if native {
                let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                assert!(child.is_sizing_pass());
                body(&mut child);
            } else {
                craft::ui::runtime::measure::run(&ctx, || body(ui));
            }
            let (rects, hits) = crate::widgets::take_registries(&ctx);
            assert!(rects.is_empty() && hits.is_empty(), "sizing registered automation, native={native}");
            assert_eq!((intent, paper), (ProofIntent::Relative, true));
        }
        // Exercise the real strip too, ignoring its intentionally unported profile/header probes.
        for native in [true, false] {
            crate::widgets::take_registries(&ctx);
            if native {
                let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                soft_proofing(&mut app, &mut child, id);
            } else {
                craft::ui::runtime::measure::run(&ctx, || soft_proofing(&mut app, ui, id));
            }
            let (rects, hits) = crate::widgets::take_registries(&ctx);
            for name in [PERCEPTUAL, RELATIVE, PAPER, COPY] {
                assert!(!rects.iter().any(|(n, _)| n == name));
                assert!(!hits.iter().any(|(n, _)| n == name));
            }
        }
    });
    assert_eq!(app.session.catalog.photos().count(), count);
    assert_eq!((app.ui.proof_intent, app.ui.proof_paper_ink), (ProofIntent::Perceptual, false));
    assert!(app.ui.toast.is_none());
}

#[test]
fn wave155_real_copy_uses_displayed_photo_profile_name_and_success_toast() {
    for device in [false, true] {
        let (ctx, mut app, id) = fixture();
        app.ui.proof_profile = device.then(|| (String::new(), "Memory Printer".into()));
        let source = app.session.catalog.photo(id).unwrap().clone();
        let count = app.session.catalog.photos().count();
        let mut reg = (Vec::new(), Vec::new());
        for _ in 0..4 {
            reg = frame(&ctx, Vec::new(), |ui| soft_proofing(&mut app, ui, id));
        }
        click(&ctx, at(&reg, COPY), |ui| soft_proofing(&mut app, ui, id));
        assert_eq!(app.session.catalog.photos().count(), count + 1, "one activation, one copy");
        let copy = app.session.catalog.photos().find(|p| p.copy_of == Some(id)).expect("copy of displayed photo");
        assert_eq!(copy.copy_name.as_deref(), Some(if device { "Proof Copy (Memory Printer)" } else { "Proof Copy (sRGB)" }));
        assert_eq!(copy.source, source.source);
        assert_eq!(copy.develop, source.develop);
        assert_eq!(app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Proof copy created"));
    }
}

#[test]
fn wave155_disabled_sdk_controls_ignore_pointer_activation() {
    let (ctx, _app, _) = fixture();
    let (mut intent, mut paper) = (ProofIntent::Perceptual, false);
    let mut copied = false;
    let mut draw = |ui: &mut egui::Ui| {
        ui.add_enabled_ui(false, |ui| {
            proof_options(ui, &mut intent, &mut paper);
            copied |= proof_button(ui, COPY, "Create Proof Copy", false);
        });
    };
    let mut reg = (Vec::new(), Vec::new());
    for _ in 0..4 {
        reg = frame(&ctx, Vec::new(), &mut draw);
    }
    for name in [RELATIVE, PAPER, COPY] {
        click(&ctx, at(&reg, name), &mut draw);
        reg = frame(&ctx, Vec::new(), &mut draw);
    }
    assert_eq!((intent, paper, copied), (ProofIntent::Perceptual, false, false));
}

#[test]
fn proof_copy_after_selection_change_undo_restores_both_original_photos() {
    let (ctx, mut app, previous) = fixture();
    let next = *app.session.selection.ids.iter().find(|id| **id != previous).unwrap();
    for _ in 0..4 {
        frame(&ctx, vec![], |ui| soft_proofing(&mut app, ui, previous));
    }
    app.session.execute("library.select", &json!({"ids": [next.0], "active": next.0})).unwrap();
    let originals = [previous, next].map(|id| json!(app.session.catalog.photo(id).unwrap()));
    let before_count = app.session.catalog.photos().count();
    let before_undo = app.session.undo.len();
    let reg = frame(&ctx, vec![], |ui| soft_proofing(&mut app, ui, next));
    click(&ctx, at(&reg, COPY), |ui| soft_proofing(&mut app, ui, next));
    assert_eq!(app.session.catalog.photos().count(), before_count + 1);
    let copy = app.session.catalog.photos().find(|p| p.copy_of == Some(next)).expect("copy of current photo").id;
    assert!(!app.session.catalog.photos().any(|p| p.copy_of == Some(previous)));
    assert_eq!(app.session.undo.len(), before_undo + 1);
    app.run("edit.undo", json!({})).unwrap();
    assert!(app.session.catalog.photo(copy).is_none());
    assert_eq!(app.session.catalog.photos().count(), before_count);
    assert_eq!([previous, next].map(|id| json!(app.session.catalog.photo(id).unwrap())), originals);
}
