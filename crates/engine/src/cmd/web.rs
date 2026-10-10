//! Web module commands (P3.6): generate a static web gallery ([`dac_webgallery`]) from photos,
//! write it to a folder or upload it over SFTP, and keep saved galleries and upload servers.
//!
//! - `web.preview {settings?, ids?}` → the generated pages without rendering any image.
//! - `web.export {dir, settings?, ids?}` → writes the site (pages + rendered JPEGs) into `dir`.
//! - `web.upload {server, settings?, ids?, password?}` → renders and uploads over SFTP.
//! - `web.galleries` / `web.saveGallery` / `web.deleteGallery`: saved galleries.
//! - `web.servers` / `web.saveServer` / `web.deleteServer`: upload presets; passwords and key
//!   passphrases go to the keychain (service [`SFTP_SERVICE`]), never into the presets file.
//!
//! Saved galleries are saved creations in the catalog (collections of kind `web` holding the
//! gallery settings and its photos; they show in the Collections panel). Upload servers live in
//! `<config>/web.json` next to `connections.json`; galleries an older build kept there move into
//! the catalog the first time the gallery commands run. Commands that take a secret are not
//! journaled and no result contains one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dac_catalog::PhotoId;
use dac_credentials::{Key, Secret};
use dac_webgallery::sftp::{self, Auth, Server};
use dac_webgallery::{GalleryPhoto, GallerySettings, MetadataMode, Site};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::creations;
use crate::export::{ExportFormat, ExportOptions, MetadataPolicy, Resize, ResizeMode, SharpenFor, Watermark};
use crate::{Result, Session};
use dac_layout::CreationKind;

/// Keychain service for SFTP passwords / key passphrases (account = `user@host:port`).
pub const SFTP_SERVICE: &str = "web-sftp";

/// A saved gallery as older builds kept it in `web.json` (read once, to move it into the catalog).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedGallery {
    pub name: String,
    pub settings: GallerySettings,
    pub ids: Vec<u64>,
}

/// `<config>/web.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebStore {
    pub galleries: Vec<SavedGallery>,
    pub servers: Vec<Server>,
}

fn store_path(s: &Session) -> Option<PathBuf> {
    s.remote.connections_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("web.json"))
}

fn load(s: &Session) -> std::result::Result<WebStore, String> {
    let Some(path) = store_path(s) else { return Ok(WebStore::default()) };
    match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WebStore::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save(s: &Session, st: &WebStore) -> std::result::Result<(), String> {
    let path = store_path(s).ok_or("no settings folder to save web galleries in")?;
    let bytes = serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?;
    crate::export::write_file_durable(&path.to_string_lossy(), &bytes)
}

/// Moves galleries an older build kept in `web.json` into the catalog (once; servers stay).
fn migrate_galleries(s: &mut Session, c: &str) -> Result<()> {
    let mut st = load(s).map_err(|e| bad(c, e))?;
    if st.galleries.is_empty() {
        return Ok(());
    }
    for g in std::mem::take(&mut st.galleries) {
        if g.name.trim().is_empty() || creations::find_creation(s, CreationKind::Web, &g.name).is_some() {
            continue;
        }
        let photos = g.ids.iter().map(|i| PhotoId(*i)).filter(|i| s.catalog.photo(*i).is_some()).collect();
        let settings = serde_json::to_value(&g.settings).map_err(|e| bad(c, e.to_string()))?;
        creations::save_settings(s, CreationKind::Web, &g.name, settings, photos)?;
    }
    save(s, &st).map_err(|e| bad(c, e))
}

/// A saved gallery's settings (merged over the defaults: a damaged one reads as the defaults).
fn gallery_settings(v: &Value) -> GallerySettings {
    GallerySettings::default().merged(v).unwrap_or_default()
}

/// The saved gallery a `gallery` param names: a collection id, or a name.
fn find_gallery(s: &Session, p: &Value, c: &str) -> Result<Option<creations::SavedCreation>> {
    let found = match p.get("gallery") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => creations::creations_of(s, Some(CreationKind::Web)).into_iter().find(|g| Some(g.id.0) == n.as_u64()),
        Some(Value::String(name)) => creations::find_creation(s, CreationKind::Web, name),
        Some(_) => return Err(bad(c, "`gallery` is a saved gallery's name or id")),
    };
    found.map(Some).ok_or_else(|| bad(c, format!("no saved web gallery {}", p["gallery"])))
}

