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

/// The in-process spooler every test dialog gets: four queues, one the default. The real one is never read.
pub(super) fn fake_printers() -> Vec<spool::Printer> {
    ["Copy Room Laser", "Front Desk", "Slide Printer", "Photo Lab"]
        .into_iter()
        .map(|name| spool::Printer { name: name.to_owned(), description: String::new(), default: name == "Front Desk", two_sided: None })
        .collect()
}

/// What the fake spooler was handed: the job and the PDF.
static SUBMITTED: std::sync::Mutex<Vec<(spool::Job, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());

pub(super) fn fake_submit(pdf: &[u8], job: &spool::Job) -> Result<String, String> {
    SUBMITTED.lock().map_err(|e| e.to_string())?.push((job.clone(), pdf.to_vec()));
    Ok("job-1".into())
}

/// The job the fake spooler got for `printer` (each test prints to a queue of its own).
fn submitted_to(printer: &str) -> Option<(spool::Job, Vec<u8>)> {
    SUBMITTED.lock().ok()?.iter().find(|(j, _)| j.printer.as_deref() == Some(printer)).cloned()
}
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

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(T);
}

#[test]
fn one_printer_list_holds_the_pdf_file_the_system_printers_and_the_network_entry_which_waits_for_its_query_and_tick() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    cmd(&mut h, "dialog.print", json!({}));
    assert!(h.step_until(T, |h| h.app.print_out.printers.len() == 4), "the queues were listed");
    h.settle(T);
    // one dropdown, no switch between system and network
    assert!(h.app.widgets.iter().any(|(w, _)| w == "combo:printDestination"));
    for gone in ["button:printDestPdf", "button:printDestSystem", "button:printDestNetwork"] {
        assert!(!h.app.widgets.iter().any(|(w, _)| w == gone), "{gone} is gone");
    }
    let names: Vec<String> = h.app.print_out.entries().iter().map(entry_label).collect();
    assert_eq!(
        names,
        ["PDF file", "Front Desk (default)", "Copy Room Laser", "Slide Printer", "Photo Lab", "Network printer…"],
        "the PDF file, the system printers (default first), then the network entry"
    );
    assert!(h.app.print_out.can_confirm(), "a PDF file needs nothing more");
    assert_eq!(h.app.print_out.confirm_label(), "Print to PDF…");
    // a system printer is one click away in the same list
    click(&mut h, "combo:printDestination");
    click(&mut h, "item:printDestination-2");
    assert_eq!(h.app.print_out.dest, Destination::System("Copy Room Laser".into()));
    assert_eq!(h.app.print_out.confirm_label(), "Print");
    assert!(h.app.print_out.block.is_none(), "no network section for a system printer");
    // the network entry reveals the address, query and confirmation
    click(&mut h, "combo:printDestination");
    click(&mut h, "item:printDestination-5");
    assert_eq!(h.app.print_out.dest, Destination::Network);
    assert!(h.app.print_out.block.is_some(), "the network section was drawn, and blocks Print");
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
fn a_queried_network_printer_joins_the_list_by_its_reported_name_and_can_be_chosen_again() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    open_network(&mut h);
    query(&mut h);
    h.settle(T);
    let name = h.app.print_net.state.draft.printer().expect("queried").name.clone();
    let entries = h.app.print_out.entries();
    assert_eq!(entries.last(), Some(&Entry::Remembered { name, address: ADDRESS.into() }));
    assert_eq!(h.app.print_out.remembered.len(), 1, "once, however many frames drew it");
    // away to a system printer and back: the address does not have to be typed again
    choose(&mut h.app, &Entry::System { name: "Photo Lab".into(), default: false });
    h.app.print_net.state.draft = net::Draft::default();
    h.settle(T);
    choose(&mut h.app, &entries.last().cloned().expect("entry"));
    h.settle(T);
    assert_eq!(h.app.print_out.dest, Destination::Network);
    assert_eq!(h.app.print_net.state.draft.url, ADDRESS);
    assert!(h.app.print_net.state.draft.printer().is_none(), "choosing it contacts nothing: Query printer asks");
    assert!(!h.app.print_out.can_confirm());
    // "Network printer…" is the blank form again
    choose(&mut h.app, &Entry::Network);
    assert_eq!(h.app.print_net.state.draft.url, "");
}

