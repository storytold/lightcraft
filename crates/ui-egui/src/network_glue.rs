//! Print ▸ Network printer (WS-Print or IPP): the app's part of `craft_ui::printing::network`. The model,
//! its rules and the widgets are craft-ui's; the printing is craft-print's network backends through
//! `lightcraft_print::network`. This file supplies what a host supplies: the address check, the worker that
//! asks a printer what it lists, the worker that makes the tracked print and drives it, the mapping of
//! craft-print's answers onto craft-ui's light types, and the wording.
//!
//! GENERIC vs APP-SPECIFIC: everything here is the shape every Craft app's glue has (same module names as
//! pdfcraft's `network_glue`) except `LightcraftApp`'s methods at the end and the document handed over: the
//! print-ready PDF plus the print's pages as `Sheet`s, from which `SheetConverter` makes PWG raster or JPEG for a
//! printer that lists no PDF (pdfcraft renders its PDF with its own renderer instead).
//!
//! Built for targets with sockets only (not the web build). All network work runs on its own threads; the UI
//! thread only starts it and drains receivers.
//!
//! One press of Print is one tracked `PrintJob` ([`tracked`]): sent once, followed to its end, retried only where
//! that cannot print twice, with every status line naming the printer by the name it reported.
//!
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use craft_ui::printing::network::{self as net, Failure, FailureKind, JobHandle, Labels, PrinterSummary, QueryPrinter, Request, SendJob};
use lightcraft_print::network::ipp::PrinterAttrs;
use lightcraft_print::network::wsd::PrinterElements;
use lightcraft_print::network::{self as station, Station};
use lightcraft_print::network::{Duplex, Job, JobOptions, Orientation, PrintError, Quality, Scaling, UserConfirmed};
use lightcraft_print::sheets::{Sheet, SheetConverter};

mod caps;
pub(crate) mod tracked;
use tracked::{Command, Slot};

use crate::LightcraftApp;

/// Any transport that can be shared with the worker threads (tests give an in-memory printer).
pub type Transport = lightcraft_print::network::Transport;

/// The media types the dialog can produce: the print-ready PDF, or its sheets as PWG raster or (one sheet) JPEG
/// (the registry converts through `SheetConverter` when the printer lists no PDF).
pub(crate) const FORMATS: &[&str] = &["application/pdf", "image/pwg-raster", "image/jpeg"];

/// The printer last addressed: its registry (WS-Print and IPP backends) and, with the test env var, its
/// WS-Eventing listener, kept for the query and for the jobs that follow, keyed by the typed address.
pub(crate) type Stations = Arc<Mutex<Option<(String, Arc<Station>)>>>;

/// The station for `address`: the one already open for that text, else a new one. `LIGHTCRAFT_TEST_EVENTS_BIND`
/// (test builds) turns the WS-Eventing listener on for a WS-Print printer.
fn station_for(stations: &Stations, address: &str, transport: Option<Transport>) -> Result<Arc<Station>, PrintError> {
    let mut slot = lock(stations);
    if let Some((typed, station)) = slot.as_ref()
        && typed == address
    {
        return Ok(Arc::clone(station));
    }
    let events = match station::events_from_env() {
        Some(Ok(config)) => Some(config),
        Some(Err(e)) => return Err(e),
        None => None,
    };
    let station = Arc::new(Station::open(address, transport, events)?);
    *slot = Some((address.to_owned(), Arc::clone(&station)));
    Ok(station)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
const MAX_JOB_NAME: usize = 100;

/// How the print sits on the paper, a choice the person makes next to the destination: `Exact` keeps the composed
/// page at its size (the Print module has already laid the photos inside its margins), `ShrinkToFit` lets the printer
/// scale the page into what it can print.
pub(crate) type Fit = Scaling;

/// The destination's state and the closures its workers are made from.
#[derive(Default)]
pub(crate) struct NetTasks {
    transport: Option<Transport>,
    pub(crate) state: net::State,
    query: Option<QueryPrinter>,
    stations: Stations,
    /// The tracked print of the last job (TEST BUILD): its status line, buttons and the small status row.
    pub(crate) tracked: Slot,
}

impl NetTasks {
    pub(crate) fn set_transport(&mut self, transport: Transport) {
        self.transport = Some(transport);
        self.query = None;
        *lock(&self.stations) = None;
    }

    /// The Print dialog opened: a finished job or query from last time is not shown as news.
    pub(crate) fn dialog_opened(&mut self) {
        self.state.dialog_opened();
    }

    /// A worker that queries a printer with this destination's transport and station (tests drive the query with it).
    #[cfg(test)]
    pub(crate) fn query_worker(&self) -> QueryPrinter {
        query_printer(self.transport.clone(), None, Arc::clone(&self.stations))
    }

    pub(crate) fn transport(&self) -> Option<Transport> {
        self.transport.clone()
    }

    /// Draw the section; `sheets_ok`: the dialog's settings give something to print.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, sheets_ok: bool) -> net::Response {
        let (transport, ctx, stations) = (self.transport.clone(), ui.ctx().clone(), Arc::clone(&self.stations));
        let query = self.query.get_or_insert_with(|| query_printer(transport, Some(ctx), stations));
        let host = net::Host { validate: &validate, query, sheets_ok, formats: FORMATS };
        let response = self.state.show(ui, egui::Id::new("network-printer"), &host, &labels());
        status_block(ui, &self.tracked, false);
        response
    }
}