/// `settings` param (partial objects merge over the defaults), or a saved gallery by `gallery`
/// (name or collection id).
fn settings_param(s: &Session, p: &Value, c: &str) -> Result<GallerySettings> {
    let base = match find_gallery(s, p, c)? {
        Some(g) => gallery_settings(&g.settings),
        None => GallerySettings::default(),
    };
    match p.get("settings") {
        None | Some(Value::Null) => Ok(base),
        Some(v @ Value::Object(_)) => base.merged(v).map_err(|e| bad(c, e)),
        Some(_) => Err(bad(c, "`settings` must be an object")),
    }
}

/// Target photos: `ids`, else a saved `gallery`'s photos, else the selection, else every visible
/// photo.
fn photo_ids(s: &mut Session, p: &Value) -> Vec<PhotoId> {
    if !super::names_photos(p)
        && let Ok(Some(g)) = find_gallery(s, p, "web")
        && !g.photos.is_empty()
    {
        return g
            .photos
            .into_iter()
            .filter(|id| s.catalog.photo(*id).is_some_and(|ph| !ph.deleted))
            .take(dac_webgallery::site::MAX_PHOTOS.saturating_add(1))
            .collect();
    }
    let t = s.targets(p);
    let t = if t.len() > 1 || super::names_photos(p) { t } else { s.visible().to_vec() };
    t.into_iter().filter(|id| s.catalog.photo(*id).is_some_and(|ph| !ph.deleted)).take(dac_webgallery::site::MAX_PHOTOS.saturating_add(1)).collect()
}

/// The token fields of one photo.
fn fields(s: &Session, id: PhotoId) -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    let Some(p) = s.catalog.photo(id) else { return f };
    let m = &p.meta;
    let mut put = |k: &str, v: String| {
        if !v.trim().is_empty() {
            f.insert(k.to_string(), v);
        }
    };
    put("title", m.title.clone());
    put("caption", m.caption.clone());
    put("filename", p.file_name.clone());
    put("date", p.captured.clone().unwrap_or_default().chars().take(10).collect());
    put("camera", m.camera.clone());
    put("lens", m.lens.clone());
    put("copyright", m.copyright.clone());
    put("creator", m.creator.clone());
    put("keywords", m.keywords.join(", "));
    put("rating", if p.rating > 0 { "★".repeat(usize::from(p.rating.min(5))) } else { String::new() });
    put("iso", m.iso.map(|i| format!("ISO {i}")).unwrap_or_default());
    put("aperture", m.aperture.filter(|a| a.is_finite()).map(|a| format!("f/{a:.1}")).unwrap_or_default());
    put("shutter", if m.shutter.is_empty() { String::new() } else { format!("{} s", m.shutter) });
    put("focal", m.focal_mm.filter(|a| a.is_finite()).map(|a| format!("{a:.0} mm")).unwrap_or_default());
    f
}

fn image_options(g: &GallerySettings, long_edge: u32, large: bool) -> ExportOptions {
    let mut o = ExportOptions {
        format: ExportFormat::Jpeg,
        quality: if large { g.output.quality } else { g.output.quality.min(85) },
        resize: Some(Resize { mode: ResizeMode::LongEdge, value: long_edge as f32, height: long_edge, dont_enlarge: true }),
        sharpen: if g.output.sharpen { SharpenFor::Screen } else { SharpenFor::None },
        metadata: match (large, g.output.metadata) {
            (false, _) => MetadataPolicy::None,
            (true, MetadataMode::All) => MetadataPolicy::All,
            (true, MetadataMode::CopyrightOnly) => MetadataPolicy::Copyright,
        },
        remove_location: g.output.metadata != MetadataMode::All,
        ..ExportOptions::default()
    };
    if large && !g.output.watermark.trim().is_empty() {
        o.watermark = Some(Watermark { text: g.output.watermark.trim().to_string(), ..Watermark::default() });
    }
    o
}

