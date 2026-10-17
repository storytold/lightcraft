//! Where a print goes: a PDF file, a printer of this computer (the system spooler) or a network printer
//! (WS-Print or IPP). The Print dialog (`print_view`) draws the choice as one Printer list (the PDF file, the
//! system's printers, then "Network printer…" and the network printers this session has queried); this module
//! owns what lies behind it.
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
    /// Composed gray: the driver is asked for monochrome too.
    gray: bool,
}

/// A job handed to the system spooler: the printer and the spooler's answer.
type SpoolJob = (String, Receiver<Result<String, String>>);

/// Lists the system's printers (the spooler, or a fake in tests).
type Lister = fn() -> Vec<spool::Printer>;
/// Hands a PDF to the system spooler (the spooler, or a fake in tests).
type Submitter = fn(&[u8], &spool::Job) -> Result<String, String>;

#[cfg(not(test))]
fn spooler_submit(pdf: &[u8], job: &spool::Job) -> Result<String, String> {
    spool::submit(pdf, job).map_err(|e| e.to_string())
}

/// What the Print dialog remembers about where the print goes (not saved).
pub struct PrintOut {
    lister: Lister,
    submitter: Submitter,
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
    /// Network printers queried this session, by the name they reported and the address typed: one more entry
    /// each in the Printer list, so the same printer can be chosen again without retyping. Not saved.
    remembered: Vec<(String, String)>,
}

/// The system spooler; tests never reach it (they get the in-process fakes of `tests`).
#[cfg(not(test))]
fn default_spooler() -> (Lister, Submitter) {
    (spool::printers, spooler_submit)
}

#[cfg(test)]
fn default_spooler() -> (Lister, Submitter) {
    (tests::fake_printers, tests::fake_submit)
}

impl Default for PrintOut {
    fn default() -> Self {
        let (lister, submitter) = default_spooler();
        Self {
            lister,
            submitter,
            dest: Destination::default(),
            shrink_to_fit: false,
            printers: Vec::new(),
            printers_loading: false,
            printers_rx: None,
            composing: None,
            jobs: Vec::new(),
            block: None,
            remembered: Vec::new(),
        }
    }
}

