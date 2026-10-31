//! The lensfun lens database, downloaded on request: the desktop app offers it once on the first launch
//! ([`fetch_on_start`]: nothing is fetched without the user's click).
//!
//! lensfun's `data/db` (CC BY-SA 3.0, <https://github.com/lensfun/lensfun>) is data, never bundled with LightCraft and
//! never committed to its repository: the desktop app fetches it at run time, when the user chooses Download, into its data folder
//! ([`default_dir`]), with a row in the activity stack, and [`crate::lens_profiles`] reads it from there. The files are
//! pinned to one lensfun commit ([`COMMIT`]) by name, size and SHA-256 ([`FILES`]), so whatever mirror serves them
//! ([`URL_ENV`] lists others, e.g. a copy on LightCraft's own server), what arrives is exactly that commit's data or
//! it is thrown away. A new pin in a later build replaces the folder's files once ([`installed`] compares the pin).
//!
//! The transfer is [`lightcraft_fetch`] (pure-Rust HTTPS, resume, size and hash checks) on a background thread; the
//! files land in a staging folder beside the database folder, the marker is written there last, and only then does the
//! staged folder replace the database folder as a whole ([`publish`]): the previous complete database stays in use
//! until the new one is in place, and comes back if the swap fails. The loader reads the app's folder only when it has
//! the marker ([`complete`]), and [`installed`] also checks every pinned file is there, so a damaged folder is
//! downloaded again. Failure is a message, never a panic.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::activity::{Activity, Cancel, TaskHandle, Unit};

/// The lensfun commit the files are pinned to (2026-10-08).
pub const COMMIT: &str = "43f9e001ab66c4fcdd4f400463f13b72f94b2288";
/// Where the files are fetched from by default: lensfun's repository at [`COMMIT`].
pub const DEFAULT_MIRROR: &str = "https://raw.githubusercontent.com/lensfun/lensfun/43f9e001ab66c4fcdd4f400463f13b72f94b2288/data/db";
/// Other places to fetch the same files from, tried first (base URLs separated by commas or spaces).
pub const URL_ENV: &str = "LIGHTCRAFT_LENSFUN_URL";
/// The file written once every database file is in place: the pin it holds says which data the folder has.
pub const MARKER: &str = "SOURCE.txt";
/// The suffix of the folder beside the database folder where files wait until all have arrived.
#[cfg(not(target_arch = "wasm32"))]
const STAGING: &str = "download";
/// The suffix of the folder the previous database is moved to while the new one takes its place.
const PREVIOUS: &str = "previous";
/// Largest database file accepted (the biggest is under 1 MB).
#[cfg(not(target_arch = "wasm32"))]
const MAX_FILE: u64 = 8 << 20;