#[test]
fn grayscale_is_the_word_everywhere_and_the_preview_and_the_pages_are_gray() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    let labels = crate::network_glue::labels();
    assert_eq!(labels.color_monochrome, "Grayscale");
    assert!(labels.unsupported_color.contains("grayscale"), "{}", labels.unsupported_color);
    h.app.renderer.keep_pixels = true;
    cmd(&mut h, "dialog.print", json!({}));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:printGrayscale"));
    let id = h.app.session.selection.ids[0];
    // in colour first: the library's thumbnail is drawn and there is no gray one
    assert!(h.step_until(T, |h| h.app.renderer.thumb(id).is_some()), "the colour thumbnail rendered");
    assert!(h.app.renderer.print_gray(id).is_none());
    let is_gray = |t: &crate::render::Tex| t.pixels.as_ref().is_some_and(|p| p.pixels.iter().all(|c| c.r() == c.g() && c.g() == c.b()));
    let has_colour = |t: &crate::render::Tex| t.pixels.as_ref().is_some_and(|p| p.pixels.iter().any(|c| c.r() != c.b()));
    assert!(h.app.renderer.thumb(id).is_some_and(has_colour), "the demo photo has colour");
    click(&mut h, "check:printGrayscale");
    assert!(h.app.session.print.job.grayscale, "the box sets the job's grayscale");
    assert!(h.step_until(T, |h| h.app.renderer.print_gray(id).is_some()), "the gray preview rendered");
    assert!(h.app.renderer.print_gray(id).is_some_and(is_gray), "every preview pixel is gray");
    assert!(h.app.renderer.thumb(id).is_some_and(has_colour), "the colour thumbnail is still cached next to it, for the toggle back");
    // closing the dialog with Grayscale still on releases the gray previews (they belong to the dialog)
    h.app.ui.dialog = None;
    assert!(h.step_until(T, |h| h.app.renderer.print_gray(id).is_none()), "the gray previews are released when the dialog closes");
    assert!(h.app.session.print.job.grayscale, "the setting itself stays");
    cmd(&mut h, "dialog.print", json!({}));
    assert!(h.step_until(T, |h| h.app.renderer.print_gray(id).is_some()), "reopened, the gray preview renders again");
    // back to colour: the colour thumbnail is drawn at once and the gray ones are dropped
    click(&mut h, "check:printGrayscale");
    assert!(!h.app.session.print.job.grayscale);
    assert!(h.app.renderer.print_gray(id).is_none());
    // a network printer's Color setting set to Grayscale makes the print gray as well
    open_network(&mut h);
    assert!(!grayscale_wanted(&h.app));
    h.app.print_net.state.draft.color = Some(net::ColorMode::Monochrome);
    assert!(grayscale_wanted(&h.app), "the printer's Grayscale choice composes gray pages and a gray preview");
}

#[test]
fn a_gray_print_to_a_system_printer_asks_the_driver_for_monochrome_too() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    cmd(&mut h, "print.set", json!({"job": {"grayscale": true}}));
    cmd(&mut h, "dialog.print", json!({}));
    choose(&mut h.app, &Entry::System { name: "Photo Lab".into(), default: false });
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.step_until(T, |_| submitted_to("Photo Lab").is_some()), "handed to the spooler");
    let (job, pdf) = submitted_to("Photo Lab").expect("job");
    assert!(job.grayscale);
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn scaling_is_exact_for_a_chosen_paper_and_shrink_to_fit_for_a_size_no_queue_lists() {
    use spool::Scaling::{Exact, ShrinkToFit};
    for paper in ["a4", "letter", "4x6", "13x19"] {
        assert_eq!(scaling_for(paper, false), Exact, "{paper} was chosen from the papers: demanded exactly");
        assert_eq!(scaling_for(paper, true), ShrinkToFit, "the box asks for fitting");
    }
    assert_eq!(scaling_for("custom", false), ShrinkToFit, "a custom size is on the queue's own paper");
    assert_eq!(scaling_for("custom", true), ShrinkToFit);
}