/// The site for `ids` (pages only; [`Site::images`] still to render).
fn build(s: &mut Session, g: &GallerySettings, ids: &[PhotoId]) -> std::result::Result<Site, String> {
    let probe = image_options(g, g.output.large_size, true);
    let photos: Vec<GalleryPhoto> = ids
        .iter()
        .map(|id| {
            let aspect = s.catalog.photo(*id).map(|p| crate::export::output_size(p, &probe)).map(|(w, h)| w as f32 / h.max(1) as f32).unwrap_or(1.5);
            GalleryPhoto { fields: fields(s, *id), aspect }
        })
        .collect();
    dac_webgallery::generate(g, &photos)
}

/// Every file of the site with its bytes, rendering the images; `progress(done, total)`.
fn render_all(s: &mut Session, g: &GallerySettings, ids: &[PhotoId], site: Site) -> std::result::Result<Vec<(String, Vec<u8>)>, String> {
    let mut out: Vec<(String, Vec<u8>)> = site.files.into_iter().map(|f| (f.path, f.bytes)).collect();
    for (seq, r) in site.images.iter().enumerate() {
        let id = ids.get(r.photo).copied().ok_or("internal: image for an unknown photo")?;
        let o = image_options(g, r.long_edge, r.large);
        let e = crate::export::export_photo(s, id, &o, seq).map_err(|e| format!("{}: {e}", r.path))?;
        out.push((r.path.clone(), e.bytes));
    }
    Ok(out)
}

fn secret_key(server: &Server) -> Key {
    let port = if server.port == 0 { 22 } else { server.port };
    Key::new(SFTP_SERVICE, &format!("{}@{}:{port}", server.user.trim(), server.host.trim()))
}

fn server_param(s: &Session, p: &Value, c: &str) -> Result<Server> {
    match p.get("server") {
        Some(Value::String(name)) => {
            let st = load(s).map_err(|e| bad(c, e))?;
            st.servers.into_iter().find(|x| x.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| bad(c, format!("no upload server `{name}`")))
        }
        Some(v @ Value::Object(_)) => serde_json::from_value(v.clone()).map_err(|e| bad(c, format!("invalid `server`: {e}"))),
        _ => Err(bad(c, "missing `server` (a saved server's name or {host, port, user, path, keyFile})")),
    }
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.preview";
    let g = settings_param(s, p, ID)?;
    let ids = photo_ids(s, p);
    let site = build(s, &g, &ids).map_err(|e| bad(ID, e))?;
    let files: Vec<Value> =
        site.files.iter().map(|f| json!({"path": f.path, "bytes": f.bytes.len(), "text": String::from_utf8_lossy(&f.bytes)})).collect();
    let images: Vec<Value> =
        site.images.iter().map(|r| json!({"path": r.path, "photo": ids.get(r.photo).map(|i| i.0), "longEdge": r.long_edge})).collect();
    Ok(json!({"settings": g, "photos": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "files": files, "images": images}))
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.export";
    let dir = str_param(p, "dir").map(str::trim).filter(|d| !d.is_empty()).ok_or_else(|| bad(ID, "missing `dir`"))?.to_string();
    let g = settings_param(s, p, ID)?;
    let ids = photo_ids(s, p);
    let site = build(s, &g, &ids).map_err(|e| bad(ID, e))?;
    let files = render_all(s, &g, &ids, site).map_err(|e| bad(ID, e))?;
    let mut bytes = 0usize;
    for (rel, data) in &files {
        let path = std::path::Path::new(&dir).join(rel).to_string_lossy().to_string();
        s.check_write_target(&path).map_err(|e| bad(ID, e))?;
        crate::export::write_file(&path, data).map_err(|e| bad(ID, e))?;
        bytes = bytes.saturating_add(data.len());
    }
    let index = std::path::Path::new(&dir).join("index.html").to_string_lossy().to_string();
    Ok(json!({"dir": dir, "index": index, "files": files.len(), "photos": ids.len(), "bytes": bytes}))
}

