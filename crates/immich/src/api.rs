//! The calls v1 needs, in terms of [`crate::http`]: ping, version, paged search, albums, download,
//! upload, stack. Every path is built from the user's base URL, so a server on a LAN address, on a
//! Tailscale name or behind a reverse-proxied prefix is the same code.
//!
//! Responses deserialize into `Option`-heavy, unknown-field-tolerant structs: an older or newer 3.x
//! server that omits (or adds) a field is a `None`, never a decode failure. A JSON answer we cannot
//! read becomes [`Error::Api { status: 0, .. }`], because the server did answer.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;

use crate::error::Error;
use crate::http::{self, Limits as HttpLimits, Response, Url};
use crate::multipart::{self, Part};

/// Timeouts and caps for every call made through one [`Client`].
#[derive(Clone, Debug)]
pub struct Limits {
    pub connect: Duration,
    pub stall: Duration,
    /// Cap on one JSON answer (a page of results is tens of kilobytes).
    pub max_json: u64,
    /// Cap on one downloaded original.
    pub max_download: u64,
    /// Cap on one downloaded thumbnail (a screen-sized JPEG is far under this).
    pub max_thumb: u64,
    /// Cap on one uploaded derivative.
    pub max_upload: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(15),
            stall: Duration::from_secs(30),
            max_json: 8 * 1024 * 1024,
            max_download: 4 * 1024 * 1024 * 1024,
            max_thumb: 32 * 1024 * 1024,
            max_upload: 256 * 1024 * 1024,
        }
    }
}

/// What `GET /server/version` reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for ServerVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// An asset, as far as browsing and importing care. Immich's JSON is camelCase; the snake_case
/// aliases are for servers and proxies that spell it the other way. Fields other servers omit are
/// `None`; fields we do not use (EXIF dumps, people, thumbnails, stacked siblings) are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    /// Content hash, hex — the same value a LightCraft catalog stores for the same photo.
    pub checksum: String,
    #[serde(alias = "original_file_name")]
    pub original_file_name: String,
    #[serde(alias = "file_created_at")]
    pub file_created_at: Option<String>,
    #[serde(alias = "is_favorite")]
    pub is_favorite: bool,
    /// 1–5 where the owner rated it; the rating filter itself is deprecated server-side since 3.2.
    pub rating: Option<u32>,
    pub visibility: Option<String>,
    #[serde(alias = "library_id")]
    pub library_id: Option<String>,
}

/// An album, as far as choosing a publish target cares.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    #[serde(alias = "album_name")]
    pub album_name: String,
    #[serde(alias = "asset_count")]
    pub asset_count: u32,
}

/// What to browse for. Empty fields are simply not sent.
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    pub album_ids: Vec<String>,
    pub tag_ids: Vec<String>,
    pub is_favorite: Option<bool>,
    /// 1–5 (the server deprecates the filter in 3.2 but still honours it).
    pub rating: Option<u32>,
    pub checksum: Option<String>,
    /// ISO 8601 file-creation range, for browsing by date.
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    /// ISO 8601, for incremental imports.
    pub updated_after: Option<String>,
    /// `"IMAGE"` / `"VIDEO"`, as the server spells them.
    pub asset_type: Option<String>,
    /// 1-based. We page by number rather than by the server's `nextPage` token, whose shape has
    /// moved around across 3.x.
    pub page: u32,
    pub page_size: u32,
}

