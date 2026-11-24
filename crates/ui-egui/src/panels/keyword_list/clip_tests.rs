//! Portable native/SDK geometry receipt; actual clipped hit ownership, not a simulated stop.
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: Value) {
    let answer = h.request(method, params.clone(), T);
    assert_eq!(answer["ok"], true, "{method} {params}: {answer}");
    h.settle(SETTLE);
}

fn rect(h: &Headless, widget: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(name, _)| name == widget).expect("published geometry").1
}

fn coords(rect: egui::Rect) -> Value {
    if rect.is_positive() { json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]) } else { Value::Null }
}

#[test]
fn scrolled_actual_keyword_hit_map_excludes_hidden_rows_and_controls() {
    let mut session = lightcraft_engine::Session::with_demo();
    for n in 0..50 {
        session.execute("keyword.create", &json!({"name": format!("Seam clip {n:02}")})).expect("fixture keyword");
    }
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 760.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    let target = rect(&h, "keywordRow:Seam clip 25");
    ask(&mut h, "ui.hoverWidget", json!({"id": "keywordList:topLevel"}));
    ask(&mut h, "ui.scroll", json!({"dx": 0.0, "dy": -(target.top() - 300.0)}));
    // read_response can expose the previous pass while the automation registry already
    // contains the current pass. Let scroll easing actually stop before comparing them.
    let mut previous = rect(&h, "keywordRow:Seam clip 25");
    let mut stable = 0;
    for _ in 0..120 {
        h.step();
        let current = rect(&h, "keywordRow:Seam clip 25");
        stable = if current == previous { stable + 1 } else { 0 };
        previous = current;
        if stable >= 3 {
            break;
        }
    }
    assert!(stable >= 3, "bounded scroll geometry did not settle");
    let tick = "keywordCheck:Seam clip 25";
    eprintln!(
        "DBGCLIP row={:?} tick={:?} hit={:?} top={:?} box={:?}",
        rect(&h, "keywordRow:Seam clip 25"),
        h.app.widgets.iter().find(|(n, _)| n == tick).map(|(_, r)| *r),
        h.app.widget_hits.iter().find(|(n, _)| n == tick).map(|(_, hit)| (hit.id, hit.interact_rect, hit.layer_id)),
        h.app.widgets.iter().find(|(n, _)| n == "keywordList:topLevel").map(|(_, r)| *r),
        h.app.widgets.iter().find(|(n, _)| n == "field:keyword").map(|(_, r)| *r),
    );
    let tick_hit = h.app.widget_hits.iter().find(|(name, _)| name == tick).expect("tick hit").1;
    let actual_tick = h
        .view
        .ctx
        .read_response(egui::Id::unique(("keyword-list-tick", "seam clip 25".to_owned())))
        .or_else(|| h.view.ctx.read_response(tick_hit.id))
        .expect("actual tick response");
    assert_eq!(actual_tick.rect, rect(&h, tick).expand(3.0), "receipt and actual control are from synchronized geometry");
    let mut map = std::collections::BTreeMap::new();
    let mut hidden = 0;
    let mut visible = 0;
    for n in 0..50 {
        let path = format!("Seam clip {n:02}");
        for prefix in ["keywordRow", "keywordShow", "keywordCheck"] {
            let widget = format!("{prefix}:{path}");
            let visual = rect(&h, &widget);
            let hit = h.app.widget_hits.iter().find(|(name, _)| name == &widget).expect("published real hit").1;
            if hit.interact_rect.is_positive() {
                assert!(hit.interact_rect.top() >= 44.0, "{widget}: no hit under the top bar: {:?}", hit.interact_rect);
                assert!(hit.interact_rect.bottom() <= 760.0, "{widget}: no hit outside the viewport");
                if prefix == "keywordRow" {
                    visible += 1;
                }
            } else if prefix == "keywordRow" {
                hidden += 1;
            }
            // Native checkbox aliases are 13-point hover probes over a 19-point real control.
            // Resolve the native control explicitly; the SDK alias is its actual response.
            let actual = if prefix == "keywordCheck" {
                h.view.ctx.read_response(egui::Id::unique(("keyword-list-tick", path.to_lowercase()))).or_else(|| h.view.ctx.read_response(hit.id))
            } else {
                h.view.ctx.read_response(hit.id)
            };
            map.insert(
                widget,
                json!({"visual": coords(visual), "automation_hit": coords(hit.interact_rect),
                "actual_hit": actual.as_ref().map(|response| coords(response.interact_rect)),
                "clickable": actual.as_ref().is_some_and(|response| response.sense.senses_click())}),
            );
        }
    }
    assert!(hidden > 0 && visible > 0, "the receipt covers clipped and shown rows");
    println!("KEYWORD_SCROLLED_HIT_MAP {}", serde_json::to_string(&map).unwrap());
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Seam clip 25"}));
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Seam clip 25"));
    let selected = h.app.ui.keyword_list_selected.clone();
    let before = h.app.session.undo.len();
    let refused = h.request("ui.clickWidget", json!({"id": "keywordRow:Seam clip 00"}), T);
    h.settle(SETTLE);
    assert_eq!(refused["ok"], false, "an entirely clipped row is not an automation target: {refused}");
    assert_eq!(h.app.ui.keyword_list_selected, selected, "refusal cannot click through the top bar");
    assert_eq!(h.app.session.undo.len(), before);
}
