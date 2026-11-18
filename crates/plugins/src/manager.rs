//! [`Manager`]: installed plug-ins, their grants, enabled state, logs and data.
//!
//! On disk (a folder, usually `<settings>/plugins`): `<id>.wasm` per plug-in, `plugins.json` with
//! each plug-in's `{enabled, grant, denied, signer?}` (what the user allowed, what they said no to,
//! and the author key pinned at its first signed install, [`Pin`]), `data/<id>.json`, its metadata namespace (per library), and
//! `settings/<id>.json`, its settings and remembered dialog values (for the whole app), and
//! `previous/<id>.wasm` + `previous/<id>.json`, the version an update replaced, its settings and
//! its signer (checked when it goes back), to
//! go back to ([`Manager::rollback`]; nothing loads from that folder). A manager without a folder
//! keeps everything in memory (tests, the web build).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::{CallContext, HostApi, JobReporter, Unwatched};
use crate::manifest::{Field, dialog_args, parse_version};
use crate::permissions::{Permissions, Request, refused_root};
use crate::runtime::{Limits, Plugin};
use crate::settings::{MAX_SETTINGS_BYTES, SettingsStore, command_scope, destination_scope};
use crate::signature::{Signature, Signer};
use crate::store::{NamespaceStore, write_atomic};
use crate::{Error, Result};

/// Log lines kept per plug-in.
const MAX_LOG: usize = 500;
/// Most plug-ins loaded from a folder.
const MAX_PLUGINS: usize = 256;
/// Most photos / files one hook request carries.
const MAX_BATCH: usize = 10_000;
/// Longest message a `prepare` hook may show.
const MAX_PREPARE_MESSAGE: usize = 4096;
/// Largest `previous/<id>.json` read back: the settings it keeps, and room for its version.
const MAX_PREVIOUS_BYTES: u64 = MAX_SETTINGS_BYTES as u64 + 64 * 1024;

/// One installed plug-in.
#[derive(Debug)]
pub struct Installed {
    pub plugin: Arc<Plugin>,
    pub enabled: bool,
    /// What the user granted (the effective set is this ∩ the manifest's request).
    pub grant: Permissions,
    /// What the user said no to when asked, with the date: never asked again.
    pub denied: Vec<Denial>,
    /// Folders the manifest asks for that can't be granted, with why ([`refused_root`]).
    refused: Vec<(String, String)>,
    pub log: VecDeque<String>,
    store: Option<NamespaceStore>,
    /// Its settings and remembered dialog values (shared with a call running off the lock).
    settings: Arc<Mutex<SettingsStore>>,
    /// A call is running off the manager's lock ([`Manager::begin`] … [`Manager::end`]): its store
    /// is lent to it. What it is, for the error other calls get meanwhile.
    busy: Option<String>,
    /// [`Installed::effective`] as of the last grant change (nothing while disabled or once
    /// uninstalled), shared with a call running off the lock, so a job's next `{continue}` call
    /// gets a revoke, a disable or a removal made while it runs.
    live: Arc<Mutex<Permissions>>,
    /// Its module file (`None` without a folder): `<id>.wasm`, or the file it was copied in as.
    path: Option<PathBuf>,
    /// The author key pinned for it.
    pin: Pin,
}

impl Installed {
    /// The author key pinned for it ([`Pin`]).
    pub fn pin(&self) -> &Pin {
        &self.pin
    }

    /// Its `plugins.json` entry.
    fn entry(&self) -> Entry {
        Entry { enabled: self.enabled, grant: self.grant.clone(), denied: self.denied.clone(), signer: self.pin.stored() }
    }

    /// Its module file in the plug-ins folder (`None` without a folder).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The capabilities calls actually get.
    pub fn effective(&self) -> Permissions {
        self.grant.intersect(&self.plugin.manifest().permissions).without_refused(|p| self.refused.iter().any(|(r, _)| r == p))
    }

    /// Where `r` stands: allowed, denied (on a date), refused (a folder that can't be granted)
    /// or never asked.
    pub fn state(&self, r: &Request) -> RequestState {
        if let Some((_, why)) = self.refused.iter().find(|(p, _)| r.group == crate::Group::Fs && r.value.as_deref() == Some(p)) {
            RequestState::Refused(why.clone())
        } else if self.effective().holds(r) {
            RequestState::Allowed
        } else if let Some(d) = self.denied.iter().find(|d| d.request == *r) {
            RequestState::Denied(d.on.clone())
        } else {
            RequestState::Never
        }
    }

    /// Each request of the manifest with its state, as `plugin.list` gives them.
    pub fn requests(&self) -> Vec<Value> {
        let requested = &self.plugin.manifest().permissions;
        requested
            .requests()
            .into_iter()
            .map(|r| {
                let mut v = json!({"group": r.group, "value": r.value});
                if r.group == crate::Group::Fs {
                    v["write"] = json!(requested.fs.iter().any(|f| Some(&f.path) == r.value.as_ref() && f.write));
                }
                match self.state(&r) {
                    RequestState::Allowed => v["state"] = json!("allowed"),
                    RequestState::Never => v["state"] = json!("never"),
                    RequestState::Denied(on) => {
                        v["state"] = json!("denied");
                        v["deniedOn"] = json!(on);
                    }
                    RequestState::Refused(why) => {
                        v["state"] = json!("refused");
                        v["reason"] = json!(why);
                    }
                }
                v
            })
            .collect()
    }

    /// Hands [`Installed::effective`] (nothing while disabled) to calls running now: their next
    /// call gets it.
    fn share_effective(&self) {
        let perms = if self.enabled { self.effective() } else { Permissions::default() };
        *self.live.lock().unwrap_or_else(PoisonError::into_inner) = perms;
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= MAX_LOG {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// The plug-in as JSON for lists (`plugin.list`).
    pub fn info(&self) -> Value {
        let m = self.plugin.manifest();
        json!({
            "id": m.id, "name": m.name, "version": m.version, "description": m.description, "author": m.author,
            "requires": m.requires,
            "enabled": self.enabled, "size": self.plugin.size(),
            "requested": m.permissions, "granted": self.effective(), "requests": self.requests(),
            "commands": m.commands, "hooks": m.hooks, "publish": m.publish,
            "exportDestinations": m.export_destinations, "metadataFields": m.metadata_fields,
            "settings": m.settings,
            // its own pictures (`plugin.picture` gives them)
            "icon": m.icon.is_some(), "header": m.header.is_some(),
            // where it comes from: homepage, repository, changelog, license, issues
            "about": m.about.info(),
            // the author key pinned for it, as a fingerprint; null: unsigned
            "signer": self.pin.signer().map(|s| s.fingerprint()),
            // a page showing the settings is out of date when either changes (an update, or a
            // value set by the plug-in or another window)
            "moduleHash": self.plugin.module_hash(), "settingsRevision": self.settings().revision(),
        })
    }

    fn settings(&self) -> std::sync::MutexGuard<'_, SettingsStore> {
        self.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A plug-in's settings as its page shows them ([`Manager::settings`]).
#[derive(Debug, Clone)]
pub struct SettingsView {
    /// The manifest's settings fields, as the `settings.prepare` hook prepared them.
    pub fields: Vec<Field>,
    /// Per field, the stored value when the field (as prepared) takes it, else its default.
    pub values: serde_json::Map<String, Value>,
    /// Why the prepare step failed (the fields are then as declared).
    pub error: Option<String>,
}

/// How [`Manager::begin_command`] went: the prepare step's message (nothing runs), or the call.
pub enum Begun {
    Message(String),
    Call(Box<PendingCall>),
}

/// What a command's `prepare` step decided ([`Manager::prepare_command`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Prepared {
    /// Show the dialog with these fields (the manifest's, with the options and defaults the
    /// plug-in gave). `error`: the prepare step failed, and these are the manifest's own fields.
    Dialog { fields: Vec<Field>, error: Option<String> },
    /// Run the command at once with these arguments, without the dialog.
    Run(Value),
    /// Show this message and run nothing.
    Message(String),
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    plugins: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    enabled: bool,
    grant: Permissions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    denied: Vec<Denial>,
    /// The pinned author key in standard base64 ([`Pin`]); none: unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signer: Option<String>,
}

impl Entry {
    fn pin(&self) -> Pin {
        match &self.signer {
            None => Pin::None,
            Some(text) => Signer::from_base64(text).map_or_else(|_| Pin::Unreadable(text.clone()), Pin::Key),
        }
    }
}

/// The author key pinned for an installed plug-in: set at its first signed install, kept while
/// later files are signed by the same key, and changed only when the user says yes to another
/// signer ([`SignerAnswer`]). A file that no longer matches it doesn't load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pin {
    /// Installed unsigned: its first signed version pins its key.
    None,
    Key(Signer),
    /// `plugins.json` holds something that isn't a key (kept as it was): treated as another signer.
    Unreadable(String),
}

impl Pin {
    /// The pinned key, when there is one that can be read.
    pub fn signer(&self) -> Option<Signer> {
        match self {
            Pin::Key(s) => Some(*s),
            _ => None,
        }
    }

    /// As `plugins.json` keeps it.
    fn stored(&self) -> Option<String> {
        match self {
            Pin::None => None,
            Pin::Key(s) => Some(s.to_base64()),
            Pin::Unreadable(text) => Some(text.clone()),
        }
    }

    /// The pin a file signed as `signature` (none or valid) sets when nothing was pinned.
    fn of(signature: &Signature) -> Pin {
        signature.signer().map_or(Pin::None, Pin::Key)
    }

    /// A pin as a JSON file keeps it ([`Pin::stored`]): `null` for none, a key in base64, and
    /// anything else as [`Pin::Unreadable`].
    fn read(value: &Value) -> Pin {
        match value {
            Value::Null => Pin::None,
            Value::String(text) => Signer::from_base64(text).map_or_else(|_| Pin::Unreadable(text.clone()), Pin::Key),
            other => Pin::Unreadable(other.to_string()),
        }
    }

    /// Whether a file signed as `file` is signed exactly as this pin says: by its key, or (for
    /// [`Pin::None`]) not at all.
    fn matches(&self, file: &Signature) -> bool {
        match (self, file) {
            (Pin::Key(k), Signature::Valid(s)) => k == s,
            (Pin::None, Signature::None) => true,
            _ => false,
        }
    }
}

/// The user's answer to "this file isn't signed by the key pinned for the plug-in: install it
/// anyway?" ([`Manager::install_loaded_as`]), as `plugin.install` takes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SignerAnswer {
    /// Not asked: a file that isn't signed by the pinned key is refused, saying why.
    #[default]
    None,
    /// Yes to the file's signer (`acceptSigner`): its full public key in base64, as
    /// `plugin.inspectFile` gives it in `signerKey`, or its fingerprint (spaces and case ignored).
    /// It becomes the pinned key. Only for a file signed by exactly that key.
    Signer(String),
    /// Yes to an unsigned file (`acceptUnsigned`): the pin goes.
    Unsigned,
}

