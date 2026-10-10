//! The Web module (P3.6): a live preview of the gallery on the left of the centre and the
//! settings column on its right (Layout Style, Site Info, Colour Palette, Appearance, Image Info,
//! Output Settings, Upload Settings, Saved Galleries). Everything it does goes through the
//! engine's `web.*` commands, so agents and the CLI get the same galleries.
//!
//! The preview is drawn by egui from the same settings (palette, columns, thumbnail size, cell
//! numbers, borders, captions); the exported HTML is the reference (`web.preview` returns it).
//!
//! Export, upload and Share via Immich run on a worker thread ([`run`]: `webui.*`).

use dac_webgallery::Server;
use dac_webgallery::{GallerySettings, MetadataMode, Template};
use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::DacApp;
use crate::module::{Edge, Module, ModuleId, ModuleKey, PanelId};
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

/// The Web module's live state (kept for the session; saved galleries persist).
#[derive(Clone, Default)]
pub struct WebUi {
    pub settings: GallerySettings,
    pub gallery_name: String,
    pub server: Server,
    pub password: String,
    pub status: String,
    pub galleries: Vec<String>,
    pub servers: Vec<Server>,
    pub loaded: bool,
    /// Share via Immich (IMM-SHARELINK).
    pub share_account: String,
    pub share_album: String,
    pub share_days: u32,
    pub share_password: String,
    pub share_no_download: bool,
    pub share_hide_metadata: bool,
    /// The last shared link.
    pub share_url: String,
}

fn state_id() -> egui::Id {
    egui::Id::new("dac-web-module")
}

thread_local! {
    /// A saved gallery (collection id) to load into the module on its next frame (`creation.open`).
    static PENDING: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Opens a saved web gallery (a saved creation of kind `web`): its settings load into the module
/// on the next frame; its photos are the collection's (the caller shows that collection).
pub fn open_creation(app: &mut DacApp, id: u64) -> Result<Value, String> {
    let c = dac_engine::creations::creations_of(&app.session, Some(dac_layout::CreationKind::Web))
        .into_iter()
        .find(|c| c.id.0 == id)
        .ok_or("not a saved web gallery")?;
    PENDING.with(|p| p.set(Some(id)));
    Ok(json!({"opened": c.name, "id": id, "photos": c.photos.len()}))
}

/// Loads a pending saved gallery into `st`.
fn take_pending(app: &mut DacApp, st: &mut WebUi) {
    let Some(id) = PENDING.with(std::cell::Cell::take) else { return };
    if let Some(c) = dac_engine::creations::creations_of(&app.session, Some(dac_layout::CreationKind::Web)).into_iter().find(|c| c.id.0 == id) {
        st.settings = GallerySettings::default().merged(&c.settings).unwrap_or_default();
        st.gallery_name = c.name;
        st.loaded = false;
    }
}

pub fn state(ctx: &egui::Context) -> WebUi {
    ctx.data(|d| d.get_temp::<WebUi>(state_id())).unwrap_or_default()
}

fn store(ctx: &egui::Context, s: WebUi) {
    ctx.data_mut(|d| d.insert_temp(state_id(), s));
}

pub struct WebModule;

pub static WEB: WebModule = WebModule;

/// Most thumbnails the preview draws.
const PREVIEW_MAX: usize = 60;
const COLUMN_W: f32 = 300.0;

impl Module for WebModule {
    fn id(&self) -> ModuleId {
        ModuleId::Web
    }
    fn left_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn toolbar(&self, _ui: &mut egui::Ui, _app: &mut DacApp) {}
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        let t = Tokens::get(ui.ctx());
        let mut area = ui.available_rect_before_wrap();
        if crate::module::edge_visible(app, Edge::Bottom) {
            let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
            area.max.y = film.top();
            crate::panels::detail::filmstrip(app, ui, film);
        }
        app.canvas_rect = Some(area);
        register(ui.ctx(), "view:module:web", area);
        let mut st = state(ui.ctx());
        take_pending(app, &mut st);
        if !st.loaded {
            refresh_lists(app, &mut st);
            st.loaded = true;
        }
        let col_w = COLUMN_W.min(area.width() * 0.5);
        let preview_rect = Rect::from_min_max(area.min, pos2(area.right() - col_w, area.bottom()));
        let col_rect = Rect::from_min_max(pos2(preview_rect.right(), area.top()), area.max);
        ui.painter().rect_filled(col_rect, 0.0, t.chrome);
        preview(ui, app, &st.settings, preview_rect);
        let mut col = ui.new_child(egui::UiBuilder::new().max_rect(col_rect.shrink2(vec2(10.0, 8.0))).id_salt("web-settings"));
        register(ui.ctx(), "panel:web:settings", col_rect);
        egui::ScrollArea::vertical().id_salt("web-settings-scroll").auto_shrink([false, false]).show(&mut col, |ui| {
            settings_column(ui, app, &mut st);
        });
        ui.allocate_rect(area, Sense::hover());
        store(ui.ctx(), st);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        &[]
    }
}

