//! Control-channel matte exports keep decoding and mask evaluation off the UI thread.

use std::sync::mpsc::Sender;

use serde_json::Value;

use crate::LightcraftApp;
use crate::control::{self, Outcome};

pub(crate) const LABEL: &str = "Export Mask";

pub(crate) fn ask(app: &mut LightcraftApp, params: &Value, reply: Sender<Value>) -> Outcome {
    // One pending PNG at a time: completed results await a UI frame and must not
    // build an unbounded queue outside the worker's memory permit.
    if app.tasks.is_running(LABEL) {
        return Outcome::Done(control::failure("mask.export: an export is already running"));
    }
    let job = match lightcraft_engine::cmd::masks::prepare_matte(&mut app.session, params) {
        Ok(job) => job,
        Err(e) => return Outcome::Done(control::failure(e)),
    };
    let library = app.session.library_generation();
    let started = crate::tasks::spawn(
        app,
        LABEL,
        Some("export"),
        move || lightcraft_engine::guard::catch(LABEL, || job.run()).and_then(|r| r),
        move |app, _, result| {
            let result = if app.session.library_generation() != library {
                Err("mask.export: another library was opened before the export ended".to_string())
            } else {
                result
            };
            match result {
                Err(e) => {
                    let _ = reply.send(control::failure(e));
                }
                Ok(output) => {
                    // Snapshot the current catalog again before publishing; the
                    // filesystem write itself must not stall a frame on a slow drive.
                    let guard = app.session.original_guard();
                    let published = reply.clone();
                    let started = crate::tasks::spawn(
                        app,
                        LABEL,
                        Some("export"),
                        move || lightcraft_engine::guard::catch(LABEL, || output.publish_guarded(&guard).map_err(|e| e.to_string())).and_then(|r| r),
                        move |_, _, result| {
                            let answer = match result {
                                Ok(value) => control::success(value),
                                Err(e) => control::failure(e),
                            };
                            let _ = published.send(answer);
                        },
                    );
                    if let Err(e) = started {
                        let _ = reply.send(control::failure(e));
                    }
                }
            }
        },
    );
    match started {
        Ok(()) => Outcome::Later,
        Err(e) => Outcome::Done(control::failure(e)),
    }
}