/// What `current` (`None`: not installed) becomes when a file signed as `file` installs over it,
/// or why that needs the user's yes. `trusted`: a file the user had installed (a rollback).
fn decide_pin(name: &str, current: Option<&Pin>, file: &Signature, answer: &SignerAnswer, trusted: bool) -> Result<Pin> {
    let signer = file.signer();
    let current = match current {
        None | Some(Pin::None) => return Ok(Pin::of(file)),
        Some(Pin::Key(k)) if signer == Some(*k) => return Ok(Pin::Key(*k)),
        Some(c) => c,
    };
    if trusted {
        return Ok(Pin::of(file));
    }
    if let Some(pin) = answered(file, answer) {
        return Ok(pin);
    }
    let was = match current {
        Pin::Key(k) => format!("was installed signed by {}", k.fingerprint()),
        _ => "has a pinned signer in plugins.json that can't be read".to_string(),
    };
    Err(signer_error(name, &was, file))
}

/// The pin the user's `answer` says yes to for a file signed as `file`, when it says yes to exactly
/// that file's signer (or to its being unsigned).
fn answered(file: &Signature, answer: &SignerAnswer) -> Option<Pin> {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match (file, answer) {
        (Signature::Valid(s), SignerAnswer::Signer(given))
            if Signer::from_base64(given.trim()).ok() == Some(*s) || squash(given) == squash(&s.fingerprint()) =>
        {
            Some(Pin::Key(*s))
        }
        (Signature::None, SignerAnswer::Unsigned) => Some(Pin::None),
        _ => None,
    }
}

/// Why a file signed as `file` needs the user's yes: plug-in `name` `was` (how it was signed).
fn signer_error(name: &str, was: &str, file: &Signature) -> Error {
    let (is, ask) = match file.signer() {
        // the full key, so a yes from a script covers exactly this signer (the fingerprint works too)
        Some(s) => (format!("this file is signed by {}", s.fingerprint()), format!("acceptSigner: \"{}\"", s.to_base64())),
        None => ("this file isn't signed".to_string(), "acceptUnsigned: true".to_string()),
    };
    Error::Signer(format!("\"{name}\" {was}, but {is}: install it only if you trust where it came from ({ask} says yes, and pins the new signer)"))
}

/// A request the user said no to, and when (`YYYY-MM-DD`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    #[serde(flatten)]
    pub request: Request,
    pub on: String,
}

/// Where one request of a plug-in stands ([`Installed::state`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestState {
    Allowed,
    /// The user said no, on this date: later calls fail without asking.
    Denied(String),
    /// A folder no one can grant, and why ([`refused_root`]).
    Refused(String),
    Never,
}

/// A plug-in call prepared by [`Manager::begin`]: it owns what the call needs, so it can run
/// without the manager (on another thread), and goes back with [`Manager::end`].
pub struct PendingCall {
    id: String,
    plugin: Arc<Plugin>,
    /// The plug-in's capabilities, read afresh for each call a job makes ([`Installed::live`]).
    perms: Arc<Mutex<Permissions>>,
    store: NamespaceStore,
    settings: Arc<Mutex<SettingsStore>>,
    request: Value,
    lent: Vec<PathBuf>,
    /// Folders lent for reading (a program run's folder in `program.end`).
    lent_dirs: Vec<PathBuf>,
    /// Where `program.start` makes run folders (`None`: [`crate::programs::runs_root`]).
    runs_root: Option<PathBuf>,
    /// A prepare step: its host calls may read, never write.
    read_only: bool,
    /// A job ([`crate::CommandDecl::job`]), by its command's label: its `job.*` calls reach a
    /// [`JobReporter`], and a reply `{continue: state}` calls it again.
    job: Option<String>,
    /// What the plug-in may be asked about: its manifest's request without the folders no one
    /// may grant.
    askable: Permissions,
    /// The requests the user said no to: refused without asking.
    denied: Vec<Request>,
    /// The user can be asked ([`PendingCall::ask_user`]): a refused call that was never asked
    /// about is noted in `needs` instead of telling the plug-in to go to the Plug-in Manager.
    asking: bool,
    /// What calls were refused for that the user was never asked about ([`PendingCall::take_needs`]).
    needs: Vec<Request>,
}

impl PendingCall {
    /// The plug-in this call is for.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether this call is a job ([`PendingCall::run_with`] reports its progress).
    pub fn is_job(&self) -> bool {
        self.job.is_some()
    }

    /// Lets the call note what it needs that the user was never asked about, for the host to ask
    /// once it ends ([`PendingCall::take_needs`]): for a command the user started, with the user at
    /// the app. Not for a job, which runs with nobody to ask.
    pub fn ask_user(&mut self) {
        self.asking = self.job.is_none();
    }

    /// The requests calls were refused for that the user was never asked about (only with
    /// [`PendingCall::ask_user`]), each once, in the order they came.
    pub fn take_needs(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.needs)
    }

    /// A job's command label (as the manifest gives it).
    pub fn job_label(&self) -> Option<&str> {
        self.job.as_deref()
    }

    /// The plug-in's display name (its manifest's `name`).
    pub fn plugin_name(&self) -> &str {
        &self.plugin.manifest().name
    }

    /// The command this call runs (`None`: a hook).
    pub fn command(&self) -> Option<&str> {
        self.request.get("command").and_then(Value::as_str)
    }

    /// Runs the call (the plug-in's code: possibly long, e.g. an upload). A job runs to its end
    /// with nobody watching ([`Unwatched`]).
    pub fn run(&mut self, host: &mut dyn HostApi) -> crate::Outcome {
        self.run_with(host, &Unwatched)
    }

    /// [`PendingCall::run`], a job reporting to `job`. Each reply `{continue: state}` calls the job
    /// again, in a fresh instance under a fresh budget, with the request plus `state`, until a
    /// reply without `continue`, an error, or a cancel (`job`): then no further call is made. A
    /// state that makes the next request larger than the message cap fails the job. Between calls
    /// the plug-in's namespace data is saved. A call that isn't a job ignores `job`.
    pub fn run_with(&mut self, host: &mut dyn HostApi, job: &dyn JobReporter) -> crate::Outcome {
        if self.job.is_none() {
            return self.call_once(host, None);
        }
        let mut logs: Vec<String> = Vec::new();
        loop {
            let outcome = self.call_once(host, Some(job));
            logs.extend(outcome.logs);
            if logs.len() > MAX_LOG {
                logs.drain(..logs.len() - MAX_LOG);
            }
            let value = match outcome.value {
                // a call that failed after a cancel was stopped by it
                Err(_) if job.cancelled() => Err(Error::Cancelled),
                Ok(v) => match v.get("continue") {
                    None => Ok(v),
                    Some(_) if job.cancelled() => Err(Error::Cancelled),
                    Some(state) => {
                        // the next request carries the state: it must fit in a message
                        let mut next = self.request.clone();
                        next["state"] = state.clone();
                        let (max, size) = (self.plugin.limits().max_message_bytes, serde_json::to_vec(&next).map_or(usize::MAX, |b| b.len()));
                        if size > max {
                            Err(Error::Limit(format!(
                                "message size: with its continue state the next call would be {size} bytes, more than the {max} a message may have"
                            )))
                        } else if let Err(e) = self.store.save() {
                            // what the calls so far wrote is kept now, so a quit or a crash
                            // during the next call loses only that call's writes
                            Err(e)
                        } else {
                            self.request = next;
                            continue;
                        }
                    }
                },
                Err(e) => Err(e),
            };
            return crate::Outcome { value, logs };
        }
    }

    fn call_once(&mut self, host: &mut dyn HostApi, job: Option<&dyn JobReporter>) -> crate::Outcome {
        let plugin = self.plugin.clone();
        let manifest = plugin.manifest();
        // never more than this module's own manifest asks for: after an update the cell holds the
        // new version's grant, while a job still runs the old code
        let perms = self.perms.lock().unwrap_or_else(PoisonError::into_inner).intersect(&manifest.permissions);
        let mut ctx = CallContext {
            plugin: &self.id,
            plugin_name: &manifest.name,
            requested: &manifest.permissions,
            perms: &perms,
            lent: &self.lent,
            lent_dirs: &self.lent_dirs,
            runs_root: self.runs_root.as_deref(),
            host,
            store: &mut self.store,
            notes: Vec::new(),
            settings: &self.settings,
            fields: &manifest.settings,
            read_only: self.read_only,
            job,
            askable: &self.askable,
            denied: &self.denied,
            asking: self.asking,
            needs: &mut self.needs,
        };
        let mut outcome = plugin.invoke(&self.request, &mut ctx);
        // what the host did on the plug-in's behalf that the user should see in its log
        let notes = std::mem::take(&mut ctx.notes);
        outcome.logs.extend(notes);
        outcome
    }
}

/// Refuses a grant from a command that holds anything `granted` doesn't (a permission, a host, a
/// folder or its `write`, a program or a wider entry of one, [`Permissions::more_than`]). Repeating
/// or leaving out what is granted is fine: only the user adds, when the plug-in asks or in the
/// Plug-in Manager.
fn adds_nothing(grant: &Permissions, requested: &Permissions, granted: &Permissions) -> Result<()> {
    // what the manifest doesn't ask for is cut back, except a program entry: a wider one than the
    // granted entry is an error, not the manifest's entry in its place
    let mut cut = grant.intersect(requested);
    cut.programs = grant.programs.clone();
    match cut.more_than(granted) {
        Some(r) => Err(Error::Params(format!(
            "permissions can only be allowed by the user, when the plug-in asks or in the Plug-in Manager (File ▸ Plug-ins ▸ Plug-in Manager…); this grant adds `{r}`, which the user hasn't allowed"
        ))),
        None => Ok(()),
    }
}

/// Why a file doesn't load against its plug-in's pin ([`Pin::Key`] it isn't signed by, or
/// [`Pin::Unreadable`]); its grant and data are kept.
fn pin_error(pin: &Pin, file: &Signature) -> String {
    let is = match file.signer() {
        Some(s) => format!("it is signed by {}", s.fingerprint()),
        None => "it isn't signed".to_string(),
    };
    match pin {
        Pin::Key(k) => format!(
            "its file isn't signed by the key pinned when it was installed ({}): {is}. It may have been replaced; install it again to choose whether to trust it",
            k.fingerprint()
        ),
        _ => "the signer pinned for it in plugins.json can't be read; install it again to choose whether to trust its signer".to_string(),
    }
}

/// A module in the plug-in folder that didn't load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub file: String,
    pub error: String,
    /// The host features it requires that this version lacks (empty: it failed for another reason).
    pub missing: Vec<String>,
    /// The plug-in's id, to remove it by ([`Manager::uninstall`]): its manifest's when that could be
    /// read, else the file's name without `.wasm` (installed modules are `<id>.wasm`). Empty: not a
    /// module in the folder.
    pub id: String,
    /// The module file, deleted when the plug-in is removed.
    pub path: Option<PathBuf>,
}

impl Failed {
    /// A failure that isn't a module in the folder (the folder itself couldn't be opened).
    pub fn new(file: String, error: String) -> Failed {
        Failed { file, error, missing: Vec::new(), id: String::new(), path: None }
    }
}

