//! Every paste entry point reports refusal; a pending paste is not a completed paste.

use std::{sync::Arc, time::Duration};

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, SegMask};
use lightcraft_engine::Session;
use serde_json::{Value, json};

use crate::{LightcraftApp, Services, headless::Headless, state::Dialog};

const WAIT: Duration = Duration::from_secs(60);

struct Fixture {
    h: Headless,
    ids: [PhotoId; 2],
    before: [Arc<DevelopSettings>; 2],
    commits: u64,
}

impl Fixture {
    fn new() -> Self {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let source = ids[0];
        let targets = [ids[1], ids[2]];
        s.execute("library.select", &json!({"ids": [source.0]})).unwrap();
        let mut d = DevelopSettings::default();
        d.light.exposure = 0.7;
        d.masks.push(Mask {
            name: "Cached source sky".into(),
            on_photo: true,
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Prompt { text: "sky".into(), seg: Some(SegMask::from_logits(1, &[8.0])), detail: vec![], edge: 0.0 },
            }],
            ..Default::default()
        });
        s.set_develop(source, d, "Source").unwrap();
        s.execute("develop.copy", &json!({"groups": ["light", "masks"]})).unwrap();
        s.execute("library.select", &json!({"ids": [targets[0].0, targets[1].0], "active": targets[0].0})).unwrap();
        // No weights and no download: refusal must never install the cached source shape.
        s.segmenter.dir = None;
        assert!(s.segmenter.require_model().is_err());
        let before = targets.map(|id| s.develop_of(id).unwrap());
        let commits = s.commits();
        let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1800.0, 1200.0], 1.0);
        assert!(h.settle(WAIT));
        Self { h, ids: targets, before, commits }
    }

    fn unchanged(&self) {
        for (i, id) in self.ids.into_iter().enumerate() {
            assert_eq!(self.h.app.session.develop_of(id).unwrap(), self.before[i]);
        }
        assert_eq!(self.h.app.session.commits(), self.commits);
        assert!(self.h.app.session.activity.list().is_empty());
    }

    fn visible_error(&self) {
        let status = &self.h.app.ui.status;
        assert!(status.contains("SAM 3") || status.contains("AI masks"), "{status}");
        assert_eq!(self.h.app.ui.toast.as_ref().map(|t| &t.0), Some(status), "show the actual refusal, not success or an invisible status");
    }
}

fn request(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, WAIT);
    assert_eq!(r["ok"], true, "{method}: {r}");
}

fn open_dialog(h: &mut Headless) {
    h.app.run("dialog.pasteSettings", json!({})).unwrap();
    assert!(h.settle(WAIT));
}

#[test]
fn bottom_bar_reports_refusal_without_announcing_success() {
    let mut f = Fixture::new();
    request(&mut f.h, "ui.clickWidget", json!({"id": "button:copySettings"}));
    f.unchanged();
    f.visible_error();
}

#[test]
fn both_paste_commands_report_immediate_errors() {
    for command in ["develop.paste", "develop.pastePrevious"] {
        let mut f = Fixture::new();
        assert!(f.h.app.run(command, json!({})).is_err());
        f.unchanged();
        f.visible_error();
    }
}

#[test]
fn selected_settings_refusal_keeps_choices_and_allows_retry_without_masks() {
    let mut f = Fixture::new();
    open_dialog(&mut f.h);
    request(&mut f.h, "ui.clickWidget", json!({"id": "pasteGroup:light"}));
    request(&mut f.h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    f.unchanged();
    f.visible_error();
    assert!(matches!(&f.h.app.ui.dialog, Some(Dialog::PasteSettings { groups }) if groups == &["masks"]));
    // The user can correct the existing choices without opening a new dialog or copying again.
    request(&mut f.h, "ui.clickWidget", json!({"id": "pasteGroup:masks"}));
    request(&mut f.h, "ui.clickWidget", json!({"id": "pasteGroup:light"}));
    request(&mut f.h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert!(f.h.app.ui.dialog.is_none());
    assert_eq!(f.h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Settings pasted"));
    for id in f.ids {
        assert_eq!(f.h.app.session.develop_of(id).unwrap().light.exposure, 0.7);
    }
    assert_eq!(f.h.app.session.commits(), f.commits + 1);
    f.h.app.session.undo_step().unwrap();
    for (i, id) in f.ids.into_iter().enumerate() {
        assert_eq!(f.h.app.session.develop_of(id).unwrap(), f.before[i]);
    }
}

#[test]
fn control_confirmation_keeps_refused_paste_choices_too() {
    let mut f = Fixture::new();
    open_dialog(&mut f.h);
    let r = f.h.request("ui.dialog.confirm", json!({}), WAIT);
    assert_eq!(r["ok"], false, "{r}");
    f.unchanged();
    f.visible_error();
    assert!(matches!(&f.h.app.ui.dialog, Some(Dialog::PasteSettings { groups }) if groups == &["light", "masks"]));
}

#[test]
fn successful_synchronous_batch_pastes_announce_completion_from_both_commands() {
    for command in ["develop.paste", "develop.pastePrevious"] {
        let mut f = Fixture::new();
        let r = f.h.app.run(command, json!({"groups": ["light"]})).unwrap();
        assert_eq!(r["changed"], 2);
        assert_eq!(f.h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Settings pasted"));
        assert!(f.h.app.session.activity.list().is_empty());
    }
}

fn click_painted(h: &mut Headless, label: &str) {
    let p = h
        .view
        .painted_shapes()
        .into_iter()
        .find_map(|shape| match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label && text.pos.y < h.size.y - 80.0 => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{label} not painted: {:?}", h.painted_text()));
    request(h, "ui.click", json!({"x": p.x, "y": p.y}));
}

#[test]
fn both_context_menu_paste_actions_report_refusal_for_the_whole_selection() {
    for selected_settings in [false, true] {
        let mut f = Fixture::new();
        f.h.app.ui.view = crate::state::ViewMode::PhotoGrid;
        assert!(f.h.settle(WAIT));
        request(&mut f.h, "ui.clickWidget", json!({"id": format!("thumb:{}", f.ids[0].0), "button": "right"}));
        f.h.step();
        if selected_settings {
            click_painted(&mut f.h, "Paste Selected Settings…");
            assert!(f.h.settle(WAIT));
            request(&mut f.h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
            assert!(matches!(f.h.app.ui.dialog, Some(Dialog::PasteSettings { .. })));
        } else {
            click_painted(&mut f.h, "Paste Edit Settings");
        }
        f.unchanged();
        f.visible_error();
        assert_eq!(f.h.app.session.selection.ids, f.ids);
    }
}