fn upload(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.upload";
    let mut server = server_param(s, p, ID)?;
    let g = settings_param(s, p, ID)?;
    let secret = match str_param(p, "password") {
        Some(pw) => Some(pw.to_string()),
        None => s.secret_store().ok().and_then(|st| st.get(&secret_key(&server)).ok().flatten()).map(|x| x.expose().to_string()),
    };
    let auth = if server.key_file.trim().is_empty() {
        Auth::Password(secret.ok_or_else(|| bad(ID, "no password: give `password` or save one with web.saveServer"))?)
    } else {
        let pem = std::fs::read_to_string(server.key_file.trim()).map_err(|e| bad(ID, format!("{}: {e}", server.key_file.trim())))?;
        Auth::Key { pem, passphrase: secret }
    };
    let ids = photo_ids(s, p);
    let site = build(s, &g, &ids).map_err(|e| bad(ID, e))?;
    let files = render_all(s, &g, &ids, site).map_err(|e| bad(ID, e))?;
    let done = sftp::upload(&server, auth, &files, &mut |_, _| true).map_err(|e| bad(ID, e))?;
    // trust on first use: remember the host key of a saved server
    if server.known_fingerprint.is_empty() && !server.name.is_empty() {
        server.known_fingerprint = done.fingerprint.clone();
        if let Ok(mut st) = load(s)
            && let Some(x) = st.servers.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&server.name))
            && x.known_fingerprint.is_empty()
        {
            x.known_fingerprint = done.fingerprint.clone();
            let _ = save(s, &st);
        }
    }
    Ok(
        json!({"host": server.host, "path": server.path, "files": done.files, "bytes": done.bytes, "photos": ids.len(), "fingerprint": done.fingerprint}),
    )
}

fn galleries_json(s: &Session) -> Value {
    let v: Vec<Value> = creations::creations_of(s, Some(CreationKind::Web))
        .into_iter()
        .map(|g| json!({"id": g.id.0, "name": g.name, "settings": gallery_settings(&g.settings), "ids": g.photos.iter().map(|i| i.0).collect::<Vec<_>>()}))
        .collect();
    json!(v)
}

fn galleries(s: &mut Session, _: &Value) -> Result<Value> {
    migrate_galleries(s, "web.galleries")?;
    Ok(galleries_json(s))
}

fn servers_json(st: &WebStore) -> Value {
    json!(st.servers)
}

fn save_gallery(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.saveGallery";
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(ID, "missing `name`"))?.to_string();
    let settings = settings_param(s, p, ID)?;
    migrate_galleries(s, ID)?;
    let ids: Option<Vec<PhotoId>> = super::names_photos(p).then(|| s.targets(p).into_iter().filter(|i| s.catalog.photo(*i).is_some()).collect());
    let settings = serde_json::to_value(&settings).map_err(|e| bad(ID, e.to_string()))?;
    match creations::find_creation(s, CreationKind::Web, &name) {
        Some(g) => creations::update_settings(s, g.id, CreationKind::Web, settings, ids)?,
        None => {
            creations::save_settings(s, CreationKind::Web, &name, settings, ids.unwrap_or_default())?;
        }
    }
    Ok(galleries_json(s))
}

fn delete_gallery(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.deleteGallery";
    let name = str_param(p, "name").ok_or_else(|| bad(ID, "missing `name`"))?;
    migrate_galleries(s, ID)?;
    let g = creations::find_creation(s, CreationKind::Web, name).ok_or_else(|| bad(ID, format!("no saved web gallery `{name}`")))?;
    s.commit("Delete Web Gallery", dac_catalog::Op::RemoveAlbum { id: g.id })?;
    Ok(galleries_json(s))
}

fn save_server(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.saveServer";
    let server = server_param(s, p, ID)?;
    if server.name.trim().is_empty() || server.host.trim().is_empty() || server.user.trim().is_empty() {
        return Err(bad(ID, "an upload server needs `name`, `host` and `user`"));
    }
    if let Some(pw) = str_param(p, "password") {
        let store = s.secret_store().map_err(|e| bad(ID, e))?;
        store.set(&secret_key(&server), &Secret::new(pw)).map_err(|e| bad(ID, e.to_string()))?;
    }
    let mut st = load(s).map_err(|e| bad(ID, e))?;
    match st.servers.iter_mut().find(|x| x.name.eq_ignore_ascii_case(server.name.trim())) {
        Some(x) => *x = server,
        None => st.servers.push(server),
    }
    save(s, &st).map_err(|e| bad(ID, e))?;
    Ok(servers_json(&st))
}