fn c32(hex: &str) -> Color32 {
    let [r, g, b] = dac_webgallery::settings::color_rgb(hex);
    Color32::from_rgb(r, g, b)
}

fn to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The gallery as it will look: palette, header, cells with thumbnails.
fn preview(ui: &mut egui::Ui, app: &mut DacApp, s: &GallerySettings, rect: Rect) {
    let s = s.sanitized();
    let t = Tokens::get(ui.ctx());
    register(ui.ctx(), "view:web:preview", rect);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, c32(&s.palette.background));
    let pad = 16.0;
    let mut y = rect.top() + pad;
    let text = c32(&s.palette.text);
    let detail = c32(&s.palette.detail_text);
    if !s.site.title.is_empty() {
        let g = p.layout_no_wrap(s.site.title.clone(), t.font(22.0), text);
        y += g.size().y;
        p.galley(pos2(rect.left() + pad, y - g.size().y), g, text);
    }
    if !s.site.collection_title.is_empty() {
        let g = p.layout_no_wrap(s.site.collection_title.clone(), t.font(14.0), detail);
        p.galley(pos2(rect.left() + pad, y + 4.0), g.clone(), detail);
        y += g.size().y + 4.0;
    }
    if !s.site.description.is_empty() {
        let g = p.layout(s.site.description.clone(), t.font(12.0), detail, (rect.width() - 2.0 * pad).max(10.0));
        p.galley(pos2(rect.left() + pad, y + 6.0), g.clone(), detail);
        y += g.size().y + 6.0;
    }
    y += 12.0;
    let ids: Vec<dac_catalog::PhotoId> = {
        let sel = app.session.selection.ids.clone();
        if sel.len() > 1 { sel } else { app.session.visible().to_vec() }
    };
    let ids: Vec<_> = ids.into_iter().take(PREVIEW_MAX).collect();
    let inner_w = (rect.width() - 2.0 * pad).max(40.0);
    let gap = 8.0;
    let scale = 0.6; // the preview is a scaled-down page
    let border = if s.appearance.photo_borders { s.appearance.border_width as f32 * scale } else { 0.0 };
    let border_c = c32(&s.palette.border);
    if s.template == Template::Track || s.template == Template::Single {
        // large image area, then a strip
        let stage_h = (rect.bottom() - y - pad - 90.0).max(60.0);
        let stage = Rect::from_min_size(pos2(rect.left() + pad, y), vec2(inner_w, stage_h));
        if let Some(id) = ids.first() {
            draw_photo(&p, app, *id, stage, border, border_c);
        }
        if s.template == Template::Track {
            let cell = 70.0;
            let mut x = rect.left() + pad;
            for (i, id) in ids.iter().enumerate() {
                let r = Rect::from_min_size(pos2(x, stage.bottom() + 12.0), vec2(cell, cell));
                if r.left() > rect.right() {
                    break;
                }
                p.rect_filled(r, 0.0, c32(&s.palette.cell));
                draw_photo(&p, app, *id, r.shrink(4.0), border, border_c);
                if s.appearance.cell_numbers {
                    p.text(r.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, format!("{}", i + 1), t.font(9.0), detail);
                }
                x += cell + gap;
            }
        }
        return;
    }
    let cols = match s.template {
        Template::Square => s.appearance.columns as usize,
        _ => ((inner_w + gap) / (s.appearance.thumb_size as f32 * 0.75 * scale + gap)).floor().max(1.0) as usize,
    }
    .max(1);
    let cell_w = (inner_w - gap * (cols as f32 - 1.0)) / cols as f32;
    let cap_h = if s.appearance.thumb_captions { 16.0 } else { 0.0 };
    let cell_h = (cell_w).min(s.appearance.thumb_size as f32 * scale + 16.0).max(20.0) + cap_h;
    for (i, id) in ids.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let r = Rect::from_min_size(pos2(rect.left() + pad + col as f32 * (cell_w + gap), y + row as f32 * (cell_h + gap)), vec2(cell_w, cell_h));
        if r.top() > rect.bottom() {
            break;
        }
        p.rect_filled(r, 0.0, c32(&s.palette.cell));
        let img = Rect::from_min_max(r.min + vec2(6.0, 6.0), r.max - vec2(6.0, 6.0 + cap_h));
        if s.template == Template::Square {
            let side = img.width().min(img.height());
            draw_photo(&p, app, *id, Rect::from_center_size(img.center(), vec2(side, side)), border, border_c);
        } else {
            draw_photo(&p, app, *id, img, border, border_c);
        }
        if s.appearance.cell_numbers {
            p.text(r.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, format!("{}", i + 1), t.font(9.0), detail);
        }
        if cap_h > 0.0 {
            let title = app.session.catalog.photo(*id).map(|ph| ph.meta.title.clone()).unwrap_or_default();
            p.text(pos2(r.center().x, r.bottom() - 4.0), Align2::CENTER_BOTTOM, title, t.font(10.0), text);
        }
    }
}