/// One line of the Printer list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A PDF file the person chooses.
    Pdf,
    /// A printer of this computer, and whether it is the system default.
    System { name: String, default: bool },
    /// "Network printer…": the address field and the query.
    Network,
    /// A network printer queried earlier in this session: its reported name and the address it answered at.
    Remembered { name: String, address: String },
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
        self.refresh_printers();
    }

    /// The Printer list: the PDF file, the system printers (the default first), "Network printer…", then the
    /// network printers queried before in this session.
    pub fn entries(&self) -> Vec<Entry> {
        let mut list = vec![Entry::Pdf];
        let mut printers: Vec<&spool::Printer> = self.printers.iter().collect();
        printers.sort_by_key(|p| !p.default); // (stable: the others keep the spooler's order)
        list.extend(printers.into_iter().map(|p| Entry::System { name: p.name.clone(), default: p.default }));
        list.push(Entry::Network);
        list.extend(self.remembered.iter().map(|(name, address)| Entry::Remembered { name: name.clone(), address: address.clone() }));
        list
    }

    /// Re-read the system's printers in the background (the "Refresh printers" button).
    pub fn refresh_printers(&mut self) {
        if self.printers_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.printers_loading = true;
        self.printers_rx = Some(rx);
        let lister = self.lister;
        let spawned = std::thread::Builder::new().name("printers".into()).spawn(move || {
            let _ = tx.send(lister());
        });
        if spawned.is_err() {
            self.printers_rx = None;
            self.printers_loading = false;
        }
    }

    /// Note a queried network printer for the list (by address; a new answer renames the entry).
    fn remember(&mut self, name: &str, address: &str) {
        let key = craft_ui::printing::network::normalize_url(address);
        if name.trim().is_empty() || key.is_empty() {
            return;
        }
        match self.remembered.iter_mut().find(|(_, a)| craft_ui::printing::network::normalize_url(a) == key) {
            Some(entry) => entry.0 = name.to_owned(),
            None => self.remembered.push((name.to_owned(), address.to_owned())),
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

/// How a job to a printer treats the page size. A named paper the person picked in the dialog (A4, Letter, 4 x 6…) is
/// demanded exactly, so a queue that lacks it refuses rather than guesses. A size no paper tray lists (the custom
/// size) was not chosen from the queue's papers: the job goes on the queue's own default paper, centred and shrunk to
/// fit. The "Shrink to the printer's printable area" box asks for the same with any paper.
pub(crate) fn scaling_for(paper: &str, shrink_to_fit: bool) -> spool::Scaling {
    if shrink_to_fit || media_for_paper(paper).is_none() { spool::Scaling::ShrinkToFit } else { spool::Scaling::Exact }
}

/// Is this print grayscale? The dialog's box, or the network printer's Color setting set to Grayscale: the pages are
/// composed gray (and the preview shows them so) and the driver is asked for monochrome as well.
pub(crate) fn grayscale_wanted(app: &LightcraftApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    if app.print_out.dest == Destination::Network && app.print_net.state.draft.color == Some(craft_ui::printing::network::ColorMode::Monochrome) {
        return true;
    }
    app.session.print.job.grayscale
}

/// The label of a Printer list entry.
#[cfg(not(target_arch = "wasm32"))]
fn entry_label(e: &Entry) -> String {
    match e {
        Entry::Pdf => crate::i18n::tr("PDF file").to_string(),
        Entry::System { name, default: true } => crate::network_glue::tracked::fill(crate::i18n::tr("{printer} (default)"), &[("printer", name)]),
        Entry::System { name, .. } => name.clone(),
        Entry::Network => crate::i18n::tr("Network printer…").to_string(),
        Entry::Remembered { name, .. } => name.clone(),
    }
}

/// Is `e` what is chosen now?
#[cfg(not(target_arch = "wasm32"))]
fn entry_selected(app: &LightcraftApp, e: &Entry) -> bool {
    let out = &app.print_out;
    match (e, &out.dest) {
        (Entry::Pdf, Destination::Pdf) => true,
        (Entry::System { name, .. }, Destination::System(current)) => name == current,
        (Entry::Network, Destination::Network) => !out.remembered.iter().any(|(_, a)| same_address(a, &app.print_net.state.draft.url)),
        (Entry::Remembered { address, .. }, Destination::Network) => same_address(address, &app.print_net.state.draft.url),
        _ => false,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn same_address(a: &str, b: &str) -> bool {
    craft_ui::printing::network::normalize_url(a) == craft_ui::printing::network::normalize_url(b)
}

/// Choose a Printer list entry. A remembered network printer fills in its address (nothing is contacted: Query
/// printer asks it, and the confirmation is still to be ticked).
#[cfg(not(target_arch = "wasm32"))]
pub fn choose(app: &mut LightcraftApp, e: &Entry) {
    match e {
        Entry::Pdf => app.print_out.dest = Destination::Pdf,
        Entry::System { name, .. } => app.print_out.dest = Destination::System(name.clone()),
        Entry::Network => {
            // the address last typed stays for the session; it is not a remembered printer's
            if app.print_out.remembered.iter().any(|(_, a)| same_address(a, &app.print_net.state.draft.url)) {
                app.print_net.state.draft = craft_ui::printing::network::Draft::default();
            }
            app.print_out.dest = Destination::Network;
        }
        Entry::Remembered { address, .. } => {
            if !same_address(address, &app.print_net.state.draft.url) {
                app.print_net.state.draft = craft_ui::printing::network::Draft::for_address(address);
            }
            app.print_out.dest = Destination::Network;
        }
    }
}

/// The Grayscale box: the job's setting, which the pages, the PDF and the preview follow.
fn grayscale_box(app: &mut LightcraftApp, ui: &mut Ui) {
    let gray = app.session.print.job.grayscale;
    let r = ui.checkbox(&mut { gray }, crate::i18n::tr("Grayscale"));
    register(ui.ctx(), "check:printGrayscale", r.rect);
    if r.clicked()
        && let Err(e) = app.run("print.set", json!({"job": {"grayscale": !gray}}))
    {
        app.toast(&ui.ctx().clone(), e);
    }
    r.on_hover_text(crate::i18n::tr("The pages and the preview print in shades of gray."));
}

/// The "Print To" section at the top of the Print dialog's settings.
pub fn section(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, photos: usize) {
    ui.add_space(8.0);
    #[cfg(not(target_arch = "wasm32"))]
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Print To")).font(t.semibold(12.0)).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if text_button(ui, "printRefreshPrinters", "Refresh printers", false).clicked() {
                app.print_out.refresh_printers();
            }
        });
    });
    #[cfg(target_arch = "wasm32")]
    ui.label(egui::RichText::new(crate::i18n::tr("Print To")).font(t.semibold(12.0)).color(t.text));
    ui.add_space(2.0);
    #[cfg(not(target_arch = "wasm32"))]
    {
        if app.print_out.printers_loading {
            ui.label(egui::RichText::new(crate::i18n::tr("Looking for printers…")).color(t.text_dim));
        }
        let entries = app.print_out.entries();
        let selected_text = entries.iter().find(|e| entry_selected(app, e)).map_or_else(String::new, entry_label);
        let mut picked = None;
        ui.horizontal(|ui| {
            let combo = egui::ComboBox::from_id_salt("printDestination").width(150.0).selected_text(selected_text).show_ui(ui, |ui| {
                for (i, e) in entries.iter().enumerate() {
                    let r = ui.selectable_label(entry_selected(app, e), entry_label(e));
                    register(ui.ctx(), format!("item:printDestination-{i}"), r.rect);
                    if r.clicked() {
                        picked = Some(e.clone());
                    }
                }
            });
            register(ui.ctx(), "combo:printDestination", combo.response.rect);
            grayscale_box(app, ui);
        });
        if let Some(e) = picked {
            choose(app, &e);
        }
        if app.print_out.printers.is_empty() && !app.print_out.printers_loading {
            ui.label(egui::RichText::new(crate::i18n::tr("No printers found on this computer.")).color(t.text_dim));
        }
        let out = &mut app.print_out;
        if out.dest != Destination::Pdf {
            let r = ui.checkbox(&mut out.shrink_to_fit, crate::i18n::tr("Shrink to the printer's printable area"));
            register(ui.ctx(), "check:printShrinkToFit", r.rect);
            r.on_hover_text(crate::i18n::tr("Off: the page keeps the size it has in the preview. On: the printer scales it into what it can print."));
        }
        if app.print_out.dest == Destination::Network {
            let response = app.print_net.show(ui, photos > 0);
            app.print_out.block = response.block;
            if let Some(p) = app.print_net.state.draft.printer() {
                let (name, address) = (p.name.clone(), app.print_net.state.draft.url.clone());
                app.print_out.remember(&name, &address);
            }
        } else {
            app.print_out.block = None;
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (photos, text_button);
        ui.label(egui::RichText::new(crate::i18n::tr("Print to a PDF file, then print the PDF from your viewer.")).color(t.text_dim));
        grayscale_box(app, ui);
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
    let mut settings = app.session.print.clone();
    settings.job.grayscale = grayscale_wanted(app);
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
    let fit = scaling_for(&paper, app.print_out.shrink_to_fit);
    let gray = settings.job.grayscale;
    app.print_out.composing = Some(Composing { rx, progress, total, guard, to, paper, title, fit, gray });
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
            let job = spool::Job {
                printer: Some(printer.clone()),
                title: task.title.clone(),
                scaling: task.fit,
                grayscale: task.gray,
                ..spool::Job::default()
            };
            let (tx, rx) = channel();
            let pdf = composed.pdf;
            let submit = app.print_out.submitter;
            let spawned = std::thread::Builder::new().name("spool".into()).spawn(move || {
                let _ = tx.send(submit(&pdf, &job));
            });
            match spawned {
                Ok(_) => app.print_out.jobs.push((printer, rx)),
                Err(e) => app.toast(ctx, e.to_string()),
            }
        }
        Destination::Network => {
            let payload = crate::network_glue::Payload {
                name: task.title,
                pdf: Arc::new(composed.pdf),
                sheets: Arc::new(composed.sheets),
                fit: task.fit,
                gray: task.gray,
            };
            if !app.network_send(Some(ctx), payload, &task.paper) {
                app.toast(ctx, crate::i18n::tr("The print was not sent: query the printer and tick the confirmation."));
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