/// The set of installed plug-ins.
#[derive(Debug)]
pub struct Manager {
    dir: Option<PathBuf>,
    /// Folders no plug-in may be granted (the settings folder; the plug-ins folder always is).
    protected: Vec<PathBuf>,
    limits: Limits,
    /// The host features plug-ins may require ([`crate::HOST_FEATURES`]).
    features: Vec<String>,
    plugins: BTreeMap<String, Installed>,
    /// Modules in the folder that failed to load.
    pub failed: Vec<Failed>,
    /// Modules in the folder past the most it loads ([`MAX_PLUGINS`], counting every `.wasm`,
    /// pixel filters too), by file name: none of them was looked at.
    pub skipped: Vec<PathBuf>,
    /// `plugins.json` entries of modules that didn't load, written back unchanged, so a plug-in that
    /// loads again (the app updated, the module replaced) keeps its enabled state and grant.
    kept: BTreeMap<String, Entry>,
    /// The versions of modules that didn't load because they need features this version lacks, by
    /// id: installing an older one over them is still a downgrade.
    unloaded: BTreeMap<String, String>,
    /// The version kept to go back to ([`Manager::rollback`]), by id: of loaded plug-ins only.
    previous: BTreeMap<String, String>,
    /// The open library's id: plug-in data is kept per library (photo ids are per library).
    library: String,
    /// Where `program.start` makes run folders (`None`: [`crate::programs::runs_root`]).
    runs_root: Option<PathBuf>,
}

impl Default for Manager {
    fn default() -> Self {
        Manager {
            dir: None,
            protected: Vec::new(),
            limits: Limits::default(),
            features: crate::HOST_FEATURES.iter().map(|f| f.to_string()).collect(),
            plugins: BTreeMap::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
            kept: BTreeMap::new(),
            unloaded: BTreeMap::new(),
            previous: BTreeMap::new(),
            library: String::new(),
            runs_root: None,
        }
    }
}

impl Manager {
    /// A manager that keeps nothing on disk.
    pub fn in_memory() -> Manager {
        Manager::default()
    }

    /// Opens the plug-in folder `dir` (created when missing) and loads every installed module;
    /// a module that fails to load is listed in [`Manager::failed`] and skipped.
    pub fn open(dir: &Path) -> Result<Manager> {
        Manager::open_with_features(dir, crate::HOST_FEATURES)
    }

