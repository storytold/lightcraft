//! Stdio progress and cancellation between photos. The backend keeps its session on the
//! serving thread; only stdin and stdout move to transport threads. Other calls wait in order.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Server, response};
use crate::backend::ProgressHook;

type Input = Option<std::io::Result<String>>;

#[derive(Clone)]
pub(super) struct Wire {
    inbox: Arc<Mutex<Receiver<Input>>>,
    out: Sender<String>,
    deferred: Arc<Mutex<VecDeque<Input>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Server {
    pub(super) fn serve_wire(&mut self, input: impl BufRead + Send, output: impl Write + Send) -> std::io::Result<()> {
        std::thread::scope(|scope| {
            let (in_tx, in_rx) = std::sync::mpsc::channel::<Input>();
            let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
            let wire = Wire { inbox: Arc::new(Mutex::new(in_rx)), out: out_tx, deferred: Arc::new(Mutex::new(VecDeque::new())) };
            let writer = std::thread::Builder::new().name("mcp-output".into()).spawn_scoped(scope, move || -> std::io::Result<()> {
                let mut output = output;
                for line in out_rx {
                    output.write_all(line.as_bytes())?;
                    output.write_all(b"\n")?;
                    output.flush()?;
                }
                Ok(())
            })?;
            let reader = match std::thread::Builder::new().name("mcp-input".into()).spawn_scoped(scope, move || {
                for line in input.lines() {
                    let failed = line.is_err();
                    if in_tx.send(Some(line)).is_err() || failed {
                        return;
                    }
                }
                let _ = in_tx.send(None);
            }) {
                Ok(reader) => reader,
                Err(error) => {
                    drop(wire);
                    let _ = writer.join();
                    return Err(error);
                }
            };
            self.wire = Some(wire.clone());
            let served = (|| -> std::io::Result<()> {
                loop {
                    let queued = lock(&wire.deferred).pop_front();
                    let line = match queued {
                        Some(line) => line,
                        None => lock(&wire.inbox).recv().ok().flatten(),
                    };
                    let Some(line) = line else { return Ok(()) };
                    if let Some(reply) = self.handle_line(&line?) {
                        wire.out.send(reply).map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "MCP output closed"))?;
                    }
                }
            })();
            self.wire = None;
            drop(wire);
            let written = writer.join().map_err(|_| std::io::Error::other("MCP output thread panicked")).and_then(|r| r);
            let read = reader.join().map_err(|_| std::io::Error::other("MCP input thread panicked"));
            served.and(written).and(read)
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
                let next = lock(&wire.inbox).try_recv();
                let line = match next {
                    Ok(Some(Ok(line))) => line,
                    Ok(Some(Err(error))) => {
                        lock(&wire.deferred).push_back(Some(Err(error)));
                        closed = true;
                        break;
                    }
                    Ok(None) | Err(TryRecvError::Disconnected) => {
                        lock(&wire.deferred).push_back(None);
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
                        let _ = wire.out.send(response(msg["id"].clone(), json!({})).to_string());
                    }
                    _ => lock(&wire.deferred).push_back(Some(Ok(line))),
                }
            }
            if suppress.load(Ordering::Relaxed) {
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
                let _ = wire.out.send(notification.to_string());
            }
            true
        }))
    }
}

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
