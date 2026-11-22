//! The Plug-in Marketplace window (File ▸ Plug-ins ▸ Plug-in Marketplace…): the plug-ins and pixel
//! filters the marketplace lists, searched and filtered by tier, each with the platforms it
//! supports and what it may ask for, and Install, Update, Remove and automatic updates.
//!
//! The list is the engine's (`marketplace.list`); the window fetches it again every time it opens,
//! so it always shows each plug-in's newest release. Everything that waits for the network runs
//! on a worker with a row in the activity stack; installing runs here, where the session is.

use std::sync::Mutex;

use lcc_engine::activity::{Cancel, TaskGuard};
use lcc_engine::cmd::marketplace::{self as market, Download, InstallAs};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};

/// The question a module not signed by the key pinned for the installed plug-in asks.
#[derive(Clone, Debug, PartialEq)]
struct SignerAsk {
    id: String,
    name: String,
    reason: String,
}

#[derive(Clone, Debug, Default)]
struct State {
    open: bool,
    /// The list was fetched for this opening of the window.
    fetched_open: bool,
    query: String,
    selected: Option<String>,
    message: String,
    ask: Option<SignerAsk>,
    /// The entry whose Remove is being confirmed.
    removing: Option<String>,
    /// When the automatic pass last looked at whether it should run.
    auto_looked: Option<std::time::Instant>,
}

fn key() -> egui::Id {
    egui::Id::new("plugin-marketplace")
}

#[cfg(not(test))]
static STATE: Mutex<Option<State>> = Mutex::new(None);

// Under test every test has a window of its own, as the Plug-in Manager's.
#[cfg(test)]
thread_local! {
    static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) };
    /// Whether this test lets the window reach its (served) network: without it, opening the
    /// window fetches nothing and the automatic pass never starts, so no other test ever
    /// contacts the real marketplace.
    pub(crate) static LIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Lets the automatic pass look again at whether it is due, as a restart of the app would.
#[cfg(test)]
pub(crate) fn look_again() {
    let mut s = state();
    s.auto_looked = None;
    set_state(s);
}

fn live() -> bool {
    #[cfg(test)]
    return LIVE.get();
    #[cfg(not(test))]
    true
}

fn state() -> State {
    #[cfg(not(test))]
    let s = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    #[cfg(test)]
    let s = STATE.with_borrow(Clone::clone);
    s.unwrap_or_default()
}

fn set_state(s: State) {
    #[cfg(not(test))]
    {
        *STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(s);
    }
    #[cfg(test)]
    STATE.set(Some(s));
}

fn app_ctx(app: &LccApp) -> egui::Context {
    app.tasks.repaint.clone().unwrap_or_default()
}

/// UI commands: `plugins.marketplace {open?, query?, id?}` opens / closes the window, optionally
/// with a search typed and an entry selected; `plugins.marketplaceLink {id?, link}` opens an
/// entry's `homepage`, `repository` or `changelog`, or (no `id`) the marketplace's own page.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let ctx = app_ctx(app);
    let mut s = state();
    let r = match id {
        "plugins.marketplace" => {
            s.open = p.get("open").and_then(Value::as_bool).unwrap_or(!s.open);
            if let Some(q) = p.get("query").and_then(Value::as_str) {
                s.query = q.chars().take(200).collect();
            }
            if let Some(id) = p.get("id").and_then(Value::as_str) {
                s.selected = Some(id.to_string());
            }
            Ok(json!({"open": s.open, "query": s.query, "selected": s.selected}))
        }
        "plugins.marketplaceLink" => open_link(app, p),
        _ => return None,
    };
    set_state(s);
    ctx.request_repaint();
    Some(r)
}

/// Field `key` of JSON object `v`, `Null` when it has none: a read that can't panic.
fn at<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

/// Whether field `key` of `v` is `true`.
fn is(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

/// Field `key` of the first update an update pass installed.
fn first_updated<'a>(r: &'a Value, key: &str) -> Option<&'a str> {
    r.get("updated")?.get(0)?.get(key)?.as_str()
}

const LINKS: &[&str] = &["homepage", "repository", "changelog"];