/// The database files at [`COMMIT`]: name, size in bytes, SHA-256.
pub const FILES: &[(&str, u64, &str)] = &[
    ("6x6.xml", 29550, "ca13c2756b32d728675372695c072517a0e38af5eff2ecd3e17599dee7fc58b3"),
    ("actioncams.xml", 17188, "79a5da0322574b95690464444c9d9ba1da0e1f827da44fd6bff120a0ec10e4b4"),
    ("compact-canon.xml", 247046, "bae47e4b3a0f8852641038ae2dcca5b28ed7965e65ca795fd676f9de857c4670"),
    ("compact-casio.xml", 6889, "ab35433879c4574ef8f97cd4e3d114d7ff8db4e7279da9d502fc9e911b51ee27"),
    ("compact-fujifilm.xml", 67954, "58af9ebef2410c428ae2c5d01003d2247330d47d918823e63522b4d897a997d1"),
    ("compact-kodak.xml", 2091, "377018ccd74f58e966b37a11e5a8689be4268153bebece9f1f2a0dbfa8234845"),
    ("compact-konica-minolta.xml", 13349, "832a7473951ab625e10c48af6cf60f89b57280016d45729e2584cf9e41ad3cc7"),
    ("compact-leica.xml", 9113, "8e4676679377576dc19e7900ce86fc69b0248ea1d4538aa22ee1ee09431a3be3"),
    ("compact-nikon.xml", 40003, "5393bdb8223e13d21b85818e96571e97c035470b49b0452dfcedc2e1ff512d3b"),
    ("compact-olympus.xml", 34454, "7637d469ad54a02ebce9492a9aa9424a2ff1ec07b0398158076f762fdc08389b"),
    ("compact-panasonic.xml", 70621, "f191fc2a5d49fb50c06f4c399ee8c1a83a05f93a0efaecd352ec5c1479b8e60b"),
    ("compact-pentax.xml", 6689, "596fae3edaadcf2d90ae930fc1bec42597ca0b2bd11842e686e1b915f3736390"),
    ("compact-ricoh.xml", 18424, "46df96ebc367d5170cb6b7dfc3742093f0ad946264518fc5488573ba052f29d7"),
    ("compact-samsung.xml", 13875, "36e3a778046d56385c387d48474cd32a89a5da94d99e640b5ce716ce747f898d"),
    ("compact-sigma.xml", 2395, "0ddc265e3e580da05dcd4664d1b9c2464838d74dd02519c826520950c8f1759e"),
    ("compact-sony.xml", 126229, "be146388b4f6f7bf0d73394e20386ce6ccef3559ab39f1268ba3c14aaef03207"),
    ("contax.xml", 1696, "5eba12e6fd4913470afb6fe25fee47a263084b6cec830297273909872f47669a"),
    ("generic.xml", 5416, "cd572d4110981546b2c4cd296c2ff16755b30063fa136d8b06ad2ca13ab4c59f"),
    ("mil-canon.xml", 247994, "2de8e9467a6af6f83cc9da834d3d3b94d1aaf54519f60c68ac6bc02780a77516"),
    ("mil-fujifilm.xml", 210586, "64586336153b948fe2325f6c46679149a4e3c114a70bee4bd1de88721cb4de72"),
    ("mil-hasselblad.xml", 1048, "13e1fa55d36c2ed7d22964c21ce6c21d887bfe4506e840e425da1ce4157c3756"),
    ("mil-leica.xml", 11663, "c691037e2e0743388473c2a0545e340462c412b7e3b57e451f4f9c89dde2388a"),
    ("mil-nikon.xml", 178941, "dcae5ec2e505411a7f00b6b8dd03825e55620403b558b156be15e9ddfbeaf159"),
    ("mil-olympus.xml", 147123, "989a3d88ef07a286efc2755df214792fdf71d60366932a1b369a77ad39cf2abb"),
    ("mil-panasonic.xml", 299264, "eb5f01f33e65721f3900786d5970c7e220dd042aaa56968be33c009a358ff3c5"),
    ("mil-pentax.xml", 1385, "675a94f52794868ad9f1d79e17b7128783c34b3db23e9eac346c5963812a16e5"),
    ("mil-samsung.xml", 13007, "10130e9e125e0882a16fd3033cfed0175e9c14cb7d35e630cdb94736f27e5dc3"),
    ("mil-samyang.xml", 25990, "ae3bf262f12facdc6f81b837cf6d36fdde2a930d2bfb0918d5abaa47fcfea3ab"),
    ("mil-sigma.xml", 126080, "5f39b232efba5c208e5a98d8afe92cb2eee879213322d47827fcc6f0dc3a06f9"),
    ("mil-sony.xml", 274541, "f9f7e5d7461ac58f76eb55dcc5b211f2e9a451402ddb4a9ecb12d1e4dc04f2b8"),
    ("mil-tamron.xml", 106950, "2f0c9c4dfb9b0419ba0fac2e618bc251c2e71e068b6e09a23d7994b0ff62db0a"),
    ("mil-tokina.xml", 1014, "addb27bd82b0d729644b9da1342f92401f63cc5945309ed17f38f4b7af1649e3"),
    ("mil-zeiss.xml", 19171, "ef8a238904fc9ea6fdb6171863dfd70cd94814af19ecd8c31ee000ca1864589d"),
    ("misc.xml", 176039, "2000a347d9dc37d908ea5b56c760de5b2b4ab475857959ae2e901c31339b01bb"),
    ("om-system.xml", 35344, "a53a627e6ba79fbabb0fa7552ce2b6e56e206425d2eaf0e8ccdb6b8c40c20755"),
    ("rf-leica.xml", 6712, "f93636e1980ad308d1873cc3ac078cdc77c0d529d777cd45c5c8d0e4781981e3"),
    ("slr-canon.xml", 780353, "5244e6df8f2522f829c57e2073313ce31a624e9ed9605310fe43613b9d588c99"),
    ("slr-hasselblad.xml", 642, "97602c467699904bf101179ea2fa24f05f3e1879250498a6ba306cd95a3b7a6b"),
    ("slr-konica-minolta.xml", 47170, "5639ca94f121f609247c2c120b65830b3ef7784db6ecfa7e8eb3d29ff5046c1b"),
    ("slr-leica.xml", 432, "c5e74cb511712e99ed60a066e85c842fea58f693c5039c3819485619f5293d0e"),
    ("slr-nikon.xml", 550157, "5e591e8b1032203cce4c822f1da3d49524a78e7d71c594ea5a4533f2e57aacdf"),
    ("slr-olympus.xml", 116602, "f7693b6779edd8ed10d7dc23113669255bfa4987cea7db0e843f7f5f2bbbe615"),
    ("slr-panasonic.xml", 419, "cd79acb49d9ce5f2a4cecc572b3e117f03da380bed2dcbfd52327192d8ce2381"),
    ("slr-pentax.xml", 209552, "2fba773e41a1cfb70f0c85efdab6d78944bf80ddf89bc2035ef6db39da352f29"),
    ("slr-ricoh.xml", 2869, "c835798136a53535fdf4f81312ac9aad0ea3de3ef732c183d8cc4d2f432011d9"),
    ("slr-samsung.xml", 1110, "dc0a29e424ff96e4316ddeb76126fa6f6c73a8836905a3e1f56f0c493826148f"),
    ("slr-samyang.xml", 16811, "fb1585bea91f5f59e1fae8811dd76c92252ee5d52e3647df2aff4695048443b5"),
    ("slr-schneider.xml", 5890, "de607cfe4ba6e7debc61e282ce39b72825c07601572e14af2bc9108ba67faf81"),
    ("slr-sigma.xml", 444640, "f55f41c144fbb72e3c51d2dfef40e918930d37db1b4150bd02bd746406c036d2"),
    ("slr-soligor.xml", 573, "1aba0aab19bce87ea86175b6551cb5306a512eb54be1698b89991b86008f6a04"),
    ("slr-sony.xml", 88865, "e5ca7f532afe23ff380bf2f9e5458f687bed603355d73a254d85c4e692942fdc"),
    ("slr-tamron.xml", 201992, "c62090789cebe6bbb663111c6e97beeb1111852bb735088e29cf8782df4bab45"),
    ("slr-tokina.xml", 91977, "b1213eda81a613bc3976340398c6b8ce8b7dc29423476fc565d5f8e5cf612ecf"),
    ("slr-ussr.xml", 19024, "1fd6acc8a8f40373379bea2899ffd96fc34b22d9b044d0474dabd4c89af44efb"),
    ("slr-vivitar.xml", 2157, "c5c67e3fe38e8fa19044acadc7730047690435f66aac563f55ffe04997a88111"),
    ("slr-zeiss.xml", 14957, "49aa211a41fd5fe391d26af5fa65bdf9428d60863ffff15d3916fa1c520dc101"),
];