/// The photo's thumbnail fitted into `r` (a placeholder until it is rendered).
fn draw_photo(p: &egui::Painter, app: &DacApp, id: dac_catalog::PhotoId, r: Rect, border: f32, border_c: Color32) {
    let Some(tex) = app.renderer.thumb(id) else {
        p.rect_filled(r, 0.0, Color32::from_gray(60));
        return;
    };
    let [w, h] = tex.size;
    let aspect = w.max(1) as f32 / h.max(1) as f32;
    let fit = if r.width() / r.height().max(1.0) > aspect { vec2(r.height() * aspect, r.height()) } else { vec2(r.width(), r.width() / aspect) };
    let dst = Rect::from_center_size(r.center(), fit);
    p.image(tex.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    if border > 0.0 {
        p.rect_stroke(dst, 0.0, Stroke::new(border, border_c), egui::StrokeKind::Outside);
    }
}

fn refresh_lists(app: &mut DacApp, st: &mut WebUi) {
    if let Ok(Value::Array(g)) = app.session.execute("web.galleries", &json!({})) {
        st.galleries = g.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect();
    }
    if let Ok(v) = app.session.execute("web.servers", &json!({})) {
        st.servers = serde_json::from_value(v).unwrap_or_default();
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.semibold(12.5)).color(t.text));
    ui.add_space(2.0);
}

fn field(ui: &mut egui::Ui, id: &str, label: &str, text: &mut String) {
    ui.label(crate::i18n::tr(label));
    TextField::singleline(&format!("field:web:{id}"), text).width(ui.available_width()).show(ui);
}

fn color_row(ui: &mut egui::Ui, label: &str, hex: &mut String) {
    ui.horizontal(|ui| {
        let mut c = dac_webgallery::settings::color_rgb(hex);
        if ui.color_edit_button_srgb(&mut c).changed() {
            *hex = to_hex(c);
        }
        ui.label(crate::i18n::tr(label));
    });
}

