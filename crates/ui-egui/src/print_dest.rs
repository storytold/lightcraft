//! Where a print goes besides a PDF file: a printer of this computer (the system spooler) or a network
//! printer (WS-Print or IPP). The Print dialog (`print_view`) draws the choice; this module owns what lies
//! behind it.
//!
//! The pages are the Print module's, composed exactly as for "Print to PDF": the layout, the colour
//! management and the resolution the dialog shows are what is sent. Composing needs the session, so it starts
//! on the UI thread ([`lightcraft_engine::print::compose::prepare`]) and renders on a worker (its row in the
//! activity stack shows the pages, with ✕); the worker hands back the print-ready PDF and the pages as 8-bit
//! sRGB sheets. Each frame [`poll`] collects what finished: the pages go to the spooler, or to the network glue
//! (`network_glue`) which sends them on its own threads and follows the job.
//!
//! Nothing here contacts a device until the person presses Print: listing the system's printers reads the
//! spooler only, and the network printer is queried by its own button and sent a job only with the box ticked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use egui::Ui;
use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_print::sheets::Sheet;
use lightcraft_print::spool;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

/// Where the Print dialog sends the print.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Destination {
    /// A PDF file the person chooses (the dialog's original behaviour).
    #[default]
    Pdf,
    /// A printer of this computer, by the name the spooler lists it under.
    System(String),
    /// A network printer (WS-Print or IPP) at an address the person types.
    Network,
}

/// The finished composition of a print: its PDF and its pages.
struct Composed {
    pdf: Vec<u8>,
    sheets: Vec<Sheet>,
}

/// A print being composed on a worker.
struct Composing {
    rx: Receiver<Result<Composed, String>>,
    progress: Arc<Mutex<(usize, String)>>,
    total: usize,
    guard: TaskGuard,
    to: Destination,
    /// The paper's name in the Print module (`a4`, `4x6`…), for the printer's media size.
    paper: String,
    title: String,
    fit: spool::Scaling,
}

/// A job handed to the system spooler: the printer and the spooler's answer.
type SpoolJob = (String, Receiver<Result<String, String>>);

/// What the Print dialog remembers about where the print goes (not saved).
#[derive(Default)]
pub struct PrintOut {
    pub dest: Destination,
    /// Let the printer scale the page into what it can print (otherwise the composed page keeps its size).
    pub shrink_to_fit: bool,
    printers: Vec<spool::Printer>,
    printers_loading: bool,
    printers_rx: Option<Receiver<Vec<spool::Printer>>>,
    composing: Option<Composing>,
    jobs: Vec<SpoolJob>,
    /// Why Print is unavailable for the network destination right now (set while it is drawn).
    block: Option<craft_ui::printing::network::Block>,
}

impl PrintOut {
    /// The dialog opened: forget the last choice's leftovers and list the system's printers in the background.
    pub fn dialog_opened(&mut self) {
        self.block = None;
        if let Destination::System(name) = &self.dest
            && !self.printers.iter().any(|p| &p.name == name)
        {
            self.dest = Destination::Pdf;
        }
        if self.printers_rx.is_none() {
            let (tx, rx) = channel();
            self.printers_loading = true;
            self.printers_rx = Some(rx);
            let spawned = std::thread::Builder::new().name("printers".into()).spawn(move || {
                let _ = tx.send(spool::printers());
            });
            if spawned.is_err() {
                self.printers_rx = None;
                self.printers_loading = false;
            }
        }
    }

    /// The Print button's words for the chosen destination.
    pub fn confirm_label(&self) -> &'static str {
        match self.dest {
            Destination::Pdf => "Print to PDF…",
            Destination::System(_) | Destination::Network => "Print",
        }
    }

    /// Can Print be pressed? The network destination waits for its address, query and tick.
    pub fn can_confirm(&self) -> bool {
        match self.dest {
            Destination::Pdf => true,
            Destination::System(_) => self.composing.is_none(),
            Destination::Network => self.block.is_none() && self.composing.is_none(),
        }
    }

    /// The dialog stays open after Print for the network destination: it shows the job's status.
    pub fn keeps_dialog_open(&self) -> bool {
        self.dest == Destination::Network
    }

    /// A print is being composed or a job is being followed (for `ui.inspect` and the quit prompt's sake).
    pub fn busy(&self) -> bool {
        self.composing.is_some()
    }
}

