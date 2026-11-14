//! The thread that writes the catalog log for [`crate::FsStore`] (natively).
//!
//! An append hands its bytes over and returns; the thread writes them in order and makes each batch
//! durable (`sync_data`) before the next. The caller (the UI thread, for the app) never waits for
//! the drive: on Windows a write to a file waits for a sync of that file in flight, so with the log
//! written and synced on the UI thread every flag, rating and step waited for the drive, 30–90 ms
//! on a busy disk in a test and over a second on a photographer's machine while previews were
//! written.
//!
//! **Nothing handed over is ever dropped.** The store's log is the file's first `good` bytes (written
//! and synced) followed by `held`, the bytes handed over and not yet synced. A write or sync that
//! fails keeps `held` and retries it (on every later message, and every [`RETRY`] meanwhile), first
//! cutting the file back to `good` so a fragment of the failed write never stays in it. While a
//! failure stands, [`LogWriter::append`] refuses new bytes with the failure (the journal keeps those
//! records queued, as for any failed append) and [`LogWriter::unwritten`] says how many records are
//! held, so the app counts them as not saved and its quit prompt warns before anything could be
//! lost. Cutting the log back, replacing it and reading it act on that logical content, after
//! everything handed over before; the caller waits for them. Dropping the writer waits for
//! everything handed over and tries a last time.
//!
//! **Every write asks the [`OpenedFolder`] first**, on this thread (a `stat` never makes the caller
//! wait for the drive): when the log's path no longer leads to the folder the store opened (a share
//! mounted over it, issue #3240) nothing is written. The bytes are held like those of any other
//! write that failed, and written once the folder that was opened is back at the path.
//!
//! **Successful batches reopen the log by path.** A failed batch keeps its descriptor only to
//! roll back its own bytes, never to append into a moved-away folder. A copy of the library's folder
//! put at the path counts as the opened one (it holds the lock's note) and has a log of its own: a
//! file kept open from before would go on writing into the folder that was moved away. A log that
//! isn't as long as what this thread wrote (a fragment of its own failed write is cut first) is not
//! the one it wrote, and appending to it would leave a gap: what it was handed is held like a failed
//! write (reported; later appends are taken meanwhile, since retrying can't heal it), and the
//! snapshot the app takes for held records, which holds every change, replaces the log.