fn delete_server(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "web.deleteServer";
    let name = str_param(p, "name").ok_or_else(|| bad(ID, "missing `name`"))?;
    let mut st = load(s).map_err(|e| bad(ID, e))?;
    let Some(pos) = st.servers.iter().position(|x| x.name.eq_ignore_ascii_case(name.trim())) else {
        return Err(bad(ID, format!("no upload server `{name}`")));
    };
    let gone = st.servers.remove(pos);
    if let Ok(store) = s.secret_store() {
        let _ = store.delete(&secret_key(&gone));
    }
    save(s, &st).map_err(|e| bad(ID, e))?;
    Ok(servers_json(&st))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "web.preview", "Preview Web Gallery", [], None,
            "{settings?: partial web gallery settings {template: grid|square|track|single, site: {title, collectionTitle, description, contact, link}, palette: {background, text, detailText, cell, cellHover, border, accent}, appearance: {columns, thumbSize, cellNumbers, photoBorders, borderWidth, imagePageSize, thumbCaptions}, imageInfo: {title, caption} (tokens {title} {caption} {filename} {date} {camera} {lens} {copyright} {creator} {keywords} {rating} {iso} {aperture} {shutter} {focal}), output: {largeSize, quality, metadata: all|copyrightOnly, watermark, sharpen}}, gallery?: saved gallery name, ids?: photo ids (default: the selection, or every visible photo)} → {settings, photos, files: [{path, bytes, text}], images: [{path, photo, longEdge}]}; renders nothing",
            always, preview),
        cmd!(query "web.export", "Export Web Gallery", [], None,
            "{dir, settings?, gallery?, ids?} — writes index.html, content/, assets/ and the rendered images/ into dir → {dir, index, files, photos, bytes}",
            always, export),
        cmd!(query "web.upload", "Upload Web Gallery", [], None,
            "{server: saved server name | {host, port?, user, path, keyFile?, knownFingerprint?}, password?, settings?, gallery?, ids?} — renders the gallery and uploads it over SFTP (host key trusted on first use and remembered for saved servers) → {host, path, files, bytes, photos, fingerprint}",
            always, upload),
        cmd!(query "web.galleries", "Saved Web Galleries", [], None, "{} → [{id, name, settings, ids}] (saved creations of kind web)", always, galleries),
        cmd!(
            "web.saveGallery",
            "Save Web Gallery",
            [],
            None,
            "{name, settings?, gallery?, ids?} — adds or replaces a saved gallery: a collection holding the settings and its photos (replaced only when ids are given) → galleries",
            always,
            save_gallery
        ),
        cmd!("web.deleteGallery", "Delete Web Gallery", [], None, "{name} → galleries", always, delete_gallery),
        cmd!(query "web.servers", "Web Upload Servers", [], None, "{} → [{name, host, port, user, path, keyFile, knownFingerprint}] (never a password)", always,
            |s, _| Ok(servers_json(&load(s).map_err(|e| bad("web.servers", e))?))),
        cmd!(query "web.saveServer", "Save Web Upload Server", [], None,
            "{server: {name, host, port?, user, path, keyFile?}, password?: kept in the keychain} → servers", always, save_server),
        cmd!(query "web.deleteServer", "Delete Web Upload Server", [], None, "{name} — also forgets its keychain secret → servers", always, delete_server),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::Session;
    use crate::remote::MemorySecrets;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dac-web-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn session(tag: &str) -> (Session, std::path::PathBuf) {
        let dir = temp_dir(tag);
        let mut s = Session::with_demo();
        s.remote.connections_path = Some(dir.join("connections.json"));
        s.set_secret_store(Arc::new(MemorySecrets::default()));
        (s, dir)
    }

    #[test]
    fn preview_lists_pages_and_images_for_the_visible_photos() {
        let (mut s, _) = session("preview");
        let n = s.visible().len();
        let v = s.execute("web.preview", &json!({"settings": {"template": "square", "site": {"title": "Demo <b>"}}})).unwrap();
        assert_eq!(v["photos"].as_array().unwrap().len(), n);
        assert_eq!(v["images"].as_array().unwrap().len(), n * 2);
        let index = v["files"].as_array().unwrap().iter().find(|f| f["path"] == "index.html").unwrap();
        assert!(index["text"].as_str().unwrap().contains("Demo &lt;b&gt;"));
        assert!(s.execute("web.preview", &json!({"settings": 3})).is_err());
        assert!(s.execute("web.preview", &json!({"gallery": "missing"})).is_err());
    }

    #[test]
    fn export_writes_a_complete_site() {
        let (mut s, dir) = session("export");
        let ids: Vec<u64> = s.visible().iter().take(2).map(|i| i.0).collect();
        let out = dir.join("site");
        let v = s
            .execute(
                "web.export",
                &json!({"dir": out.to_string_lossy(), "ids": ids, "settings": {"output": {"largeSize": 400, "watermark": "© me"}, "appearance": {"thumbSize": 100}}}),
            )
            .unwrap();
        assert_eq!(v["photos"], 2);
        for f in ["index.html", "assets/gallery.css", "assets/gallery.js", "content/0002.html", "images/large/0001.jpg", "images/thumb/0002.jpg"] {
            assert!(out.join(f).is_file(), "{f}");
        }
        let jpg = std::fs::read(out.join("images/large/0001.jpg")).unwrap();
        assert_eq!(&jpg[..2], &[0xff, 0xd8]);
        assert!(s.execute("web.export", &json!({})).is_err());
    }

    #[test]
    fn saved_galleries_and_servers_round_trip_without_secrets() {
        let (mut s, dir) = session("saved");
        s.execute("web.saveGallery", &json!({"name": "Trip", "settings": {"site": {"title": "Trip"}}})).unwrap();
        let g = s.execute("web.galleries", &json!({})).unwrap();
        assert_eq!(g[0]["settings"]["site"]["title"], "Trip");
        let v = s.execute("web.preview", &json!({"gallery": "trip", "settings": {"template": "track"}})).unwrap();
        assert_eq!(v["settings"]["site"]["title"], "Trip");
        assert_eq!(v["settings"]["template"], "track");
        s.execute(
            "web.saveServer",
            &json!({"server": {"name": "Home", "host": "example.org", "user": "ann", "path": "/www"}, "password": "hunter2"}),
        )
        .unwrap();
        let file = std::fs::read_to_string(dir.join("web.json")).unwrap();
        assert!(file.contains("example.org") && !file.contains("hunter2"));
        assert!(!s.execute("web.servers", &json!({})).unwrap().to_string().contains("hunter2"));
        s.execute("web.deleteServer", &json!({"name": "home"})).unwrap();
        s.execute("web.deleteGallery", &json!({"name": "Trip"})).unwrap();
        assert!(s.execute("web.deleteGallery", &json!({"name": "Trip"})).is_err());
        assert!(s.execute("web.saveServer", &json!({"server": {"name": "x"}})).is_err());
    }

    #[test]
    fn upload_to_an_unreachable_server_is_an_error() {
        let (mut s, _) = session("upload");
        let r = s.execute("web.upload", &json!({"server": {"host": "127.0.0.1", "port": 1, "user": "u", "path": "g"}, "password": "x"}));
        assert!(r.is_err());
        assert!(s.execute("web.upload", &json!({"server": {"host": "127.0.0.1", "user": "u"}})).is_err(), "no password");
    }

    #[test]
    fn galleries_are_catalog_creations_and_old_ones_move_in() {
        let (mut s, dir) = session("migrate");
        let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
        let old = json!({"galleries": [{"name": "Old", "settings": {"site": {"title": "Old"}}, "ids": ids}], "servers": [{"name": "Home", "host": "example.org", "user": "ann", "path": "/www"}]});
        std::fs::write(dir.join("web.json"), old.to_string()).unwrap();
        let g = s.execute("web.galleries", &json!({})).unwrap();
        assert_eq!(g[0]["name"], "Old");
        assert_eq!(g[0]["ids"].as_array().unwrap().len(), 2);
        let id = g[0]["id"].as_u64().unwrap();
        let a = s.catalog.album(dac_catalog::AlbumId(id)).unwrap();
        assert_eq!(a.creation.as_ref().unwrap().kind, "web");
        // moved: the file keeps the server only, a second call adds nothing
        let file = std::fs::read_to_string(dir.join("web.json")).unwrap();
        assert!(!file.contains("\"Old\"") && file.contains("example.org"), "{file}");
        assert_eq!(s.execute("web.galleries", &json!({})).unwrap().as_array().unwrap().len(), 1);
        // a saved gallery's photos are the gallery's
        let v = s.execute("web.preview", &json!({"gallery": id})).unwrap();
        assert_eq!(v["photos"].as_array().map(Vec::len), Some(2));
        assert_eq!(v["settings"]["site"]["title"], "Old");
        s.execute("web.deleteGallery", &json!({"name": "old"})).unwrap();
        assert!(s.catalog.album(dac_catalog::AlbumId(id)).is_none());
    }
}