fn open_link(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let link = p.get("link").and_then(Value::as_str).unwrap_or("source");
    let list = market::listing(&json!({"community": true}));
    let url = match p.get("id").and_then(Value::as_str) {
        None => at(&list, "source").as_str().map(str::to_string),
        Some(id) if LINKS.contains(&link) => at(&list, "entries")
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| at(e, "id").as_str() == Some(id))
            .and_then(|e| at(e, link).as_str())
            .map(str::to_string),
        Some(_) => return Err(format!("plugins.marketplaceLink: no link {link:?} (homepage, repository or changelog)")),
    };
    let url = url.ok_or_else(|| tr("The marketplace has no such link").to_string())?;
    crate::links::open(app, &url)
}

/// Work on the marketplace's worker ([`start`]).
enum Done {
    Refreshed(Result<(), String>),
    /// An entry's module downloaded (or why not), to install here.
    Downloaded {
        id: String,
        how: InstallAs,
        download: Result<Box<Download>, String>,
    },
    /// The updates of a pass downloaded, to install here; `auto`: the automatic pass.
    Updates {
        auto: bool,
        downloads: Vec<Result<Download, (String, String)>>,
    },
}

struct Task {
    done: std::sync::mpsc::Receiver<Done>,
    /// Its row in the activity stack, while it runs.
    _guard: TaskGuard,
}

#[cfg(not(test))]
static TASK: Mutex<Option<Task>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    static TASK: &'static Mutex<Option<Task>> = Box::leak(Box::new(Mutex::new(None)));
}

fn task_lock() -> std::sync::MutexGuard<'static, Option<Task>> {
    #[cfg(not(test))]
    let task = &TASK;
    #[cfg(test)]
    let task = TASK.with(|t| *t);
    task.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether the marketplace is fetching or downloading (`ui.inspect`, tests).
pub fn running() -> bool {
    task_lock().is_some()
}

/// Runs `work` on a worker with an activity row `label`, one at a time.
fn start(app: &mut LccApp, ctx: &egui::Context, label: &str, work: impl FnOnce() -> Done + Send + 'static) -> Result<(), String> {
    let mut g = task_lock();
    if g.is_some() {
        return Err(tr("The marketplace is busy; try again in a moment").to_string());
    }
    let (tx, done) = std::sync::mpsc::channel();
    let repaint = ctx.clone();
    std::thread::Builder::new()
        .name("plugin-marketplace".into())
        .spawn(move || {
            let _ = tx.send(work());
            repaint.request_repaint();
        })
        .map_err(|e| e.to_string())?;
    let guard = app.session.activity.start("plugins", label, Cancel::No);
    *g = Some(Task { done, _guard: guard });
    Ok(())
}

fn start_refresh(app: &mut LccApp, ctx: &egui::Context) -> Result<(), String> {
    start(app, ctx, "Fetching the plug-in marketplace", || Done::Refreshed(market::refresh()))
}

fn start_install(app: &mut LccApp, ctx: &egui::Context, id: String, how: InstallAs) -> Result<(), String> {
    start(app, ctx, "Downloading a plug-in", move || {
        let download = market::download(&id).map(Box::new);
        Done::Downloaded { id, how, download }
    })
}

/// The automatic pass: the list fetched again, then the updates that may install unasked.
fn start_auto(app: &mut LccApp, ctx: &egui::Context) -> Result<(), String> {
    start(app, ctx, "Checking plug-ins for updates", || {
        if let Err(e) = market::refresh() {
            log::info!("marketplace: the automatic update pass fetched nothing: {e}");
            return Done::Updates { auto: true, downloads: Vec::new() };
        }
        Done::Updates { auto: true, downloads: market::download_updates(true) }
    })
}