use std::io::{self, Seek, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::opened_folder::OpenedFolder;

/// How long a caller waits for the thread at most (a cut, a replace, a read, a flush).
const WAIT: Duration = Duration::from_secs(60);
/// How often held bytes are retried while a failure stands and nothing else arrives.
pub(crate) const RETRY: Duration = Duration::from_millis(500);

enum Msg {
    Append(Vec<u8>),
    Truncate(u64, Sender<io::Result<()>>),
    Replace(Vec<u8>, Sender<io::Result<()>>),
    Read(Sender<io::Result<Option<Vec<u8>>>>),
    /// Reply once everything before is written and synced (or why it isn't).
    Flush(Sender<io::Result<()>>),
}

/// What the caller can see without waiting.
#[derive(Default)]
struct Shared {
    /// Why the held bytes couldn't be written (until they are, or the log is cut or replaced).
    failed: Option<String>,
    /// Records (lines) handed over and not yet written and synced, while `failed` stands.
    held_records: usize,
    /// The file isn't the log the thread wrote: only a replace helps, so appends are taken.
    foreign: bool,
}

pub(crate) struct LogWriter {
    pub(crate) name: String,
    tx: Option<Sender<Msg>>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn stopped() -> io::Error {
    io::Error::other("the catalog log writer stopped")
}

fn not_written(why: &str) -> io::Error {
    io::Error::other(format!("the catalog log could not be written: {why}"))
}

impl LogWriter {
    /// A writer for `name` in the folder (`None` when no thread can start: the store then writes itself).
    pub(crate) fn start(folder: OpenedFolder, name: &str, expected: Option<u64>) -> Option<LogWriter> {
        let (tx, rx) = channel::<Msg>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sh = shared.clone();
        let mut w = Writer::new(folder, name);
        w.good = expected;
        let thread = std::thread::Builder::new().name("catalog-log".into()).spawn(move || w.run(&rx, &sh)).ok()?;
        Some(LogWriter { name: name.to_string(), tx: Some(tx), shared, thread: Some(thread) })
    }

    fn send(&self, m: Msg) -> io::Result<()> {
        self.tx.as_ref().ok_or_else(stopped)?.send(m).map_err(|_| stopped())
    }

    fn ask<T>(&self, make: impl FnOnce(Sender<io::Result<T>>) -> Msg) -> io::Result<T> {
        let (tx, rx) = channel();
        self.send(make(tx))?;
        rx.recv_timeout(WAIT).map_err(|e| match e {
            RecvTimeoutError::Timeout => io::Error::new(io::ErrorKind::TimedOut, "the drive took over a minute to write the catalog log"),
            RecvTimeoutError::Disconnected => stopped(),
        })?
    }

    /// Hand `data` over (never waits for the drive). Refused while bytes handed over before can't
    /// be written: the caller keeps `data` and appends it again later.
    pub(crate) fn append(&self, data: &[u8]) -> io::Result<()> {
        let sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(why) = sh.failed.as_deref().filter(|_| !sh.foreign) {
            return Err(not_written(why));
        }
        drop(sh);
        self.send(Msg::Append(data.to_vec()))
    }

    /// Cut the log to `len` bytes, after everything handed over; waits for it.
    pub(crate) fn truncate(&self, len: u64) -> io::Result<()> {
        self.ask(|r| Msg::Truncate(len, r))
    }

    /// Replace the log with `data` (atomically, durably), after everything handed over; waits.
    pub(crate) fn replace(&self, data: &[u8]) -> io::Result<()> {
        self.ask(|r| Msg::Replace(data.to_vec(), r))
    }

    /// The log as appended so far (written or still held), after everything handed over.
    pub(crate) fn read(&self) -> io::Result<Option<Vec<u8>>> {
        self.ask(Msg::Read)
    }

    /// Wait until everything handed over is written and synced; an error says why it isn't (the
    /// bytes are still held and retried).
    pub(crate) fn flush(&self) -> io::Result<()> {
        self.ask(Msg::Flush)
    }

    /// Records handed over that can't be written yet, and why (`None`: nothing is held back by a
    /// failure). Never waits.
    pub(crate) fn unwritten(&self) -> Option<(usize, String)> {
        let sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        sh.failed.clone().map(|why| (sh.held_records.max(1), why))
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The thread's side: how much of the file is written, and what is held for it.
struct Writer {
    dir: PathBuf,
    path: PathBuf,
    tmp: PathBuf,
    /// The folder the store opened: asked before anything is written.
    folder: OpenedFolder,
    /// Bytes read by the journal or written whole and synced (`None` until first observed).
    good: Option<u64>,
    /// Handed over, not yet written and synced.
    held: Vec<u8>,
    failed: Option<String>,
    /// The exact file whose append failed; only this descriptor can have its torn bytes removed.
    torn: Option<std::fs::File>,
    /// The file isn't the log this thread wrote ([`Writer::open`]).
    foreign: bool,
}

fn lines(b: &[u8]) -> usize {
    b.iter().filter(|c| **c == b'\n').count()
}

impl Writer {
    fn new(folder: OpenedFolder, name: &str) -> Writer {
        let dir = folder.path().to_path_buf();
        let path = dir.join(name);
        let tmp = dir.join(format!("{name}.tmp"));
        Writer { dir, path, tmp, folder, good: None, held: Vec::new(), failed: None, torn: None, foreign: false }
    }

    fn hook(&self, step: crate::fault::Step) -> io::Result<()> {
        crate::fault::check(&self.dir, step)
    }

    /// Open the file by its path, positioned at `good`, and say where that is. A fragment a failed
    /// write left past `good` is cut; any other length is not the log this thread wrote (a copy was
    /// put at the path, or it was changed): refused.
    fn open(&mut self) -> io::Result<(std::fs::File, u64)> {
        self.folder.check()?;
        if let Some(good) = self.good {
            crate::store::rollback_torn(&mut self.torn, good)?;
        }
        // (a link or a device planted at the name meanwhile is refused, as at the first append)
        crate::store::check_regular_directory_entry(&self.path)?;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(&self.path)?;
        let len = f.metadata()?.len();
        let good = match self.good {
            None => len,
            Some(g) if len == g => g,
            Some(g) => {
                self.foreign = true;
                return Err(io::Error::other(format!(
                    "{} holds {len} bytes, the log this session wrote has {g}: it is another file, so nothing is appended to it and a snapshot replaces it",
                    self.path.display()
                )));
            }
        };
        f.seek(io::SeekFrom::Start(good))?;
        self.good = Some(good);
        self.foreign = false;
        Ok((f, good))
    }

    /// Write and sync what is held; a failure leaves `good` as it was, and the retry cuts what was left past it.
    fn write_held(&mut self) -> io::Result<()> {
        if self.held.is_empty() {
            return Ok(());
        }
        self.hook(crate::fault::Step::Write)?;
        let (mut f, good) = self.open()?;
        let written = f.write_all(&self.held).and_then(|()| self.hook(crate::fault::Step::Sync)).and_then(|()| f.sync_data());
        self.torn = written.is_err().then_some(f);
        written?;
        self.good = Some(good.saturating_add(self.held.len() as u64));
        self.held.clear();
        Ok(())
    }

    /// Cut the logical log (file then held) to `len` bytes.
    fn truncate(&mut self, len: u64) -> io::Result<()> {
        let good = match self.good {
            Some(g) => g,
            None => match self.open() {
                Ok((_, g)) => g,
                Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e),
            },
        };
        // (to exactly `good` with bytes held: the file is cut, since a write that failed may have
        // left some of them past `good`)
        if len > good || (len == good && self.held.is_empty()) {
            // within what is held: nothing on the drive changes (what a failed write left past
            // `good` is cut before the next write)
            let keep = usize::try_from(len - good).unwrap_or(usize::MAX).min(self.held.len());
            self.held.truncate(keep);
            return Ok(());
        }
        self.folder.check()?;
        crate::store::rollback_torn(&mut self.torn, good)?;
        crate::store::check_regular_directory_entry(&self.path)?;
        let f = std::fs::OpenOptions::new().write(true).open(&self.path)?;
        if f.metadata()?.len() != good {
            self.foreign = true;
            return Err(io::Error::other("the log changed before truncate: a snapshot must replace it"));
        }
        f.set_len(len)?;
        f.sync_all()?;
        self.good = Some(len);
        self.held.clear();
        Ok(())
    }

    fn replace(&mut self, data: &[u8]) -> io::Result<()> {
        self.folder.check()?;
        self.hook(crate::fault::Step::Write)?;
        let mut file = crate::store::create_staging(&self.tmp)?;
        let written = (|| {
            file.write_all(data)?;
            self.hook(crate::fault::Step::Sync)?;
            file.sync_all()?;
            crate::store::check_staging(&self.folder, &self.tmp, &file)?;
            std::fs::rename(&self.tmp, &self.path)
        })();
        match written {
            Ok(()) => {
                // what was held is part of what the caller replaced the log with (a snapshot holds
                // the records before it, `data` the ones after)
                self.good = Some(data.len() as u64);
                self.held.clear();
                self.failed = None;
                self.torn = None;
                self.foreign = false;
                Ok(())
            }
            Err(e) => {
                if crate::store::check_staging(&self.folder, &self.tmp, &file).is_ok() {
                    let _ = std::fs::remove_file(&self.tmp);
                }
                Err(e)
            }
        }
    }

    fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.held.is_empty() => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        if let Some(g) = self.good {
            bytes.truncate(usize::try_from(g).unwrap_or(usize::MAX));
        }
        bytes.extend_from_slice(&self.held);
        Ok(Some(bytes))
    }

    fn publish(&self, shared: &Mutex<Shared>) {
        let mut sh = shared.lock().unwrap_or_else(PoisonError::into_inner);
        sh.failed = self.failed.clone().filter(|_| !self.held.is_empty());
        sh.foreign = self.foreign && sh.failed.is_some();
        sh.held_records = if sh.failed.is_some() { lines(&self.held) } else { 0 };
    }

    fn run(&mut self, rx: &Receiver<Msg>, shared: &Mutex<Shared>) {
        loop {
            let first = if self.failed.is_some() && !self.held.is_empty() {
                match rx.recv_timeout(RETRY) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                }
            };
            let mut batch: Vec<Msg> = first.into_iter().collect();
            while let Ok(m) = rx.try_recv() {
                batch.push(m);
            }
            let mut flushes = Vec::new();
            for m in batch {
                match m {
                    Msg::Append(data) => self.held.extend_from_slice(&data),
                    // (published before the reply: the caller reads what it left at once)
                    Msg::Truncate(len, reply) => {
                        let r = self.truncate(len);
                        self.publish(shared);
                        let _ = reply.send(r);
                    }
                    Msg::Replace(data, reply) => {
                        let r = self.replace(&data);
                        self.publish(shared);
                        let _ = reply.send(r);
                    }
                    Msg::Read(reply) => {
                        let _ = reply.send(self.read());
                    }
                    Msg::Flush(reply) => flushes.push(reply),
                }
            }
            self.step();
            self.publish(shared);
            for r in flushes {
                let _ = r.send(match &self.failed {
                    Some(why) if !self.held.is_empty() => Err(not_written(why)),
                    _ => Ok(()),
                });
            }
        }
        // the store is closing: everything handed over gets one more try
        self.step();
        self.publish(shared);
        if !self.held.is_empty() {
            log::error!("catalog: {} record(s) could not be written to {} (the closing snapshot holds them)", lines(&self.held), self.path.display());
        }
    }

    /// Write what is held; a failure is kept (and logged when it is new).
    fn step(&mut self) {
        match self.write_held() {
            Ok(()) => {
                if self.failed.take().is_some() {
                    log::info!("catalog: {} written again", self.path.display());
                }
            }
            Err(e) => {
                let why = e.to_string();
                if self.failed.as_deref() != Some(why.as_str()) {
                    log::error!("catalog: {}: {why} ({} record(s) held, retried)", self.path.display(), lines(&self.held));
                }
                self.failed = Some(why);
            }
        }
    }
}
