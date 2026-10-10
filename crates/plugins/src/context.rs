//! The host side of `host.call`: every request a plug-in makes, checked against its effective
//! capabilities before anything happens.
//!
//! | method                | capability                  | effect                                  |
//! |-----------------------|-----------------------------|-----------------------------------------|
//! | `catalog.query`, `photo.inspect`, `catalog.stats`, `keyword.list` | `catalog` | [`HostApi::catalog`] |
//! | `metadata.setStandard`| `metadataWrite`             | [`HostApi::set_metadata`]               |
//! | `ns.get`, `ns.set`, `ns.photos` | always (own namespace) | [`NamespaceStore`]               |
//! | `fs.readText`, `fs.list`, `fs.exists` | a readable root | std::fs                           |
//! | `fs.writeText`        | a writable root (or a file lent for this call) | std::fs              |
//! | `http.request`        | the URL's host in `network` | `dac-net` (native only)                 |

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::permissions::Permissions;
use crate::store::NamespaceStore;

/// Largest file `fs.readText` returns / `fs.writeText` writes, and largest HTTP body.
pub const MAX_IO_BYTES: usize = 8 << 20;
/// Most entries `fs.list` returns.
pub const MAX_LIST: usize = 10_000;

/// Catalog access the application provides. The default methods refuse, so a host without a
/// catalog (tests, the CLI before a library is open) only implements what it has.
pub trait HostApi {
    /// A read-only catalog request: `method` is one of `catalog.query`, `photo.inspect`,
    /// `catalog.stats`, `keyword.list`, with that engine command's parameters.
    fn catalog(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        let _ = params;
        Err(format!("{method}: no catalog is available"))
    }
    /// Writes standard metadata: `{ids: [photo ids], title?, caption?, keywords?: [..], …}`.
    fn set_metadata(&mut self, params: &Value) -> Result<Value, String> {
        let _ = params;
        Err("metadata.setStandard: not available".into())
    }
}

/// A host with no catalog.
pub struct NoHost;
impl HostApi for NoHost {}

/// Everything one plug-in call may reach.
pub(crate) struct CallContext<'a> {
    pub perms: &'a Permissions,
    /// Files lent for this call only (read and write), e.g. the exported file a hook post-processes.
    pub lent: &'a [PathBuf],
    pub host: &'a mut dyn HostApi,
    pub store: &'a mut NamespaceStore,
}

fn str_arg<'v>(p: &'v Value, key: &str) -> Result<&'v str, String> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing string `{key}`"))
}

impl CallContext<'_> {
    pub fn dispatch(&mut self, method: &str, p: &Value) -> Result<Value, String> {
        match method {
            "catalog.query" | "photo.inspect" | "catalog.stats" | "keyword.list" => {
                if !self.perms.catalog {
                    return Err(denied("catalog"));
                }
                self.host.catalog(method, p)
            }
            "metadata.setStandard" => {
                if !self.perms.metadata_write {
                    return Err(denied("metadataWrite"));
                }
                self.host.set_metadata(p)
            }
            "ns.get" => Ok(self.store.get(str_arg(p, "photo")?)),
            "ns.set" => {
                self.store.set(str_arg(p, "photo")?, str_arg(p, "key")?, p.get("value").cloned().unwrap_or(Value::Null))?;
                Ok(Value::Null)
            }
            "ns.photos" => Ok(json!(self.store.photos())),
            "fs.readText" => {
                let path = self.perms.check_path(str_arg(p, "path")?, false, self.lent)?;
                let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                if meta.len() > MAX_IO_BYTES as u64 {
                    return Err(format!("{}: larger than {MAX_IO_BYTES} bytes", path.display()));
                }
                std::fs::read_to_string(&path).map(Value::from).map_err(|e| format!("{}: {e}", path.display()))
            }
            "fs.writeText" => {
                let path = self.perms.check_path(str_arg(p, "path")?, true, self.lent)?;
                let text = str_arg(p, "text")?;
                if text.len() > MAX_IO_BYTES {
                    return Err(format!("larger than {MAX_IO_BYTES} bytes"));
                }
                std::fs::write(&path, text).map(|_| Value::Null).map_err(|e| format!("{}: {e}", path.display()))
            }
            "fs.exists" => {
                let path = self.perms.check_path(str_arg(p, "path")?, false, self.lent)?;
                Ok(Value::from(path.exists()))
            }
            "fs.list" => {
                let path = self.perms.check_path(str_arg(p, "path")?, false, self.lent)?;
                let rd = std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut names: Vec<Value> = rd
                    .filter_map(|e| e.ok())
                    .take(MAX_LIST)
                    .map(|e| json!({"name": e.file_name().to_string_lossy(), "dir": e.file_type().is_ok_and(|t| t.is_dir())}))
                    .collect();
                names.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                Ok(Value::Array(names))
            }
            "http.request" => self.http(p),
            _ => Err(format!("unknown host method {method:?}")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn http(&mut self, p: &Value) -> Result<Value, String> {
        use dac_net::{Client, ClientConfig, Method, Url};
        let url = str_arg(p, "url")?;
        let parsed = Url::parse(url).map_err(|e| e.to_string())?;
        if !self.perms.allows_host(&parsed.host) {
            return Err(format!("{}: {}", parsed.host, denied("network")));
        }
        let method = match p.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            m => return Err(format!("unsupported HTTP method {m}")),
        };
        // No automatic redirects: a redirect to another host must be a new, checked request.
        let config = ClientConfig { max_redirects: 0, max_body: MAX_IO_BYTES as u64, ..ClientConfig::default() };
        let client = Client::new(config).map_err(|e| e.to_string())?;
        let mut req = client.request(method, url);
        if let Some(h) = p.get("headers").and_then(Value::as_object) {
            for (k, v) in h.iter().take(64) {
                if let Some(v) = v.as_str() {
                    req = req.secret_header(k, v);
                }
            }
        }
        if let Some(body) = p.get("body").and_then(Value::as_str) {
            if body.len() > MAX_IO_BYTES {
                return Err(format!("request body larger than {MAX_IO_BYTES} bytes"));
            }
            let ct = p.get("contentType").and_then(Value::as_str).unwrap_or("application/json");
            req = req.body(ct, body.as_bytes().to_vec());
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status;
        let location = resp.header("location").map(str::to_string);
        let ctype = resp.header("content-type").map(str::to_string);
        let body = resp.bytes().map_err(|e| e.to_string())?;
        Ok(json!({"status": status, "contentType": ctype, "location": location, "body": String::from_utf8_lossy(&body)}))
    }

    #[cfg(target_arch = "wasm32")]
    fn http(&mut self, _: &Value) -> Result<Value, String> {
        Err("http.request is not available in the web build".into())
    }
}

fn denied(cap: &str) -> String {
    format!("permission `{cap}` not granted to this plug-in")
}