/// How often the app looks at whether the automatic pass is due while it runs.
const AUTO_LOOK: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Per frame: starts the automatic update pass when it is due (once when the app starts, then a
/// day after the last fetch, and only while a plug-in is set to update by itself), and takes a
/// finished worker's result: installs what it downloaded and says what happened.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let mut s = state();
    let first = s.auto_looked.is_none();
    if live() && s.auto_looked.is_none_or(|at| at.elapsed() >= AUTO_LOOK) && !running() {
        s.auto_looked = Some(std::time::Instant::now());
        if market::auto_update_wanted()
            && (first || market::refresh_due())
            && let Err(e) = start_auto(app, ctx)
        {
            log::info!("marketplace: the automatic update pass didn't start: {e}");
        }
        set_state(s.clone());
    }
    let done = {
        let mut g = task_lock();
        let Some(task) = g.as_ref() else { return };
        let done = match task.done.try_recv() {
            Ok(d) => d,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Done::Refreshed(Err(tr("the plug-in worker stopped unexpectedly").to_string())),
        };
        *g = None;
        done
    };
    match done {
        Done::Refreshed(Ok(())) => {}
        // the list says why under its date: no second copy here
        Done::Refreshed(Err(e)) => log::info!("marketplace: {e}"),
        Done::Downloaded { download: Err(e), .. } => s.message = e,
        Done::Downloaded { id, how, download: Ok(d) } => match market::install_downloaded(&mut app.session, &d, &how) {
            Ok(v) if is(&v, "installed") => {
                let (name, version) = (d.entry.name.as_str(), d.entry.version.as_str());
                app.toast(
                    ctx,
                    match at(&v, "from").as_str() {
                        Some(_) => tr_format!("Updated {name} to version {version}", name = name, version = version),
                        None => tr_format!("Installed {name} {version}", name = name, version = version),
                    },
                );
            }
            Ok(v) => s.ask = Some(SignerAsk { id, name: d.entry.name.clone(), reason: at(&v, "reason").as_str().unwrap_or_default().to_string() }),
            Err(e) => s.message = e.to_string(),
        },
        Done::Updates { auto, downloads } => {
            let r = market::install_updates(&mut app.session, &downloads, auto);
            let updated = at(&r, "updated").as_array().map_or(0, Vec::len);
            match (updated, first_updated(&r, "name"), first_updated(&r, "version")) {
                (0, ..) => {}
                (1, Some(name), Some(version)) => app.toast(ctx, tr_format!("Updated {name} to version {version}", name = name, version = version)),
                (n, ..) => app.toast(ctx, tr_format!("Updated {count} plug-ins", count = n)),
            }
            // an update the user asked for and didn't get says why; the automatic pass keeps its
            // reasons on each entry's page
            if !auto && let Some(why) = at(&r, "waiting").get(0).and_then(|w| w.get("why")).and_then(Value::as_str) {
                s.message = why.to_string();
            }
        }
    }
    set_state(s);
    ctx.request_repaint();
}

