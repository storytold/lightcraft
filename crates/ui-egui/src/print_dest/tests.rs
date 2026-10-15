//! The Print dialog's destinations, end to end against in-process fake printers: the pages the Print module
//! composes reach a network printer in the format it lists, with the settings the dialog chose; nothing is
//! sent without the query and the tick; the person is told how it went.

use std::sync::Arc;
use std::time::Duration;

use craft_print::wsd::testing::{FakeConfig as Config, FakePrinter, MemTransport};
use craft_ui::printing::network::{self as net, JobView, Outcome};
use serde_json::json;

use super::*;
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);
const ADDRESS: &str = "127.0.0.1:5357/printer";

fn demo_with(printer: &Arc<FakePrinter>) -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.set_print_transport(Arc::new(MemTransport(Arc::clone(printer))));
    app.print_net.state.draft = net::Draft::for_address(ADDRESS);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn cmd(h: &mut Headless, id: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(T);
}

/// Ask the printer as the Query button does, running frames until it answered.
fn query(h: &mut Headless) {
    let worker = h.app.print_net.query_worker();
    assert!(h.app.print_net.state.start_query(&worker, true));
    assert!(h.step_until(T, |h| !h.app.print_net.state.draft.querying()));
}

fn open_network(h: &mut Headless) {
    cmd(h, "dialog.print", json!({}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Print)));
    h.app.print_out.dest = Destination::Network;
    h.settle(T);
}

fn one_photo_on_4x6(h: &mut Headless) {
    let ids: Vec<u64> = h.app.session.catalog.photos().take(1).map(|p| p.id.0).collect();
    h.app.session.selection.ids = ids.iter().map(|i| lightcraft_catalog::PhotoId(*i)).collect();
    cmd(
        h,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 1, "rows": 1}, "page": {"paper": "4x6"}}, "job": {"ppi": 72.0}}),
    );
}

#[test]
fn the_dialog_offers_the_three_destinations_and_the_network_one_waits_for_its_query_and_tick() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    cmd(&mut h, "dialog.print", json!({}));
    for id in ["button:printDestPdf", "button:printDestSystem", "button:printDestNetwork"] {
        assert!(h.app.widgets.iter().any(|(w, _)| w == id), "no {id}");
    }
    assert!(h.app.print_out.can_confirm(), "a PDF file needs nothing more");
    assert_eq!(h.app.print_out.confirm_label(), "Print to PDF…");
    let r = h.request("ui.clickWidget", json!({"id": "button:printDestNetwork"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert_eq!(h.app.print_out.dest, Destination::Network);
    assert_eq!(h.app.print_out.confirm_label(), "Print");
    assert!(!h.app.print_out.can_confirm(), "not queried, not confirmed");
    assert!(crate::panels::dialogs::keeps_open(&h.app, &Dialog::Print), "the dialog stays to show the job");
    // Print without the query and the tick sends nothing
    assert!(crate::print_dest::start(&mut h.app).is_err());
    assert_eq!(printer.jobs(), 0);
    query(&mut h);
    h.settle(T);
    assert!(!h.app.print_out.can_confirm(), "queried, not confirmed");
    h.app.print_net.state.draft.confirmed = true;
    h.settle(T);
    assert!(h.app.print_out.can_confirm());
    assert_eq!(printer.jobs(), 0, "reading the printer is all that happened");
}

#[test]
fn a_print_composed_by_the_dialog_reaches_a_raster_only_printer_as_pwg_at_its_resolution() {
    let printer = FakePrinter::new(Config { formats: vec!["image/pwg-raster"], resolutions: vec![(150, 150)], ..Config::default() });
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    open_network(&mut h);
    query(&mut h);
    h.app.print_net.state.draft.confirmed = true;
    h.settle(T);
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.app.print_out.busy(), "composing on a worker");
    assert!(h.step_until(T, |h| matches!(h.app.print_net.state.draft.job, JobView::Finished(_) | JobView::Failed(_))), "the job ended");
    match &h.app.print_net.state.draft.job {
        JobView::Finished(r) => assert_eq!(r.outcome, Outcome::Completed),
        other => panic!("{other:?}"),
    }
    let sent = printer.job(1).expect("a job").document.expect("a document");
    assert_eq!(&sent[..4], b"RaS2");
    let at = |o: usize| u32::from_be_bytes([sent[4 + o], sent[4 + o + 1], sent[4 + o + 2], sent[4 + o + 3]]);
    assert_eq!(at(276), 150, "the listed resolution");
    // a 4 x 6 in page at 150 dpi
    assert_eq!((at(372), at(376)), (600, 900));
    assert_eq!(at(452), 1, "one page");
    assert!(!h.app.print_net.state.draft.confirmed, "the tick was spent");
}

#[test]
fn a_printer_that_lists_pdf_gets_the_print_ready_pdf_with_the_dialogs_paper() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    open_network(&mut h);
    query(&mut h);
    h.app.print_net.state.draft.confirmed = true;
    h.settle(T);
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.step_until(T, |h| matches!(h.app.print_net.state.draft.job, JobView::Finished(_) | JobView::Failed(_))));
    let sent = printer.job(1).expect("a job").document.expect("a document");
    assert!(sent.starts_with(b"%PDF-"), "the Print module's PDF as it is");
    let text = String::from_utf8_lossy(&sent);
    assert!(text.contains("/MediaBox [0 0 288 432]"), "4 x 6 in in points");
}