#[test]
fn a_slide_shaped_custom_page_goes_on_the_queues_own_paper_and_a_chosen_paper_is_exact() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    // 16:9, 13.33 x 7.5 in: no tray has it, and nobody chose a paper
    cmd(&mut h, "print.set", json!({"layout": {"page": {"paper": "custom", "customWidth": 13.33, "customHeight": 7.5}}}));
    cmd(&mut h, "dialog.print", json!({}));
    choose(&mut h.app, &Entry::System { name: "Slide Printer".into(), default: false });
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.step_until(T, |_| submitted_to("Slide Printer").is_some()), "handed to the spooler");
    let (job, _) = submitted_to("Slide Printer").expect("job");
    assert_eq!(job.scaling, spool::Scaling::ShrinkToFit, "not refused for lack of a 13.33 x 7.50 in paper");
    assert!(job.options.is_empty(), "no paper is asked for");
    // 4 x 6 in was picked in the dialog: the queue must have it
    cmd(&mut h, "print.set", json!({"layout": {"page": {"paper": "4x6"}}}));
    h.app.ui.dialog = Some(Dialog::Print);
    choose(&mut h.app, &Entry::System { name: "Copy Room Laser".into(), default: false });
    assert!(h.step_until(T, |h| !h.app.print_out.busy()));
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.step_until(T, |_| submitted_to("Copy Room Laser").is_some()), "handed to the spooler");
    assert_eq!(submitted_to("Copy Room Laser").expect("job").0.scaling, spool::Scaling::Exact);
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

/// The pages are composed on a worker, so the job starts after Print was pressed. What goes out is what the
/// dialog said at the press: a change made in between (here Copies, as the review traced it) sends nothing and
/// does not spend the confirmation.
#[test]
fn a_setting_changed_while_the_pages_are_composed_sends_nothing_and_keeps_the_confirmation() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    open_network(&mut h);
    query(&mut h);
    h.app.print_net.state.draft.confirmed = true;
    h.settle(T);
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.app.print_out.busy(), "the pages are composed on a worker");
    // Frames have not run since Print: the composition cannot have been delivered yet.
    h.app.print_net.state.draft.copies = 3;
    assert!(h.step_until(T, |h| !h.app.print_out.busy()), "the composition ended");
    h.settle(T);
    assert_eq!(printer.jobs(), 0, "nothing was sent: the dialog no longer said what Print was pressed for");
    assert!(h.app.print_net.state.draft.confirmed, "the confirmation was not spent");
    assert!(!h.app.print_net.state.draft.running());
}

/// An unchanged dialog still prints: the capture does not get in the way.
#[test]
fn an_unchanged_dialog_still_sends_the_job_it_captured() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    open_network(&mut h);
    query(&mut h);
    h.app.print_net.state.draft.copies = 2;
    h.app.print_net.state.draft.confirmed = true;
    h.settle(T);
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    assert!(h.step_until(T, |h| matches!(h.app.print_net.state.draft.job, JobView::Finished(_) | JobView::Failed(_))));
    assert_eq!(printer.jobs(), 1);
}

/// The print adjustment (Print Job panel) is in the PDF; a printer that takes only raster must get it too.
#[test]
fn the_print_adjustment_reaches_the_raster_sent_to_a_raster_only_printer() {
    fn raster_with(brightness: i32) -> Vec<u8> {
        let printer = FakePrinter::new(Config { formats: vec!["image/pwg-raster"], resolutions: vec![(150, 150)], ..Config::default() });
        let mut h = demo_with(&printer);
        one_photo_on_4x6(&mut h);
        cmd(&mut h, "print.set", json!({"job": {"ppi": 72.0, "color": {"brightness": brightness}}}));
        open_network(&mut h);
        query(&mut h);
        h.app.print_net.state.draft.confirmed = true;
        h.settle(T);
        crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
        assert!(h.step_until(T, |h| matches!(h.app.print_net.state.draft.job, JobView::Finished(_) | JobView::Failed(_))));
        printer.job(1).expect("a job").document.expect("a document")
    }
    let plain = raster_with(0);
    let bright = raster_with(60);
    assert_eq!(&plain[..4], b"RaS2");
    assert_ne!(plain, bright, "brightness +60 changes the raster that is sent");
}

/// While the pages are composed the Printer list and the Grayscale box beside it are locked, like the network section:
/// the dialog never shows one destination while the job goes to another.
#[test]
fn the_printer_list_and_grayscale_are_locked_while_the_pages_are_composed() {
    let printer = FakePrinter::new(Config::default());
    let mut h = demo_with(&printer);
    one_photo_on_4x6(&mut h);
    cmd(&mut h, "dialog.print", json!({}));
    assert!(h.step_until(T, |h| h.app.print_out.printers.len() == 4), "the queues were listed");
    choose(&mut h.app, &Entry::System { name: "Front Desk".into(), default: true });
    h.settle(T);
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::Print).expect("the print starts");
    // Hold the composition open: its worker's answer never arrives.
    let (_hold, rx) = channel();
    h.app.print_out.composing.as_mut().expect("composing").rx = rx;
    let _ = h.request("ui.clickWidget", json!({"id": "combo:printDestination"}), T);
    let _ = h.request("ui.clickWidget", json!({"id": "item:printDestination-2"}), T);
    h.settle(T);
    assert_eq!(h.app.print_out.dest, Destination::System("Front Desk".into()), "the destination did not change");
    let _ = h.request("ui.clickWidget", json!({"id": "check:printGrayscale"}), T);
    h.settle(T);
    assert!(!h.app.session.print.job.grayscale, "the Grayscale box did not change");
    // composed: the row is unlocked again
    h.app.print_out.composing = None;
    h.settle(T);
    click(&mut h, "combo:printDestination");
    click(&mut h, "item:printDestination-2");
    assert_eq!(h.app.print_out.dest, Destination::System("Copy Room Laser".into()));
}