const LIST_W: f32 = 270.0;
const BODY_H: f32 = 460.0;
const MIN_PAGE_W: f32 = 380.0;

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    let mut s = state();
    if !s.open {
        if s.fetched_open || s.ask.is_some() {
            s.fetched_open = false;
            s.ask = None;
            set_state(s);
        }
        return;
    }
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    if !s.fetched_open {
        // every opening fetches the list as it is now: nothing is pinned
        s.fetched_open = true;
        if live() {
            actions.push(("market.refresh", Value::Null));
        }
    }
    let list = market::listing(&json!({"query": s.query}));
    let entries: Vec<&Value> = at(&list, "entries").as_array().into_iter().flatten().collect();
    if !entries.iter().any(|e| s.selected.as_deref() == at(e, "id").as_str()) {
        s.selected = entries.first().and_then(|e| at(e, "id").as_str()).map(str::to_string);
        s.removing = None;
    }
    let selected = entries.iter().find(|e| s.selected.as_deref() == at(e, "id").as_str()).copied();
    let busy = running();
    let open = !crate::dialog_frame::esc_closes(app.ui.dialog.is_some(), ctx);
    let mut closed = false;
    egui::Window::new(tr("Plug-in Marketplace"))
        .id(key())
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .default_width(LIST_W + 580.0)
        .min_width(LIST_W + MIN_PAGE_W)
        .show(ctx, |ui| {
            use crate::button::{Button, Kind};
            if crate::dialog_frame::card_header(ui, tr("Plug-in Marketplace"), Some("plugin-marketplace:close")) {
                closed = true;
            }
            if !is(&list, "available") {
                ui.label(tr("This build has no plug-in marketplace. Plug-ins install from files in the Plug-in Manager."));
                return;
            }
            ui.horizontal(|ui| {
                let field = crate::text_field::TextField::singleline("marketplaceSearch", &mut s.query).hint(tr("Search plug-ins")).width(LIST_W - 8.0);
                field.show(ui);
                let updates = at(&list, "updates").as_u64().unwrap_or(0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if Button::new("Refresh").busy(busy).enabled(!busy).widget("button:marketplaceRefresh").show(ui).clicked() {
                        actions.push(("market.refresh", Value::Null));
                    }
                    if updates > 0 && Button::new("Update All").kind(Kind::Primary).enabled(!busy).widget("button:marketplaceUpdateAll").show(ui).clicked() {
                        actions.push(("market.updateAll", Value::Null));
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                let mut community = at(&list, "community").as_bool().unwrap_or(false);
                let c = ui.checkbox(&mut community, tr("Show community plug-ins (higher risk)"));
                crate::widgets::register(ui.ctx(), "marketplaceCommunity", c.rect);
                if c.changed() {
                    actions.push(("marketplace.setCommunity", json!({"on": community})));
                }
                ui.add_space(12.0);
                let mut auto = at(&list, "autoUpdate").as_bool().unwrap_or(false);
                let a = ui.checkbox(&mut auto, tr("Update installed plug-ins automatically"));
                crate::widgets::register(ui.ctx(), "marketplaceAutoUpdate", a.rect);
                if a.changed() {
                    actions.push(("marketplace.setAutoUpdate", json!({"on": auto})));
                }
            });
            if is(&list, "community") {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    tr("Community plug-ins passed automatic checks only: nobody has reviewed what they do. Install one only if you trust its author."),
                );
            }
            if !s.message.is_empty() {
                ui.colored_label(ui.visuals().warn_fg_color, &s.message);
            }
            if let Some(err) = at(&list, "error").as_str() {
                ui.colored_label(ui.visuals().warn_fg_color, tr_format!("Couldn't fetch the marketplace: {error}", error = err));
            }
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(LIST_W, BODY_H), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(egui::vec2(LIST_W, BODY_H));
                    egui::ScrollArea::vertical().id_salt("marketplace-list").auto_shrink([false, false]).max_height(BODY_H).show(ui, |ui| {
                        list_ui(ui, &list, &entries, s.selected.as_deref(), busy, &mut actions);
                    });
                });
                let (line, _) = ui.allocate_exact_size(egui::vec2(9.0, BODY_H), egui::Sense::hover());
                ui.painter().vline(line.center().x, line.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                let w = ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(w, BODY_H), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_height(BODY_H);
                    ui.set_max_width(w);
                    if let Some(e) = selected {
                        egui::ScrollArea::vertical().id_salt("marketplace-page").auto_shrink([false, false]).max_height(BODY_H).show(ui, |ui| {
                            page_ui(ui, e, at(&list, "platform").as_str().unwrap_or(""), busy, &mut s.removing, &mut actions);
                        });
                    }
                });
            });
            ui.separator();
            footer_ui(ui, &list, &mut actions);
        });
    s.open = open && !closed;
    signer_ask_ui(ctx, &mut s, &mut actions);
    for (id, p) in actions {
        s.message.clear();
        let r: Result<(), String> = match id {
            "market.select" => {
                s.selected = at(&p, "id").as_str().map(str::to_string);
                s.removing = None;
                Ok(())
            }
            "market.refresh" => start_refresh(app, ctx),
            // the user's own click, unless the control channel made it (then as the command)
            "market.install" => {
                let how =
                    InstallAs { by_user: !app.synthetic_frame, any_signer: is(&p, "anySigner") && !app.synthetic_frame, ..InstallAs::default() };
                start_install(app, ctx, at(&p, "id").as_str().unwrap_or("").to_string(), how)
            }
            "market.updateAll" => {
                start(app, ctx, "Downloading plug-in updates", || Done::Updates { auto: false, downloads: market::download_updates(false) })
            }
            "market.askRemove" => {
                s.removing = at(&p, "id").as_str().map(str::to_string);
                Ok(())
            }
            "market.manager" => crate::panels::plugins::run(app, "plugins.page", &json!({"id": at(&p, "id")})).unwrap_or(Ok(Value::Null)).map(|_| ()),
            "market.link" => open_link(app, &p).map(|_| ()),
            _ => app.session.execute(id, &p).map(|_| ()).map_err(|e| e.to_string()),
        };
        if let Err(e) = r {
            s.message = e;
        }
    }
    set_state(s);
}