/// The app's own copy of the database: `<config dir>/lensfun`.
pub fn default_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("lensfun"))
}

/// The folder holds the pinned data: [`MARKER`] names [`COMMIT`] and every pinned file is there at its pinned size
/// (a folder whose files went missing is downloaded again).
pub fn installed(dir: &Path) -> bool {
    installed_with(dir, FILES)
}

fn installed_with(dir: &Path, files: &[Pin]) -> bool {
    std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|s| s.contains(COMMIT))
        && files.iter().all(|(name, size, _)| std::fs::metadata(dir.join(name)).is_ok_and(|m| m.is_file() && m.len() == *size))
}

/// The folder holds a whole database, of this pin or an older one: its [`MARKER`], written last, is there. The app's
/// own folder is read only then ([`crate::lens_profiles::database`]).
pub fn complete(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// `dir` with `suffix` added to its name: a folder beside it (`lensfun` → `lensfun.download`).
fn beside(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{suffix}"));
    dir.with_file_name(name)
}

/// Put back the previous database when a swap ([`publish`]) was interrupted between its two moves (the app quit or
/// the machine stopped): `dir` is missing and the previous folder is whole. Best effort; says why it couldn't.
pub fn recover(dir: &Path) {
    let previous = beside(dir, PREVIOUS);
    if !dir.exists()
        && complete(&previous)
        && let Err(e) = std::fs::rename(&previous, dir)
    {
        log::warn!("lens database: could not put back the previous database: {e}");
    }
}

/// The user's answer to the first-launch offer of the database (the desktop app keeps it in its settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Offer {
    /// Not asked yet: the first start asks.
    #[default]
    Ask,
    /// Download was clicked (in the offer or in Settings): a start downloads the pinned database when it is missing
    /// or from an older pin (an interrupted or failed download is tried again).
    Accepted,
    /// Not Now: nothing is downloaded and the start doesn't ask again (Settings can download it, or turn the offer
    /// back on).
    Declined,
}