/// The address checked against the network policy (a numeric loopback, link-local or private
/// address). The error is what the dialog shows under the field.
pub(crate) fn validate(text: &str) -> Result<(), Failure> {
    station::validate(text).map(|_| ()).map_err(|e| Failure::new(FailureKind::Policy, e.to_string()))
}

/// What went wrong, as craft-ui words it. `open`: the failure is the printer's address being refused.
fn failure(e: &PrintError, open: bool) -> Failure {
    tracked::failure_of(&lightcraft_print::network::AttemptError::from(e), open)
}

fn summary(elements: PrinterElements) -> PrinterSummary {
    let caps = caps::wsd(&elements);
    let description = elements.description.unwrap_or_default();
    let status = elements.status.unwrap_or_default();
    PrinterSummary {
        name: description.name,
        color: description.color_supported,
        state: status.state,
        queued_jobs: status.queued_jobs,
        reasons: status.reasons,
        caps,
    }
}

fn repaint(ctx: &Option<egui::Context>) {
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

/// What an IPP printer's attributes say, as craft-ui's light types (the keywords are the dialog's tokens).
fn ipp_summary(attrs: &PrinterAttrs) -> PrinterSummary {
    let state = match attrs.state {
        Some(3) => "Idle",
        Some(4) => "Processing",
        Some(5) => "Stopped",
        _ => "Unknown",
    };
    let mut reasons: Vec<String> = attrs.state_reasons.iter().filter(|r| r.as_str() != "none").cloned().collect();
    if attrs.accepting_jobs == Some(false) {
        reasons.push("not-accepting-jobs".to_owned());
    }
    PrinterSummary {
        name: attrs.name.clone().or_else(|| attrs.info.clone()).or_else(|| attrs.make_and_model.clone()).unwrap_or_default(),
        color: attrs.color_supported,
        state: state.to_owned(),
        queued_jobs: None,
        reasons,
        caps: caps::ipp(attrs),
    }
}

/// The worker that asks the printer at the (normalised) address what it is and what it lists, over the
/// protocol its address names (WS-Print or IPP). It sends no job.
pub(crate) fn query_printer(transport: Option<Transport>, ctx: Option<egui::Context>, stations: Stations) -> QueryPrinter {
    Box::new(move |address: &str| {
        let (tx, rx) = mpsc::channel();
        let (address, transport, ctx, stations) = (address.to_owned(), transport.clone(), ctx.clone(), Arc::clone(&stations));
        std::thread::spawn(move || {
            let result = match station_for(&stations, &address, transport) {
                Ok(station) => match station.protocol() {
                    station::Protocol::Wsd => station.wsd_info().map(summary).map_err(|e| failure(&e, false)),
                    station::Protocol::Ipp => station.ipp_info().map(|attrs| ipp_summary(&attrs)).map_err(|e| failure(&e, false)),
                }
                .inspect(|summary| station.set_display_name(&summary.name)),
                Err(e) => Err(failure(&e, true)),
            };
            let _ = tx.send(result);
            repaint(&ctx);
        });
        rx
    })
}

/// The job and its per-job options for the registry. `Photo` is craft-print's `Quality::Photo`: the
/// backend maps it to the printer's own quality when the printer lists one and says so when it does not.
fn options(request: &Request, job_name: &str) -> (Job, JobOptions) {
    let name: String = job_name.chars().filter(|c| !c.is_control()).take(MAX_JOB_NAME).collect();
    let mut job = Job::default();
    if !name.trim().is_empty() {
        job.title = name;
    }
    job.copies = request.copies.unwrap_or(1);
    job.duplex = request.sides.map(|s| match s {
        net::Sides::OneSided => Duplex::Off,
        net::Sides::LongEdge => Duplex::LongEdge,
        net::Sides::ShortEdge => Duplex::ShortEdge,
    });
    let mut options = JobOptions::new();
    if let Some(o) = request.orientation {
        options = options.with_orientation(match o {
            net::Orientation::Portrait => Orientation::Portrait,
            net::Orientation::Landscape => Orientation::Landscape,
            net::Orientation::ReversePortrait => Orientation::ReversePortrait,
            net::Orientation::ReverseLandscape => Orientation::ReverseLandscape,
        });
    }
    match request.quality {
        Some(net::Quality::Draft) => options = options.with_quality(Quality::Draft),
        Some(net::Quality::Normal) => options = options.with_quality(Quality::Normal),
        Some(net::Quality::High) => options = options.with_quality(Quality::High),
        Some(net::Quality::Photo) => options = options.with_quality(Quality::Photo),
        None => {}
    }
    options = caps::apply_request(options, request);
    if let Some(media) = &request.media_size {
        options = options.with_media(media.as_str());
    }
    if let Some(pages) = request.expected_pages {
        options = options.with_expected_pages(pages);
    }
    (job, options)
}

/// What one print hands to its worker: the print-ready PDF, the pages it was made from (for a printer that lists no PDF)
/// and how the pages sit on the paper.
pub(crate) struct Payload {
    pub name: String,
    pub pdf: Arc<Vec<u8>>,
    pub sheets: Arc<Vec<Sheet>>,
    pub fit: Fit,
}

/// The worker that makes the tracked print of the payload and drives it (see [`tracked`]). Each call makes a new
/// print, with the person's `confirmed` (never `interactive()`: it is the attestation of the ticked box, taken when
/// Print was pressed).
pub(crate) fn send_job(
    transport: Option<Transport>,
    ctx: Option<egui::Context>,
    payload: Payload,
    (stations, slot): (Stations, Slot),
    confirmed: UserConfirmed,
) -> SendJob {
    Box::new(move |address: &str, request: Request| {
        let (mut job, options) = options(&request, &payload.name);
        job.scaling = payload.fit;
        match station_for(&stations, address, transport.clone()) {
            Ok(station) => {
                // This print's pages are what the registry converts from, when the printer lists no PDF.
                station.set_converter(Arc::new(SheetConverter::new(Arc::clone(&payload.sheets), payload.pdf.len())));
                let print = station.job(payload.pdf.as_ref().clone(), job, options, confirmed);
                tracked::spawn(station, print, &slot, ctx.clone())
            }
            Err(e) => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Err(failure(&e, true)));
                JobHandle::new(rx, Box::new(|| {}))
            }
        }
    })
}