fn list_ui(ui: &mut egui::Ui, list: &Value, entries: &[&Value], selected: Option<&str>, busy: bool, actions: &mut Vec<(&'static str, Value)>) {
    if entries.is_empty() {
        ui.add_space(8.0);
        let text = if at(list, "fetchedAt").is_null() {
            if busy { tr("Fetching the list…") } else { tr("The list hasn't been fetched yet. Press Refresh.") }
        } else if at(list, "total").as_u64() == Some(0) {
            tr("The marketplace lists nothing yet.")
        } else {
            tr("No plug-ins match.")
        };
        ui.weak(text);
    }
    for e in entries {
        let id = at(e, "id").as_str().unwrap_or("");
        if entry_row(ui, e, selected == Some(id)) {
            actions.push(("market.select", json!({"id": id})));
        }
    }
    if let Some(n) = at(list, "hiddenCommunity").as_u64().filter(|n| *n > 0) {
        ui.add_space(6.0);
        ui.weak(tr_format!("{count} community plug-ins aren't shown.", count = n));
    }
}

/// An entry's state here, as its row and page word it.
fn status(e: &Value) -> Option<&'static str> {
    if is(e, "update") {
        Some(tr("Update"))
    } else if !at(e, "installed").is_null() {
        Some(tr("Installed"))
    } else if !is(e, "supported") {
        Some(tr("Not for this system"))
    } else {
        None
    }
}

/// The tier's mark, drawn by the app: a filled dot with a tick for a vetted entry, an open ring
/// for a community one.
fn paint_tier(painter: &egui::Painter, c: egui::Pos2, vetted: bool, t: &crate::theme::Tokens) {
    if vetted {
        painter.circle_filled(c, 5.5, t.accent);
        let tick = vec![c + egui::vec2(-2.6, 0.2), c + egui::vec2(-0.8, 2.0), c + egui::vec2(2.8, -1.9)];
        painter.add(egui::Shape::line(tick, egui::Stroke::new(1.4, t.chrome)));
    } else {
        painter.circle_stroke(c, 4.5, egui::Stroke::new(1.4, t.caution));
    }
}

/// One row of the list (`marketplaceEntry:<id>`): icon, tier mark, name, and its version or state.
fn entry_row(ui: &mut egui::Ui, e: &Value, selected: bool) -> bool {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (id, name) = (at(e, "id").as_str().unwrap_or(""), at(e, "name").as_str().unwrap_or(""));
    let vetted = at(e, "tier").as_str() == Some("vetted");
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 42.0), egui::Sense::click());
    crate::widgets::register(ui.ctx(), format!("marketplaceEntry:{id}"), rect);
    let tier = if vetted { tr("Vetted") } else { tr("Community") };
    crate::widgets::announce_selected(&resp, format!("{name} ({tier})"), selected);
    let fill = if selected {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let icon = egui::Rect::from_min_size(rect.min + egui::vec2(5.0, 7.0), egui::vec2(28.0, 28.0));
    super::plugin_icon::paint_for(ui, icon, id);
    paint_tier(ui.painter(), egui::pos2(icon.right() - 1.0, icon.bottom() - 1.0), vetted, &t);
    let left = icon.right() + 10.0;
    let right_text = status(e).unwrap_or_else(|| at(e, "version").as_str().unwrap_or(""));
    let right_color = if is(e, "update") { t.accent } else { t.text_dim };
    let right = ui.painter().text(
        egui::pos2(rect.right() - 6.0, rect.top() + 13.0),
        egui::Align2::RIGHT_CENTER,
        right_text,
        egui::FontId::proportional(11.5),
        right_color,
    );
    let name_clip = egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(right.left() - 6.0, rect.center().y + 1.0));
    let painter = ui.painter();
    painter.with_clip_rect(name_clip).text(
        egui::pos2(left, rect.top() + 13.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        t.text,
    );
    let sub_clip = egui::Rect::from_min_max(egui::pos2(left, rect.center().y), egui::pos2(rect.right() - 6.0, rect.bottom()));
    let summary = at(e, "summary").as_str().filter(|s| !s.is_empty()).or(at(e, "author").as_str()).unwrap_or("");
    painter.with_clip_rect(sub_clip).text(
        egui::pos2(left, rect.bottom() - 12.0),
        egui::Align2::LEFT_CENTER,
        summary,
        egui::FontId::proportional(11.0),
        t.text_dim,
    );
    resp.clicked()
}

