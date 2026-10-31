//! Stdio progress and cancellation between photos. The backend keeps its session on the
//! serving thread; only stdin and stdout move to transport threads. Other calls wait in order.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Server, response};
use crate::backend::ProgressHook;

enum Input {
    Line(std::io::Result<String>),
    Closed,
    OutputFailed,
}

#[derive(Clone)]
pub(super) struct Wire {
    inbox: Arc<Mutex<Receiver<Input>>>,
    out: Sender<Option<String>>,
    failed: Arc<AtomicBool>,
    deferred: Arc<Mutex<VecDeque<Input>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_input(input: impl BufRead, tx: Sender<Input>) {
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for line in input.lines() {
            let failed = line.is_err();
            if tx.send(Input::Line(line)).is_err() || failed {
                return;
            }
        }
        let _ = tx.send(Input::Closed);
    }));
    if read.is_err() {
        let _ = tx.send(Input::Line(Err(std::io::Error::other("MCP input thread panicked"))));
    }
}

impl Server {
    /// Serve process stdin once, writing replies to `output` (normally stdout).
    ///
    /// The caller must finish the process after this returns. On output failure, the single
    /// stdin worker can remain blocked until process exit; it owns no backend or borrowed data.
    /// Further calls are refused, including after EOF, so blocked readers cannot accumulate.
    pub fn serve_stdio(&mut self, output: impl Write + Send) -> std::io::Result<()> {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::AcqRel) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "MCP stdin already claimed"));
        }
        let (in_tx, in_rx) = std::sync::mpsc::channel();
        let reader_tx = in_tx.clone();
        let reader = match std::thread::Builder::new().name("mcp-input".into()).spawn(move || read_input(BufReader::new(std::io::stdin()), reader_tx))
        {
            Ok(reader) => reader,
            Err(error) => {
                STARTED.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let served = self.serve_messages(in_rx, in_tx, output);
        let read = if reader.is_finished() { reader.join().map_err(|_| std::io::Error::other("MCP input thread panicked")) } else { Ok(()) };
        served.and(read)
    }

    pub(super) fn serve_wire(&mut self, input: impl BufRead + Send, output: impl Write + Send) -> std::io::Result<()> {
        std::thread::scope(|scope| {
            let (in_tx, in_rx) = std::sync::mpsc::channel();
            let reader_tx = in_tx.clone();
            let reader = std::thread::Builder::new().name("mcp-input".into()).spawn_scoped(scope, move || read_input(input, reader_tx))?;
            let served = self.serve_messages(in_rx, in_tx, output);
            let read = reader.join().map_err(|_| std::io::Error::other("MCP input thread panicked"));
            served.and(read)
        })
    }

    fn serve_messages(&mut self, in_rx: Receiver<Input>, in_tx: Sender<Input>, output: impl Write + Send) -> std::io::Result<()> {
        std::thread::scope(|scope| {
            let (out_tx, out_rx) = std::sync::mpsc::channel::<Option<String>>();
            let failed = Arc::new(AtomicBool::new(false));
            let wire =
                Wire { inbox: Arc::new(Mutex::new(in_rx)), out: out_tx, failed: failed.clone(), deferred: Arc::new(Mutex::new(VecDeque::new())) };
            let writer = std::thread::Builder::new().name("mcp-output".into()).spawn_scoped(scope, move || -> std::io::Result<()> {
                let mut output = output;
                let written = (|| -> std::io::Result<()> {
                    while let Ok(Some(line)) = out_rx.recv() {
                        output.write_all(line.as_bytes())?;
                        output.write_all(b"\n")?;
                        output.flush()?;
                    }
                    Ok(())
                })();
                if written.is_err() {
                    failed.store(true, Ordering::Release);
                    let _ = in_tx.send(Input::OutputFailed);
                }
                written
            })?;
            self.wire = Some(wire.clone());
            let served = (|| -> std::io::Result<()> {
                loop {
                    if wire.failed.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let queued = lock(&wire.deferred).pop_front();
                    let line = match queued {
                        Some(line) => line,
                        None => {
                            if wire.failed.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            lock(&wire.inbox).recv().map_err(|_| std::io::Error::other("MCP input disconnected without EOF"))?
                        }
                    };
                    let line = match line {
                        Input::Line(line) => line?,
                        Input::Closed | Input::OutputFailed => return Ok(()),
                    };
                    if wire.failed.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if let Some(reply) = self.handle_line(&line) {
                        wire.out.send(Some(reply)).map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "MCP output closed"))?;
                    }
                }
            })();
            self.wire = None;
            // A backend that panicked while clearing its progress hook may still hold a sender.
            let _ = wire.out.send(None);
            drop(wire);
            let written = writer.join().map_err(|_| std::io::Error::other("MCP output thread panicked")).and_then(|r| r);
            written.and(served)
        })
    }

    /// Called at photo boundaries, including completion. EOF finishes the pending export;
    /// cancellation stops before the next photo. No file scan or output deletion is needed:
    /// the existing writer replaces each whole file atomically.
    pub(super) fn progress_hook(&self) -> Option<ProgressHook> {
        let wire = self.wire.clone()?;
        let (id, token) = self.current.clone()?;
        let suppress = self.suppress.clone();
        let mut closed = false;
        let mut last = None;
        let mut reported: Option<Instant> = None;
        Some(Box::new(move |done, total, name| {
            while !closed {
                if wire.failed.load(Ordering::Acquire) {
                    suppress.store(true, Ordering::Relaxed);
                    return false;
                }
                let next = lock(&wire.inbox).try_recv();
                let line = match next {
                    Ok(Input::Line(Ok(line))) => line,
                    Ok(Input::Line(Err(error))) => {
                        lock(&wire.deferred).push_back(Input::Line(Err(error)));
                        closed = true;
                        break;
                    }
                    Ok(Input::OutputFailed) => {
                        suppress.store(true, Ordering::Relaxed);
                        return false;
                    }
                    Ok(Input::Closed) => {
                        lock(&wire.deferred).push_back(Input::Closed);
                        closed = true;
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        lock(&wire.deferred).push_back(Input::Line(Err(std::io::Error::other("MCP input disconnected without EOF"))));
                        closed = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                };
                let msg: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                match msg.get("method").and_then(Value::as_str) {
                    Some("notifications/cancelled") if msg.get("id").is_none() && msg["params"]["requestId"] == id => {
                        suppress.store(true, Ordering::Relaxed);
                    }
                    Some("ping")
                        if msg.get("id").is_some_and(|i| i.is_string() || i.is_number())
                            && msg.get("params").is_none_or(|p| p.is_null() || p.is_object()) =>
                    {
                        let _ = wire.out.send(Some(response(msg["id"].clone(), json!({})).to_string()));
                    }
                    _ => lock(&wire.deferred).push_back(Input::Line(Ok(line))),
                }
            }
            if suppress.load(Ordering::Relaxed) || wire.failed.load(Ordering::Acquire) {
                suppress.store(true, Ordering::Relaxed);
                return false;
            }
            if let Some(token) = &token
                && total > 0
                && last.is_none_or(|last| done > last)
                && (done == total || reported.is_none_or(|at| at.elapsed() >= Duration::from_millis(100)))
            {
                last = Some(done);
                reported = Some(Instant::now());
                let message = if done < total {
                    format!("Exporting {name} ({} of {total})", done.saturating_add(1))
                } else {
                    format!("Exported {total} photo(s)")
                };
                let notification = json!({"jsonrpc":"2.0","method":"notifications/progress",
                    "params":{"progressToken":token,"progress":done,"total":total,"message":message}});
                let _ = wire.out.send(Some(notification.to_string()));
            }
            true
        }))
    }
}

#[cfg(test)]
mod shutdown_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_lock_recovers_after_panic() {
        let state = Arc::new(Mutex::new(VecDeque::new()));
        let held = state.clone();
        let _ = std::thread::spawn(move || {
            held.lock().unwrap().push_back(7);
            let _guard = held.lock().unwrap();
            panic!("synthetic transport panic");
        })
        .join();
        assert_eq!(lock(&state).pop_front(), Some(7));
        lock(&state).push_back(8);
        assert_eq!(lock(&state).pop_front(), Some(8));
    }
}