/// The status of the tracked print (a line built from its `JobView`, the retry advice, the last answer of a
/// command) and the buttons that make sense for the state. `row`: the small row shown with the dialog closed
/// (adds Dismiss).
pub(crate) fn status_block(ui: &mut egui::Ui, slot: &Slot, row: bool) -> bool {
    let Some(view) = lock(slot).as_ref().and_then(|t| t.view()) else { return false };
    let mut dismiss = false;
    ui.add_space(4.0);
    ui.label(egui::RichText::new(format!("{}: {}", view.view.title, tracked::status_text(&view.view, &view.name))).strong());
    if let Some(advice) = tracked::advice_text(view.advice, view.lifecycle, &view.name) {
        ui.label(advice);
    }
    if let Some(stale) = tracked::stale_text(view.stale, &view.name) {
        ui.label(stale);
    }
    if let Some(note) = &view.note {
        ui.label(egui::RichText::new(note).small());
    }
    if let Some(events) = &view.events {
        ui.label(egui::RichText::new(tracked::fill(crate::i18n::tr("WS-Eventing listener: {events}"), &[("events", events)])).small().weak());
    }
    let buttons = tracked::buttons(&view);
    ui.horizontal_wrapped(|ui| {
        let guard = lock(slot);
        let Some(tracked) = guard.as_ref() else { return };
        let mut ask = |enabled: bool, label: &str, tip: &str, command: Command| {
            if ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(tip).clicked() {
                tracked.send(command);
            }
        };
        ask(buttons.cancel, crate::i18n::tr("Cancel print"), crate::i18n::tr("Ask the printer to cancel this print"), Command::Cancel);
        ask(
            buttons.retry,
            crate::i18n::tr("Retry"),
            crate::i18n::tr("Send this print again as a new attempt (check the printer first if it says so)"),
            Command::Retry,
        );
        ask(
            buttons.reconcile,
            crate::i18n::tr("Reconcile"),
            crate::i18n::tr("Look at the printer's job lists (reads only) for a job from the earlier attempt"),
            Command::Reconcile,
        );
        ask(
            buttons.accept,
            crate::i18n::tr("Accept found job"),
            crate::i18n::tr("Follow the job the printer already has instead of sending again"),
            Command::AcceptFound,
        );
        ask(buttons.refresh, crate::i18n::tr("Check now"), crate::i18n::tr("Read the job's status now"), Command::Refresh);
        if row && ui.button(crate::i18n::tr("Dismiss")).clicked() {
            dismiss = true;
        }
    });
    if dismiss {
        *lock(slot) = None;
    }
    true
}