/// A small rounded label.
fn chip(ui: &mut egui::Ui, text: &str, fill: egui::Color32, fg: egui::Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(11.5), fg);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + egui::vec2(14.0, 6.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 9.0, fill);
    ui.painter().galley(rect.min + egui::vec2(7.0, 3.0), galley, fg);
    resp
}

/// A platform as people call it. These are names, the same in every language.
fn platform_name(p: &str) -> &str {
    match p {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "android" => "Android",
        other => other,
    }
}

/// The platforms the app runs plug-ins on, in the order the page shows them.
const SHOWN_PLATFORMS: &[&str] = &["macos", "windows", "linux", "android"];

fn strings(v: &Value) -> Vec<&str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// What an entry may ask the user for, one line each, as the Plug-in Manager words permissions.
fn asks(e: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for g in strings(at(e, "permissions")) {
        let values = match g {
            "network" => strings(at(e, "network")),
            "fs" => strings(at(e, "fs")),
            "programs" => strings(at(e, "programs")),
            _ => Vec::new(),
        };
        let label = super::plugins::group_label(g);
        if values.is_empty() {
            out.push(label.to_string());
        } else {
            out.push(format!("{label}: {}", values.join(", ")));
        }
    }
    out
}

/// The selected entry's page.
fn page_ui(ui: &mut egui::Ui, e: &Value, platform: &str, busy: bool, removing: &mut Option<String>, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::{Button, Kind};
    let t = crate::theme::Tokens::get(ui.ctx());
    let text = |key: &str| e[key].as_str().unwrap_or("");
    let (id, name, version) = (text("id"), text("name"), text("version"));
    let vetted = at(e, "tier").as_str() == Some("vetted");
    let filter = at(e, "kind").as_str() == Some("filter");
    let installed = at(e, "installed").as_str();
    ui.horizontal(|ui| {
        let (icon, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
        super::plugin_icon::paint_for(ui, icon, id);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(name).font(t.semibold(16.0)).color(t.text));
            let by = match text("author") {
                "" => tr_format!("Version {version}", version = version),
                author => format!("{} · {author}", tr_format!("Version {version}", version = version)),
            };
            ui.weak(by);
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let tier =
            if vetted { chip(ui, tr("Vetted"), t.accent, t.chrome) } else { chip(ui, tr("Community"), t.caution.gamma_multiply(0.25), t.text) };
        crate::widgets::register(ui.ctx(), format!("marketplaceTier:{id}"), tier.rect);
        chip(ui, if filter { tr("Pixel filter") } else { tr("Plug-in") }, t.inset, t.text_label);
    });
    let reviewed = match (vetted, text("reviewedBy"), text("reviewedOn")) {
        (true, "", _) => tr("A marketplace maintainer reviewed its source.").to_string(),
        (true, by, "") => tr_format!("Reviewed by {reviewer}, a marketplace maintainer.", reviewer = by),
        (true, by, on) => tr_format!("Reviewed by {reviewer}, a marketplace maintainer, on {date}.", reviewer = by, date = on),
        (false, ..) => tr("Passed automatic checks only. Nobody has reviewed what it does.").to_string(),
    };
    ui.label(egui::RichText::new(reviewed).color(if vetted { t.text_label } else { ui.visuals().warn_fg_color }));
    ui.add_space(6.0);
    if let Some(w) = at(e, "waiting").as_object() {
        let why = w.get("why").and_then(Value::as_str).unwrap_or("");
        ui.colored_label(ui.visuals().warn_fg_color, tr_format!("Not updated automatically: {reason}", reason = why));
    }
    if is(e, "conflict") {
        ui.colored_label(ui.visuals().warn_fg_color, tr("Another kind of module with this id is installed. Remove it in the Plug-in Manager first."));
    }
    ui.horizontal_wrapped(|ui| {
        let can = is(e, "installable") && !is(e, "conflict") && !busy;
        match (installed, is(e, "update")) {
            (None, _) => {
                if Button::new("Install")
                    .kind(Kind::Primary)
                    .enabled(can)
                    .busy(busy)
                    .widget(format!("button:marketplaceInstall:{id}"))
                    .show(ui)
                    .clicked()
                {
                    actions.push(("market.install", json!({"id": id})));
                }
            }
            (Some(_), true) => {
                if Button::new("Update")
                    .kind(Kind::Primary)
                    .enabled(can)
                    .busy(busy)
                    .widget(format!("button:marketplaceUpdate:{id}"))
                    .show(ui)
                    .clicked()
                {
                    actions.push(("market.install", json!({"id": id})));
                }
            }
            (Some(_), false) => {}
        }
        if let Some(have) = installed {
            if Button::new("Remove").enabled(!busy).widget(format!("button:marketplaceRemove:{id}")).show(ui).clicked() {
                actions.push(("market.askRemove", json!({"id": id})));
            }
            if Button::new("Open in Plug-in Manager").widget(format!("button:marketplaceManage:{id}")).show(ui).clicked() {
                actions.push(("market.manager", json!({"id": id})));
            }
            ui.weak(tr_format!("Version {version} is installed", version = have));
        }
    });
    if installed.is_some() {
        let mut auto = at(e, "autoUpdate").as_bool().unwrap_or(false);
        let r = ui.checkbox(&mut auto, tr("Update this one automatically"));
        crate::widgets::register(ui.ctx(), format!("marketplaceAuto:{id}"), r.rect);
        if r.changed() {
            actions.push(("marketplace.setAutoUpdate", json!({"id": id, "on": auto})));
        }
    }
    if removing.as_deref() == Some(id) {
        let mut done = false;
        egui::Frame::NONE.fill(t.inset).stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.7))).corner_radius(4.0).inner_margin(8).show(
            ui,
            |ui| {
                ui.label(tr_format!("Remove {name}?", name = name));
                if !filter {
                    ui.weak(tr("Its data and settings are removed too. To keep them, remove it in the Plug-in Manager."));
                }
                ui.horizontal(|ui| {
                    if Button::new("Remove").kind(Kind::Primary).widget(format!("button:marketplaceRemoveConfirm:{id}")).show(ui).clicked() {
                        actions.push(("plugin.uninstall", json!({"id": id})));
                        done = true;
                    }
                    if Button::new("Cancel").widget(format!("button:marketplaceRemoveCancel:{id}")).show(ui).clicked() {
                        done = true;
                    }
                });
            },
        );
        if done {
            *removing = None;
        }
    }
    ui.add_space(8.0);
    let description = match text("description") {
        "" => text("summary"),
        d => d,
    };
    if !description.is_empty() {
        ui.label(description);
        ui.add_space(6.0);
    }

    ui.label(egui::RichText::new(tr("Works on").to_uppercase()).small().strong());
    ui.horizontal_wrapped(|ui| {
        let supported = strings(at(e, "platforms"));
        for p in SHOWN_PLATFORMS {
            let on = supported.contains(p);
            let label = format!("{} {}", if on { "✓" } else { "×" }, platform_name(p));
            let r = if on { chip(ui, &label, t.inset, t.text) } else { chip(ui, &label, egui::Color32::TRANSPARENT, t.text_disabled) };
            crate::widgets::register(ui.ctx(), format!("marketplacePlatform:{id}:{p}:{}", if on { "yes" } else { "no" }), r.rect);
            r.on_hover_text(if on { tr("Supported") } else { tr("Not supported") });
        }
    });
    if !is(e, "supported") {
        ui.colored_label(ui.visuals().warn_fg_color, tr_format!("It isn't available for {platform}.", platform = platform_name(platform)));
    }
    let missing = strings(at(e, "missing"));
    if !missing.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("{} {}", tr("It needs features this version of the app doesn't have:"), missing.join(", ")),
        );
    }
    ui.add_space(6.0);

    ui.label(egui::RichText::new(tr("May ask you for").to_uppercase()).small().strong());
    let asks = asks(e);
    if filter {
        ui.label(tr("Nothing. A pixel filter only computes pixels: it can't reach your files, your library or the network."));
    } else if asks.is_empty() {
        ui.label(tr("Nothing."));
    } else {
        for a in &asks {
            ui.label(format!("•  {a}"));
        }
        ui.weak(tr("Installing allows none of it: the plug-in asks you the first time it needs each one."));
    }
    ui.add_space(6.0);

    let mut facts: Vec<(&str, String)> = Vec::new();
    match at(e, "signer").as_str() {
        Some(fp) => facts.push((tr("Signed by"), fp.to_string())),
        None if !filter => facts.push((tr("Signed by"), tr("Nobody: the file isn't signed").to_string())),
        None => {}
    }
    if !text("license").is_empty() {
        facts.push((tr("Licence"), text("license").to_string()));
    }
    if let Some(day) = text("released").get(..10) {
        facts.push((tr("Released"), day.to_string()));
    }
    if let Some(bytes) = at(e, "size").as_u64().filter(|n| *n > 0) {
        facts.push((tr("Size"), format!("{} KB", bytes.div_ceil(1024))));
    }
    egui::Grid::new(("marketplace-facts", id)).num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
        for (name, value) in &facts {
            ui.label(egui::RichText::new(*name).color(t.text_dim));
            ui.label(value);
            ui.end_row();
        }
    });
    ui.horizontal_wrapped(|ui| {
        for (link, label) in [("homepage", tr("Homepage")), ("repository", tr("Source Code")), ("changelog", tr("Changelog"))] {
            if let Some(url) = at(e, link).as_str()
                && let Some(host) = lcc_engine::cmd::plugins::link_host(url)
            {
                let r = ui.link(format!("{label} ({host})")).on_hover_text(url);
                crate::widgets::register(ui.ctx(), format!("link:marketplace-{link}-{id}"), r.rect);
                if r.clicked() {
                    actions.push(("market.link", json!({"id": id, "link": link})));
                }
            }
        }
    });
}