/// The page-setup name of the paper as printers list it (PWG media names), for the sizes that have one.
pub(crate) fn media_for_paper(paper: &str) -> Option<&'static str> {
    Some(match paper.to_ascii_lowercase().as_str() {
        "letter" => "na_letter_8.5x11in",
        "legal" => "na_legal_8.5x14in",
        "tabloid" => "na_ledger_11x17in",
        "a3" => "iso_a3_297x420mm",
        "a4" => "iso_a4_210x297mm",
        "a5" => "iso_a5_148x210mm",
        "4x6" => "na_index-4x6_4x6in",
        "5x7" => "na_5x7_5x7in",
        "8x10" => "na_8x10_8x10in",
        "13x19" => "na_super-b_13x19in",
        _ => return None,
    })
}

/// The "Print To" section at the top of the Print dialog's settings.
pub fn section(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, photos: usize) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(crate::i18n::tr("Print To")).font(t.semibold(12.0)).color(t.text));
    ui.add_space(2.0);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let out = &mut app.print_out;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if text_button(ui, "printDestPdf", "PDF file", out.dest == Destination::Pdf).clicked() {
                out.dest = Destination::Pdf;
            }
            let system = matches!(out.dest, Destination::System(_));
            if text_button(ui, "printDestSystem", "Printer", system).clicked() && !system {
                out.dest = spool::default_printer(&out.printers).map_or(Destination::Pdf, |p| Destination::System(p.name.clone()));
            }
            if text_button(ui, "printDestNetwork", "Network printer", out.dest == Destination::Network).clicked() {
                out.dest = Destination::Network;
            }
        });
        if let Destination::System(current) = out.dest.clone() {
            if out.printers_loading {
                ui.label(egui::RichText::new(crate::i18n::tr("Looking for printers…")).color(t.text_dim));
            }
            if out.printers.is_empty() && !out.printers_loading {
                ui.label(egui::RichText::new(crate::i18n::tr("No printers found on this computer.")).color(t.text_dim));
            }
            let combo = egui::ComboBox::from_id_salt("printSystemPrinter").width(220.0).selected_text(current.clone()).show_ui(ui, |ui| {
                for p in out.printers.clone() {
                    if ui.selectable_label(p.name == current, &p.name).clicked() {
                        out.dest = Destination::System(p.name.clone());
                    }
                }
            });
            register(ui.ctx(), "combo:printSystemPrinter", combo.response.rect);
        }
        if out.dest != Destination::Pdf {
            let r = ui.checkbox(&mut out.shrink_to_fit, crate::i18n::tr("Shrink to the printer's printable area"));
            register(ui.ctx(), "check:printShrinkToFit", r.rect);
            r.on_hover_text(crate::i18n::tr("Off: the page keeps the size it has in the preview. On: the printer scales it into what it can print."));
        }
        if out.dest == Destination::Network {
            let response = app.print_net.show(ui, photos > 0);
            app.print_out.block = response.block;
        } else {
            app.print_out.block = None;
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, photos, text_button, register);
        ui.label(egui::RichText::new(crate::i18n::tr("Print to a PDF file, then print the PDF from your viewer.")).color(t.text_dim));
    }
}

/// Print was pressed with a printer destination: compose the print on a worker. The pages are handed to the
/// spooler or the network glue by [`poll`] when they are ready.
#[cfg(not(target_arch = "wasm32"))]
pub fn start(app: &mut LightcraftApp) -> Result<Value, String> {
    use lightcraft_engine::print::{compose, output};
    if app.export.is_some() || app.print_out.composing.is_some() {
        return Err(crate::i18n::tr("a print or an export is already running").to_string());
    }
    let to = app.print_out.dest.clone();
    if to == Destination::Pdf {
        return Err("the destination is a PDF file".into());
    }
    if to == Destination::Network && app.print_out.block.is_some() {
        return Err(crate::i18n::tr("Query the printer and tick the confirmation first.").to_string());
    }
    let settings = app.session.print.clone();
    let ids = app.session.targets(&json!({})).map_err(|e| e.to_string())?;
    let prepared = compose::prepare(&mut app.session, &settings, &ids)?;
    let total = prepared.len();
    let title = crate::i18n::tr("LightCraft print").to_string();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    let (tx, rx) = channel();
    let job = settings.job.clone();
    let doc_title = title.clone();
    let work = move || {
        let result = prepared
            .run(&mut |done, name| {
                if let Ok(mut g) = p.lock() {
                    *g = (done, name.to_string());
                }
                !c.load(Ordering::Relaxed)
            })
            .and_then(|pages| {
                if c.load(Ordering::Relaxed) {
                    return Err("Print cancelled".into());
                }
                let pdf = output::write(&pages, &job, &doc_title)?;
                let mut sheets = Vec::with_capacity(pages.len());
                for page in &pages {
                    let rgba = output::page_to_srgb8(page)?;
                    let rgb: Vec<u8> = rgba.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
                    let ppi = f64::from(page.ppi);
                    sheets.push(
                        Sheet::new(page.width, page.height, f64::from(page.width) / ppi, f64::from(page.height) / ppi, rgb)
                            .ok_or("a page has no usable size")?,
                    );
                }
                Ok(Composed { pdf, sheets })
            });
        let _ = tx.send(result);
    };
    std::thread::Builder::new().name("print".into()).spawn(work).map_err(|e| e.to_string())?;
    let guard = app.session.activity.start("export", "Preparing print", Cancel::Flag(cancel));
    guard.progress(0, total as u64);
    let paper = settings.layout.page.paper.clone();
    let fit = if app.print_out.shrink_to_fit { spool::Scaling::ShrinkToFit } else { spool::Scaling::Exact };
    app.print_out.composing = Some(Composing { rx, progress, total, guard, to, paper, title, fit });
    Ok(json!({"background": true, "total": total}))
}