/// What a start does about the database ([`fetch_on_start`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    Nothing,
    /// Ask the user (once): download into this folder, or not now.
    Ask(PathBuf),
    /// Download into this folder (the user said yes before).
    Fetch(PathBuf),
}

/// The first-start policy, in one place: what a start does about the database. Nothing touches the network
/// without the user's click: the host must allow the question at all (`auto`, [`crate::Session::lens_auto_download`],
/// the desktop app only), a folder of the user's own (`LIGHTCRAFT_LENSFUN_DIR`) is never replaced, an installed
/// database needs nothing, and otherwise the user's answer (`offer`) decides: ask, download, or nothing.
pub fn fetch_on_start(auto: bool, offer: Offer) -> Start {
    let own = std::env::var_os(crate::lens_profiles::ENV).is_some_and(|v| !v.is_empty());
    let dir = default_dir();
    if let Some(d) = dir.as_deref().filter(|_| auto && !own) {
        recover(d);
    }
    let installed = dir.as_deref().is_some_and(installed);
    decide(auto, own, dir, installed, offer)
}

/// [`fetch_on_start`] without the file system: `own` (`LIGHTCRAFT_LENSFUN_DIR` is set), the app's folder `dir` and
/// whether it holds the pinned data.
pub fn decide(auto: bool, own: bool, dir: Option<PathBuf>, installed: bool, offer: Offer) -> Start {
    let Some(dir) = dir.filter(|_| auto && !own && !installed) else { return Start::Nothing };
    match offer {
        Offer::Ask => Start::Ask(dir),
        Offer::Accepted => Start::Fetch(dir),
        Offer::Declined => Start::Nothing,
    }
}

/// The download's size in bytes: the pinned files' sizes added up (what the offer says).
pub fn download_size() -> u64 {
    FILES.iter().map(|f| f.1).fold(0u64, u64::saturating_add)
}

/// [`download_size`] in whole megabytes for people ("about 5 MB"), never 0.
pub fn download_megabytes() -> u64 {
    (download_size().saturating_add(500_000) / 1_000_000).max(1)
}

/// The mirrors to try, in order: [`URL_ENV`]'s, then lensfun's own ([`DEFAULT_MIRROR`]).
pub fn mirrors() -> Vec<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        lightcraft_fetch::mirrors(std::env::var(URL_ENV).ok().as_deref(), None, &[DEFAULT_MIRROR])
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}

/// Where the download stands.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum State {
    /// Not started this session.
    #[default]
    Idle,
    Running {
        bytes: u64,
        total: u64,
    },
    /// In place and loaded: `files` database files.
    Done {
        files: usize,
    },
    Failed(String),
    Cancelled,
}

struct Shared {
    state: Mutex<State>,
    cancel: Arc<AtomicBool>,
    task: Option<TaskHandle>,
}

