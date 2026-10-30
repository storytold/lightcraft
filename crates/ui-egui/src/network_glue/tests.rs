use super::*;
use craft_print::wsd::adapter::{DeviceUrl, Request as WireRequest, Response, TransportKind, WireError};
use craft_print::wsd::testing::{Ending, FakeConfig as Config, FakePrinter, MemTransport};
use craft_ui::printing::network::Outcome;
use lightcraft_print::network::{ErrorKind, Phase, RetrySafety};
use net::{JobView, Query};
use std::time::Duration;

const ADDRESS: &str = "127.0.0.1:5357/printer";

fn app_with(transport: Transport) -> LightcraftApp {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
    app.set_print_transport(transport);
    app.print_net.state.draft = net::Draft::for_address(ADDRESS);
    app
}

fn app(config: Config) -> (LightcraftApp, Arc<FakePrinter>) {
    let printer = FakePrinter::new(config);
    (app_with(Arc::new(MemTransport(printer.clone()))), printer)
}

fn wait(app: &mut LightcraftApp, done: impl Fn(&LightcraftApp) -> bool) {
    for _ in 0..400 {
        app.poll_network(&egui::Context::default());
        if done(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting");
}

fn state(app: &LightcraftApp) -> &net::State {
    &app.print_net.state
}

/// Ask the printer, as the Query button does, and wait for the answer.
fn query(app: &mut LightcraftApp) {
    let address_ok = validate(&app.print_net.state.draft.normalized_url()).is_ok();
    let worker = query_printer(app.print_net.transport(), None, Arc::clone(&app.print_net.stations));
    let started = app.print_net.state.start_query(&worker, address_ok);
    assert_eq!(started, address_ok);
    wait(app, |a| !state(a).draft.querying());
}

fn tiny_pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R>>endobj\ntrailer<</Root 1 0 R/Size 4>>\n%%EOF\n".to_vec()
}

/// One 2 x 2 inch page: the PDF the printer may take as it is and the same page as pixels.
fn payload() -> Payload {
    let page = lightcraft_engine::print::PageRaster { width: 40, height: 40, ppi: 20.0, rgb: vec![46_000; 40 * 40 * 3] };
    Payload {
        name: "report.pdf".into(),
        pdf: Arc::new(tiny_pdf()),
        pages: Arc::new(vec![page]),
        job: lightcraft_engine::print::PrintJob::default(),
        fit: Scaling::Exact,
        gray: false,
    }
}

fn send(app: &mut LightcraftApp) -> bool {
    app.network_send(None, payload(), "a4")
}

#[test]
fn the_policy_refuses_what_it_should_before_anything_is_sent() {
    for bad in ["printer.local", "http://8.8.8.8/", "https://10.0.0.1/", "http://user@10.0.0.1/", "http://0.0.0.0/"] {
        assert_eq!(validate(&net::normalize_url(bad)).map_err(|f| f.kind), Err(FailureKind::Policy), "{bad}");
    }
    assert!(validate(&net::normalize_url("192.168.1.50:5357/service")).is_ok());
    assert!(validate("http://[::1]:5357/x").is_ok());
}

#[test]
fn errors_map_onto_the_failures_the_dialog_words_and_keep_whether_the_outcome_is_unknown() {
    for (kind, want) in [
        (ErrorKind::InvalidRequest, FailureKind::Invalid),
        (ErrorKind::UnsupportedFormat, FailureKind::NoCommonFormat),
        (ErrorKind::Unsupported, FailureKind::Unsupported),
        (ErrorKind::Unavailable, FailureKind::Transport),
        (ErrorKind::Timeout, FailureKind::Transport),
        (ErrorKind::Failed, FailureKind::Protocol),
        (ErrorKind::Rejected, FailureKind::Fault),
        (ErrorKind::Cancelled, FailureKind::Stopped),
    ] {
        assert_eq!(failure(&PrintError::new(kind, "x"), false).kind, want, "{kind:?}");
    }
    assert_eq!(failure(&PrintError::new(ErrorKind::InvalidRequest, "x"), true).kind, FailureKind::Policy);
    // A document that could not be made into a format the printer takes is "couldn't be prepared".
    assert_eq!(failure(&PrintError::new(ErrorKind::Failed, "x").with_phase(Phase::Prepare), false).kind, FailureKind::Encode);
    assert!(failure(&PrintError::new(ErrorKind::Timeout, "x").with_safety(RetrySafety::OutcomeUnknown), false).outcome_unknown);
    assert!(!failure(&PrintError::new(ErrorKind::Busy, "x").with_safety(RetrySafety::Safe), false).outcome_unknown);
}

#[test]
fn a_query_fills_in_the_printer_and_what_it_lists() {
    let (mut app, printer) = app(Config::default());
    query(&mut app);
    let d = &state(&app).draft;
    assert!(matches!(d.query, Query::Done(_)), "{:?}", d.query);
    assert_eq!(d.offered_sides().len(), 2);
    assert_eq!(d.max_copies(), 10);
    // Reading is all that happened: no job exists.
    assert_eq!(printer.jobs(), 0);
}

#[test]
fn a_refused_address_is_never_contacted_and_a_failed_query_is_shown() {
    let (mut app, printer) = app(Config { http_error: Some(503), ..Default::default() });
    app.print_net.state.draft.url = "8.8.8.8".into();
    query(&mut app);
    assert!(!state(&app).draft.querying() && printer.actions().is_empty());
    app.print_net.state.draft.url = "127.0.0.1:1/x".into();
    query(&mut app);
    assert!(matches!(&state(&app).draft.query, Query::Failed(f) if f.kind == FailureKind::Transport), "{:?}", state(&app).draft.query);
}

#[test]
fn nothing_is_sent_without_the_confirmation_and_a_query() {
    let (mut app, printer) = app(Config::default());
    assert!(!send(&mut app), "not queried");
    query(&mut app);
    assert!(!send(&mut app), "not confirmed");
    assert_eq!(printer.jobs(), 0);
}

#[test]
fn a_confirmed_job_runs_to_the_end_and_spends_the_confirmation() {
    let (mut app, printer) = app(Config::default());
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    app.print_net.state.draft.copies = 2;
    assert!(send(&mut app));
    assert!(!state(&app).draft.confirmed && state(&app).draft.running());
    wait(&mut app, |a| !state(a).draft.running());
    match &state(&app).draft.job {
        JobView::Finished(r) => assert_eq!(r.outcome, Outcome::Completed),
        other => panic!("{other:?}"),
    }
    assert_eq!(printer.jobs(), 1);
    // The next print needs a new yes.
    assert!(!send(&mut app));
    assert_eq!(printer.jobs(), 1);
}

#[test]
fn the_job_is_accepted_first_and_only_then_followed_to_its_end() {
    let (mut app, printer) = app(Config { ending: Ending::Hangs, ..Default::default() });
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    // The printer has the document and the job is still going: accepted, not finished.
    wait(&mut app, |a| matches!(state(a).draft.job, JobView::Accepted { .. }));
    assert_eq!(printer.jobs(), 1);
    assert!(state(&app).draft.running());
    // It carries on at the printer; the dialog can cancel it, once.
    assert!(app.print_net.state.cancel_job());
    wait(&mut app, |a| !state(a).draft.running());
    assert!(matches!(&state(&app).draft.job, JobView::Finished(r) if r.outcome == Outcome::Canceled), "{:?}", state(&app).draft.job);
    assert_eq!(printer.actions().iter().filter(|a| *a == "SendDocument").count(), 1);
    assert_eq!(printer.actions().iter().filter(|a| *a == "CancelJob").count(), 1);
}

#[test]
fn a_network_print_is_a_row_of_the_activity_stack_until_it_settles_and_its_cross_cancels_it() {
    let (mut app, printer) = app(Config { ending: Ending::Hangs, ..Default::default() });
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    let rows = |app: &LightcraftApp| app.session.activity.list().into_iter().filter(|t| t.kind == "print").collect::<Vec<_>>();
    let row = rows(&app);
    assert_eq!(row.len(), 1, "{row:?}");
    assert!(row[0].cancellable);
    wait(&mut app, |a| matches!(state(a).draft.job, JobView::Accepted { .. }));
    assert_eq!(rows(&app).len(), 1, "still going at the printer");
    // The row's ✕ cancels the print at the printer, once.
    app.session.activity.cancel(row[0].id).unwrap();
    wait(&mut app, |a| !state(a).draft.running());
    assert!(matches!(&state(&app).draft.job, JobView::Finished(r) if r.outcome == Outcome::Canceled), "{:?}", state(&app).draft.job);
    assert_eq!(printer.actions().iter().filter(|a| *a == "CancelJob").count(), 1);
    wait(&mut app, |a| rows(a).is_empty());
}

#[test]
fn the_documents_page_count_gives_the_progress_a_total_the_printer_does_not_report() {
    let (mut app, _printer) = app(Config { ending: Ending::Hangs, ..Default::default() });
    query(&mut app);
    let draft = &mut app.print_net.state.draft;
    draft.confirmed = true;
    draft.copies = 2;
    assert!(send(&mut app));
    // One sheet, two copies: the app supplied the page count, craft-ui the estimate; WS-Print reports no total.
    assert_eq!(state(&app).draft.expected_pages, Some(1));
    wait(&mut app, |a| matches!(&state(a).draft.job, JobView::Accepted { progress, .. } if progress.sheets_total.is_some()));
    match &state(&app).draft.job {
        JobView::Accepted { progress, .. } => assert_eq!(progress.sheets_total, Some(2), "{progress:?}"),
        other => panic!("{other:?}"),
    }
    assert!(app.print_net.state.cancel_job());
    wait(&mut app, |a| !state(a).draft.running());
}

#[test]
fn a_job_the_printer_aborts_is_reported_as_such() {
    let (mut app, _printer) = app(Config { ending: Ending::Aborts("DocumentFormatError"), ..Default::default() });
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    wait(&mut app, |a| !state(a).draft.running());
    assert!(matches!(&state(&app).draft.job, JobView::Finished(r) if r.outcome == Outcome::Aborted), "{:?}", state(&app).draft.job);
}

/// Delivers every request to the fake and loses the reply to `SendDocument`: the printer did the
/// work and the client saw the connection close with no answer.
struct LoseSendReply(Arc<FakePrinter>);

impl craft_print::wsd::adapter::Transport for LoseSendReply {
    fn post(&self, _: &DeviceUrl, request: WireRequest) -> Result<Response, WireError> {
        let (status, content_type, body) = self.0.handle(&request.content_type, &request.body);
        if request.soap_action.ends_with("SendDocument") {
            return Err(WireError::transport(TransportKind::ConnectionClosed, "closed without a reply"));
        }
        Ok(Response::new(status, Some(content_type), body))
    }
}

#[test]
fn a_send_whose_outcome_is_unknown_asks_to_check_the_printer_and_is_never_repeated() {
    let printer = FakePrinter::new(Config::default());
    let mut app = app_with(Arc::new(LoseSendReply(printer.clone())));
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    wait(&mut app, |a| !state(a).draft.running());
    match &state(&app).draft.job {
        JobView::Failed(f) => assert!(f.outcome_unknown, "{f:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(printer.actions().iter().filter(|a| *a == "SendDocument").count(), 1, "{:?}", printer.actions());
    assert!(!printer.actions().iter().any(|a| a == "CancelJob"), "a job that may be printing is left alone");
    // The confirmation was spent: sending again needs a new yes.
    assert!(!send(&mut app));
}

#[test]
fn options_carry_only_what_was_requested() {
    let r = Request {
        copies: Some(2),
        sides: Some(net::Sides::LongEdge),
        orientation: Some(net::Orientation::Landscape),
        quality: Some(net::Quality::High),
        color: Some(net::ColorMode::Monochrome),
        resolution: Some(net::Resolution::new(600, 300)),
        media_size: Some("iso_a4_210x297mm".into()),
        expected_pages: Some(5),
    };
    let (job, o) = options(&r, "a\u{7}b.pdf");
    assert_eq!((job.copies, job.title.as_str(), job.duplex), (2, "ab.pdf", Some(Duplex::LongEdge)));
    assert_eq!(o.orientation, Some(Orientation::Landscape));
    assert_eq!(o.quality, Some(Quality::High));
    assert_eq!((o.colour, o.resolution), (craft_print::ColourMode::Mono, Some(craft_print::Resolution::new(600, 300))));
    assert_eq!((o.media.as_deref(), o.expected_pages), (Some("iso_a4_210x297mm"), Some(5)));
    let (job, o) = options(&Request::default(), "");
    assert_eq!((job.title, job.copies, job.duplex), (Job::default().title, 1, None));
    assert_eq!(o, JobOptions::new());
    // Photo is craft-print's own quality now: it goes to the registry like the others.
    let (_, o) = options(&Request { quality: Some(net::Quality::Photo), ..Request::default() }, "x");
    assert_eq!(o.quality, Some(Quality::Photo));
}

#[test]
fn paper_names_map_to_the_sizes_printers_list() {
    use crate::print_dest::media_for_paper;
    assert_eq!(media_for_paper("a4"), Some("iso_a4_210x297mm"));
    assert_eq!(media_for_paper("A4"), Some("iso_a4_210x297mm"));
    assert_eq!(media_for_paper("4x6"), Some("na_index-4x6_4x6in"));
    assert_eq!(media_for_paper("custom"), None);
    assert_eq!(media_for_paper("postcard"), Some("jpn_hagaki_100x148mm"));
    assert_eq!(media_for_paper("double-postcard"), Some("jpn_oufuku_148x200mm"));
    // every paper the Print module offers either has a printer name or is a custom size: the Japanese photo
    // sizes and the business card have no standard media name, so they go on the queue's own paper, shrunk to fit
    let custom = ["l", "2l", "kg", "6cut", "4cut", "business-card"];
    for name in lightcraft_engine::print::layout::paper_names() {
        assert_eq!(media_for_paper(name).is_none(), custom.contains(&name), "{name}");
    }
}

fn reads(printer: &FakePrinter) -> usize {
    printer.actions().iter().filter(|a| *a == "GetPrinterElements").count()
}

#[test]
fn photo_quality_chosen_in_the_dialog_reaches_the_ticket() {
    let (mut app, printer) = app(Config { qualities: vec!["Normal", "Photo"], ..Default::default() });
    query(&mut app);
    assert!(state(&app).draft.offered_qualities().contains(&net::Quality::Photo), "the printer lists it, so the dialog offers it");
    app.print_net.state.draft.confirmed = true;
    app.print_net.state.draft.quality = Some(net::Quality::Photo);
    assert!(send(&mut app));
    wait(&mut app, |a| !state(a).draft.running());
    assert!(matches!(&state(&app).draft.job, JobView::Finished(r) if r.outcome == Outcome::Completed), "{:?}", state(&app).draft.job);
    let ticket = printer.job(1).unwrap().ticket;
    assert!(ticket.contains("Photo"), "Photo missing from {ticket}");
}

#[test]
fn a_printer_without_pdf_gets_the_pages_as_raster_and_is_read_twice_for_the_tracked_job() {
    let (mut app, printer) = app(Config { formats: vec!["image/pwg-raster"], resolutions: vec![(150, 150), (600, 600)], ..Default::default() });
    query(&mut app);
    assert_eq!(reads(&printer), 1, "the query");
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    wait(&mut app, |a| !state(a).draft.running());
    assert!(matches!(&state(&app).draft.job, JobView::Finished(r) if r.outcome == Outcome::Completed), "{:?}", state(&app).draft.job);
    let sent = printer.job(1).unwrap().document.unwrap();
    assert_eq!(&sent[..4], b"RaS2");
    // The listed resolution nearest 300 dpi (150), in the raster header's HWResolution.
    assert_eq!(u32::from_be_bytes(sent[4 + 276..4 + 280].try_into().unwrap()), 150);
    // TEST BUILD: the tracked `PrintJob` reads the printer's details itself (the registry's choice of format), then
    // the backend reads its capabilities to plan the ticket: two reads during the job after the query's.
    assert_eq!(reads(&printer), 3, "{:?}", printer.actions());
}

#[test]
fn a_printer_that_lists_pdf_gets_the_print_ready_pdf_unchanged() {
    let (mut app, printer) = app(Config::default());
    query(&mut app);
    app.print_net.state.draft.confirmed = true;
    assert!(send(&mut app));
    wait(&mut app, |a| !state(a).draft.running());
    assert_eq!(printer.job(1).unwrap().document.as_deref(), Some(&tiny_pdf()[..]));
    assert_eq!(reads(&printer), 3, "the query, the registry's and the backend's own: {:?}", printer.actions());
}

#[test]
fn the_notice_for_a_print_that_ended_names_the_printer_and_no_address() {
    let labels = labels();
    let finished =
        |outcome| JobView::Finished(net::Report { outcome, printer: "http://127.0.0.1:5357/printer".to_owned(), sheets: 1, notes: Vec::new() });
    let aborted = notice_text(&labels, &finished(Outcome::Aborted), "Office Laser").unwrap();
    assert_eq!(aborted, format!("Office Laser: {}", labels.outcome_aborted));
    let unnamed = notice_text(&labels, &finished(Outcome::Aborted), "").unwrap();
    assert_eq!(unnamed, labels.outcome_aborted);
    // A report that carries an address instead of a name never shows it.
    let sent = notice_text(&labels, &finished(Outcome::Completed), "").unwrap();
    assert!(!sent.contains("127.0.0.1") && !sent.contains("http"), "{sent}");
    let failure = net::Failure::new(net::FailureKind::Transport, "no answer from http://127.0.0.1:5357/printer").with_outcome_unknown(true);
    let failed = notice_text(&labels, &JobView::Failed(failure), "Office Laser").unwrap();
    assert!(failed.starts_with("Office Laser: "), "{failed}");
    assert!(!failed.contains("127.0.0.1") && !failed.contains("http"), "{failed}");
    assert_eq!(notice_text(&labels, &JobView::Idle, "Office Laser"), None);
}

/// What the print becomes for a printer that lists no PDF, from the job's settings to the raster bytes: the type the
/// registry names is the type written, in the colour space the type says, and the preference follows the job.
#[test]
fn the_converter_follows_the_job_and_writes_the_type_it_is_asked_for() {
    use craft_print::{ConvertContext, Document, DocumentConverter, DocumentFormat};
    use lightcraft_engine::print_color::{ManagedSpace, PrintProfile};
    use lightcraft_print::sheets::RasterType::{self, *};

    let mut wide = payload();
    wide.job.color.profile = PrintProfile::Managed { space: ManagedSpace::AdobeRgb };
    wide.job.color.sixteen_bit = true;
    let converter = converter_for(&wide);
    assert_eq!(converter.raster_types(), [AdobeRgb16, Srgb16, Srgb8, Sgray16, Sgray8], "Adobe RGB, 16 bits");
    let mut srgb = payload();
    srgb.job.color.profile = PrintProfile::Managed { space: ManagedSpace::Srgb };
    srgb.job.color.sixteen_bit = true;
    assert_eq!(converter_for(&srgb).raster_types(), [Srgb16, Srgb8, Sgray16, Sgray8], "managed sRGB never offers Adobe RGB");
    srgb.job.color.sixteen_bit = false;
    assert_eq!(converter_for(&srgb).raster_types(), [Srgb8, Sgray8], "8 bits");

    let pdf = tiny_pdf();
    let header_of = |kind: RasterType| {
        let mut context = ConvertContext::new(craft_print::Resolution::new(20, 20), !kind.is_gray());
        context.raster = kind;
        let out = converter.convert(Document::pdf(&pdf), DocumentFormat::PwgRaster, &context).expect("pwg");
        let h = out.bytes[4..4 + 1796].to_vec();
        let at = |o: usize| u32::from_be_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);
        ((at(400), at(384), at(420)), out.bytes)
    };
    for kind in RasterType::ALL {
        let (fields, _) = header_of(kind);
        assert_eq!(fields, (kind.colour_space(), kind.bits(), kind.channels() as u32), "{kind:?}");
    }
    // The page is the same colour in two spaces: the samples differ, and the 16-bit ones are the finer description.
    let (_, adobe) = header_of(AdobeRgb16);
    let (_, plain) = header_of(Srgb16);
    assert_ne!(adobe[4 + 1796..], plain[4 + 1796..], "Adobe RGB and sRGB describe the colour differently");
}

/// The fake lists A4 only. A 4 x 6 in page kept at its size would print on A4 cut to its corner: the paper is left out
/// of the request, so the page is shrunk to fit the printer's paper and the person is told. A listed paper stays exact.
#[test]
fn a_paper_the_printer_does_not_list_is_shrunk_to_fit_and_the_person_is_told() {
    let run = |paper: &str| {
        let (mut app, _printer) = app(Config::default());
        query(&mut app);
        app.print_net.state.draft.confirmed = true;
        assert!(app.network_send(Some(&egui::Context::default()), payload(), paper));
        let toast = app.ui.toast.clone().map(|t| t.0);
        wait(&mut app, |a| !state(a).draft.running());
        let notes = match &state(&app).draft.job {
            JobView::Finished(r) => r.notes.clone(),
            other => panic!("{other:?}"),
        };
        (toast, notes)
    };
    let scaled = |notes: &[String]| notes.iter().any(|n| n.contains("scaling"));
    let (toast, notes) = run("4x6");
    assert!(toast.as_deref().is_some_and(|t| t.contains("does not list this paper size")), "{toast:?}");
    assert!(scaled(&notes), "the job asked for shrink to fit: {notes:?}");
    let (toast, notes) = run("a4");
    assert_eq!(toast, None, "a listed paper says nothing");
    assert!(!scaled(&notes), "a listed paper stays exact: {notes:?}");
}

/// With the Grayscale box ticked the pages are gray: the printer's Color setting set to Color must not undo that.
#[test]
fn the_grayscale_box_wins_over_the_printers_color_setting() {
    let raster = |gray: bool| {
        let (mut app, printer) = app(Config { formats: vec!["image/pwg-raster"], resolutions: vec![(150, 150)], ..Default::default() });
        query(&mut app);
        app.print_net.state.draft.confirmed = true;
        app.print_net.state.draft.color = Some(net::ColorMode::Color);
        assert!(app.network_send(None, Payload { gray, ..payload() }, "a4"));
        wait(&mut app, |a| !state(a).draft.running());
        printer.job(1).unwrap().document.unwrap()
    };
    let colours = |doc: &[u8]| u32::from_be_bytes(doc[4 + 420..4 + 424].try_into().unwrap());
    assert_eq!(colours(&raster(false)), 3, "Color, in colour: RGB raster");
    assert_eq!(colours(&raster(true)), 1, "Grayscale ticked: a gray raster whatever the Color setting says");
}