    /// [`Manager::open`] for a host that has `features` (tests; the app has [`crate::HOST_FEATURES`]).
    pub fn open_with_features(dir: &Path, features: &[&str]) -> Result<Manager> {
        let io = |e: std::io::Error| Error::Io(format!("{}: {e}", dir.display()));
        std::fs::create_dir_all(dir).map_err(io)?;
        // unreadable: never written over by the pins set below, so the grants in it aren't lost
        let mut readable = true;
        let state: State = match std::fs::read(dir.join("plugins.json")) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::warn!("plugins.json is unreadable ({e}); plug-ins load disabled");
                readable = false;
                State::default()
            }),
            Err(e) => {
                readable = e.kind() == std::io::ErrorKind::NotFound;
                State::default()
            }
        };
        let mut pinned_now = false;
        // run folders a program still used when the app last quit
        #[cfg(not(target_arch = "wasm32"))]
        crate::programs::sweep_old_runs(&crate::programs::runs_root(), crate::programs::STALE_RUN_AGE);
        let mut m = Manager { dir: Some(dir.to_path_buf()), ..Manager::default() };
        m.set_features(features);
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wasm")))
            .collect();
        files.sort();
        m.skipped = files.split_off(files.len().min(MAX_PLUGINS));
        // the first module (by file name) with an id is the one that loads
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for f in files {
            let file = f.display().to_string();
            // installed modules are `<id>.wasm`: the id when the manifest can't be read
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let loaded = read_module(&f, &m.limits).and_then(|b| Plugin::load(&b, m.limits.clone()));
            let p = match loaded {
                Ok(p) => p,
                Err(e) => {
                    m.keep(&state, &stem);
                    m.failed.push(Failed { file, error: e.to_string(), missing: Vec::new(), id: stem, path: Some(f) });
                    continue;
                }
            };
            let id = p.id().to_string();
            if let Some(first) = seen.get(&id) {
                m.failed.push(Failed { error: duplicate_error(first), file, missing: Vec::new(), id, path: Some(f) });
                continue;
            }
            let missing = p.manifest().missing_features(&m.features);
            if !missing.is_empty() {
                // the app was downgraded: keep its grant and data for when it's updated again
                m.keep(&state, &id);
                m.unloaded.entry(id.clone()).or_insert_with(|| p.manifest().version.clone());
                m.failed.push(Failed { file, error: Error::Unsupported(missing.clone()).to_string(), missing, id, path: Some(f) });
                continue;
            }
            // a file that isn't signed by the pinned key doesn't load (replaced behind the app's
            // back); not in plugins.json, or pinned as unsigned: its own signer is pinned
            let pin = match state.plugins.get(&id).map(Entry::pin) {
                Some(Pin::Key(k)) if p.signature().signer() == Some(k) => Pin::Key(k),
                Some(Pin::None) | None => {
                    // written at once, so a file swapped in before the next save can't take its place
                    pinned_now |= p.signature().signer().is_some();
                    Pin::of(p.signature())
                }
                Some(pin) => {
                    m.keep(&state, &id);
                    m.failed.push(Failed { file, error: pin_error(&pin, p.signature()), missing: Vec::new(), id, path: Some(f) });
                    continue;
                }
            };
            // only a module that loads holds its id: one that can't gives way to a later file
            seen.insert(id.clone(), file.clone());
            m.unloaded.remove(&id);
            // not in plugins.json (copied in by hand): disabled, nothing granted
            let (enabled, grant, denied) =
                state.plugins.get(&id).map_or((false, Permissions::default(), Vec::new()), |e| (e.enabled, e.grant.clone(), e.denied.clone()));
            let mut log = VecDeque::new();
            if let Err(e) = p.manifest().semver() {
                log.push_back(format!("loaded without a valid version ({e}); installing a new version will need one"));
            }
            let (settings, damaged) = m.open_settings(&id);
            log.extend(damaged);
            let live = Arc::new(Mutex::new(Permissions::default()));
            let refused = m.refused_roots(&p.manifest().permissions);
            let installed =
                Installed { plugin: Arc::new(p), enabled, grant, denied, refused, log, store: None, settings, busy: None, live, path: Some(f), pin };
            installed.share_effective();
            if let Some((version, ..)) = read_previous(dir, &id) {
                m.previous.insert(id.clone(), version);
            }
            m.plugins.insert(id, installed);
        }
        if pinned_now
            && readable
            && let Err(e) = m.save_over(&state)
        {
            log::warn!("plug-ins: the signers pinned at start-up couldn't be saved to plugins.json: {e}");
        }
        Ok(m)
    }

    /// Plug-in `id`'s settings: from its file, or in memory without a folder. A damaged file gives
    /// the defaults and a log line (also returned, for the plug-in's log).
    fn open_settings(&self, id: &str) -> (Arc<Mutex<SettingsStore>>, Option<String>) {
        let (store, damaged) = match &self.dir {
            Some(d) => SettingsStore::open(SettingsStore::path_in(d, id)),
            None => (SettingsStore::in_memory(), None),
        };
        let damaged = damaged.map(|e| {
            log::warn!("plug-in {id}: settings: {e}");
            format!("settings: {e}")
        });
        (Arc::new(Mutex::new(store)), damaged)
    }

    /// Keeps `id`'s `plugins.json` entry from `state` (its module didn't load).
    fn keep(&mut self, state: &State, id: &str) {
        if let Some(e) = state.plugins.get(id) {
            self.kept.insert(id.to_string(), e.clone());
        }
    }

    /// The host features plug-ins may require (default [`crate::HOST_FEATURES`]). Affects installs
    /// from now on; [`Manager::open_with_features`] sets them for loading.
    pub fn set_features(&mut self, features: &[&str]) {
        self.features = features.iter().map(|f| f.to_string()).collect();
    }

    /// The host features plug-ins may require.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// The version of plug-in `id` in the folder: loaded, or kept there because it needs features
    /// this version lacks.
    pub fn installed_version(&self, id: &str) -> Option<String> {
        self.plugins.get(id).map(|i| i.plugin.manifest().version.clone()).or_else(|| self.unloaded.get(id).cloned())
    }

    /// A plug-in in the folder that didn't load because it needs features this version lacks: its
    /// version and the grant it keeps.
    pub fn unloaded(&self, id: &str) -> Option<(String, Permissions)> {
        let version = self.unloaded.get(id)?.clone();
        Some((version, self.kept.get(id).map(|e| e.grant.clone()).unwrap_or_default()))
    }

    /// Why `manifest` can't be installed over what is installed now, as `(missing features,
    /// version problem, downgrade from)`, for the install dialog ([`Manager::install_bytes`]
    /// refuses each one; a downgrade only without `allow_downgrade`).
    pub fn install_check(&self, manifest: &crate::Manifest) -> (Vec<String>, Option<String>, Option<String>) {
        let missing = manifest.missing_features(&self.features);
        let new = manifest.semver();
        let old = self.installed_version(&manifest.id);
        let downgrade = match (&new, old) {
            (Ok(new), Some(old)) => parse_version(&old).ok().filter(|o| new.cmp_precedence(o) == std::cmp::Ordering::Less).map(|_| old),
            _ => None,
        };
        (missing, new.err(), downgrade)
    }

    /// Limits for plug-ins installed from now on.
    /// The library now open (a stable id; `""`: none). Plug-in data is kept per library, since
    /// photo ids are only unique within one: every read and write from now on goes to this one's.
    /// The first library with an id claims data saved before data was kept per library.
    pub fn set_library(&mut self, library: &str) {
        self.library = library.to_string();
        for p in self.plugins.values_mut() {
            if let Some(store) = p.store.as_mut() {
                scope(store, library);
            }
        }
    }

    /// The open library's id (see [`Manager::set_library`]).
    pub fn library(&self) -> &str {
        &self.library
    }

    /// Where `program.start` makes run folders from now on (tests; `None`, the default:
    /// [`crate::programs::runs_root`]).
    pub fn set_runs_root(&mut self, root: Option<PathBuf>) {
        self.runs_root = root;
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Installs (or updates) a module, as a command does. `None` grants nothing new: a fresh
    /// install gets no permissions (the plug-in asks the user for each as it needs it), an update
    /// keeps the previous grant (cut back to what the new version still asks for) and the
    /// denials, so an update can never gain a capability without the user allowing it. The
    /// plug-in starts enabled.
    ///
    /// Refused, with nothing written: a plug-in that requires host features this version lacks, a
    /// `version` that isn't semver, and a lower version than the installed one unless
    /// `allow_downgrade` (the same version again is a repair, and allowed).
    ///
    /// `grant` can't add anything ([`Permissions::more_than`]): it may repeat or leave out what the
    /// installed version has granted; anything more is an error and installs nothing. A module
    /// whose code differs from the installed one (another digest) keeps none of its programs, and
    /// its log says so: the user allowed them for the code they had.
    pub fn install_bytes(&mut self, bytes: &[u8], grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        self.install_checked(bytes, grant, allow_downgrade, false)
    }

    /// [`Manager::install_bytes`] as the user started it from the Plug-in Manager: a module whose
    /// code differs keeps its programs. `grant` is taken as given (the install dialog passes none;
    /// tests set one up this way). Never reachable from a command.
    pub fn install_bytes_by_user(&mut self, bytes: &[u8], grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        self.install_checked(bytes, grant, allow_downgrade, true)
    }

    /// Loads a module with the limits of plug-ins installed from now on, without installing it:
    /// to look at its manifest first, then install it with [`Manager::install_loaded`].
    pub fn load(&self, bytes: &[u8]) -> Result<Plugin> {
        Plugin::load(bytes, self.limits.clone())
    }

    /// [`Manager::install_bytes`] for a module already loaded by [`Manager::load`] from `bytes`.
    pub fn install_loaded(&mut self, plugin: Plugin, bytes: &[u8], grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        self.install_plugin(plugin, bytes, grant, allow_downgrade, false, false, &SignerAnswer::None)
    }

    /// [`Manager::install_loaded`] as the user approved it ([`Manager::install_bytes_by_user`]).
    pub fn install_loaded_by_user(&mut self, plugin: Plugin, bytes: &[u8], grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        self.install_plugin(plugin, bytes, grant, allow_downgrade, true, false, &SignerAnswer::None)
    }

    /// [`Manager::install_loaded`] (`by_user`: [`Manager::install_loaded_by_user`]) with the user's
    /// answer about a file that isn't signed by the key pinned for its plug-in. The first signed
    /// install pins the file's key; a file signed by the pinned key keeps it; one signed by another
    /// key, unsigned, or over a pin that can't be read is refused ([`Error::Signer`]) unless
    /// `signer` says yes to exactly that ([`SignerAnswer`]), which pins the new signer (or none).
    pub fn install_loaded_as(
        &mut self,
        plugin: Plugin,
        bytes: &[u8],
        grant: Option<Permissions>,
        allow_downgrade: bool,
        by_user: bool,
        signer: &SignerAnswer,
    ) -> Result<&Installed> {
        self.install_plugin(plugin, bytes, grant, allow_downgrade, by_user, false, signer)
    }

    /// The pin plug-in `id` has: loaded, or kept for a module in the folder that didn't load.
    /// `None`: not installed.
    pub fn pin(&self, id: &str) -> Option<Pin> {
        self.plugins.get(id).map(|i| i.pin.clone()).or_else(|| self.kept.get(id).map(Entry::pin))
    }

    fn install_checked(&mut self, bytes: &[u8], grant: Option<Permissions>, allow_downgrade: bool, by_user: bool) -> Result<&Installed> {
        let plugin = Plugin::load(bytes, self.limits.clone())?;
        self.install_plugin(plugin, bytes, grant, allow_downgrade, by_user, false, &SignerAnswer::None)
    }

    /// `rollback`: going back to the version kept ([`Manager::rollback`]): it keeps no version to
    /// go back to, logs the rollback, and pins that version's signer (the user had installed it).
    #[allow(clippy::too_many_arguments)]
    fn install_plugin(
        &mut self,
        plugin: Plugin,
        bytes: &[u8],
        grant: Option<Permissions>,
        allow_downgrade: bool,
        by_user: bool,
        rollback: bool,
        signer: &SignerAnswer,
    ) -> Result<&Installed> {
        let (missing, version, downgrade) = self.install_check(plugin.manifest());
        if !missing.is_empty() {
            return Err(Error::Unsupported(missing));
        }
        if let Some(e) = version {
            return Err(Error::Manifest(e));
        }
        let new = plugin.manifest().version.clone();
        if let Some(installed) = &downgrade
            && !allow_downgrade
        {
            return Err(Error::Downgrade { installed: installed.clone(), new });
        }
        let id = plugin.id().to_string();
        // before anything is written: a file not signed by the pinned key needs the user's yes
        let old_pin = self.pin(&id);
        let pin = decide_pin(&plugin.manifest().name, old_pin.as_ref(), plugin.signature(), signer, rollback)?;
        let pin_line = match (&old_pin, &pin) {
            (_, Pin::Key(k)) if old_pin.as_ref().and_then(Pin::signer) == Some(*k) => None,
            (None | Some(Pin::None), Pin::Key(k)) => Some(format!("signed by {}: pinned as its signer", k.fingerprint())),
            (Some(_), Pin::Key(k)) if rollback => Some(format!("went back to the version signed by {}: pinned as its signer", k.fingerprint())),
            (Some(_), Pin::Key(k)) => Some(format!("signer changed to {}, as you said yes to", k.fingerprint())),
            (Some(Pin::Key(_) | Pin::Unreadable(_)), Pin::None) if rollback => {
                Some("went back to the unsigned version: no signer is pinned".to_string())
            }
            (Some(Pin::Key(_) | Pin::Unreadable(_)), Pin::None) => Some("installed unsigned, as you said yes to: no signer is pinned".to_string()),
            _ => None,
        };
        // a module that stopped loading keeps its grant for the version that replaces it
        let mut previous = self.plugins.get(&id).map(|i| i.grant.clone()).or_else(|| self.kept.get(&id).map(|e| e.grant.clone())).unwrap_or_default();
        // programs were allowed for the code the user saw: different code from a command keeps
        // none (a module that stopped loading counts as different)
        let same_code = self.plugins.get(&id).is_some_and(|i| i.plugin.digest() == plugin.digest());
        let mut dropped = Vec::new();
        if !by_user && !same_code {
            let declared = &plugin.manifest().permissions.programs;
            dropped = std::mem::take(&mut previous.programs)
                .into_iter()
                .filter(|p| declared.iter().any(|d| d.id == p.id))
                .map(|p| format!("{}: this version's code is different, so it may no longer start it; allow it again in the Plug-in Manager", p.name))
                .collect();
        }
        // a grant from a command never adds to what the plug-in had: a fresh install starts with
        // nothing, and only the user allows more, when the plug-in asks or in the Plug-in Manager
        if let Some(g) = &grant
            && !by_user
        {
            adds_nothing(g, &plugin.manifest().permissions, &previous)?;
        }
        let denied = self.plugins.get(&id).map(|i| i.denied.clone()).or_else(|| self.kept.get(&id).map(|e| e.denied.clone())).unwrap_or_default();
        let grant = grant.unwrap_or_else(|| previous.clone()).intersect(&plugin.manifest().permissions);
        // a granted program whose entry the update widened is no longer allowed: say why
        let widened: Vec<String> = plugin
            .manifest()
            .permissions
            .programs
            .iter()
            .filter(|p| previous.program(&p.id).is_some_and(|g| !p.within(g)) && grant.program(&p.id).is_none())
            .map(|p| format!("{}: this version finds it in more places than you allowed; allow it again in the Plug-in Manager", p.name))
            .collect();
        let old_version = self.plugins.get(&id).map(|i| i.plugin.manifest().version.clone());
        // an update keeps the version it replaces, module and settings (before they're pruned), to
        // go back to; read now, since the write replaces its file
        let keep = match (&self.dir, self.plugins.get(&id)) {
            (Some(_), Some(old)) if !rollback && !same_code => Some(
                old.path
                    .as_deref()
                    .ok_or_else(|| Error::NotFound(format!("{id}.wasm")))
                    .and_then(|p| read_module(p, &self.limits))
                    .map(|b| (b, old.plugin.manifest().version.clone(), old.settings().snapshot(), old.pin.clone())),
            ),
            _ => None,
        };
        let path = self.dir.as_ref().map(|d| d.join(format!("{id}.wasm")));
        if let Some(path) = &path {
            write_atomic(path, bytes)?;
            let file = path.display().to_string();
            self.failed.retain(|f| f.file != file);
        }
        // the version it replaces was copied in under another name: that file goes, or it would
        // load again in place of this one at the next start
        let stale = self.plugins.get(&id).and_then(|i| i.path.clone()).filter(|p| Some(p) != path.as_ref());
        let mut stale_error = None;
        if let Some(stale) = stale {
            match std::fs::remove_file(&stale) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    stale_error =
                        Some(format!("the old module {} couldn't be removed ({e}); remove it by hand, or it may load again", stale.display()))
                }
            }
        }
        let mut kept_error = None;
        if let Some(dir) = &self.dir
            && !rollback
        {
            match keep {
                Some(keep) => {
                    match keep.and_then(|(b, version, settings, pin)| keep_previous(dir, &id, &b, &version, &settings, &pin).map(|_| version)) {
                        Ok(version) => {
                            self.previous.insert(id.clone(), version);
                        }
                        Err(e) => {
                            // none rather than an older one under the wrong version
                            let _ = discard_previous(dir, &id);
                            self.previous.remove(&id);
                            kept_error = Some(format!("the version it replaces couldn't be kept to go back to: {e}"));
                        }
                    }
                }
                // the same code again keeps what it had; a fresh install finds one a removal kept
                None if old_version.is_none() => match read_previous(dir, &id) {
                    Some((version, ..)) => {
                        self.previous.insert(id.clone(), version);
                    }
                    None => {
                        self.previous.remove(&id);
                    }
                },
                None => {}
            }
        }
        self.kept.remove(&id);
        self.unloaded.remove(&id);
        let old = self.plugins.remove(&id);
        let mut log = old.as_ref().map(|i| i.log.clone()).unwrap_or_default();
        log.extend(stale_error);
        log.extend(kept_error);
        // a call still running keeps the cell, and gets the new version's grant on its next call
        let live = old.as_ref().map_or_else(|| Arc::new(Mutex::new(Permissions::default())), |i| i.live.clone());
        let settings = match old.map(|i| i.settings) {
            Some(s) => s,
            None => {
                let (s, damaged) = self.open_settings(&id);
                log.extend(damaged);
                s
            }
        };
        // an update: what the new version no longer declares goes, new settings start at their defaults
        let pruned = {
            let mut s = settings.lock().unwrap_or_else(PoisonError::into_inner);
            s.prune(plugin.manifest());
            s.save()
        };
        if let Err(e) = pruned {
            log.push_back(format!("settings: {e}"));
        }
        let refused = self.refused_roots(&plugin.manifest().permissions);
        let installed =
            Installed { plugin: Arc::new(plugin), enabled: true, grant, denied, refused, log, store: None, settings, busy: None, live, path, pin };
        installed.share_effective();
        self.plugins.insert(id.clone(), installed);
        self.save()?;
        self.plugins
            .get_mut(&id)
            .map(|i| {
                i.push_log(match (rollback, old_version, downgrade) {
                    (true, Some(old), _) => format!("rolled back from {old} to {new}"),
                    (_, _, Some(old)) => format!("downgraded from {old} to {new}"),
                    _ => format!("installed version {new}"),
                });
                let unknown = &i.plugin.manifest().unknown_fields;
                let unknown = (!unknown.is_empty()).then(|| format!("unknown manifest fields: {}", unknown.join(", ")));
                let ignored = i.plugin.manifest().dropped.iter().map(|d| format!("manifest value dropped: {d}")).collect::<Vec<_>>();
                for line in pin_line.into_iter().chain(dropped).chain(widened).chain(unknown).chain(ignored) {
                    i.push_log(line);
                }
                &*i
            })
            .ok_or(Error::NotFound(id))
    }

    /// Reads a `.wasm` file and installs it (see [`Manager::install_bytes`]).
    pub fn install_file(&mut self, path: &Path, grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        let bytes = read_module(path, &self.limits)?;
        self.install_bytes(&bytes, grant, allow_downgrade)
    }

    /// [`Manager::install_file`] as the user approved it ([`Manager::install_bytes_by_user`]).
    pub fn install_file_by_user(&mut self, path: &Path, grant: Option<Permissions>, allow_downgrade: bool) -> Result<&Installed> {
        let bytes = read_module(path, &self.limits)?;
        self.install_bytes_by_user(&bytes, grant, allow_downgrade)
    }

    /// The version plug-in `id` goes back to ([`Manager::rollback`]), if one is kept.
    pub fn previous_version(&self, id: &str) -> Option<&str> {
        self.previous.get(id).map(String::as_str)
    }

    /// Goes back to the version of plug-in `id` its last update replaced: its module and its
    /// settings as they were then. A downgrade nobody needs to confirm; the grant is kept, cut back
    /// to what that version asks for, without programs (code that differs from a command, as for
    /// [`Manager::install_bytes`]; the user's rollback keeps them, [`Manager::rollback_by_user`]).
    /// No version is kept after it (one level deep).
    ///
    /// When the module can't be written, the current version stays as it was, settings too.
    pub fn rollback(&mut self, id: &str) -> Result<&Installed> {
        self.rollback_as(id, false, &SignerAnswer::None)
    }

    /// [`Manager::rollback`] as the user asked for it on the plug-in's page: it keeps the programs
    /// granted (those the old version asks for). Never reachable from a command.
    pub fn rollback_by_user(&mut self, id: &str) -> Result<&Installed> {
        self.rollback_as(id, true, &SignerAnswer::None)
    }

    /// [`Manager::rollback`] (`by_user`: [`Manager::rollback_by_user`]) with the user's answer
    /// about a kept version that isn't signed as it was when it was replaced: the signer recorded
    /// beside it in `previous/<id>.json` (another key, unsigned, or invalid: changed since). Such a
    /// file is refused ([`Error::Signer`]) unless `signer` says yes to exactly its signer, as for
    /// [`Manager::install_loaded_as`]. A version kept before signers were recorded goes back as it is.
    pub fn rollback_as(&mut self, id: &str, by_user: bool, signer: &SignerAnswer) -> Result<&Installed> {
        let none = || Error::Params(format!("{id} has no previous version to go back to"));
        if !self.plugins.contains_key(id) {
            return Err(Error::NotFound(id.into()));
        }
        let Some(dir) = self.dir.clone().filter(|_| self.previous.contains_key(id)) else { return Err(none()) };
        let (_, settings, recorded) = read_previous(&dir, id).ok_or_else(none)?;
        let bytes = read_module(&previous_paths(&dir, id).0, &self.limits)?;
        let plugin = Plugin::load(&bytes, self.limits.clone())?;
        if plugin.id() != id {
            return Err(Error::Failed(format!("the version kept to go back to is another plug-in ({})", plugin.id())));
        }
        // the kept file is still the one the update replaced: signed as it was then, or the user's yes
        if let Some(recorded) = recorded
            && !recorded.matches(plugin.signature())
            && answered(plugin.signature(), signer).is_none()
        {
            let was = match &recorded {
                Pin::Key(k) => format!("version {} was kept signed by {}", plugin.manifest().version, k.fingerprint()),
                Pin::None => format!("version {} was kept unsigned", plugin.manifest().version),
                Pin::Unreadable(_) => format!("version {} was kept with a signer that can't be read", plugin.manifest().version),
            };
            return Err(signer_error(&plugin.manifest().name, &was, plugin.signature()));
        }
        // the settings go back with the module or not at all: staged beside their file first
        let file = SettingsStore::path_in(&dir, id);
        let mut staged = file.clone().into_os_string();
        staged.push(".rollback");
        let staged = PathBuf::from(staged);
        let restore = match &settings {
            Value::Null => None,
            s => Some(SettingsStore::snapshot_file(s).map_err(|e| Error::Failed(format!("the settings kept to go back to: {e}")))?),
        };
        if let Some(b) = &restore {
            write_atomic(&staged, b)?;
        }
        match self.install_plugin(plugin, &bytes, None, true, by_user, true, &SignerAnswer::None) {
            Ok(_) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return Err(e);
            }
        }
        let mut lines = Vec::new();
        if restore.is_some() {
            // under the store's lock, so a call running off the manager's can't save the settings
            // it had over the file between the two
            let store = self.plugins.get(id).map(|p| p.settings.clone());
            let mut store = store.as_ref().map(|s| s.lock().unwrap_or_else(PoisonError::into_inner));
            match std::fs::rename(&staged, &file) {
                Ok(()) => {
                    if let Some(s) = store.as_mut() {
                        s.restored(&settings);
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&staged);
                    lines.push(format!("settings: the ones kept couldn't be put back ({e}); it keeps the current ones"));
                }
            }
        }
        self.previous.remove(id);
        if let Err(e) = discard_previous(&dir, id) {
            lines.push(format!("the version gone back to couldn't be removed from previous/: {e}"));
        }
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        for line in lines {
            p.push_log(line);
        }
        Ok(&*p)
    }

    /// Removes a plug-in, its module, its data and its settings.
    ///
    /// A plug-in in the folder that didn't load ([`Manager::failed`]: it needs features this version
    /// lacks, or its module can't be read) is removed the same way, by its id there.
    pub fn uninstall(&mut self, id: &str) -> Result<()> {
        self.uninstall_with(id, false)
    }

    /// [`Manager::uninstall`]; `keep_data`: its data (`data/<id>.json`) and settings stay in the
    /// folder, for a later install of the same id to pick up. Its grant and enabled state go either
    /// way, and a call still running can't write its settings back.
    pub fn uninstall_with(&mut self, id: &str, keep_data: bool) -> Result<()> {
        let is_failed = |f: &Failed| !id.is_empty() && f.id == id;
        if !self.plugins.contains_key(id) && !self.failed.iter().any(is_failed) {
            return Err(Error::NotFound(id.into()));
        }
        // a call still running keeps the store: held while the files go, and forgotten once they
        // have, it can't write its file back (and if one can't be deleted, it keeps saving)
        let store = self.plugins.get(id).map(|p| p.settings.clone());
        let mut store = store.as_ref().map(|s| s.lock().unwrap_or_else(PoisonError::into_inner));
        // kept settings are the latest: a change whose save failed is written now, or nothing goes
        if keep_data && let Some(s) = store.as_mut() {
            s.save()?;
        }
        // the files first: if one can't be deleted, nothing is forgotten (its entry stays saved)
        if let Some(dir) = &self.dir {
            let modules: Vec<PathBuf> = self.failed.iter().filter(|f| is_failed(f)).filter_map(|f| f.path.clone()).collect();
            // the modules before the data: a module that can't be deleted keeps its data too
            let settings = SettingsStore::path_in(dir, id);
            let mut bad = settings.clone().into_os_string();
            bad.push(".bad");
            // its own module, when it was copied in under another name
            let own = self.plugins.get(id).and_then(|p| p.path.clone());
            let (previous, about) = previous_paths(dir, id);
            let data = [dir.join("data").join(format!("{id}.json")), settings, bad.into(), about, previous];
            let data = if keep_data { Vec::new() } else { data.to_vec() };
            let files = std::iter::once(dir.join(format!("{id}.wasm"))).chain(own).chain(modules).chain(data);
            for f in files {
                match std::fs::remove_file(&f) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::Io(format!("{}: {e}", f.display()))),
                }
            }
        }
        if let Some(s) = store.as_mut() {
            s.forget();
        }
        drop(store);
        // a job still running gets nothing more from its next call
        if let Some(p) = self.plugins.remove(id) {
            *p.live.lock().unwrap_or_else(PoisonError::into_inner) = Permissions::default();
        }
        self.failed.retain(|f| !is_failed(f));
        self.kept.remove(id);
        self.unloaded.remove(id);
        self.previous.remove(id);
        self.save()
    }

    /// Deletes module `file` ([`Failed::file`]) that didn't load and drops its row; nothing else
    /// changes (its id's entry and data stay): for a module another one with its id stands in for.
    pub fn remove_failed_module(&mut self, file: &str) -> Result<()> {
        let Some(at) = self.failed.iter().position(|f| f.file == file) else { return Err(Error::NotFound(file.into())) };
        if let Some(path) = self.failed.get(at).and_then(|f| f.path.clone()) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::Io(format!("{}: {e}", path.display()))),
            }
        }
        self.failed.remove(at);
        Ok(())
    }

    /// Unloads plug-in `id` and lists its module as failed with `error`, keeping its entry, grant
    /// and data (as for a module that stopped loading): another module with its id comes first.
    pub fn demote(&mut self, id: &str, error: String) -> bool {
        let Some(p) = self.plugins.remove(id) else { return false };
        *p.live.lock().unwrap_or_else(PoisonError::into_inner) = Permissions::default();
        self.kept.insert(id.to_string(), p.entry());
        let file = p.path.as_ref().map_or_else(|| format!("{id}.wasm"), |f| f.display().to_string());
        self.failed.push(Failed { file, error, missing: Vec::new(), id: id.to_string(), path: p.path });
        true
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        p.enabled = enabled;
        p.share_effective();
        p.push_log(if enabled { "enabled".into() } else { "disabled".into() });
        self.save()
    }

    /// Replaces the grant (revoking what it leaves out); it can never exceed the request. It can't
    /// add anything ([`Permissions::more_than`]): a grant holding a permission, host, folder or
    /// program the plug-in doesn't have is an error and changes nothing (only the user adds,
    /// [`Manager::allow_by_user`]). Repeating what is granted keeps it; leaving it out revokes it.
    pub fn set_grant(&mut self, id: &str, grant: Permissions) -> Result<Permissions> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        adds_nothing(&grant, &p.plugin.manifest().permissions, &p.effective())?;
        self.set_grant_by_user(id, grant)
    }

    /// [`Manager::set_grant`] without the check: the whole grant at once, as the user set it.
    /// Never reachable from a command (the Plug-in Manager allows one request at a time,
    /// [`Manager::allow_by_user`]; tests set a grant this way). Folders that can't be granted
    /// ([`refused_root`]) stay out of what calls get.
    pub fn set_grant_by_user(&mut self, id: &str, grant: Permissions) -> Result<Permissions> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        p.grant = grant.intersect(&p.plugin.manifest().permissions);
        p.share_effective();
        let e = p.effective();
        p.push_log(format!("permissions set to {}", serde_json::to_string(&e).unwrap_or_default()));
        self.save()?;
        Ok(e)
    }

    /// The user allows request `r` of plug-in `id` (when it asked, or on its Permissions tab): it
    /// is granted as the manifest asks for it, and a denial of it is forgotten. An error when the
    /// manifest doesn't ask for it, or it is a folder no one may grant ([`refused_root`]). Never
    /// reachable from a command.
    pub fn allow_by_user(&mut self, id: &str, r: &Request) -> Result<Permissions> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let requested = &p.plugin.manifest().permissions;
        if !requested.holds(r) || (r.value.is_none() && r.group.has_values()) {
            return Err(Error::Params(format!("{id} doesn't ask for `{r}`")));
        }
        if let RequestState::Refused(why) = p.state(r) {
            return Err(Error::Params(format!("`{r}` can't be granted: {why}")));
        }
        p.grant = p.grant.with(r, requested).intersect(requested);
        p.denied.retain(|d| d.request != *r);
        p.share_effective();
        p.push_log(format!("allowed {r}"));
        let e = p.effective();
        self.save()?;
        Ok(e)
    }

    /// The user said no to request `r` of plug-in `id` on `on` (`YYYY-MM-DD`): it isn't granted,
    /// and calls that need it fail without asking until the user allows it. Never reachable from
    /// a command.
    pub fn deny_by_user(&mut self, id: &str, r: &Request, on: &str) -> Result<()> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        if !p.plugin.manifest().permissions.holds(r) || (r.value.is_none() && r.group.has_values()) {
            return Err(Error::Params(format!("{id} doesn't ask for `{r}`")));
        }
        p.grant = p.grant.without(r);
        p.denied.retain(|d| d.request != *r);
        p.denied.push(Denial { request: r.clone(), on: on.to_string() });
        p.share_effective();
        p.push_log(format!("denied {r}"));
        self.save()
    }

    /// Revokes request `r` of plug-in `id` (every value of its group when `r` names none): open to
    /// commands. What is revoked counts as never asked: the plug-in may ask again.
    pub fn revoke(&mut self, id: &str, r: &Request) -> Result<Permissions> {
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        p.grant = p.grant.without(r);
        p.share_effective();
        p.push_log(format!("revoked {r}"));
        let e = p.effective();
        self.save()?;
        Ok(e)
    }

    /// Folders no plug-in may be granted, besides the plug-ins folder itself: the settings folder.
    /// A request for a folder that holds one of them (or lies inside the plug-ins folder) is
    /// refused: a plug-in that could write there could rewrite its own grant or another module.
    pub fn set_protected(&mut self, folders: Vec<PathBuf>) {
        self.protected = folders;
        let refused: Vec<(String, Vec<(String, String)>)> =
            self.plugins.iter().map(|(id, p)| (id.clone(), self.refused_roots(&p.plugin.manifest().permissions))).collect();
        for (id, r) in refused {
            if let Some(p) = self.plugins.get_mut(&id) {
                p.refused = r;
                p.share_effective();
            }
        }
    }

    /// The folders `requested` asks for that can't be granted, with why.
    fn refused_roots(&self, requested: &Permissions) -> Vec<(String, String)> {
        let mut protected = self.protected.clone();
        protected.extend(self.dir.clone());
        requested.fs.iter().filter_map(|f| refused_root(&f.path, &protected, self.dir.as_deref()).map(|why| (f.path.clone(), why))).collect()
    }

    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.plugins.get(id)
    }

    /// Installed plug-ins, by id.
    pub fn list(&self) -> impl Iterator<Item = &Installed> {
        self.plugins.values()
    }

    fn save(&self) -> Result<()> {
        self.save_over(&State::default())
    }

    /// [`Manager::save`] over `base`, the `plugins.json` read at start-up: its entries this manager
    /// doesn't hold (modules past the most it loads, files it couldn't place) are written back as
    /// they were, so a save nobody asked for drops nothing.
    fn save_over(&self, base: &State) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let mut plugins = base.plugins.clone();
        plugins.extend(self.kept.iter().map(|(id, e)| (id.clone(), e.clone())));
        plugins.extend(self.plugins.iter().map(|(id, p)| (id.clone(), p.entry())));
        let state = State { plugins };
        let bytes = serde_json::to_vec_pretty(&state).map_err(|e| Error::Io(e.to_string()))?;
        write_atomic(&dir.join("plugins.json"), &bytes)
    }

    /// Sends `request` to plug-in `id` with its effective capabilities, `lent` files added for this
    /// call. Logs what it printed and how the call ended.
    pub fn invoke(&mut self, id: &str, request: &Value, host: &mut dyn HostApi, lent: &[PathBuf]) -> Result<Value> {
        let mut call = self.begin(id, request.clone(), lent.to_vec())?;
        let outcome = call.run(host);
        self.end(call, Some(outcome))
    }

    /// Prepares a call of plug-in `id` that runs without this manager ([`PendingCall::run`], e.g. on a
    /// worker thread while the manager's lock is free), then is handed back with [`Manager::end`]. The
    /// plug-in's data is lent to the call: until then, other calls of the same plug-in are refused as busy.
    pub fn begin(&mut self, id: &str, request: Value, lent: Vec<PathBuf>) -> Result<PendingCall> {
        self.begin_with_dirs(id, request, lent, Vec::new())
    }

    /// [`Manager::begin`] with folders lent to the call for reading.
    pub fn begin_with_dirs(&mut self, id: &str, request: Value, lent: Vec<PathBuf>, lent_dirs: Vec<PathBuf>) -> Result<PendingCall> {
        self.begin_as(id, request, lent, lent_dirs, None)
    }

    /// [`Manager::begin_with_dirs`]; `job`: the call is a job with this label (named in the error
    /// other calls of the plug-in get while it runs).
    fn begin_as(&mut self, id: &str, request: Value, lent: Vec<PathBuf>, lent_dirs: Vec<PathBuf>, job: Option<&str>) -> Result<PendingCall> {
        let library = self.library.clone();
        let dir = self.dir.clone();
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        if !p.enabled {
            return Err(Error::Disabled(id.into()));
        }
        if let Some(why) = &p.busy {
            return Err(Error::Busy(id.into(), why.clone()));
        }
        if p.store.is_none() {
            p.store = Some(match &dir {
                Some(d) => NamespaceStore::open(d.join("data").join(format!("{id}.json")))?,
                None => NamespaceStore::in_memory(),
            });
        }
        if let Some(store) = p.store.as_mut() {
            scope(store, &library);
        }
        p.share_effective();
        let perms = p.live.clone();
        let askable = p.plugin.manifest().permissions.without_refused(|f| p.refused.iter().any(|(r, _)| r == f));
        let denied = p.denied.iter().map(|d| d.request.clone()).collect();
        let plugin = p.plugin.clone();
        let store = p.store.take().unwrap_or_default();
        let settings = p.settings.clone();
        p.busy = Some(match job {
            Some(label) => format!("its job \"{label}\" is running; wait for it to end, or stop it in the activity stack"),
            None => "an earlier call of it is still running (an export's hooks, or a program's end)".into(),
        });
        // the host builds every request, so a plug-in can't make its prepare step anything else
        let read_only = matches!(request.get("hook").and_then(Value::as_str), Some("command.prepare" | "settings.prepare"));
        Ok(PendingCall {
            id: id.to_string(),
            plugin,
            perms,
            store,
            settings,
            request,
            lent,
            lent_dirs,
            runs_root: self.runs_root.clone(),
            read_only,
            job: job.map(str::to_string),
            askable,
            denied,
            asking: false,
            needs: Vec::new(),
        })
    }

    /// The `program.end` call for a run of plug-in `id` ([`crate::programs`]): `{"hook": "program.end",
    /// "run", "program", "status", "code", "tmpDir"}` (`code`: the program's exit code, null when
    /// unknown), with the run's folder lent for reading. Run it like any [`Manager::begin`] call,
    /// then delete the folder once the program has exited too.
    pub fn program_end_call(
        &mut self,
        id: &str,
        run: &str,
        program: &str,
        status: crate::EndStatus,
        code: Option<i32>,
        dir: &Path,
    ) -> Result<PendingCall> {
        let req = json!({"hook": "program.end", "run": run, "program": program, "status": status.as_str(), "code": code, "tmpDir": dir});
        self.begin_with_dirs(id, req, Vec::new(), vec![dir.to_path_buf()])
    }

    /// Hands a call from [`Manager::begin`] back with how it went (`None`: it never ran, e.g.
    /// cancelled): its data is saved and returned to the plug-in, its log lines kept.
    pub fn end(&mut self, call: PendingCall, outcome: Option<crate::Outcome>) -> Result<Value> {
        let PendingCall { id, mut store, request, .. } = call;
        let saved = store.save();
        // uninstalled meanwhile: nothing to hand back to
        let Some(p) = self.plugins.get_mut(&id) else {
            saved?;
            return outcome.map_or_else(|| Err(Error::NotFound(id.clone())), |o| o.value);
        };
        p.store = Some(store);
        p.busy = None;
        let Some(outcome) = outcome else {
            saved?;
            return Err(Error::Failed("cancelled".into()));
        };
        let what = request.get("hook").and_then(Value::as_str).unwrap_or("call");
        for l in outcome.logs {
            p.push_log(l);
        }
        match &outcome.value {
            Err(Error::Cancelled) => p.push_log(format!("{what} cancelled")),
            Err(e) => p.push_log(format!("{what} failed: {e}")),
            Ok(_) => {}
        }
        saved?;
        outcome.value
    }

    /// The calls an export makes, in order: each enabled export hook (`hook: "export"`) on the
    /// written files `[{path, photo?}]`, then the export destination `(plugin, destination, args)`
    /// when one is named. Each is `(plugin id, destination id for the destination call, its
    /// request and lent files or why it can't be made)`; nothing runs.
    #[allow(clippy::type_complexity)]
    pub fn export_calls(
        &mut self,
        files: &[Value],
        destination: Option<(&str, &str, &Value)>,
    ) -> Vec<(String, Option<String>, Result<(Value, Vec<PathBuf>)>)> {
        let files: Vec<&Value> = files.iter().filter(|f| f.get("path").and_then(Value::as_str).is_some()).take(MAX_BATCH).collect();
        let lent: Vec<PathBuf> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(PathBuf::from).collect();
        let mut out: Vec<_> = self
            .with_hook(|m| m.hooks.export)
            .into_iter()
            .map(|id| (id, None, Ok((json!({"hook": "export", "files": files}), lent.clone()))))
            .collect();
        if let Some((id, dest, args)) = destination {
            let req = self.destination_request(id, dest, args, &files);
            out.push((id.to_string(), Some(dest.to_string()), req.map(|r| (r, lent.clone()))));
        }
        out
    }

    /// The destination's request; `args` given (not null) are remembered for its dialog.
    fn destination_request(&mut self, id: &str, destination: &str, args: &Value, files: &[&Value]) -> Result<Value> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let decl = p
            .plugin
            .manifest()
            .destination(destination)
            .ok_or_else(|| Error::Params(format!("plug-in {id} has no export destination {destination:?}")))?;
        let checked = dialog_args(&decl.dialog, args)?;
        if !args.is_null() {
            let fields = decl.dialog.clone();
            self.remember(id, &destination_scope(destination), &fields, &checked);
        }
        Ok(json!({"hook": "export.destination", "destination": destination, "args": checked, "files": files}))
    }

    /// Keeps the values a dialog ran with; a failure to save them only goes to the plug-in's log.
    fn remember(&mut self, id: &str, scope: &str, fields: &[Field], args: &Value) {
        let Some(p) = self.plugins.get_mut(id) else { return };
        let r = {
            let mut s = p.settings();
            s.remember(scope, fields, args).and_then(|_| s.save())
        };
        if let Err(e) = r {
            log::warn!("plug-in {id}: remembering dialog values: {e}");
            p.push_log(format!("remembering dialog values: {e}"));
        }
    }

    /// The values a command's dialog opens with: per field, what it last ran with (when the field
    /// remembers and still takes it), else its default. `fields`: the dialog as prepared.
    pub fn command_values(&self, id: &str, command: &str, fields: &[Field]) -> serde_json::Map<String, Value> {
        self.plugins.get(id).map(|p| p.settings().dialog_values(&command_scope(command), fields)).unwrap_or_default()
    }

    /// The values an export destination's dialog opens with (see [`Manager::command_values`]).
    pub fn destination_values(&self, id: &str, destination: &str) -> serde_json::Map<String, Value> {
        let Some(p) = self.plugins.get(id) else { return Default::default() };
        let Some(d) = p.plugin.manifest().destination(destination) else { return Default::default() };
        p.settings().dialog_values(&destination_scope(destination), &d.dialog)
    }

    /// Asks plug-in `id` how to run `command` on `selection` (`{"hook": "command.prepare"}`) when
    /// the command declares `prepare`; otherwise its dialog as declared. A prepare step that fails
    /// (traps, runs out of budget, is disabled or busy, answers something invalid) is logged and
    /// gives the declared dialog with the error, or the error when that dialog can't be shown (a
    /// choice whose options only the plug-in can give).
    pub fn prepare_command(&mut self, id: &str, command: &str, selection: &[String], host: &mut dyn HostApi) -> Result<Prepared> {
        self.prepare_command_from(id, command, selection, &Value::Null, host)
    }

    /// [`Manager::prepare_command`] for a command started from `source`, the Library source the
    /// user was in (`{kind, id?, name?, path?}`; null: none), which the request carries.
    pub fn prepare_command_from(
        &mut self,
        id: &str,
        command: &str,
        selection: &[String],
        source: &Value,
        host: &mut dyn HostApi,
    ) -> Result<Prepared> {
        self.prepare_from(id, command, selection, source, host, None)
    }

    /// [`Manager::prepare_command_from`] for a command the user started, with the user at the app
    /// ([`PendingCall::ask_user`]): what the prepare step was refused that the user was never
    /// asked about is added to `needs` (each once), for the host to ask once it ends.
    pub fn prepare_command_asking(
        &mut self,
        id: &str,
        command: &str,
        selection: &[String],
        source: &Value,
        host: &mut dyn HostApi,
        needs: &mut Vec<Request>,
    ) -> Result<Prepared> {
        self.prepare_from(id, command, selection, source, host, Some(needs))
    }

    fn prepare_from(
        &mut self,
        id: &str,
        command: &str,
        selection: &[String],
        source: &Value,
        host: &mut dyn HostApi,
        needs: Option<&mut Vec<Request>>,
    ) -> Result<Prepared> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let decl = p.plugin.manifest().command(command).ok_or_else(|| Error::Params(format!("plug-in {id} has no command {command:?}")))?.clone();
        if !decl.prepare {
            return Ok(Prepared::Dialog { fields: decl.dialog, error: None });
        }
        let mut req = json!({"hook": "command.prepare", "command": command, "selection": cap(selection)});
        if !source.is_null() {
            req["source"] = source.clone();
        }
        let answer = self.begin(id, req, Vec::new()).and_then(|mut call| {
            if needs.is_some() {
                call.ask_user();
            }
            let outcome = call.run(host);
            if let Some(needs) = needs {
                for r in call.take_needs() {
                    if !needs.contains(&r) {
                        needs.push(r);
                    }
                }
            }
            self.end(call, Some(outcome))
        });
        let answer = answer.and_then(|v| {
            prepared_answer(&decl.dialog, &v).map_err(|e| {
                // invoke logged failed calls; an answer that doesn't fit is logged here
                if let Some(p) = self.plugins.get_mut(id) {
                    p.push_log(format!("command.prepare: {e}"));
                }
                Error::Failed(format!("command.prepare: {e}"))
            })
        });
        match answer {
            Ok(a) => Ok(a),
            Err(e) if decl.dialog.iter().any(Field::options_at_run_time) => Err(e),
            Err(e) => Ok(Prepared::Dialog { fields: decl.dialog, error: Some(e.to_string()) }),
        }
    }

    /// Runs a plug-in's menu command: `args` are checked against its dialog, `selection` are the
    /// selected photo ids. Args given (not null) are remembered for the dialog.
    ///
    /// A command with `prepare` is asked first ([`Manager::prepare_command`]): a message comes back
    /// as `{message, ran: false}` and nothing runs; with the arguments it gave, it runs with those
    /// when `args` is null; with a dialog, `args` are checked against the fields as prepared.
    pub fn run_command(&mut self, id: &str, command: &str, args: &Value, selection: &[String], host: &mut dyn HostApi) -> Result<Value> {
        self.run_command_from(id, command, args, selection, &Value::Null, host)
    }

    /// [`Manager::run_command`] for a command started from `source` (see
    /// [`Manager::prepare_command_from`]): its prepare step and the command get it. A job runs to
    /// its end here, with nobody watching ([`Manager::begin_command`] runs one on a worker).
    pub fn run_command_from(
        &mut self,
        id: &str,
        command: &str,
        args: &Value,
        selection: &[String],
        source: &Value,
        host: &mut dyn HostApi,
    ) -> Result<Value> {
        match self.begin_command(id, command, args, selection, source, host)? {
            Begun::Message(m) => Ok(json!({"message": m, "ran": false})),
            Begun::Call(mut call) => {
                let outcome = call.run(host);
                self.end(*call, Some(outcome))
            }
        }
    }

    /// Prepares a run of plug-in `id`'s `command` as [`Manager::run_command_from`] does (its
    /// prepare step runs here, through `host`), and hands back the call to run without the manager
    /// ([`PendingCall::run_with`], e.g. a job on a worker), then [`Manager::end`]. A job is named
    /// by its command's label in the error other calls of the plug-in get until then.
    pub fn begin_command(
        &mut self,
        id: &str,
        command: &str,
        args: &Value,
        selection: &[String],
        source: &Value,
        host: &mut dyn HostApi,
    ) -> Result<Begun> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let decl = p.plugin.manifest().command(command).ok_or_else(|| Error::Params(format!("plug-in {id} has no command {command:?}")))?;
        let declared = decl.dialog.clone();
        let job = decl.job.then(|| decl.label.clone());
        let (fields, checked) = match self.prepare_command_from(id, command, selection, source, host)? {
            Prepared::Message(m) => return Ok(Begun::Message(m)),
            Prepared::Run(given) if args.is_null() => (declared, given),
            Prepared::Run(_) => {
                let a = dialog_args(&declared, args)?;
                (declared, a)
            }
            // the prepare step failed: run only with arguments someone chose, never on defaults
            Prepared::Dialog { error: Some(e), .. } if args.is_null() => return Err(Error::Failed(e)),
            Prepared::Dialog { fields, .. } => {
                let a = dialog_args(&fields, args)?;
                (fields, a)
            }
        };
        if !args.is_null() {
            self.remember(id, &command_scope(command), &fields, &checked);
        }
        let mut req = json!({"hook": "command", "command": command, "args": checked, "selection": cap(selection)});
        if !source.is_null() {
            req["source"] = source.clone();
        }
        self.begin_as(id, req, Vec::new(), Vec::new(), job.as_deref()).map(|c| Begun::Call(Box::new(c)))
    }

    /// Plug-in `id`'s settings fields and values: the manifest's fields, prepared by the
    /// `settings.prepare` hook when one of them declares `prepare` (a failure is logged and gives
    /// the declared fields, with the error), and per field its value (the stored one when the field
    /// as prepared takes it, else its default).
    pub fn settings(&mut self, id: &str, host: &mut dyn HostApi) -> Result<SettingsView> {
        let (fields, error) = self.prepare_settings(id, host)?;
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let values = p.settings().values(&fields);
        Ok(SettingsView { fields, values, error })
    }

    fn prepare_settings(&mut self, id: &str, host: &mut dyn HostApi) -> Result<(Vec<Field>, Option<String>)> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let declared = p.plugin.manifest().settings.clone();
        if !declared.iter().any(|f| f.prepare) {
            return Ok((declared, None));
        }
        let answer = self.invoke(id, &json!({"hook": "settings.prepare"}), host, &[]).and_then(|v| {
            prepared_settings(&declared, &v).map_err(|e| {
                if let Some(p) = self.plugins.get_mut(id) {
                    p.push_log(format!("settings.prepare: {e}"));
                }
                Error::Failed(format!("settings.prepare: {e}"))
            })
        });
        Ok(match answer {
            Ok(fields) => (fields, None),
            Err(e) => (declared, Some(e.to_string())),
        })
    }

    /// Changes plug-in `id`'s settings (`values`: key → value, `null` back to the default). Every
    /// value is checked against its field (as prepared, see [`Manager::settings`]) before any is
    /// kept: an unknown key or a value that doesn't fit refuses them all. Returns the settings now.
    pub fn set_settings(&mut self, id: &str, values: &Value, host: &mut dyn HostApi) -> Result<serde_json::Map<String, Value>> {
        let values = values.as_object().ok_or_else(|| Error::Params("values must be an object {key: value}".into()))?;
        let (fields, _) = self.prepare_settings(id, host)?;
        let mut checked = Vec::with_capacity(values.len());
        for (k, v) in values {
            let f = fields.iter().find(|f| f.key == *k).ok_or_else(|| Error::Params(format!("plug-in {id} has no setting {k:?}")))?;
            // a choice whose options weren't given (the prepare step failed) shows "" while unset:
            // saving the page then keeps it unset rather than refusing every other value
            let unset = v.is_null() || (f.options_at_run_time() && v.as_str() == Some(""));
            checked.push((k.clone(), if unset { None } else { Some(f.coerce(v).map_err(Error::Params)?) }));
        }
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let mut s = p.settings();
        let before: Vec<(String, Option<Value>)> = checked.iter().map(|(k, _)| (k.clone(), s.stored(k))).collect();
        let revision = s.revision();
        for (k, v) in checked {
            if let Err(e) = s.set(&k, v) {
                // all or nothing: put back what this call changed (and its revision: nothing changed)
                for (k, v) in before {
                    let _ = s.set(&k, v);
                }
                s.rewind(revision);
                return Err(e);
            }
        }
        s.save()?;
        Ok(s.values(&fields))
    }

    /// Runs every enabled export hook on the exported files `[{path, photo?}]`. Each hook may read
    /// and rewrite those files (lent for the call). Returns one result per plug-in; a failing
    /// hook doesn't stop the others.
    pub fn post_export(&mut self, files: &[Value], host: &mut dyn HostApi) -> Vec<Value> {
        let lent: Vec<PathBuf> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(PathBuf::from).take(MAX_BATCH).collect();
        let ids = self.with_hook(|m| m.hooks.export);
        let req = json!({"hook": "export", "files": files.iter().take(MAX_BATCH).collect::<Vec<_>>()});
        ids.into_iter().map(|id| outcome(&id, self.invoke(&id, &req, host, &lent))).collect()
    }

    /// Asks every enabled metadata provider about `photos` (photo JSON, as `photo.inspect`
    /// returns it). Each result is `{plugin, ok: {suggestions: [{photo, fields?, keywords?}]}}`
    /// or `{plugin, error}`; nothing is written.
    pub fn suggest_metadata(&mut self, photos: &[Value], host: &mut dyn HostApi) -> Vec<Value> {
        let ids = self.with_hook(|m| m.hooks.metadata);
        let req = json!({"hook": "metadata", "photos": photos.iter().take(MAX_BATCH).collect::<Vec<_>>()});
        ids.into_iter().map(|id| outcome(&id, self.invoke(&id, &req, host, &[]))).collect()
    }

    /// Enabled plug-ins' export destinations: `(plugin id, destination)`.
    pub fn export_destinations(&self) -> Vec<(String, crate::DestinationDecl)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .flat_map(|p| p.plugin.manifest().export_destinations.iter().map(|d| (p.plugin.id().to_string(), d.clone())))
            .collect()
    }

    /// Hands exported files `[{path, photo?}]` to a plug-in's export destination. `args` are
    /// checked against the destination's dialog; the files are lent to the call.
    pub fn export_to(&mut self, id: &str, destination: &str, args: &Value, files: &[Value], host: &mut dyn HostApi) -> Result<Value> {
        let files: Vec<&Value> = files.iter().filter(|f| f.get("path").and_then(Value::as_str).is_some()).take(MAX_BATCH).collect();
        let lent: Vec<PathBuf> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).map(PathBuf::from).collect();
        let req = self.destination_request(id, destination, args, &files)?;
        self.invoke(id, &req, host, &lent)
    }

    fn store_mut(&mut self, id: &str) -> Result<&mut Installed> {
        let library = self.library.clone();
        let dir = self.dir.clone();
        let p = self.plugins.get_mut(id).ok_or_else(|| Error::NotFound(id.into()))?;
        // its data is lent to a call running elsewhere: opening it again would fork it
        if let Some(why) = &p.busy {
            return Err(Error::Busy(id.into(), why.clone()));
        }
        if p.store.is_none() {
            p.store = Some(match &dir {
                Some(d) => NamespaceStore::open(d.join("data").join(format!("{id}.json")))?,
                None => NamespaceStore::in_memory(),
            });
        }
        if let Some(store) = p.store.as_mut() {
            scope(store, &library);
        }
        Ok(p)
    }

    /// The custom metadata fields of every enabled plug-in, with their values for `photo`
    /// (from each plug-in's namespace; no plug-in code runs). A plug-in whose data can't be read
    /// is listed with an `error`.
    pub fn metadata_fields(&mut self, photo: &str) -> Vec<Value> {
        let ids: Vec<String> = self.with_hook(|m| !m.metadata_fields.is_empty());
        let mut out = Vec::new();
        for id in ids {
            let r = self.store_mut(&id).map(|p| {
                let data = p.store.as_ref().map(|s| s.get(photo)).unwrap_or(Value::Null);
                let m = p.plugin.manifest();
                let fields: Vec<Value> = m
                    .metadata_fields
                    .iter()
                    .map(|f| {
                        let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
                        // a stored value the field no longer accepts shows as empty
                        v["value"] = data.get(&f.key).and_then(|x| f.as_field().coerce(x).ok()).unwrap_or(Value::Null);
                        v
                    })
                    .collect();
                json!({"plugin": m.id, "pluginName": m.name, "fields": fields})
            });
            out.push(r.unwrap_or_else(|e| json!({"plugin": id, "error": e.to_string()})));
        }
        out
    }

    /// Every enabled plug-in's custom metadata field values, by photo: `photo → {"<plug-in>/<key>": value}`
    /// (values the field accepts; empty ones left out). A plug-in whose data can't be read (or that is
    /// busy) is skipped and named in the second list.
    pub fn field_values(&mut self) -> (BTreeMap<String, BTreeMap<String, Value>>, Vec<String>) {
        let ids: Vec<String> = self.with_hook(|m| !m.metadata_fields.is_empty());
        let mut out: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        let mut skipped = Vec::new();
        for id in ids {
            let Ok(p) = self.store_mut(&id) else {
                skipped.push(id);
                continue;
            };
            let Some(store) = p.store.as_ref() else {
                skipped.push(id);
                continue;
            };
            let fields = &p.plugin.manifest().metadata_fields;
            for photo in store.photos() {
                let data = store.get(&photo);
                for f in fields {
                    if let Some(v) = data.get(&f.key).and_then(|x| f.as_field().coerce(x).ok()).filter(|v| !v.is_null()) {
                        out.entry(photo.clone()).or_default().insert(format!("{id}/{}", f.key), v);
                    }
                }
            }
        }
        (out, skipped)
    }

    /// Sets (or with `null`, clears) a custom metadata field the user edited. Read-only fields
    /// and values the field doesn't accept are refused.
    pub fn set_metadata_field(&mut self, id: &str, photo: &str, key: &str, value: &Value) -> Result<Value> {
        let p = self.plugins.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        if !p.enabled {
            return Err(Error::Disabled(id.into()));
        }
        let f = p.plugin.manifest().metadata_field(key).ok_or_else(|| Error::Params(format!("plug-in {id} has no metadata field {key:?}")))?;
        if f.read_only {
            return Err(Error::Params(format!("metadata field {key:?} is read-only")));
        }
        let v = if value.is_null() { Value::Null } else { f.as_field().coerce(value).map_err(Error::Params)? };
        let p = self.store_mut(id)?;
        let store = p.store.get_or_insert_with(NamespaceStore::in_memory);
        store.set(photo, key, v.clone()).map_err(Error::Params)?;
        store.save()?;
        Ok(v)
    }

    /// Restore a metadata field value read from an XMP sidecar (`text`, as the sidecar writes it) into
    /// plug-in `id`'s namespace for `photo`. Unlike [`Manager::set_metadata_field`] read-only fields
    /// are restored too (the plug-in set them). `Ok(false)`: nothing to do (no such plug-in or field,
    /// disabled, or the value is already there); a value the field doesn't take is an error.
    pub fn restore_metadata_field(&mut self, id: &str, photo: &str, key: &str, text: &str) -> Result<bool> {
        let Some(p) = self.plugins.get(id).filter(|p| p.enabled) else { return Ok(false) };
        let Some(f) = p.plugin.manifest().metadata_field(key) else { return Ok(false) };
        let v = f.as_field().from_text(text).map_err(Error::Params)?;
        let p = self.store_mut(id)?;
        let store = p.store.get_or_insert_with(NamespaceStore::in_memory);
        if store.get(photo).get(key) == Some(&v) {
            return Ok(false);
        }
        store.set(photo, key, v).map_err(Error::Params)?;
        store.save()?;
        Ok(true)
    }

    fn with_hook(&self, f: impl Fn(&crate::Manifest) -> bool) -> Vec<String> {
        self.plugins.values().filter(|p| p.enabled && f(p.plugin.manifest())).map(|p| p.plugin.id().to_string()).collect()
    }

    /// Enabled plug-ins that provide a publish service: `(plugin id, service)`.
    pub fn publish_services(&self) -> Vec<(String, crate::PublishDecl)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.plugin.manifest().publish.clone().map(|s| (p.plugin.id().to_string(), s)))
            .collect()
    }
}