fn footer_ui(ui: &mut egui::Ui, list: &Value, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::Button;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if Button::new("Plug-in Manager…").small().widget("button:marketplaceOpenManager").show(ui).clicked() {
            actions.push(("plugins.manager", json!({"open": true})));
        }
        if at(list, "source").is_string() && Button::new("Marketplace Website").small().widget("button:marketplaceWebsite").show(ui).clicked() {
            actions.push(("market.link", json!({})));
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let text = match at(list, "generated").as_str().and_then(|g| g.get(..10)) {
                Some(day) => tr_format!("Each plug-in's newest release, as listed on {date}.", date = day),
                None => tr("Each plug-in's newest release.").to_string(),
            };
            ui.add(egui::Label::new(egui::RichText::new(text).weak()).truncate());
        });
    });
}

/// The question a file not signed by the key pinned for the installed plug-in asks: why, then
/// **Install Anyway** (downloaded and checked again, then installed with that signer pinned) or
/// **Cancel**. Install Anyway ignores a click the control channel made.
fn signer_ask_ui(ctx: &egui::Context, s: &mut State, actions: &mut Vec<(&'static str, Value)>) {
    use crate::button::{Button, Kind};
    let Some(ask) = s.ask.clone() else { return };
    let mut keep = true;
    egui::Window::new(tr("A different signer"))
        .id(key().with("signer"))
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_max_width(440.0);
            crate::dialog_frame::card_header(ui, tr("A different signer"), None);
            ui.label(&ask.reason);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if Button::new("Cancel").kind(Kind::Primary).widget("button:marketplaceSignerCancel").show(ui).clicked() {
                    keep = false;
                }
                if Button::new("Install Anyway").widget("button:marketplaceSignerAccept").show(ui).clicked() {
                    actions.push(("market.install", json!({"id": ask.id, "anySigner": true})));
                    keep = false;
                }
            });
        });
    if !keep {
        s.ask = None;
    }
}