const CONFIRM: &str = "I confirm sending this document unencrypted over the local network to this printer";

/// Network printing is plain HTTP; the confirmation says so (CodeRabbit on #1962).
#[test]
fn the_confirmation_discloses_the_unencrypted_transport() {
    let shown = crate::network_glue::labels().confirm;
    assert!(shown.contains("unencrypted"), "the confirmation must say the document is sent unencrypted: {shown}");
    assert_eq!(shown, CONFIRM);
}

/// The app in an egui_kittest harness with the Print dialog open on the network printer, already queried.
fn print_dialog(printer: &Arc<FakePrinter>) -> egui_kittest::Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.set_print_transport(Arc::new(MemTransport(Arc::clone(printer))));
    app.print_net.state.draft = net::Draft::for_address(ADDRESS);
    let ids: Vec<lightcraft_catalog::PhotoId> = app.session.catalog.photos().take(1).map(|p| p.id).collect();
    app.session.selection.ids = ids.into_iter().collect();
    let layout = json!({"layout": {"style": "contactSheet", "grid": {"columns": 1, "rows": 1}, "page": {"paper": "4x6"}}, "job": {"ppi": 72.0}});
    let set = app.run("print.set", layout);
    assert!(set.is_ok(), "print.set: {set:?}");
    let opened = app.run("dialog.print", json!({}));
    assert!(opened.is_ok(), "dialog.print: {opened:?}");
    app.print_out.dest = Destination::Network;
    let worker = app.print_net.query_worker();
    assert!(app.print_net.state.start_query(&worker, true));
    let ctx = egui::Context::default();
    for _ in 0..600 {
        app.print_net.state.poll(&ctx);
        if !app.print_net.state.draft.querying() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(app.print_net.state.draft.printer().is_some(), "the printer answered");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(6);
    h
}

#[test]
fn ticking_the_confirmation_box_and_clicking_print_starts_one_job() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let printer = FakePrinter::new(Config::default());
    let mut h = print_dialog(&printer);
    assert!(!h.state().print_net.state.draft.confirmed, "never ticked by default");
    h.get_by_label(CONFIRM).click();
    h.run_steps(4);
    assert!(h.state().print_net.state.draft.confirmed, "the checkbox is the control that confirms");
    assert_eq!(printer.jobs(), 0, "ticking sends nothing");
    let print = h.get_by_role_and_label(egui::accesskit::Role::Button, "Print");
    assert!(!print.accesskit_node().is_disabled(), "ticked and queried: Print is enabled");
    print.click();
    for _ in 0..1200 {
        h.run_steps(1);
        if printer.jobs() > 0 && !h.state().print_out.busy() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    h.run_steps(4);
    assert_eq!(printer.jobs(), 1, "one job started");
    assert!(!h.state().print_net.state.draft.confirmed, "the job spent the confirmation");
}

#[test]
fn closing_and_reopening_the_print_dialog_clears_the_confirmation() {
    use egui_kittest::kittest::Queryable;
    let printer = FakePrinter::new(Config::default());
    let mut h = print_dialog(&printer);
    h.get_by_label(CONFIRM).click();
    h.run_steps(4);
    assert!(h.state().print_net.state.draft.confirmed);
    h.state_mut().ui.dialog = None;
    h.run_steps(6);
    assert!(h.state().print_net.state.draft.confirmed, "closing alone does not decide; reopening does");
    h.state_mut().run("dialog.print", json!({})).expect("dialog.print");
    h.run_steps(4);
    assert!(!h.state().print_net.state.draft.confirmed, "a reopened dialog starts unconfirmed");
    assert!(h.state().print_net.state.draft.printer().is_some(), "the queried printer is still there");
    assert_eq!(printer.jobs(), 0, "nothing was submitted");
}