/// A `command.prepare` answer, checked: exactly one of `{dialog: {<field key>: {options?, default?}}}`,
/// `{run: args}` (checked against `fields`) or `{message}`.
fn prepared_answer(fields: &[Field], v: &Value) -> std::result::Result<Prepared, String> {
    let o = v.as_object().filter(|o| o.len() == 1).ok_or("the answer must be an object with one of `dialog`, `run` or `message`")?;
    if let Some(m) = o.get("message") {
        let m = m.as_str().ok_or("`message` must be text")?;
        return Ok(Prepared::Message(m.chars().take(MAX_PREPARE_MESSAGE).collect()));
    }
    if let Some(a) = o.get("run") {
        if !a.is_object() {
            return Err("`run` must be an object of arguments".into());
        }
        return dialog_args(fields, a).map(Prepared::Run).map_err(|e| format!("`run`: {e}"));
    }
    let d = o.get("dialog").ok_or("the answer must have one of `dialog`, `run` or `message`")?;
    Ok(Prepared::Dialog { fields: prepare_fields(fields, d, |_| true)?, error: None })
}

/// A `settings.prepare` answer, checked: `{fields: {<key>: {options?, default?}}}` for settings
/// that declare `prepare`.
fn prepared_settings(fields: &[Field], v: &Value) -> std::result::Result<Vec<Field>, String> {
    let o = v.as_object().filter(|o| o.len() == 1).ok_or("the answer must be an object {fields}")?;
    let f = o.get("fields").ok_or("the answer must be an object {fields}")?;
    prepare_fields(fields, f, |f| f.prepare)
}