/// The wording of the destination, in the interface language. Templates keep their `{...}` holes.
pub(crate) fn labels() -> Labels<'static> {
    Labels {
        address: crate::i18n::tr("Printer address:"),
        address_hint: "http://192.168.1.50:5357/service  or  ipp://192.168.1.50:631/ipp/print",
        address_note: crate::i18n::tr("Only IP addresses on this network are accepted (private, link-local or loopback), not host names."),
        query_printer: crate::i18n::tr("Query printer"),
        querying: crate::i18n::tr("Asking the printer…"),
        query_first: crate::i18n::tr("Query the printer first to see what it supports."),
        printer_status: crate::i18n::tr("Printer status: {state}"),
        reasons: crate::i18n::tr("Reasons: {reasons}"),
        no_common_format: crate::i18n::tr("The printer accepts none of the formats this document can be sent in"),
        copies: crate::i18n::tr("Copies:"),
        sides: crate::i18n::tr("Two-sided:"),
        sides_off: crate::i18n::tr("Off"),
        sides_long_edge: crate::i18n::tr("Flip on long edge"),
        sides_short_edge: crate::i18n::tr("Flip on short edge"),
        orientation: crate::i18n::tr("Printer orientation:"),
        portrait: crate::i18n::tr("Portrait"),
        landscape: crate::i18n::tr("Landscape"),
        reverse_portrait: crate::i18n::tr("Reverse portrait"),
        reverse_landscape: crate::i18n::tr("Reverse landscape"),
        quality: crate::i18n::tr("Quality:"),
        draft: crate::i18n::tr("Draft"),
        normal: crate::i18n::tr("Normal"),
        high: crate::i18n::tr("High"),
        photo: crate::i18n::tr("Photo"),
        color: crate::i18n::tr("Color:"),
        color_color: crate::i18n::tr("Color"),
        color_monochrome: crate::i18n::tr("Black and white"),
        resolution: crate::i18n::tr("Resolution:"),
        resolution_dpi: "{dpi} dpi",
        resolution_dpi_xy: "{x} x {y} dpi",
        auto: crate::i18n::tr("Auto"),
        unsupported_copies: crate::i18n::tr("This printer prints one copy at a time."),
        unsupported_sides: crate::i18n::tr("This printer only prints one-sided."),
        unsupported_orientation: crate::i18n::tr("This printer does not take an orientation setting."),
        unsupported_quality: crate::i18n::tr("This printer does not take a quality setting."),
        unsupported_color: crate::i18n::tr("This printer only prints in black and white."),
        unsupported_resolution: crate::i18n::tr("This printer does not take a resolution setting."),
        unconfirmed_note: crate::i18n::tr("Not confirmed by the printer."),
        confirm: crate::i18n::tr("I confirm sending this document to this printer"),
        sending: crate::i18n::tr("Sending to {printer}…"),
        canceling: crate::i18n::tr("Canceling…"),
        cancel: crate::i18n::tr("Cancel"),
        sent: crate::i18n::tr("Sent to {printer}"),
        accepted: crate::i18n::tr("Accepted by {printer}"),
        stage_waiting: crate::i18n::tr("Waiting in the printer's queue"),
        stage_held: crate::i18n::tr("Held by the printer"),
        stage_printing: crate::i18n::tr("The printer is printing it"),
        stage_stopped: crate::i18n::tr("The printer paused the job; it needs attention"),
        sheets: crate::i18n::tr("Sheets printed: {n}"),
        sheets_progress: crate::i18n::tr("Sheets printed: {done} of {total}"),
        outcome_warnings: crate::i18n::tr("Printed with warnings"),
        outcome_errors: crate::i18n::tr("Printed with errors"),
        outcome_canceled: crate::i18n::tr("The job was canceled"),
        outcome_aborted: crate::i18n::tr("The printer stopped the job"),
        outcome_forgotten: crate::i18n::tr("The printer no longer lists the job; it most likely finished"),
        outcome_still_running: crate::i18n::tr("Still printing; the job continues on the printer"),
        outcome_accepted_unknown: crate::i18n::tr("Sent. The printer can't say whether it printed."),
        outcome_unknown_warning: crate::i18n::tr("The printer may have received the document. Check the printer before sending again."),
        failure_policy: crate::i18n::tr("The printer address was refused"),
        failure_transport: crate::i18n::tr("Couldn't reach the printer"),
        failure_protocol: crate::i18n::tr("The printer's reply couldn't be understood"),
        failure_fault: crate::i18n::tr("The printer refused the request"),
        failure_no_common_format: crate::i18n::tr("The printer accepts none of the formats this document can be sent in"),
        failure_unsupported: crate::i18n::tr("The printer doesn't support this setting"),
        failure_encode: crate::i18n::tr("The document couldn't be prepared"),
        failure_invalid: crate::i18n::tr("A setting isn't valid"),
        failure_stopped: crate::i18n::tr("The print job stopped unexpectedly"),
        failure_other: crate::i18n::tr("The print job stopped unexpectedly"),
    }
}