impl Shared {
    fn set(&self, s: State) {
        if let (State::Running { bytes, total }, Some(task)) = (&s, &self.task) {
            task.progress(*bytes, *total);
        }
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    fn get(&self) -> State {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The session's lens database download.
#[derive(Default)]
pub struct Download {
    job: Option<Arc<Shared>>,
}

/// One pinned file: name, size in bytes, SHA-256.
pub type Pin = (&'static str, u64, &'static str);

impl Download {
    pub fn state(&self) -> State {
        self.job.as_ref().map_or(State::Idle, |j| j.get())
    }

    pub fn running(&self) -> bool {
        matches!(self.state(), State::Running { .. })
    }

    /// Fetch `files` from `mirrors` into `dir` on a background thread, shown in the activity stack until it ends
    /// (its ✕ stops it). Started once at a time; when it ends the database is read again
    /// ([`crate::lens_profiles::replaced`]).
    pub fn start(&mut self, dir: &Path, mirrors: Vec<String>, files: &'static [Pin], activity: &Activity) -> Result<(), String> {
        if self.running() {
            return Err("the lens database is already downloading".into());
        }
        if mirrors.is_empty() {
            return Err("no download location is configured for the lens database".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let task = activity.start("download", "Downloading the lens database", Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail("lensfun (CC BY-SA 3.0)");
        let total = files.iter().map(|f| f.1).fold(0u64, u64::saturating_add);
        task.progress(0, total);
        let shared = Arc::new(Shared { state: Mutex::new(State::Running { bytes: 0, total }), cancel, task: Some(task.handle()) });
        let worker = shared.clone();
        let dir = dir.to_path_buf();
        std::thread::Builder::new()
            .name("lens-database-download".into())
            .spawn(move || {
                // the row goes when the thread ends, however it ends
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&dir, &mirrors, files, &worker)));
                if outcome.is_err() {
                    worker.set(State::Failed("the lens database download stopped unexpectedly".into()));
                }
            })
            .map_err(|e| format!("could not start the lens database download: {e}"))?;
        self.job = Some(shared);
        Ok(())
    }
}

impl Drop for Download {
    /// Quitting while it runs stops it (the staged files stay for the next try to resume).
    fn drop(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// The whole job: fetch and check every file into the staging folder, then move them into `dir`, write the marker
/// and read the database again. Always ends in a final [`State`].
fn run(dir: &Path, mirrors: &[String], files: &[Pin], shared: &Shared) {
    match fetch_all(dir, mirrors, files, shared) {
        Ok(n) => {
            crate::lens_profiles::replaced();
            shared.set(State::Done { files: n });
        }
        Err(e) if shared.cancel.load(Ordering::Relaxed) => {
            log::info!("lens database download stopped: {e}");
            shared.set(State::Cancelled);
        }
        Err(e) => {
            log::warn!("lens database download: {e}");
            shared.set(State::Failed(e));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_all(dir: &Path, mirrors: &[String], files: &[Pin], shared: &Shared) -> Result<usize, String> {
    recover(dir);
    let staging = beside(dir, STAGING);
    std::fs::create_dir_all(&staging).map_err(|e| format!("could not create {}: {e}", staging.display()))?;
    let specs: Vec<lightcraft_fetch::FileSpec<'_>> =
        files.iter().map(|(name, size, sha)| lightcraft_fetch::FileSpec { name, size: Some(*size), sha256: Some(sha), max: MAX_FILE }).collect();
    let total = files.iter().map(|f| f.1).fold(0u64, u64::saturating_add);
    lightcraft_fetch::download(&specs, mirrors, &staging, &lightcraft_fetch::Options::default(), &shared.cancel, &mut |p| {
        shared.set(State::Running { bytes: p.done.min(total), total });
    })
    .map_err(|e| match e {
        lightcraft_fetch::DownloadError::AllMirrorsFailed { errors, .. } => {
            let detail: String = errors.join("; ").chars().take(300).collect();
            format!("could not download the lens database ({detail}): check the internet connection; LightCraft tries again next time it starts")
        }
        other => other.to_string(),
    })?;
    // every file is here and checked. The staged folder becomes the database: first without database files another
    // pin left in it (a resumed older attempt), then with the marker, written last, then in place of the old folder
    // as a whole
    remove_stale(&staging, files, |p| std::fs::remove_file(p))?;
    let readme = format!(
        "lensfun lens database (data/db), commit {COMMIT}
from https://github.com/lensfun/lensfun, downloaded by LightCraft.
         Licence: Creative Commons Attribution-ShareAlike 3.0 (https://creativecommons.org/licenses/by-sa/3.0/), by the lensfun contributors.
         Data only, read by LightCraft at run time; not part of LightCraft.
"
    );
    std::fs::write(staging.join(MARKER), readme).map_err(|e| format!("could not write {}: {e}", staging.join(MARKER).display()))?;
    publish(&staging, dir, |from, to| std::fs::rename(from, to))?;
    Ok(files.len())
}

/// Replace the database folder `dir` with the complete `staged` one, moving with `rename`: the old folder is moved
/// aside, the staged one takes its place, and the old one is removed. When the second move fails the old folder is
/// put back, so `dir` is always either the previous complete database or the new one, never a mix.
#[cfg(not(target_arch = "wasm32"))]
fn publish(staged: &Path, dir: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Result<(), String> {
    let previous = beside(dir, PREVIOUS);
    // left by a swap that ended after its second move
    if previous.exists() {
        std::fs::remove_dir_all(&previous).map_err(|e| format!("could not remove {}: {e}", previous.display()))?;
    }
    let had = dir.exists();
    if had {
        rename(dir, &previous).map_err(|e| format!("could not move the old lens database aside ({}): {e}", dir.display()))?;
    }
    if let Err(e) = rename(staged, dir) {
        if had && let Err(back) = rename(&previous, dir) {
            // (the next start puts it back: [`recover`])
            log::warn!("lens database: could not put back the previous database yet: {back}");
        }
        return Err(format!("could not move the new lens database into {}: {e}", dir.display()));
    }
    if had && let Err(e) = std::fs::remove_dir_all(&previous) {
        log::warn!("lens database: could not remove the previous database {}: {e}", previous.display());
    }
    Ok(())
}

/// Remove the XML files in `dir` that are not among `files` (nothing else in the folder is touched) with `remove`: an
/// error names the first that could not be removed, or why the folder could not be listed.
#[cfg(not(target_arch = "wasm32"))]
fn remove_stale(dir: &Path, files: &[Pin], remove: impl Fn(&Path) -> std::io::Result<()>) -> Result<(), String> {
    // a folder that can't be listed (or an entry that can't be read) may still hold an old file: an error, not success
    let listing = |e: std::io::Error| format!("could not list the lens database folder {}: {e}", dir.display());
    for entry in std::fs::read_dir(dir).map_err(listing)? {
        let entry = entry.map_err(listing)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let xml = std::path::Path::new(&*name).extension().is_some_and(|e| e.eq_ignore_ascii_case("xml"));
        if xml
            && entry.path().is_file()
            && !files.iter().any(|(pinned, _, _)| *pinned == name)
            && let Err(e) = remove(&entry.path())
        {
            return Err(format!("could not remove the old lens database file {name}: {e}"));
        }
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn fetch_all(_: &Path, _: &[String], _: &[Pin], _: &Shared) -> Result<usize, String> {
    Err("the web build downloads no lens database".into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-lensfun-{name}-{}", std::process::id()));
        for leftover in [d.clone(), beside(&d, STAGING), beside(&d, PREVIOUS)] {
            let _ = std::fs::remove_dir_all(leftover);
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A local server answering `GET /db/<name>` with that file's body (404 otherwise).
    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/db", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or_default().to_string();
                match files.iter().find(|(n, _)| path == format!("/db/{n}")) {
                    Some((_, body)) => {
                        let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                        let _ = s.write_all(body);
                    }
                    None => {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        base
    }

    fn wait(what: &str, done: impl Fn() -> bool) {
        let t = std::time::Instant::now();
        while !done() {
            assert!(t.elapsed() < Duration::from_secs(20), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    const LENS: &str = r#"<lensdatabase><lens><maker>Testcam</maker><model>Testcam 50mm f/1.8</model><mount>T</mount><cropfactor>1</cropfactor>
        <calibration><distortion model="poly3" focal="50" k1="-0.01" /></calibration></lens></lensdatabase>"#;
    const CAMERA: &str =
        r#"<lensdatabase><camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1</cropfactor></camera></lensdatabase>"#;

    fn sha(s: &str) -> &'static str {
        Box::leak(lightcraft_faces::hash::sha256_hex(s.as_bytes()).into_boxed_str())
    }

    fn pins() -> &'static [Pin] {
        Box::leak(vec![("lenses.xml", LENS.len() as u64, sha(LENS)), ("cameras.xml", CAMERA.len() as u64, sha(CAMERA))].into_boxed_slice())
    }

    /// Every pinned file lands in the folder with the marker, the activity row shows the download until it ends,
    /// and the database reads.
    #[test]
    fn downloads_checks_and_installs_the_pinned_files() {
        let dir = scratch("ok");
        let base = serve(vec![("lenses.xml", LENS.as_bytes().to_vec()), ("cameras.xml", CAMERA.as_bytes().to_vec())]);
        let activity = Activity::default();
        let mut d = Download::default();
        assert!(!installed_with(&dir, pins()));
        // an older pin's database: replaced as a whole, so a file this pin doesn't list is not read beside the new ones
        std::fs::write(dir.join("dropped.xml"), CAMERA).unwrap();
        std::fs::write(dir.join(MARKER), "older").unwrap();
        // a resumed older attempt left a file this pin doesn't list in the staging folder: not published
        std::fs::create_dir_all(beside(&dir, STAGING)).unwrap();
        std::fs::write(beside(&dir, STAGING).join("old-staged.xml"), CAMERA).unwrap();
        d.start(&dir, vec![base], pins(), &activity).unwrap();
        let rows = activity.list();
        assert!(rows.iter().any(|r| r.kind == "download" && r.label == "Downloading the lens database" && r.cancellable), "{rows:?}");
        wait("the download never ended", || !d.running());
        assert_eq!(d.state(), State::Done { files: 2 });
        assert!(installed_with(&dir, pins()), "the marker names the pinned commit and every file is there");
        assert!(!beside(&dir, STAGING).exists() && !beside(&dir, PREVIOUS).exists(), "no staging or previous folder is left");
        let (db, errors) = lightcraft_lensdb::LensDb::load_dir(&dir).unwrap();
        assert!(errors.is_empty() && db.lenses.len() == 1 && db.cameras.len() == 1);
        assert!(!dir.join("dropped.xml").exists() && !dir.join("old-staged.xml").exists(), "only this pin's files");
        wait("the row stayed", || activity.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not the pinned one (another commit, a hostile mirror) is refused and nothing is installed:
    /// the previous complete database stays as it was.
    #[test]
    fn a_file_that_is_not_the_pinned_one_installs_nothing() {
        let dir = scratch("bad");
        std::fs::write(dir.join("cameras.xml"), "previous").unwrap();
        std::fs::write(dir.join(MARKER), "older").unwrap();
        let base = serve(vec![("lenses.xml", LENS.replace("0.01", "0.02").into_bytes()), ("cameras.xml", CAMERA.as_bytes().to_vec())]);
        let mut d = Download::default();
        d.start(&dir, vec![base], pins(), &Activity::default()).unwrap();
        wait("the download never ended", || !d.running());
        assert!(matches!(d.state(), State::Failed(_)), "{:?}", d.state());
        assert!(!installed_with(&dir, pins()) && complete(&dir), "the previous database is still whole");
        assert_eq!(std::fs::read_to_string(dir.join("cameras.xml")).unwrap(), "previous", "nothing moved into place");
        // no mirror at all is an error, not a thread
        assert!(Download::default().start(&dir, Vec::new(), pins(), &Activity::default()).is_err());
        let _ = std::fs::remove_dir_all(beside(&dir, STAGING));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The new database replaces the old folder as a whole or not at all: when moving it into place fails, the
    /// previous database is put back (and when even that fails, the next start puts it back).
    #[test]
    fn a_failed_swap_keeps_the_previous_database() {
        let dir = scratch("swap");
        std::fs::write(dir.join("lenses.xml"), "old").unwrap();
        std::fs::write(dir.join(MARKER), "older").unwrap();
        let staged = beside(&dir, STAGING);
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("lenses.xml"), "new").unwrap();
        std::fs::write(staged.join(MARKER), COMMIT).unwrap();
        let read = |d: &Path| std::fs::read_to_string(d.join("lenses.xml")).unwrap();
        // the staged folder can't be moved into place: the old one comes back
        let calls = std::cell::Cell::new(0);
        let second_fails = |from: &Path, to: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 { Err(std::io::Error::other("in use")) } else { std::fs::rename(from, to) }
        };
        let err = publish(&staged, &dir, second_fails).unwrap_err();
        assert!(err.contains("in use"), "{err}");
        assert_eq!(read(&dir), "old", "the previous database is in place");
        assert!(complete(&dir) && !beside(&dir, PREVIOUS).exists());
        // neither move back works either (the app stops here): the next start puts the previous database back
        let calls = std::cell::Cell::new(0);
        let all_but_first_fail = |from: &Path, to: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 { std::fs::rename(from, to) } else { Err(std::io::Error::other("in use")) }
        };
        assert!(publish(&staged, &dir, all_but_first_fail).is_err());
        assert!(!dir.exists() && complete(&beside(&dir, PREVIOUS)));
        recover(&dir);
        assert_eq!(read(&dir), "old");
        // and a swap that works leaves only the new database
        publish(&staged, &dir, |from, to| std::fs::rename(from, to)).unwrap();
        assert_eq!(read(&dir), "new");
        assert!(!staged.exists() && !beside(&dir, PREVIOUS).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder whose marker is there but whose database files went missing (or were cut short) is not installed,
    /// so the start downloads it again.
    #[test]
    fn installed_needs_every_pinned_file() {
        let dir = scratch("installed");
        std::fs::write(dir.join(MARKER), format!("commit {COMMIT}")).unwrap();
        assert!(complete(&dir) && !installed_with(&dir, pins()), "the marker alone is not a database");
        std::fs::write(dir.join("lenses.xml"), LENS).unwrap();
        std::fs::write(dir.join("cameras.xml"), CAMERA).unwrap();
        assert!(installed_with(&dir, pins()));
        std::fs::write(dir.join("cameras.xml"), &CAMERA[..10]).unwrap();
        assert!(!installed_with(&dir, pins()), "a file cut short");
        std::fs::remove_file(dir.join("cameras.xml")).unwrap();
        assert!(!installed_with(&dir, pins()), "a file gone");
        std::fs::write(dir.join(MARKER), "an older commit").unwrap();
        std::fs::write(dir.join("cameras.xml"), CAMERA).unwrap();
        assert!(!installed_with(&dir, pins()) && complete(&dir), "an older pin: whole, but replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An old database file that can't be removed is an error (so no marker is written and the next start tries
    /// again); the pinned files and other files are left alone.
    #[test]
    fn an_old_file_that_cannot_be_removed_is_an_error() {
        let dir = scratch("stale");
        for name in ["lenses.xml", "old-lenses.xml", "notes.txt"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let failing = |_: &Path| Err(std::io::Error::other("in use"));
        let err = remove_stale(&dir, pins(), failing).unwrap_err();
        assert!(err.contains("old-lenses.xml") && err.contains("in use"), "{err}");
        remove_stale(&dir, pins(), |p| std::fs::remove_file(p)).unwrap();
        assert!(!dir.join("old-lenses.xml").exists());
        assert!(dir.join("lenses.xml").exists() && dir.join("notes.txt").exists());
        // a folder that can't be listed is an error too, not "nothing stale"
        let not_a_folder = dir.join("notes.txt");
        let err = remove_stale(&not_a_folder, pins(), |p| std::fs::remove_file(p)).unwrap_err();
        assert!(err.contains("could not list"), "{err}");
        let err = remove_stale(&dir.join("missing"), pins(), |p| std::fs::remove_file(p)).unwrap_err();
        assert!(err.contains("could not list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The first start asks before anything is downloaded; Download (or Settings) means a start fetches a missing
    /// database; Not Now means nothing, ever, on its own; an installed database, a folder of the user's own or a host
    /// that may not ask (CLI, MCP) mean nothing whatever the answer.
    #[test]
    fn the_start_asks_once_and_downloads_only_after_a_yes() {
        let dir = || Some(PathBuf::from("lensfun"));
        assert_eq!(decide(true, false, dir(), false, Offer::Ask), Start::Ask(PathBuf::from("lensfun")), "the first start asks");
        assert_eq!(decide(true, false, dir(), false, Offer::Accepted), Start::Fetch(PathBuf::from("lensfun")));
        assert_eq!(decide(true, false, dir(), false, Offer::Declined), Start::Nothing, "Not Now: never asked again");
        for offer in [Offer::Ask, Offer::Accepted, Offer::Declined] {
            assert_eq!(decide(true, false, dir(), true, offer), Start::Nothing, "installed: no question, no download");
            assert_eq!(decide(false, false, dir(), false, offer), Start::Nothing, "a headless host never asks or fetches");
            assert_eq!(decide(true, true, dir(), false, offer), Start::Nothing, "the user's own folder");
            assert_eq!(decide(true, false, None, false, offer), Start::Nothing, "no data folder");
        }
        // the size the offer names comes from the pins
        assert_eq!(download_size(), FILES.iter().map(|f| f.1).sum::<u64>());
        assert!((4..=8).contains(&download_megabytes()), "{}", download_megabytes());
    }

    /// The pinned list is lensfun's whole `data/db` at the commit: unique XML names, sane sizes, hex hashes.
    #[test]
    fn the_pins_are_well_formed() {
        assert_eq!(FILES.len(), 56);
        let mut names: Vec<&str> = FILES.iter().map(|f| f.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), FILES.len());
        for (name, size, sha) in FILES {
            assert!(name.ends_with(".xml") && !name.contains(['/', '\\']), "{name}");
            assert!(*size > 0 && *size < MAX_FILE, "{name}");
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        }
        assert!(DEFAULT_MIRROR.contains(COMMIT));
    }
}
