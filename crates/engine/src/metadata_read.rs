//! A read-only metadata request that can be moved to a UI worker.

use crate::{Session, merge::ByteReader, sidecar::SidecarLookup};
use lightcraft_catalog::{PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Private desktop subcommand used for a metadata read that the parent can terminate on timeout.
pub const WORKER_ARG: &str = "--metadata-read-worker";

type ReadResult = Result<(Value, Option<String>), String>;

#[derive(Serialize, Deserialize)]
struct WorkerInput {
    path: String,
    sidecar: SidecarLookup,
}

/// A detached request for all readable EXIF and XMP attached to one catalog photo.
pub struct Request {
    source: Source,
    read: Option<ByteReader>,
    sidecar: SidecarLookup,
}

impl Request {
    /// Capture the photo source, custom byte reader and sidecar policy from the session.
    pub fn new(session: &Session, id: PhotoId) -> Result<Self, String> {
        let photo = session.catalog.photo(id).ok_or("no such photo")?;
        Ok(Self { source: photo.source.clone(), read: session.media.file_bytes.clone(), sidecar: session.sidecar_lookup(id) })
    }

    /// Return metadata in the command API shape, including a sidecar warning when present.
    pub fn read(self) -> Result<Value, String> {
        let (mut value, warning) = self.read_with_diagnostics()?;
        if let Some(warning) = warning
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert("warning".into(), Value::String(warning));
        }
        Ok(value)
    }

    /// Return metadata and a non-fatal sidecar warning as separate values for the UI.
    pub fn read_with_diagnostics(self) -> ReadResult {
        let Source::File { path } = self.source else {
            return Ok((json!({"exif": [], "xmp": [], "note": "a generated demo photo has no file"}), None));
        };
        read_file(path, self.read, self.sidecar)
    }

    /// Read in a child process so a blocked filesystem call can be terminated at the deadline.
    /// Custom readers stay in-process because their closures cannot cross a process boundary.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_with_deadline(self, timeout: std::time::Duration) -> ReadResult {
        if self.read.is_some() {
            return self.read_with_diagnostics();
        }
        let Source::File { path } = self.source else {
            return Ok((json!({"exif": [], "xmp": [], "note": "a generated demo photo has no file"}), None));
        };
        read_in_worker(WorkerInput { path, sidecar: self.sidecar }, timeout)
    }
}

/// Read and parse the original plus its chosen XMP sidecar, if one exists.
fn read_file(path: String, read: Option<ByteReader>, sidecar: SidecarLookup) -> ReadResult {
    let bytes = match read {
        Some(read) => read(&path),
        None => std::fs::read(&path).map_err(|e| format!("{path}: {e}")),
    }?;
    let exif: Vec<Value> = lightcraft_meta::file_tag_rows(&bytes)
        .into_iter()
        .map(|row| json!({"group": row.group, "tag": row.tag, "name": row.name, "value": row.value}))
        .collect();
    let packet = match sidecar.find(&path) {
        Some(sidecar) => std::fs::read_to_string(&sidecar).map(Some).map_err(|error| format!("{}: {error}", sidecar.display())),
        None => Ok(lightcraft_meta::embedded(&bytes).xmp),
    };
    let parsed = packet.and_then(|packet| packet.map(|xmp| lightcraft_meta::parse_xmp(&xmp).map_err(|error| format!("XMP: {error}"))).transpose());
    let warning = parsed.as_ref().err().cloned();
    let xmp: Vec<Value> = parsed
        .ok()
        .flatten()
        .map(|d| {
            d.properties
                .into_iter()
                .filter(|(key, _)| !key.starts_with("lc:"))
                .map(|(key, values)| json!({"name": key, "value": values.join("; ")}))
                .collect()
        })
        .unwrap_or_default();
    Ok((json!({"exif": exif, "xmp": xmp}), warning))
}

/// Serve one request on standard input/output. The desktop app calls this before initialising UI.
#[cfg(not(target_arch = "wasm32"))]
pub fn worker_main() -> Result<(), String> {
    let input: WorkerInput = serde_json::from_reader(std::io::stdin().lock()).map_err(|error| format!("metadata worker input: {error}"))?;
    let result = read_file(input.path, None, input.sidecar);
    serde_json::to_writer(std::io::stdout().lock(), &result).map_err(|error| format!("metadata worker output: {error}"))
}

/// The task remains alive after the deadline until the killed child is confirmed stopped. This
/// preserves the read cap even on a filesystem whose kernel call is temporarily uninterruptible.
#[cfg(not(target_arch = "wasm32"))]
fn read_in_worker(input: WorkerInput, timeout: std::time::Duration) -> ReadResult {
    use std::io::{Read as _, Write as _};
    use std::process::{Command, Stdio};

    // Drop also covers failed pipe setup, writes and polling, which must not orphan a worker.
    struct Worker(std::process::Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let payload = serde_json::to_vec(&input).map_err(|error| format!("metadata worker input: {error}"))?;
    let exe = std::env::current_exe().map_err(|error| format!("metadata worker executable: {error}"))?;
    let mut worker = Worker(
        Command::new(exe)
            .arg(WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("metadata worker: {error}"))?,
    );
    let child = &mut worker.0;
    let Some(mut stdin) = child.stdin.take() else { return Err("metadata worker has no input pipe".into()) };
    stdin.write_all(&payload).map_err(|error| format!("metadata worker input: {error}"))?;
    drop(stdin);
    let Some(mut stdout) = child.stdout.take() else { return Err("metadata worker has no output pipe".into()) };
    let output = std::thread::Builder::new()
        .name("metadata-worker-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })
        .map_err(|error| format!("metadata worker output thread: {error}"))?;
    let started = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait().map_err(|error| format!("metadata worker: {error}"))? {
            Some(status) => {
                let bytes = output
                    .join()
                    .map_err(|_| "metadata worker output thread stopped".to_owned())?
                    .map_err(|error| format!("metadata worker output: {error}"))?;
                if timed_out {
                    return Err("metadata read timed out. Try Refresh.".into());
                }
                if !status.success() {
                    return Err(format!("metadata worker stopped with {status}"));
                }
                return serde_json::from_slice(&bytes).map_err(|error| format!("metadata worker output: {error}"))?;
            }
            None if !timed_out && started.elapsed() >= timeout => {
                timed_out = true;
                let _ = child.kill();
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}