fn settings_column(ui: &mut egui::Ui, app: &mut DacApp, st: &mut WebUi) {
    let s = &mut st.settings;
    heading(ui, "Layout Style");
    ui.horizontal_wrapped(|ui| {
        for tpl in Template::ALL {
            if ui.selectable_label(s.template == tpl, crate::i18n::tr(tpl.label())).clicked() {
                s.template = tpl;
            }
        }
    });

    heading(ui, "Site Info");
    field(ui, "title", "Site title", &mut s.site.title);
    field(ui, "collection", "Collection title", &mut s.site.collection_title);
    field(ui, "description", "Collection description", &mut s.site.description);
    field(ui, "contact", "Contact info", &mut s.site.contact);
    field(ui, "link", "Web or mail link", &mut s.site.link);

    heading(ui, "Color Palette");
    color_row(ui, "Background", &mut s.palette.background);
    color_row(ui, "Text", &mut s.palette.text);
    color_row(ui, "Detail text", &mut s.palette.detail_text);
    color_row(ui, "Cells", &mut s.palette.cell);
    color_row(ui, "Rollover", &mut s.palette.cell_hover);
    color_row(ui, "Grid lines", &mut s.palette.border);
    color_row(ui, "Links", &mut s.palette.accent);

    heading(ui, "Appearance");
    ui.add(egui::Slider::new(&mut s.appearance.columns, 2..=10).text(crate::i18n::tr("Columns")));
    ui.add(egui::Slider::new(&mut s.appearance.thumb_size, 80..=400).text(crate::i18n::tr("Thumbnail size")));
    ui.checkbox(&mut s.appearance.cell_numbers, crate::i18n::tr("Show cell numbers"));
    ui.checkbox(&mut s.appearance.photo_borders, crate::i18n::tr("Photo borders"));
    ui.add(egui::Slider::new(&mut s.appearance.border_width, 0..=20).text(crate::i18n::tr("Border width")));
    ui.add(egui::Slider::new(&mut s.appearance.image_page_size, 300..=4096).text(crate::i18n::tr("Image page size")));
    ui.checkbox(&mut s.appearance.thumb_captions, crate::i18n::tr("Captions under thumbnails"));

    heading(ui, "Image Info");
    field(ui, "imageTitle", "Title", &mut s.image_info.title);
    field(ui, "imageCaption", "Caption", &mut s.image_info.caption);
    ui.label(egui::RichText::new(dac_webgallery::tokens::KNOWN.iter().map(|k| format!("{{{k}}}")).collect::<Vec<_>>().join(" ")).small().weak());

    heading(ui, "Output Settings");
    ui.add(egui::Slider::new(&mut s.output.large_size, 300..=4096).text(crate::i18n::tr("Large image size")));
    ui.add(egui::Slider::new(&mut s.output.quality, 1..=100).text(crate::i18n::tr("Quality")));
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Metadata"));
        ui.selectable_value(&mut s.output.metadata, MetadataMode::All, crate::i18n::tr("All"));
        ui.selectable_value(&mut s.output.metadata, MetadataMode::CopyrightOnly, crate::i18n::tr("Copyright only"));
    });
    field(ui, "watermark", "Watermark text", &mut s.output.watermark);
    ui.checkbox(&mut s.output.sharpen, crate::i18n::tr("Sharpen for screen"));

    ui.add_space(8.0);
    if ui.button(crate::i18n::tr("Export…")).clicked() {
        export(app, st);
    }

    heading(ui, "Upload Settings");
    let sv = &mut st.server;
    field(ui, "serverName", "Server name", &mut sv.name);
    field(ui, "host", "Host", &mut sv.host);
    let mut port = if sv.port == 0 { "22".to_string() } else { sv.port.to_string() };
    field(ui, "port", "Port", &mut port);
    sv.port = port.trim().parse().unwrap_or(22);
    field(ui, "user", "User", &mut sv.user);
    field(ui, "path", "Server path", &mut sv.path);
    field(ui, "keyFile", "Private key file (optional)", &mut sv.key_file);
    ui.label(crate::i18n::tr("Password or key passphrase"));
    ui.add(egui::TextEdit::singleline(&mut st.password).password(true).desired_width(ui.available_width()));
    ui.horizontal(|ui| {
        if ui.button(crate::i18n::tr("Save Server")).clicked() {
            let mut p = json!({"server": st.server});
            if !st.password.is_empty() {
                p["password"] = json!(st.password);
            }
            st.status = match app.session.execute("web.saveServer", &p) {
                Ok(_) => crate::i18n::tr("Server saved").to_string(),
                Err(e) => e.to_string(),
            };
            refresh_lists(app, st);
        }
        if ui.button(crate::i18n::tr("Upload")).clicked() {
            upload(app, st);
        }
    });
    if !st.servers.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for x in st.servers.clone() {
                if ui.small_button(&x.name).clicked() {
                    st.server = x;
                }
            }
        });
    }

    share_section(ui, app, st);

    heading(ui, "Saved Galleries");
    field(ui, "galleryName", "Name", &mut st.gallery_name);
    if ui.button(crate::i18n::tr("Save Gallery")).clicked() {
        let r = app.session.execute("web.saveGallery", &json!({"name": st.gallery_name, "settings": st.settings}));
        st.status = match r {
            Ok(_) => crate::i18n::tr("Gallery saved").to_string(),
            Err(e) => e.to_string(),
        };
        refresh_lists(app, st);
    }
    for name in st.galleries.clone() {
        ui.horizontal(|ui| {
            if ui.small_button(crate::i18n::tr("Load")).clicked()
                && let Ok(Value::Array(all)) = app.session.execute("web.galleries", &json!({}))
            {
                let found = all.iter().find(|g| g["name"] == json!(name)).map(|g| g["settings"].clone());
                if let Some(g) = found.and_then(|x| GallerySettings::from_json(&x).ok()) {
                    st.settings = g;
                    st.gallery_name = name.clone();
                }
            }
            if ui.small_button(crate::i18n::tr("Delete")).clicked() {
                let _ = app.session.execute("web.deleteGallery", &json!({"name": name}));
                refresh_lists(app, st);
            }
            ui.label(&name);
        });
    }
    if !st.status.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(&st.status).small());
    }
}