/// The one-line notice for a print that ended with the dialog closed, naming the printer by its display name
/// (nothing when it has none, nor an address): "<name>: The printer stopped the job".
fn notice_text(labels: &Labels<'_>, job: &net::JobView, name: &str) -> Option<String> {
    let who = tracked::printer_label(name);
    let named = who != "the printer";
    let prefix = |text: String| if named { format!("{who}: {text}") } else { text };
    match job {
        net::JobView::Finished(r) => Some(match labels.outcome_text(r.outcome) {
            Some(text) => prefix(text.to_owned()),
            None => tracked::fill(labels.sent, &[("printer", &tracked::printer_label(&r.printer))]),
        }),
        net::JobView::Failed(f) => {
            let mut text = prefix(format!("{}: {}", labels.failure_heading(f.kind), tracked::scrubbed(&f.detail, &who)));
            if f.outcome_unknown {
                text = format!("{text} {}", labels.outcome_unknown_warning);
            }
            Some(text)
        }
        _ => None,
    }
}

impl LightcraftApp {
    /// Use `transport` for network printing instead of the built-in HTTP one (tests; embedding).
    pub fn set_print_transport(&mut self, transport: Transport) {
        self.print_net.set_transport(transport);
    }

    /// Send the print-ready `pdf` (laid out as `sheets`, whose count is the job's page count) to the queried
    /// printer, off the UI thread. It needs the person's confirmation, which it spends. Returns `true` when a job
    /// started.
    pub(crate) fn network_send(&mut self, ctx: Option<&egui::Context>, payload: Payload, paper: &str) -> bool {
        let page_count = payload.sheets.len();
        let net = &mut self.print_net;
        let address = net.state.draft.normalized_url();
        // The attestation comes from the ticked box and nothing else.
        let Ok(confirmed) = UserConfirmed::attest(Some(net.state.draft.confirmed)) else { return false };
        let send = send_job(net.transport(), ctx.cloned(), payload, (Arc::clone(&net.stations), Arc::clone(&net.tracked)), confirmed);
        let net = &mut self.print_net;
        // The sheets are the pages of the document sent: the total that progress shows until the printer reports one.
        net.state.draft.expected_pages = u32::try_from(page_count).ok();
        net.state.start_job(&send, validate(&address).is_ok(), true, FORMATS, crate::print_dest::media_for_paper(paper))
    }

    /// The small status row of the last tracked print, shown while the Print dialog is closed.
    pub(crate) fn show_print_status(&mut self, ctx: &egui::Context) {
        if self.ui.dialog == Some(crate::state::Dialog::Print) || lock(&self.print_net.tracked).is_none() {
            return;
        }
        let slot = Arc::clone(&self.print_net.tracked);
        egui::Window::new(crate::i18n::tr("Print status"))
            .id(egui::Id::new("print-status-row"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -12.0])
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                status_block(ui, &slot, true);
            });
    }

    /// Collect finished work (each frame), also while the dialog is closed; with the dialog gone the person still
    /// hears how a job went.
    pub(crate) fn poll_network(&mut self, ctx: &egui::Context) {
        let was_running = self.print_net.state.draft.running();
        self.print_net.state.poll(ctx);
        if !was_running || self.print_net.state.draft.running() || self.ui.dialog == Some(crate::state::Dialog::Print) {
            return;
        }
        let name = self.print_net.state.draft.printer().map(|p| p.name.clone()).unwrap_or_default();
        if let Some(message) = notice_text(&labels(), &self.print_net.state.draft.job, &name) {
            self.toast(ctx, message);
        }
    }
}

#[cfg(test)]
mod tests;