impl SearchQuery {
    fn body(&self) -> serde_json::Value {
        let page = if self.page == 0 { 1 } else { self.page };
        let size = self.page_size.clamp(1, 1000);
        let mut q = serde_json::Map::new();
        if !self.album_ids.is_empty() {
            q.insert("albumIds".into(), serde_json::json!(self.album_ids));
        }
        if !self.tag_ids.is_empty() {
            q.insert("tagIds".into(), serde_json::json!(self.tag_ids));
        }
        if let Some(f) = self.is_favorite {
            q.insert("isFavorite".into(), serde_json::json!(f));
        }
        if let Some(r) = self.rating.filter(|r| (1..=5).contains(r)) {
            q.insert("rating".into(), serde_json::json!(r));
        }
        if let Some(c) = &self.checksum {
            q.insert("checksum".into(), serde_json::json!(c));
        }
        if let Some(d) = &self.created_after {
            q.insert("createdAfter".into(), serde_json::json!(d));
        }
        if let Some(d) = &self.created_before {
            q.insert("createdBefore".into(), serde_json::json!(d));
        }
        if let Some(d) = &self.updated_after {
            q.insert("updatedAfter".into(), serde_json::json!(d));
        }
        if let Some(t) = &self.asset_type {
            q.insert("type".into(), serde_json::json!(t));
        }
        q.insert("withStacked".into(), serde_json::json!(true));
        q.insert("withExif".into(), serde_json::json!(false));
        q.insert("page".into(), serde_json::json!(page));
        q.insert("size".into(), serde_json::json!(size));
        serde_json::Value::Object(q)
    }
}

/// One page of results.
#[derive(Clone, Debug)]
pub struct Paged<T> {
    pub items: Vec<T>,
    /// 1-based, the page these items came from.
    pub page: u32,
    /// A full page usually means another one exists; the caller asks for the next and stops when
    /// it comes back short or empty.
    pub maybe_more: bool,
}

/// `/search/metadata` answers `assets` as a page object `{total, count, items, nextPage}`
/// (current servers) or as a bare array (older ones). Both mean the same page here.
#[derive(Deserialize, Default)]
struct SearchResponse {
    #[serde(default)]
    assets: AssetsAnswer,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AssetsAnswer {
    Paged {
        #[serde(default)]
        items: Vec<Asset>,
        /// A page cursor (a number or a string); only its presence matters.
        #[serde(default, rename = "nextPage")]
        next_page: Option<serde_json::Value>,
    },
    Plain(Vec<Asset>),
}

impl Default for AssetsAnswer {
    fn default() -> Self {
        AssetsAnswer::Plain(Vec::new())
    }
}

impl AssetsAnswer {
    /// The page's items, and whether the server says another page follows.
    fn into_page(self) -> (Vec<Asset>, bool) {
        match self {
            AssetsAnswer::Paged { items, next_page } => (items, next_page.is_some()),
            AssetsAnswer::Plain(v) => (v, false),
        }
    }
}

#[derive(Deserialize)]
struct Ping {
    res: String,
}

/// One Immich server, as configured by the user: its URL and the key they created for themselves.
pub struct Client {
    base: Url,
    api_key: String,
    limits: Limits,
    cancel: AtomicBool,
}

/// How many same-host redirects one call may follow (an added `/api`, a trailing slash, an
/// http → https upgrade) before the server is asked to mean one address.
const MAX_REDIRECTS: usize = 4;

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key is never formatted, not even truncated.
        f.debug_struct("Client").field("base", &self.base.display()).field("api_key", &"[redacted]").field("limits", &self.limits).finish()
    }
}

impl Client {
    /// `base_url` is what the user typed (with or without a trailing `/api`), `api_key` a key from
    /// Immich's Settings > API keys.
    pub fn new(base_url: &str, api_key: &str, limits: Limits) -> Result<Client, Error> {
        let base = Url::parse(base_url).map_err(|e| Error::Transport(e.to_string()))?;
        if api_key.is_empty() || api_key.len() > 200 || api_key.chars().any(|c| c.is_control()) {
            return Err(Error::Unauthorized);
        }
        Ok(Client { base, api_key: api_key.to_string(), limits, cancel: AtomicBool::new(false) })
    }

    /// The base URL, without the key.
    #[must_use]
    pub fn server(&self) -> String {
        self.base.display()
    }