/// The Web module's background commands: `webui.export`, `webui.upload`, `webui.shareImmich`
/// take the params of `web.export` / `web.upload` / `web.shareImmich` (the folder dialog opens
/// when `webui.export` has no `dir`), set the job up on the UI thread and render, write, upload or
/// share on a worker thread (Activity "export"). The result lands in the module's status line.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let engine_id = match id {
        "webui.export" => "web.export",
        "webui.upload" => "web.upload",
        "webui.shareImmich" => "web.shareImmich",
        _ => return None,
    };
    Some(start(app, id, engine_id, p))
}

#[cfg(target_arch = "wasm32")]
fn start(_app: &mut DacApp, _id: &str, engine_id: &str, _p: &Value) -> Result<Value, String> {
    Err(format!("{engine_id}: not available in the browser"))
}

#[cfg(not(target_arch = "wasm32"))]
fn start(app: &mut DacApp, id: &str, engine_id: &'static str, p: &Value) -> Result<Value, String> {
    if id == "webui.export" && p.get("dir").and_then(Value::as_str).is_none_or(|d| d.trim().is_empty()) {
        let req = crate::pick::PickRequest::folder(crate::i18n::tr("Export Web Gallery"));
        return match crate::pick::ask(app, id, p, "dir", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
            crate::pick::Picked::Now(v) => match v.into_iter().next() {
                Some(d) => start(app, id, engine_id, &crate::pick::with_answer(p, "dir", crate::pick::PickKind::Folder, &[d])),
                None => Ok(Value::Null),
            },
            crate::pick::Picked::Later => Ok(json!({"pending": "folder dialog"})),
            crate::pick::Picked::Unavailable => Err(crate::i18n::tr("No folder dialog here: use web.export {dir}").to_string()),
        };
    }
    let job = dac_engine::cmd::web::prepare(&mut app.session, engine_id, p).map_err(|e| e.to_string())?;
    let steps = job.steps();
    let label = match engine_id {
        "web.upload" => "Uploading web gallery",
        "web.shareImmich" => "Sharing via Immich",
        _ => "Exporting web gallery",
    };
    crate::tasks::spawn(
        app,
        label,
        Some("export"),
        move || job.run(&mut |_, _| true),
        move |app, ctx, r: Result<Value, String>| {
            // (an upload's host key is remembered on first use by the job itself)
            let mut st = state(ctx);
            if let Ok(v) = &r
                && let Some(url) = v.get("url").and_then(Value::as_str)
            {
                st.share_url = url.to_string();
            }
            st.status = result_text(r.clone());
            match r {
                Ok(_) => app.toast(ctx, st.status.clone()),
                Err(e) => app.toast_error(ctx, e),
            }
            st.loaded = false; // a first upload may have remembered a host key
            store(ctx, st);
        },
    )?;
    Ok(json!({"background": true, "steps": steps}))
}

fn export(app: &mut DacApp, st: &mut WebUi) {
    st.status = background_text(app.run("webui.export", json!({"settings": st.settings})));
}