/// `fields` with the options and defaults in `given` (`{<key>: {options?, default?}}`), each for a
/// field `may` allows. A choice whose options are given at run time must get them.
fn prepare_fields(fields: &[Field], given: &Value, may: impl Fn(&Field) -> bool) -> std::result::Result<Vec<Field>, String> {
    let given = given.as_object().ok_or("expected an object {<field key>: {options?, default?}}")?;
    if let Some(k) = given.keys().find(|k| !fields.iter().any(|f| f.key == **k && may(f))) {
        return Err(format!("no field {:?} to prepare", k.chars().take(64).collect::<String>()));
    }
    fields
        .iter()
        .map(|f| match given.get(&f.key) {
            Some(o) => f.prepared(o),
            None if f.options_at_run_time() && may(f) => Err(format!("`{}`: no options given", f.key)),
            None => Ok(f.clone()),
        })
        .collect()
}

/// Points `store` at `library`; data it claims from before is saved at once, so another library
/// can't claim it on the next start.
fn scope(store: &mut NamespaceStore, library: &str) {
    if store.set_scope(library)
        && let Err(e) = store.save()
    {
        log::warn!("plug-in data: {e}");
    }
}

fn cap(ids: &[String]) -> &[String] {
    ids.get(..MAX_BATCH).unwrap_or(ids)
}

