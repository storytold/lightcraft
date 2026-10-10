//! Tethered capture (L3). First stage: **studio capture** (plan: PLAN_phase_4.md §4.1.1). The
//! camera's own app or any vendor tool writes shots into a folder; a studio session watches it
//! and imports each new shot as it lands, with a develop preset, a metadata preset and keywords,
//! a session name and naming template, and a target collection. The newest shot is selected so
//! the loupe shows it.
//!
//! This crate keeps the session ([`StudioSession`], saved as `tether.json` in the library folder)
//! and decides which files are ready ([`StudioSession::fresh`]: a file is imported once its size
//! held still between two scans, so a shot still being written waits). The engine's `tether.*`
//! commands turn ready files into a `library.import`. Native PTP capture comes later.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

/// The session file in the library folder.
pub const FILE: &str = "tether.json";
/// The token a naming template uses for the session name.
pub const SESSION_TOKEN: &str = "{session}";
/// The most files one scan looks at (a folder of millions can't stall the app).
pub const MAX_LISTING: usize = 100_000;
/// The most source paths remembered as taken.
const MAX_TAKEN: usize = 1_000_000;

/// A studio capture session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StudioSession {
    /// The session name (also the folder copies go to, and `{session}` in the naming template).
    pub name: String,
    /// The watched folder.
    pub folder: String,
    /// Copy shots into the library (`Originals/<session>/`), else add them in place.
    pub copy: bool,
    /// A develop preset id applied to every shot.
    pub preset: Option<String>,
    /// A metadata preset (by name).
    pub metadata_preset: Option<String>,
    pub keywords: Vec<String>,
    /// Copy: a file-name template (`{session}`, plus the import's rename tokens such as `{seq:4}`).
    pub naming: Option<String>,
    /// The collection (album name) shots go into; made when missing.
    pub collection: Option<String>,
    /// The next sequence number for the naming template.
    pub next_seq: usize,
    /// Watching (false: the session is kept but paused).
    pub active: bool,
    /// The newest imported shot (photo id).
    pub newest: Option<u64>,
    pub shots: usize,
    /// Source files already taken (imported, or tried and refused): never imported twice.
    taken: BTreeSet<String>,
    /// Files seen at the last scan and their sizes then.
    seen: BTreeMap<String, u64>,
}

/// A name usable as a folder or in a file name: separators, braces and control characters become
/// `_`; never empty.
pub fn safe_session_name(s: &str) -> String {
    let n: String = s.trim().chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0' | '{' | '}') || c.is_control() { '_' } else { c }).collect();
    let n = n.trim().trim_matches('.').trim().to_string();
    if n.is_empty() { "Session".into() } else { n }
}

/// The folder's files (not hidden ones, not subfolders) and their sizes, sorted by name.
pub fn list_folder(folder: &Path) -> Result<Vec<(String, u64)>, String> {
    let rd = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut out: Vec<(String, u64)> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let m = std::fs::metadata(e.path()).ok()?;
            m.is_file().then(|| (e.path().to_string_lossy().to_string(), m.len()))
        })
        .take(MAX_LISTING)
        .collect();
    out.sort();
    Ok(out)
}

impl StudioSession {
    pub fn new(name: &str, folder: &str) -> StudioSession {
        StudioSession { name: safe_session_name(name), folder: folder.trim().to_string(), next_seq: 1, active: true, ..Default::default() }
    }

    /// Read the session from the library folder `dir` (`None`: no session).
    pub fn load(dir: &Path) -> Result<Option<StudioSession>, String> {
        let path = dir.join(FILE);
        match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Option<StudioSession>>(&b).map_err(|e| format!("{} is damaged: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Save `session` (`None`: forget it) into the library folder `dir`.
    pub fn save(session: Option<&StudioSession>, dir: &Path) -> Result<(), String> {
        let path = dir.join(FILE);
        let body = serde_json::to_vec_pretty(&session).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The files of `listing` ready to import: not taken yet, not empty, and the same size as at
    /// the last scan (a shot still being written waits for the next one). They are marked taken.
    pub fn fresh(&mut self, listing: &[(String, u64)]) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeMap::new();
        for (path, size) in listing.iter().take(MAX_LISTING) {
            if self.taken.contains(path) {
                continue;
            }
            if *size > 0 && self.seen.get(path) == Some(size) {
                out.push(path.clone());
            } else {
                seen.insert(path.clone(), *size);
            }
        }
        self.seen = seen;
        if self.taken.len().saturating_add(out.len()) <= MAX_TAKEN {
            self.taken.extend(out.iter().cloned());
        }
        out
    }

    /// Mark files as taken without importing them (files already in the folder when the session
    /// started, when only new shots should come in).
    pub fn skip_existing(&mut self, listing: &[(String, u64)]) {
        for (p, _) in listing.iter().take(MAX_LISTING) {
            if self.taken.len() < MAX_TAKEN {
                self.taken.insert(p.clone());
            }
        }
    }

    /// How many source files were taken.
    pub fn taken(&self) -> usize {
        self.taken.len()
    }

    /// The naming template with `{session}` filled in.
    pub fn naming_template(&self) -> Option<String> {
        let t = self.naming.as_deref().map(str::trim).filter(|t| !t.is_empty())?;
        Some(t.replace(SESSION_TOKEN, &safe_session_name(&self.name)))
    }

    /// `library.import` params for `paths`. `destination`: where copies go (the library's
    /// `Originals/<session>`), used in copy mode.
    pub fn import_params(&self, paths: &[String], destination: Option<&Path>) -> Value {
        let mut p = json!({"paths": paths, "mode": if self.copy { "copy" } else { "add" }});
        if self.copy {
            if let Some(d) = destination {
                p["destination"] = json!(d.join(safe_session_name(&self.name)).to_string_lossy());
                p["organize"] = json!("flat");
            }
            if let Some(t) = self.naming_template() {
                p["rename"] = json!(t);
                p["renameStart"] = json!(self.next_seq.max(1));
            }
        }
        if let Some(x) = &self.preset {
            p["preset"] = json!(x);
        }
        if let Some(x) = &self.metadata_preset {
            p["metadataPreset"] = json!(x);
        }
        if !self.keywords.is_empty() {
            p["keywords"] = json!(self.keywords);
        }
        if let Some(c) = &self.collection {
            p["albumName"] = json!(c);
        }
        p
    }

    /// After an import of `imported` photo ids (in shot order): advance the sequence, remember the
    /// newest.
    pub fn imported(&mut self, imported: &[u64]) {
        if let Some(last) = imported.last() {
            self.newest = Some(*last);
        }
        self.shots = self.shots.saturating_add(imported.len());
        self.next_seq = self.next_seq.max(1).saturating_add(imported.len());
    }

    /// The session as shown to the UI and agents (without the internal file lists).
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "folder": self.folder,
            "copy": self.copy,
            "preset": self.preset,
            "metadataPreset": self.metadata_preset,
            "keywords": self.keywords,
            "naming": self.naming,
            "collection": self.collection,
            "nextSeq": self.next_seq,
            "active": self.active,
            "newest": self.newest,
            "shots": self.shots,
        })
    }
}