fn upload(app: &mut DacApp, st: &mut WebUi) {
    let mut p = json!({"server": st.server, "settings": st.settings});
    if !st.server.name.is_empty() && st.servers.iter().any(|x| x.name == st.server.name) && st.password.is_empty() {
        p["server"] = json!(st.server.name);
    }
    if !st.password.is_empty() {
        p["password"] = json!(st.password);
    }
    st.status = background_text(app.run("webui.upload", p));
}

fn share(app: &mut DacApp, st: &mut WebUi) {
    let mut p = json!({"settings": st.settings, "allowDownload": !st.share_no_download, "showMetadata": !st.share_hide_metadata});
    for (k, v) in [("account", &st.share_account), ("album", &st.share_album), ("password", &st.share_password)] {
        if !v.trim().is_empty() {
            p[k] = json!(v.trim());
        }
    }
    if st.share_days > 0 {
        p["expiresDays"] = json!(st.share_days);
    }
    st.share_url.clear();
    st.status = background_text(app.run("webui.shareImmich", p));
}

fn background_text(r: Result<Value, String>) -> String {
    match r {
        Ok(v) if v.get("background").is_some() => crate::i18n::tr("Working… (see Activity)").to_string(),
        Ok(v) if v.get("pending").is_some() => crate::i18n::tr("Choose a folder…").to_string(),
        r => result_text(r),
    }
}

fn result_text(r: Result<Value, String>) -> String {
    match r {
        Ok(v) => match (v.get("url"), v.get("files"), v.get("photos")) {
            (Some(Value::String(u)), _, _) => format!("{} photos shared: {u}", v["uploaded"]),
            (_, Some(f), Some(p)) => format!("{p} photos, {f} files"),
            _ => v.to_string(),
        },
        Err(e) => e,
    }
}

/// "Share via Immich" (IMM-SHARELINK): album, expiry, password, download and metadata switches.
fn share_section(ui: &mut egui::Ui, app: &mut DacApp, st: &mut WebUi) {
    heading(ui, "Share via Immich");
    field(ui, "shareAccount", "Account (blank: the connected one)", &mut st.share_account);
    field(ui, "shareAlbum", "Album name (blank: the collection title)", &mut st.share_album);
    ui.spacing_mut().slider_width = 90.0;
    ui.add(egui::Slider::new(&mut st.share_days, 0..=365).text(crate::i18n::tr("Expiry (days, 0 = never)")));
    ui.label(crate::i18n::tr("Link password (optional)"));
    ui.add(egui::TextEdit::singleline(&mut st.share_password).password(true).desired_width(ui.available_width()));
    let mut dl = !st.share_no_download;
    ui.checkbox(&mut dl, crate::i18n::tr("Allow download"));
    st.share_no_download = !dl;
    let mut meta = !st.share_hide_metadata;
    ui.checkbox(&mut meta, crate::i18n::tr("Show metadata"));
    st.share_hide_metadata = !meta;
    let b = ui.button(crate::i18n::tr("Share via Immich"));
    register(ui.ctx(), "web:shareImmich", b.rect);
    if b.clicked() {
        share(app, st);
    }
    if !st.share_url.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&st.share_url).small());
            if ui.small_button(crate::i18n::tr("Copy URL")).clicked() {
                ui.ctx().copy_text(st.share_url.clone());
            }
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use serde_json::json;

    use crate::DacApp;
    use crate::headless::Headless;

    #[test]
    fn web_export_runs_in_the_background() {
        let dir = std::env::temp_dir().join(format!("dac-webui-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = DacApp::new(dac_engine::Session::with_demo(), crate::Services::default());
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        let id = h.app.session.visible_cloned()[0].0;
        let r = h.app.run("webui.export", json!({"dir": dir.display().to_string(), "ids": [id]})).unwrap();
        assert_eq!(r["background"], true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !h.app.tasks.is_empty() && std::time::Instant::now() < deadline {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(h.app.tasks.is_empty(), "export did not finish");
        assert!(dir.join("index.html").is_file());
        assert!(super::state(&h.view.ctx).status.contains("1 photos"), "{}", super::state(&h.view.ctx).status);
        // set-up errors come back at once
        assert!(h.app.run("webui.shareImmich", json!({"ids": [id]})).is_err(), "no Immich account");
        let _ = std::fs::remove_dir_all(dir);
    }
}