fn outcome(id: &str, r: Result<Value>) -> Value {
    match r {
        Ok(v) => json!({"plugin": id, "ok": v}),
        Err(e) => json!({"plugin": id, "error": e.to_string()}),
    }
}

/// The version of plug-in `id` an update replaced, in `dir`: its module and `{version, settings}`.
fn previous_paths(dir: &Path, id: &str) -> (PathBuf, PathBuf) {
    let d = dir.join("previous");
    (d.join(format!("{id}.wasm")), d.join(format!("{id}.json")))
}

/// The version kept for plug-in `id` in `dir` to go back to: its version, settings (`null`: none
/// kept) and the signer pinned when it was replaced (`None`: kept before signers were recorded).
/// `None`: no version is kept, or it can't be read.
fn read_previous(dir: &Path, id: &str) -> Option<(String, Value, Option<Pin>)> {
    let (module, about) = previous_paths(dir, id);
    if !std::fs::metadata(&module).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let bytes = crate::safe_io::read_capped(&about, MAX_PREVIOUS_BYTES).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let version = v.get("version")?.as_str()?.to_string();
    Some((version, v.get("settings").cloned().unwrap_or(Value::Null), v.get("signer").map(Pin::read)))
}

/// Keeps `module`, version `version` with `settings` and the signer pinned for it (`pin`), as plug-in
/// `id`'s version to go back to.
fn keep_previous(dir: &Path, id: &str, module: &[u8], version: &str, settings: &Value, pin: &Pin) -> Result<()> {
    let (path, about) = previous_paths(dir, id);
    write_atomic(&path, module)?;
    // compact: the settings may be as large as their own file allows, and must read back
    let text =
        serde_json::to_vec(&json!({"version": version, "settings": settings, "signer": pin.stored()})).map_err(|e| Error::Io(e.to_string()))?;
    write_atomic(&about, &text)
}

/// Deletes plug-in `id`'s version to go back to (what tells it's there first).
fn discard_previous(dir: &Path, id: &str) -> Result<()> {
    let (path, about) = previous_paths(dir, id);
    for f in [about, path] {
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(format!("{}: {e}", f.display()))),
        }
    }
    Ok(())
}

/// Why a module didn't load: `first`, earlier by file name, has its id.
pub fn duplicate_error(first: &str) -> String {
    format!("another module has this id: {first} (the first by file name loads)")
}

/// Reads a module file: a regular file within the size limit (refused before it is read).
pub fn read_module(path: &Path, limits: &Limits) -> Result<Vec<u8>> {
    use std::io::Read;
    let io = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
    let meta = std::fs::metadata(path).map_err(io)?;
    if !meta.is_file() {
        return Err(Error::Io(format!("{} is not a file", path.display())));
    }
    if meta.len() > limits.max_module_bytes as u64 {
        return Err(Error::Module(format!("{} is {} bytes (limit {})", path.display(), meta.len(), limits.max_module_bytes)));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(io)?.take(limits.max_module_bytes as u64 + 1).read_to_end(&mut bytes).map_err(io)?;
    Ok(bytes)
}
