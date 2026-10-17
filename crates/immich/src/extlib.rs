//! IMM-EXTLIB: when an Immich *external library* indexes the same folders as the catalog, the
//! photos are linked by checksum (never uploaded). Immich sees the folders through its container
//! (`/mnt/photos`), the app through the host (`/home/me/Photos`): a path mapping table translates
//! between them, and the setup helper shows which catalog folders each library covers.

use serde::{Deserialize, Serialize};

use crate::types::Library;

/// One row of the mapping table: a folder as Immich's container sees it ↔ the same folder here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMap {
    pub container: String,
    pub local: String,
}

/// Separator-agnostic, trailing-slash-free form for comparisons.
fn norm(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    let t = s.trim_end_matches('/');
    if t.is_empty() && s.starts_with('/') { "/".into() } else { t.to_string() }
}

/// `path` with `prefix` replaced by `with`, when `prefix` is `path` or a whole-component prefix
/// of it.
fn swap(path: &str, prefix: &str, with: &str) -> Option<String> {
    let (path, prefix, with) = (norm(path), norm(prefix), norm(with));
    if prefix.is_empty() {
        return None;
    }
    if path == prefix {
        return Some(with);
    }
    let rest = if prefix == "/" { path.strip_prefix('/')? } else { path.strip_prefix(&prefix)?.strip_prefix('/')? };
    Some(if with == "/" { format!("/{rest}") } else { format!("{with}/{rest}") })
}

/// The local path of a container path (the longest matching row wins).
pub fn to_local(maps: &[PathMap], container_path: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for m in maps {
        if let Some(p) = swap(container_path, &m.container, &m.local)
            && best.as_ref().is_none_or(|(n, _)| norm(&m.container).len() > *n)
        {
            best = Some((norm(&m.container).len(), p));
        }
    }
    best.map(|(_, p)| p)
}

/// The container path of a local path (the longest matching row wins).
pub fn to_container(maps: &[PathMap], local_path: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for m in maps {
        if let Some(p) = swap(local_path, &m.local, &m.container)
            && best.as_ref().is_none_or(|(n, _)| norm(&m.local).len() > *n)
        {
            best = Some((norm(&m.local).len(), p));
        }
    }
    best.map(|(_, p)| p)
}

/// Which external library covers a catalog folder.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    /// The catalog folder (local path).
    pub folder: String,
    /// The folder as Immich's container sees it, when a mapping row applies.
    pub container: Option<String>,
    /// The library whose import path contains it (`None`: not covered).
    pub library: Option<String>,
    pub library_name: Option<String>,
    /// The import path that covers it.
    pub import_path: Option<String>,
}

/// For each catalog folder: its container path and the external library covering it.
pub fn coverage(folders: &[String], libraries: &[Library], maps: &[PathMap]) -> Vec<Coverage> {
    folders
        .iter()
        .map(|f| {
            let container = to_container(maps, f);
            let hit = container
                .as_deref()
                .and_then(|c| libraries.iter().find_map(|l| l.import_paths.iter().find(|ip| swap(c, ip, ip).is_some()).map(|ip| (l, ip.clone()))));
            Coverage {
                folder: f.clone(),
                container,
                library: hit.as_ref().map(|(l, _)| l.id.clone()),
                library_name: hit.as_ref().map(|(l, _)| l.name.clone()),
                import_path: hit.map(|(_, ip)| ip),
            }
        })
        .collect()
}

/// Suggested mapping rows: for every import path whose last folder name matches the last folder
/// name of a catalog folder (`/mnt/photos` ↔ `/home/me/photos`). A guess for the user to confirm.
pub fn suggest(folders: &[String], libraries: &[Library]) -> Vec<PathMap> {
    let last = |p: &str| norm(p).rsplit('/').next().unwrap_or_default().to_lowercase();
    let mut out: Vec<PathMap> = Vec::new();
    for l in libraries {
        for ip in &l.import_paths {
            let name = last(ip);
            if name.is_empty() {
                continue;
            }
            for f in folders {
                if last(f) == name && !out.iter().any(|m| m.container == norm(ip)) {
                    out.push(PathMap { container: norm(ip), local: norm(f) });
                }
            }
        }
    }
    out
}
