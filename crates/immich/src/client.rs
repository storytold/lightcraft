//! The typed client: one method per endpoint the app calls (plan/immich.md → Endpoints we need).
//! Blocking, like `dac-net`: run it on a worker thread.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use dac_credentials::Secret;
use dac_net::{ClientConfig, DownloadOptions, Fingerprint, Method, Trust};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{ImmichError, status};
use crate::types::*;

/// How to reach one server.
#[derive(Clone, Debug, Default)]
pub struct ServerOptions {
    /// A certificate fingerprint the user confirmed (`AB:CD:…`), for self-signed servers.
    pub pinned: Option<String>,
    /// Attempts after the first for idempotent requests that failed with a retryable error.
    pub retries: u32,
    /// Wait before the first retry (doubles each time).
    pub backoff: Duration,
    /// Connect timeout (default 10 s).
    pub connect_timeout: Option<Duration>,
    /// Stall timeout (default 30 s).
    pub stall_timeout: Option<Duration>,
}

impl ServerOptions {
    /// Two retries, half a second apart then one second.
    pub fn standard() -> ServerOptions {
        ServerOptions { retries: 2, backoff: Duration::from_millis(500), ..ServerOptions::default() }
    }
}

/// Normalize what the user typed: trim, add `https://` when there is no scheme, drop a trailing
/// `/` and a trailing `/api`.
pub fn normalize_url(input: &str) -> Result<String, ImmichError> {
    let mut s = input.trim().to_string();
    if s.is_empty() {
        return Err(ImmichError::BadUrl("empty".into()));
    }
    if !s.contains("://") {
        s = format!("https://{s}");
    }
    while s.ends_with('/') {
        s.pop();
    }
    if let Some(stripped) = s.strip_suffix("/api") {
        s = stripped.to_string();
    }
    dac_net::Url::parse(&s).map_err(ImmichError::from)?;
    Ok(s)
}

/// A connection to one server with one API key.
#[derive(Clone)]
pub struct Client {
    http: dac_net::Client,
    base: String,
    key: Secret,
    retries: u32,
    backoff: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("key", &self.key).finish()
    }
}

impl Client {
    pub fn new(url: &str, key: Secret, opts: &ServerOptions) -> Result<Client, ImmichError> {
        let base = normalize_url(url)?;
        let mut trust = Trust::new();
        if let Some(fp) = opts.pinned.as_deref().filter(|s| !s.is_empty()) {
            let fp = Fingerprint::parse(fp).ok_or_else(|| ImmichError::Tls(format!("not a certificate fingerprint: {fp}")))?;
            trust = trust.with_pinned(fp);
        }
        let mut cfg = ClientConfig { trust, ..ClientConfig::default() };
        if let Some(t) = opts.connect_timeout {
            cfg.connect_timeout = t;
        }
        if let Some(t) = opts.stall_timeout {
            cfg.stall_timeout = t;
        }
        let http = dac_net::Client::new(cfg)?;
        Ok(Client { http, base, key, retries: opts.retries, backoff: opts.backoff })
    }

    /// The server's address (`https://host[:port]`, no `/api`).
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The web page of an asset.
    pub fn asset_web_url(&self, id: &str) -> String {
        asset_web_url(&self.base, id)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api{path}", self.base)
    }