    /// Raise or clear the cancel flag; in-flight transfers stop at the next chunk.
    pub fn set_cancelled(&self, on: bool) {
        self.cancel.store(on, Ordering::Relaxed);
    }

    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn limits(&self) -> HttpLimits<'_> {
        HttpLimits { connect: self.limits.connect, stall: self.limits.stall, cancel: &self.cancel }
    }

    /// `<base>/api<path>`, honouring a reverse-proxied prefix and tolerating a base URL the user
    /// already ended in `/api`.
    fn url(&self, rest: &str) -> Url {
        let root = self.base.path.trim_end_matches('/');
        let root = root.strip_suffix("/api").unwrap_or(root);
        let mut u = self.base.clone();
        u.path = format!("{root}/api{rest}");
        u
    }

    fn headers(&self, content_type: Option<&str>) -> Vec<(&'static str, String)> {
        let mut h = vec![("x-api-key", self.api_key.clone())];
        if let Some(ct) = content_type {
            h.push(("Content-Type", ct.to_string()));
        }
        h
    }

    fn send(&self, method: &str, rest: &str, body: Option<&[u8]>, content_type: Option<&str>) -> Result<Response, Error> {
        let mut url = self.url(rest);
        let mut redirects = 0;
        loop {
            let mut resp = http::request(method, &url, &self.headers(content_type), body, &self.limits())?;
            // A proxied Immich often answers with a redirect: an added /api prefix, a missing
            // trailing slash, http → https. Follow it — same host only, so the API key never
            // travels to a server the user did not name.
            let location = match resp.status {
                301 | 302 | 303 | 307 | 308 => resp.header("location").map(str::to_string),
                _ => None,
            };
            let Some(location) = location else {
                if (200..300).contains(&resp.status) {
                    return Ok(resp);
                }
                return Err(self.error_from(&mut resp, rest));
            };
            if redirects >= MAX_REDIRECTS {
                return Err(Error::Api { status: resp.status, message: "the server keeps redirecting — add it with its final address".into() });
            }
            redirects += 1;
            let next = url.join(&location)?;
            if next.host != url.host {
                return Err(Error::Api {
                    status: resp.status,
                    message: format!("the server redirected to {location}, a different host — add the server with its final address instead"),
                });
            }
            url = next;
        }
    }

    /// Turn a non-2xx answer into an error, quoting the server's own message when it sent one.
    fn error_from(&self, resp: &mut Response, what: &str) -> Error {
        let status = resp.status;
        if status == 404 {
            return Error::NotFound(what.to_string());
        }
        let body = http::read_all(resp, self.limits.max_json, &self.limits()).unwrap_or_default();
        let text = String::from_utf8_lossy(&body).into_owned();
        let quoted = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| ["message", "error", "detail"].iter().find_map(|k| v.get(*k).and_then(|m| m.as_str()).map(str::to_string)));
        let message = quoted
            .or_else(|| if is_message(&text) { Some(text.trim().to_string()) } else { None })
            .or_else(|| (300..400).contains(&status).then(|| "a redirect without a Location header — check the server address".to_string()))
            .unwrap_or_else(|| "the server gave no explanation".to_string());
        match status {
            401 | 403 => Error::Unauthorized,
            _ => Error::Api { status, message },
        }
    }

    /// GET a JSON answer into `T`.
    fn get_json<T: serde::de::DeserializeOwned>(&self, rest: &str) -> Result<T, Error> {
        let mut resp = self.send("GET", rest, None, None)?;
        self.json(&mut resp)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, resp: &mut Response) -> Result<T, Error> {
        let body = http::read_all(resp, self.limits.max_json, &self.limits())?;
        serde_json::from_slice(&body).map_err(|e| Error::Api { status: 0, message: format!("the server's answer was not the JSON we expected: {e}") })
    }

    /// `GET /server/ping` — is it Immich, and is the URL right.
    pub fn ping(&self) -> Result<String, Error> {
        let p: Ping = self.get_json("/server/ping")?;
        Ok(p.res)
    }

    /// `GET /server/version`. Show it when something does not work; it is the first question.
    pub fn version(&self) -> Result<ServerVersion, Error> {
        self.get_json("/server/version")
    }

    /// `POST /search/metadata`, one page. The search DTO is the body itself — current servers
    /// ignore anything nested under a `query` key (they answer, unfiltered, which is worse than
    /// an error).
    pub fn search(&self, query: &SearchQuery) -> Result<Paged<Asset>, Error> {
        let body = serde_json::to_vec(&query.body()).map_err(|e| Error::Api { status: 0, message: e.to_string() })?;
        let mut resp = self.send("POST", "/search/metadata", Some(&body), Some("application/json"))?;
        let page: SearchResponse = self.json(&mut resp)?;
        let (assets, next_page) = page.assets.into_page();
        let got = assets.len() as u64;
        let size = u64::from(query.page_size.clamp(1, 1000));
        Ok(Paged { items: assets, page: if query.page == 0 { 1 } else { query.page }, maybe_more: next_page || got >= size })
    }

    /// Every album (`GET /albums`).
    pub fn albums(&self) -> Result<Vec<Album>, Error> {
        self.get_json("/albums")
    }

    /// The assets of one album (`GET /albums/{id}/assets`).
    pub fn album_assets(&self, album_id: &str) -> Result<Vec<Asset>, Error> {
        self.get_json(&format!("/albums/{}/assets", encode_segment(album_id)))
    }

    /// `GET /assets/{id}/original` into `out`, at most `max_download` bytes, reporting bytes so far
    /// to `progress`. Returns the number of bytes written.
    pub fn download_original<W: Write>(&self, asset_id: &str, out: &mut W, mut progress: impl FnMut(u64)) -> Result<u64, Error> {
        let rest = format!("/assets/{}/original", encode_segment(asset_id));
        let mut resp = self.send("GET", &rest, None, None)?;
        if let Some(n) = resp.content_length().filter(|n| *n > self.limits.max_download) {
            return Err(Error::Limit(format!("this photo is {n} bytes, over the {}-byte limit", self.limits.max_download)));
        }
        let mut done: u64 = 0;
        let mut buf = [0u8; 128 * 1024];
        loop {
            let n = resp.read(&mut buf, &self.limits())?;
            if n == 0 {
                return Ok(done);
            }
            let next = done.saturating_add(n as u64);
            if next > self.limits.max_download {
                return Err(Error::Limit(format!("the download passed the {}-byte limit", self.limits.max_download)));
            }
            out.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| Error::Transport(format!("could not write the download: {e}")))?;
            done = next;
            progress(done);
        }
    }

    /// `GET /assets/{id}` — one asset's record (name, creation date, rating): the cheap way to
    /// label an asset id that came from a search done earlier or from elsewhere.
    pub fn asset(&self, id: &str) -> Result<Asset, Error> {
        self.get_json(&format!("/assets/{}", encode_segment(id)))
    }

    /// `GET /assets/{id}/thumbnail?size=…` into `out`, capped by `max_thumb`. `size` is one of
    /// the server's sizes (e.g. `"preview"`); an unknown one is the server's `Api` error, not ours.
    pub fn thumbnail<W: Write>(&self, id: &str, size: &str, out: &mut W, mut progress: impl FnMut(u64)) -> Result<u64, Error> {
        let rest = format!("/assets/{}/thumbnail?size={}", encode_segment(id), encode_segment(size));
        let mut resp = self.send("GET", &rest, None, None)?;
        if let Some(n) = resp.content_length().filter(|n| *n > self.limits.max_thumb) {
            return Err(Error::Limit(format!("this thumbnail is {n} bytes, over the {}-byte limit", self.limits.max_thumb)));
        }
        let mut done: u64 = 0;
        let mut buf = [0u8; 32 * 1024];
        loop {
            let n = resp.read(&mut buf, &self.limits())?;
            if n == 0 {
                return Ok(done);
            }
            let next = done.saturating_add(n as u64);
            if next > self.limits.max_thumb {
                return Err(Error::Limit(format!("the thumbnail passed the {}-byte limit", self.limits.max_thumb)));
            }
            out.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| Error::Transport(format!("could not write the thumbnail: {e}")))?;
            done = next;
            progress(done);
        }
    }

    /// `POST /assets`: upload a derivative, with its XMP sidecar when there is one. Immich has no
    /// endpoint that replaces an existing asset's bytes, so republishing uploads a new asset.
    pub fn upload_asset(
        &self,
        bytes: &[u8],
        file_name: &str,
        content_type: &str,
        sidecar: Option<&[u8]>,
        created_at: Option<&str>,
        is_favorite: bool,
    ) -> Result<Asset, Error> {
        if bytes.is_empty() {
            return Err(Error::Limit("refusing to upload an empty file".to_string()));
        }
        if bytes.len() as u64 > self.limits.max_upload {
            return Err(Error::Limit(format!("{} bytes is over the {}-byte upload limit", bytes.len(), self.limits.max_upload)));
        }
        let mut options = serde_json::Map::new();
        if let Some(d) = created_at {
            options.insert("fileCreatedAt".into(), serde_json::json!(d));
        }
        options.insert("isFavorite".into(), serde_json::json!(is_favorite));
        let options = serde_json::to_vec(&serde_json::Value::Object(options)).map_err(|e| Error::Api { status: 0, message: e.to_string() })?;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 ^ (bytes.len() as u64) << 32)
            .unwrap_or(0x5eed);
        let bound = multipart::boundary(seed);
        let sidecar_name = format!("{file_name}.xmp");
        let mut parts: Vec<Part<'_>> = vec![Part { name: "file", filename: Some(file_name), content_type: Some(content_type), data: bytes }];
        if let Some(x) = sidecar {
            parts.push(Part { name: "sidecarData", filename: Some(&sidecar_name), content_type: Some("application/xml"), data: x });
        }
        parts.push(Part { name: "options", filename: None, content_type: Some("application/json"), data: &options });
        let body = multipart::encode(&bound, &parts).map_err(Error::Limit)?;
        let ct = format!("multipart/form-data; boundary={bound}");
        let mut resp = self.send("POST", "/assets", Some(&body), Some(&ct))?;
        self.json(&mut resp)
    }

    /// `POST /albums` — create a publish target's album.
    pub fn create_album(&self, name: &str) -> Result<Album, Error> {
        if name.trim().is_empty() || name.chars().count() > 500 {
            return Err(Error::Api { status: 0, message: "an album name is needed".to_string() });
        }
        let body = serde_json::to_vec(&serde_json::json!({ "albumName": name })).map_err(|e| Error::Api { status: 0, message: e.to_string() })?;
        let mut resp = self.send("POST", "/albums", Some(&body), Some("application/json"))?;
        self.json(&mut resp)
    }

    /// `PUT /albums/{id}/assets` — put derivatives into the album.
    pub fn add_to_album(&self, album_id: &str, asset_ids: &[String]) -> Result<(), Error> {
        if asset_ids.is_empty() {
            return Ok(());
        }
        let body = serde_json::to_vec(&serde_json::json!({ "ids": asset_ids })).map_err(|e| Error::Api { status: 0, message: e.to_string() })?;
        self.send("PUT", &format!("/albums/{}/assets", encode_segment(album_id)), Some(&body), Some("application/json"))?;
        Ok(())
    }

    /// `POST /stacks` — group a derivative under its original so the server shows one item.
    pub fn create_stack(&self, primary_id: &str, child_ids: &[String]) -> Result<String, Error> {
        let mut ids = vec![primary_id.to_string()];
        ids.extend(child_ids.iter().cloned());
        let body = serde_json::to_vec(&serde_json::json!({ "assetIds": ids })).map_err(|e| Error::Api { status: 0, message: e.to_string() })?;
        let mut resp = self.send("POST", "/stacks", Some(&body), Some("application/json"))?;
        let v: serde_json::Value = self.json(&mut resp)?;
        Ok(v.get("id").and_then(|i| i.as_str()).unwrap_or(primary_id).to_string())
    }
}

/// Is this the whole answer worth showing, rather than an HTML error page?
fn is_message(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && t.len() < 300 && !t.starts_with('<')
}

/// Percent-encode one path segment (ids come from a server, names from users).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() {
        return "~".to_string();
    }
    out
}
