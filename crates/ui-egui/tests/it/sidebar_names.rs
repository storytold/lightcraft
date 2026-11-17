//! What a screen reader is told about the sidebar's disclosure triangles: each names the row it
//! opens or closes, so a list of them is not "Expand, Expand, Expand".
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn sidebar() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.right = RightPanel::None;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    h
}

fn button_names(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).filter_map(|n| n.accesskit_node().label()).collect()
}

#[test]
fn a_disclosure_triangle_names_its_row() {
    let h = sidebar();
    let names = button_names(&h);
    // the demo library's album folder is open, with albums inside it; By Date is closed
    assert!(names.iter().any(|n| n == "Collapse Travel 2026"), "the open album folder's triangle: {names:?}");
    assert!(names.iter().any(|n| n == "Expand 2026"), "By Date's year: {names:?}");
    assert!(!names.iter().any(|n| n == "Expand" || n == "Collapse"), "no triangle is just a verb: {names:?}");
}

/// The same month in two years gets two different triangle names ("Expand March 2025", not two
/// "Expand March").
#[test]
fn a_month_triangle_names_its_year() {
    let mut session = lightcraft_engine::Session::with_demo();
    let ids: Vec<_> = session.catalog.photos().map(|p| p.id).take(2).collect();
    for (id, captured) in ids.into_iter().zip(["2025-03-05T10:00:00", "2026-03-05T10:00:00"]) {
        session.catalog.apply(lightcraft_catalog::Op::SetCaptured { id, captured: Some(captured.into()) }).expect("the capture time is set");
    }
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.right = RightPanel::None;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    for year in ["Expand 2025", "Expand 2026"] {
        h.get_by_label(year).click();
        h.run_steps(5);
    }
    let names = button_names(&h);
    assert!(names.iter().any(|n| n == "Expand March 2025"), "{names:?}");
    assert!(names.iter().any(|n| n == "Expand March 2026"), "{names:?}");
    assert!(!names.iter().any(|n| n == "Expand March"), "no month triangle lacks its year: {names:?}");
}

/// The Keyword List (right panel ▸ Keywords) is its own code path: its triangles name the keyword
/// they open, by its own name and not its whole path ("Expand Italy", not "Expand Trips|Italy").
#[test]
fn a_keyword_list_triangle_names_its_keyword() {
    let mut session = lightcraft_engine::Session::with_demo();
    for (name, parent) in [("Trips", None), ("Italy", Some("Trips")), ("Rome", Some("Trips|Italy"))] {
        let params = match parent {
            Some(p) => serde_json::json!({"name": name, "parent": p}),
            None => serde_json::json!({"name": name, "parent": null}),
        };
        session.execute("keyword.create", &params).expect("a keyword is created");
    }
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    app.ui.left_panel = false;
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.right = RightPanel::Keywords;
    // Trips is open, so Italy (which has Rome below it) shows, closed
    app.ui.keyword_list_open = vec!["trips".to_string()];
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    let names = button_names(&h);
    assert!(names.iter().any(|n| n == "Collapse Trips"), "the open keyword's triangle: {names:?}");
    assert!(names.iter().any(|n| n == "Expand Italy"), "the nested keyword's triangle: {names:?}");
    assert!(!names.iter().any(|n| n == "Expand Trips|Italy" || n == "Expand Italy|Rome"), "no triangle is named with a path: {names:?}");
}