    fn send(&self, method: Method, path: &str, body: Option<&serde_json::Value>, auth: bool) -> Result<dac_net::Response, ImmichError> {
        let mut attempt = 0;
        loop {
            let mut req = self.http.request(method, &self.url(path)).header("accept", "application/json");
            if auth {
                req = req.secret_header("x-api-key", self.key.expose());
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let r = req.send().map_err(ImmichError::from).and_then(|r| if r.is_success() { Ok(r) } else { Err(status(r.status, path)) });
            match r {
                Err(e) if e.retryable() && attempt < self.retries => {
                    let wait = self.backoff.saturating_mul(1 << attempt.min(8));
                    log::info!("immich {} {path}: {e}; retrying in {wait:?}", method.as_str());
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str, auth: bool) -> Result<T, ImmichError> {
        self.send(Method::Get, path, None, auth)?.json().map_err(|e| ImmichError::Protocol(format!("{path}: {e}")))
    }

    fn post<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T, ImmichError> {
        let v = serde_json::to_value(body).map_err(|e| ImmichError::Protocol(e.to_string()))?;
        self.send(Method::Post, path, Some(&v), true)?.json().map_err(|e| ImmichError::Protocol(format!("{path}: {e}")))
    }

    // ---- connect

    /// `GET /server/ping`: reachable and an Immich server.
    pub fn ping(&self) -> Result<(), ImmichError> {
        #[derive(serde::Deserialize)]
        struct Pong {
            res: String,
        }
        let p: Pong = self.get("/server/ping", false)?;
        if p.res == "pong" { Ok(()) } else { Err(ImmichError::Protocol("ping did not answer pong".into())) }
    }

    /// `GET /server/version` (no key needed).
    pub fn version(&self) -> Result<ServerVersion, ImmichError> {
        self.get("/server/version", false)
    }

    /// [`Client::version`], refusing servers older than [`MIN_VERSION`].
    pub fn check_version(&self) -> Result<ServerVersion, ImmichError> {
        let v = self.version()?;
        if v < MIN_VERSION { Err(ImmichError::TooOld(v)) } else { Ok(v) }
    }

    pub fn me(&self) -> Result<User, ImmichError> {
        self.get("/users/me", true)
    }

    pub fn api_key(&self) -> Result<ApiKeyInfo, ImmichError> {
        self.get("/api-keys/me", true)
    }

    /// Everything IMM-CONNECT shows: version (checked), user and the key's permissions.
    pub fn status(&self) -> Result<ServerStatus, ImmichError> {
        let version = self.check_version()?;
        let user = self.me()?;
        // a key may lack `apiKey.read`: its permissions are then unknown, not an error
        let permissions = match self.api_key() {
            Ok(k) => Some(k.permissions),
            Err(ImmichError::Forbidden(_)) | Err(ImmichError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        Ok(ServerStatus { version, user, permissions })
    }

    // ---- assets

    /// One page of `POST /search/metadata`.
    pub fn search(&self, q: &MetadataSearch) -> Result<AssetPage, ImmichError> {
        let r: SearchResponse = self.post("/search/metadata", q)?;
        Ok(r.assets)
    }

    /// Every asset matching `q`, page by page (`size` per page), calling `each` per page; stops
    /// at the end, after `max_pages`, or when `each` returns `false`.
    pub fn search_all(&self, q: &MetadataSearch, max_pages: u32, mut each: impl FnMut(&[Asset]) -> bool) -> Result<u64, ImmichError> {
        let mut q = q.clone();
        let mut page = q.page.unwrap_or(1).max(1);
        let mut seen = 0u64;
        for _ in 0..max_pages.max(1) {
            q.page = Some(page);
            let p = self.search(&q)?;
            seen = seen.saturating_add(p.items.len() as u64);
            if !each(&p.items) {
                break;
            }
            match p.next_page.as_deref().and_then(|n| n.parse::<u32>().ok()) {
                Some(n) if n > page && !p.items.is_empty() => page = n,
                _ => break,
            }
        }
        Ok(seen)
    }

    pub fn asset(&self, id: &str) -> Result<Asset, ImmichError> {
        self.get(&format!("/assets/{}", path_id(id)?), true)
    }

    /// The thumbnail (`size`: `thumbnail` or `preview`), as encoded bytes (JPEG/WebP).
    pub fn thumbnail(&self, id: &str, size: &str) -> Result<Vec<u8>, ImmichError> {
        let size = if size == "preview" { "preview" } else { "thumbnail" };
        let r = self.send(Method::Get, &format!("/assets/{}/thumbnail?size={size}", path_id(id)?), None, true)?;
        r.bytes().map_err(ImmichError::from)
    }

    /// Download an asset's original to `dest` (resumable; written as `<dest>.part` first).
    pub fn download_original(&self, id: &str, dest: &Path, cancel: Option<Arc<AtomicBool>>) -> Result<u64, ImmichError> {
        let url = self.url(&format!("/assets/{}/original", path_id(id)?));
        let opts = DownloadOptions { headers: vec![("x-api-key".into(), self.key.expose().to_string(), true)], cancel, ..DownloadOptions::default() };
        let d = self.http.download(&url, dest, opts).map_err(|e| match e {
            dac_net::NetError::Status { code, .. } => status(code, "original"),
            e => ImmichError::from(e),
        })?;
        Ok(d.bytes)
    }

    /// `POST /assets/bulk-upload-check`: which checksums the server already has.
    pub fn upload_check(&self, items: &[UploadCheck]) -> Result<Vec<UploadCheckResult>, ImmichError> {
        let r: UploadCheckResponse = self.post("/assets/bulk-upload-check", &serde_json::json!({ "assets": items }))?;
        Ok(r.results)
    }

    // ---- albums, people, libraries

    pub fn albums(&self) -> Result<Vec<Album>, ImmichError> {
        self.get("/albums", true)
    }

    pub fn people(&self) -> Result<Vec<Person>, ImmichError> {
        let p: People = self.get("/people?withHidden=false", true)?;
        Ok(p.people)
    }

    pub fn libraries(&self) -> Result<Vec<Library>, ImmichError> {
        self.get("/libraries", true)
    }

    /// `POST /libraries/{id}/scan`: Immich re-reads the library's folders (new files, changed
    /// XMP sidecars) in the background. Needs an admin key with `library.update`.
    pub fn scan_library(&self, id: &str) -> Result<(), ImmichError> {
        self.send(Method::Post, &format!("/libraries/{}/scan", path_id(id)?), Some(&serde_json::json!({})), true).map(|_| ())
    }

    /// `POST /libraries`: a new external library of `owner` over `import_paths` (container paths).
    pub fn create_library(&self, owner: &str, name: &str, import_paths: &[String]) -> Result<Library, ImmichError> {
        self.post("/libraries", &serde_json::json!({"ownerId": owner, "name": name, "importPaths": import_paths}))
    }

    /// `POST /assets/jobs` `refresh-metadata`: Immich re-reads the assets' files and XMP sidecars
    /// (a library scan skips files that did not change). Needs `job.create`.
    pub fn refresh_metadata(&self, ids: &[String]) -> Result<(), ImmichError> {
        for chunk in ids.chunks(500) {
            let body = serde_json::json!({"assetIds": chunk, "name": "refresh-metadata"});
            self.send(Method::Post, "/assets/jobs", Some(&body), true)?;
        }
        Ok(())
    }

    /// `PUT /jobs/sidecar` (discover): Immich looks for XMP sidecars of assets that have none yet
    /// and reads them. Needs an admin key (`job.create`).
    pub fn discover_sidecars(&self) -> Result<(), ImmichError> {
        let body = serde_json::json!({"command": "start", "force": false});
        self.send(Method::Put, "/jobs/sidecar", Some(&body), true).map(|_| ())
    }

    // ---- sync (IMM-SYNC)

    /// `PUT /assets/{id}`: change an asset's rating, favourite, description, location, date or
    /// visibility. The one place the (v3-deprecated) single-asset update is called, so the switch
    /// to its replacement stays local. Needs `asset.update`.
    pub fn update_asset(&self, id: &str, u: &AssetUpdate) -> Result<Asset, ImmichError> {
        let v = serde_json::to_value(u).map_err(|e| ImmichError::Protocol(e.to_string()))?;
        let path = format!("/assets/{}", path_id(id)?);
        self.send(Method::Put, &path, Some(&v), true)?.json().map_err(|e| ImmichError::Protocol(format!("{path}: {e}")))
    }

    /// `PUT /tags`: create the tags (hierarchical values `a/b/c`) that don't exist yet → all of
    /// them with their ids. Needs `tag.create`.
    pub fn upsert_tags(&self, values: &[String]) -> Result<Vec<TagInfo>, ImmichError> {
        let mut out = Vec::new();
        for chunk in values.chunks(500) {
            let v = serde_json::json!({ "tags": chunk });
            let r: Vec<TagInfo> =
                self.send(Method::Put, "/tags", Some(&v), true)?.json().map_err(|e| ImmichError::Protocol(format!("/tags: {e}")))?;
            out.extend(r);
        }
        Ok(out)
    }

    /// `PUT /tags/{id}/assets`: tag the assets. Needs `tag.asset`.
    pub fn tag_assets(&self, tag: &str, ids: &[String]) -> Result<(), ImmichError> {
        for chunk in ids.chunks(500) {
            self.send(Method::Put, &format!("/tags/{}/assets", path_id(tag)?), Some(&serde_json::json!({ "ids": chunk })), true)?;
        }
        Ok(())
    }

    /// `DELETE /tags/{id}/assets`: untag the assets. Needs `tag.asset`.
    pub fn untag_assets(&self, tag: &str, ids: &[String]) -> Result<(), ImmichError> {
        for chunk in ids.chunks(500) {
            self.send(Method::Delete, &format!("/tags/{}/assets", path_id(tag)?), Some(&serde_json::json!({ "ids": chunk })), true)?;
        }
        Ok(())
    }

    /// `GET /tags`: every tag of the user.
    pub fn tags(&self) -> Result<Vec<TagInfo>, ImmichError> {
        self.get("/tags", true)
    }

    /// `DELETE /assets` (not forced): the assets go to Immich's trash, never a hard delete.
    pub fn trash_assets(&self, ids: &[String]) -> Result<(), ImmichError> {
        for chunk in ids.chunks(500) {
            for id in chunk {
                path_id(id)?;
            }
            self.send(Method::Delete, "/assets", Some(&serde_json::json!({ "ids": chunk, "force": false })), true)?;
        }
        Ok(())
    }

    // ---- people (IMM-PEOPLE)

    /// Every person, hidden ones included (names, birth dates, hidden flag).
    pub fn people_all(&self) -> Result<Vec<Person>, ImmichError> {
        let mut out = Vec::new();
        for page in 1..=200u32 {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Page {
                #[serde(default)]
                people: Vec<Person>,
                #[serde(default)]
                has_next_page: bool,
            }
            let p: Page = self.get(&format!("/people?withHidden=true&page={page}&size=1000"), true)?;
            let n = p.people.len();
            out.extend(p.people);
            if !p.has_next_page || n == 0 {
                break;
            }
        }
        Ok(out)
    }

    /// `GET /faces?id=<assetId>`: the asset's faces with their people. Needs `face.read`.
    pub fn faces(&self, asset: &str) -> Result<Vec<Face>, ImmichError> {
        self.get(&format!("/faces?id={}", path_id(asset)?), true)
    }

    /// `PUT /people`: rename people (`(id, name)`). Needs `person.update`.
    pub fn rename_people(&self, names: &[(String, String)]) -> Result<(), ImmichError> {
        for chunk in names.chunks(500) {
            let mut people = Vec::new();
            for (id, name) in chunk {
                people.push(serde_json::json!({ "id": path_id(id)?, "name": name }));
            }
            self.send(Method::Put, "/people", Some(&serde_json::json!({ "people": people })), true)?;
        }
        Ok(())
    }

    /// `POST /people/{id}/merge`: merge `others` into `into`. Needs `person.merge`.
    pub fn merge_people(&self, into: &str, others: &[String]) -> Result<(), ImmichError> {
        for o in others {
            path_id(o)?;
        }
        self.send(Method::Post, &format!("/people/{}/merge", path_id(into)?), Some(&serde_json::json!({ "ids": others })), true).map(|_| ())
    }

    // ---- search (IMM-SEARCH)

    /// One page of `POST /search/smart` (Immich's CLIP search; needs its machine learning).
    pub fn smart_search(&self, q: &SmartSearch) -> Result<AssetPage, ImmichError> {
        let r: SearchResponse = self.post("/search/smart", q)?;
        Ok(r.assets)
    }

    /// `DELETE /libraries/{id}` (its assets leave Immich; the files stay).
    pub fn delete_library(&self, id: &str) -> Result<(), ImmichError> {
        self.send(Method::Delete, &format!("/libraries/{}", path_id(id)?), None, true).map(|_| ())
    }
}

/// The web page of an asset on server `base`.
pub fn asset_web_url(base: &str, id: &str) -> String {
    format!("{}/photos/{id}", base.trim_end_matches('/'))
}

/// The server's API-key page, where a user creates the key the app asks for.
pub fn api_key_page(base: &str) -> String {
    format!("{}/user-settings?isOpen=api-keys", base.trim_end_matches('/'))
}

/// An id that goes into a path: Immich ids are UUIDs; anything with other characters is refused
/// rather than sent (no path or query injection from a hostile listing).
fn path_id(id: &str) -> Result<&str, ImmichError> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        Ok(id)
    } else {
        Err(ImmichError::Protocol(format!("not an asset id: {id:?}")))
    }
}

/// What [`Client::status`] found.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub version: ServerVersion,
    pub user: User,
    /// `None`: the key may not read its own permissions.
    pub permissions: Option<Vec<String>>,
}

/// Permissions each feature needs (`all` covers everything).
pub const FEATURE_PERMISSIONS: &[(&str, &[&str])] = &[
    ("link", &["asset.read"]),
    ("import", &["asset.read", "asset.view", "asset.download", "album.read", "person.read"]),
    ("externalLibraries", &["library.read"]),
    ("sync", &["asset.read", "asset.update", "tag.read", "tag.create", "tag.asset"]),
    ("people", &["person.read", "face.read"]),
    ("smartSearch", &["asset.read"]),
];

/// The features a key with `perms` can't use, with the permissions each lacks.
pub fn missing_permissions(perms: &[String]) -> Vec<(&'static str, Vec<&'static str>)> {
    if perms.iter().any(|p| p == "all") {
        return Vec::new();
    }
    FEATURE_PERMISSIONS
        .iter()
        .filter_map(|(f, need)| {
            let lack: Vec<&str> = need.iter().copied().filter(|n| !perms.iter().any(|p| p == n)).collect();
            (!lack.is_empty()).then_some((*f, lack))
        })
        .collect()
}