/// The web build prints to a PDF file only.
#[cfg(target_arch = "wasm32")]
pub fn start(_: &mut LightcraftApp) -> Result<Value, String> {
    Err("printing to a device is not available in the browser".into())
}

/// Every frame: collect the printer list, the composed print and the spooler's answers; follow the network job.
#[cfg(not(target_arch = "wasm32"))]
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if let Some(rx) = &app.print_out.printers_rx {
        match rx.try_recv() {
            Ok(printers) => {
                let out = &mut app.print_out;
                out.printers = printers;
                out.printers_rx = None;
                out.printers_loading = false;
            }
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(TryRecvError::Disconnected) => {
                app.print_out.printers_rx = None;
                app.print_out.printers_loading = false;
            }
        }
    }
    poll_composing(app, ctx);
    let mut pending = Vec::new();
    for (printer, rx) in std::mem::take(&mut app.print_out.jobs) {
        match rx.try_recv() {
            Ok(result) => {
                let text = match result {
                    Ok(_) => crate::network_glue::tracked::fill(crate::i18n::tr("Sent to {printer}"), &[("printer", &printer)]),
                    Err(e) => crate::network_glue::tracked::fill(crate::i18n::tr("{printer}: {why}"), &[("printer", &printer), ("why", &e)]),
                };
                app.toast_for(ctx, text, 8.0);
            }
            Err(TryRecvError::Empty) => pending.push((printer, rx)),
            Err(TryRecvError::Disconnected) => {}
        }
    }
    app.print_out.jobs = pending;
    app.poll_network(ctx);
    app.show_print_status(ctx);
}

#[cfg(target_arch = "wasm32")]
pub fn poll(_: &mut LightcraftApp, _: &egui::Context) {}

#[cfg(not(target_arch = "wasm32"))]
fn poll_composing(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.print_out.composing else { return };
    match task.rx.try_recv() {
        Err(TryRecvError::Empty) => {
            let (done, current) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
            task.guard.progress(done as u64, task.total as u64);
            task.guard.detail(&current);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        Err(TryRecvError::Disconnected) => {
            app.print_out.composing = None;
            app.toast(ctx, crate::i18n::tr("The print stopped unexpectedly"));
        }
        Ok(result) => {
            let Some(task) = app.print_out.composing.take() else { return };
            match result {
                Err(e) => app.toast(ctx, e),
                Ok(composed) => deliver(app, ctx, task, composed),
            }
        }
    }
}

/// The composed print goes where it was asked to.
#[cfg(not(target_arch = "wasm32"))]
fn deliver(app: &mut LightcraftApp, ctx: &egui::Context, task: Composing, composed: Composed) {
    match task.to {
        Destination::Pdf => {}
        Destination::System(printer) => {
            let job = spool::Job { printer: Some(printer.clone()), title: task.title.clone(), scaling: task.fit, ..spool::Job::default() };
            let (tx, rx) = channel();
            let pdf = composed.pdf;
            let spawned = std::thread::Builder::new().name("spool".into()).spawn(move || {
                let _ = tx.send(spool::submit(&pdf, &job).map_err(|e| e.to_string()));
            });
            match spawned {
                Ok(_) => app.print_out.jobs.push((printer, rx)),
                Err(e) => app.toast(ctx, e.to_string()),
            }
        }
        Destination::Network => {
            let payload =
                crate::network_glue::Payload { name: task.title, pdf: Arc::new(composed.pdf), sheets: Arc::new(composed.sheets), fit: task.fit };
            if !app.network_send(Some(ctx), payload, &task.paper) {
                app.toast(ctx, crate::i18n::tr("The print was not sent: query the printer and tick the confirmation."));
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
