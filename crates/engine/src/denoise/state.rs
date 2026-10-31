use super::model::{Loader, default_loader};
use super::products::{Active, Progress};
use super::settings::Settings;
use super::{Arc, Downloads, HashMap, Mutex, PathBuf, PhotoId, Serialize, VecDeque};

/// Most photos waiting in the queue.
pub(super) const MAX_QUEUE: usize = 5000;

/// Most selected photos the automatic queue looks at in one pump.
pub(super) const AUTO_LOOK: usize = 64;

// ---------------------------------------------------------------------------------------------------------------------
// the session's state

/// How hard the background work may go (the app picks it from what the user is doing, like the face scan).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// Start nothing new.
    Pause,
    /// Two tiles at once.
    Light,
    /// Half the machine.
    Normal,
    /// Most of the machine: the user is watching the progress.
    Full,
}

impl Pace {
    /// `pause`, `light`, `normal` or `full`; anything else is `normal`.
    pub fn parse(name: Option<&str>) -> Pace {
        match name {
            Some("pause") => Pace::Pause,
            Some("light") => Pace::Light,
            Some("full") => Pace::Full,
            _ => Pace::Normal,
        }
    }

    /// Tiles to run at once on a machine with `cores` threads (`cap`: the user's own limit).
    pub fn parallel(self, cores: usize, cap: Option<u32>) -> usize {
        let n = match self {
            Pace::Pause | Pace::Light => 2,
            Pace::Normal => cores / 2,
            Pace::Full => cores * 4 / 5,
        }
        .clamp(1, 16);
        cap.map_or(n, |c| n.min(c.max(1) as usize))
    }
}

/// How a photo stands, for the interface.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PhotoState {
    /// Not a photo denoise applies to (not a raw, offline, a demo).
    NotApplicable,
    /// No model is chosen: nothing can be made.
    NoModel,
    /// Its picture is made: the Amount slider works.
    Ready,
    Queued {
        /// Photos ahead of it (0 = next).
        ahead: usize,
    },
    Running {
        done: usize,
        total: usize,
    },
    /// Making it failed (`why`), or denoise does not apply to this file (`unsupported`).
    Failed {
        why: String,
        unsupported: bool,
    },
    /// Nothing has asked for it yet.
    Idle,
}

#[derive(Clone)]
pub(super) struct Failure {
    pub(super) key: String,
    pub(super) why: String,
    pub(super) unsupported: bool,
}

pub(super) enum Outcome {
    Done,
    Failed(String, bool),
    Cancelled,
}

pub(super) struct Running {
    pub(super) photo: PhotoId,
    pub(super) key: String,
    pub(super) product: PathBuf,
    pub(super) progress: Arc<Progress>,
    pub(super) outcome: Arc<Mutex<Option<Outcome>>>,
    pub(super) started: web_time::Instant,
}

pub(crate) struct State {
    /// Where the models are kept (`None` where there is no file system).
    pub models_dir: Option<PathBuf>,
    /// Models being downloaded at the user's request.
    pub downloads: Downloads,
    pub(crate) install: Option<crate::cmd::denoise::InstallJob>,
    /// How a model is loaded (tests put another in).
    pub loader: Loader,
    pub(crate) active: Option<Active>,
    pub(super) active_seen: Option<web_time::Instant>,
    /// `settings.json` as last read (with the model choice).
    pub(crate) settings: Settings,
    /// The index must be made again (the library, the model or the cache changed).
    pub(super) dirty: bool,
    /// The catalog revision and time of the last index, to look again when photos changed.
    pub(super) reindexed: Option<(u64, web_time::Instant)>,
    pub(super) queue: VecDeque<PhotoId>,
    pub(super) running: Option<Running>,
    pub(super) failed: HashMap<PhotoId, Failure>,
    /// Counts every change of what is ready, so the interface knows to draw again.
    pub generation: u64,
    /// Photos made since the session started.
    pub(super) made: u64,
    /// AI Denoise's row in the activity stack, while a photo is being made or waits its turn.
    pub(super) task: Option<crate::activity::TaskGuard>,
}

impl Default for State {
    fn default() -> Self {
        State {
            models_dir: None,
            downloads: Downloads::default(),
            install: None,
            loader: default_loader(),
            active: None,
            active_seen: None,
            settings: Settings::default(),
            dirty: true,
            reindexed: None,
            queue: VecDeque::new(),
            running: None,
            failed: HashMap::new(),
            generation: 0,
            made: 0,
            task: None,
        }
    }
}

impl Drop for State {
    /// Quitting stops the job instead of leaving it to finish a photo nobody will see.
    fn drop(&mut self) {
        if let Some(r) = &self.running {
            r.progress.cancel_now();
        }
    }
}

impl State {
    /// The library was replaced: nothing carries over (photo ids are per library).
    pub fn library_changed(&mut self) {
        if let Some(r) = self.running.take() {
            r.progress.cancel_now();
        }
        self.queue.clear();
        self.failed.clear();
        self.dirty = true;
        self.generation += 1;
    }

    /// The model choice or the cache changed on disk: look again at the next pump.
    pub fn touch(&mut self) {
        self.active_seen = None;
        self.dirty = true;
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}
