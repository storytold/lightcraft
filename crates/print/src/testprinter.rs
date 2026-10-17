//! A virtual IPP printer on localhost, for tests and CI: it answers Get-Printer-Attributes,
//! Print-Job and Get-Job-Attributes like an IPP Everywhere printer and keeps every document it
//! receives, so a whole print (page setup from the printer's paper sizes, job submission, job
//! state) runs in `cargo test` without CUPS. A real CUPS queue is exercised by the CUPS workflow
//! (`docs/print.md` → CUPS test printer).
//!
//! It never panics: a malformed request gets an IPP error status, or the connection is closed.
//!
//! Sources: RFC 8010 / RFC 8011 (server side of the same operations as [`crate::ipp`]),
//! PWG 5100.13 (media-col-database). Own design.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::ipp::*;
use crate::{PrintError, Result};

/// Largest document the test printer accepts.
const MAX_DOCUMENT: usize = 512 * 1024 * 1024;

/// A paper size the test printer offers: PWG name, size and margins in hundredths of mm
/// (top, right, bottom, left).
#[derive(Clone, Debug, PartialEq)]
pub struct TestMedia {
    pub name: String,
    pub size: [i32; 2],
    pub margins: [i32; 4],
}

/// What the test printer reports.
#[derive(Clone, Debug, PartialEq)]
pub struct TestPrinterConfig {
    pub name: String,
    pub formats: Vec<String>,
    pub media: Vec<TestMedia>,
    /// The job state reported after a job arrives (9 = completed, 8 = aborted…).
    pub final_state: i32,
    /// Paper types, trays, qualities and two-sided modes it offers (the first of each is its
    /// default), like a photo inkjet.
    pub media_types: Vec<String>,
    pub media_sources: Vec<String>,
    pub qualities: Vec<i32>,
    pub sides: Vec<String>,
}

impl Default for TestPrinterConfig {
    fn default() -> Self {
        TestPrinterConfig {
            name: "Test".into(),
            formats: vec!["application/pdf".into(), "image/jpeg".into()],
            media: vec![
                TestMedia { name: "na_letter_8.5x11in".into(), size: [21590, 27940], margins: [423, 423, 423, 423] },
                TestMedia { name: "iso_a4_210x297mm".into(), size: [21000, 29700], margins: [500, 300, 500, 300] },
            ],
            final_state: JobState::COMPLETED,
            media_types: vec!["stationery".into(), "photographic-glossy".into(), "photographic-matte".into()],
            media_sources: vec!["auto".into(), "main".into(), "photo".into(), "manual".into()],
            qualities: vec![4, 3, 5],
            sides: vec!["one-sided".into(), "two-sided-long-edge".into(), "two-sided-short-edge".into()],
        }
    }
}

/// A job the test printer received.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceivedJob {
    pub id: i32,
    pub name: String,
    pub user: String,
    pub format: String,
    pub copies: i32,
    /// The printer options the job asked for (`media-col` members, `print-quality`, `sides`).
    pub media_type: Option<String>,
    pub media_source: Option<String>,
    pub print_quality: Option<i32>,
    pub sides: Option<String>,
    pub document: Vec<u8>,
}

/// A running virtual IPP printer; stops when dropped.
pub struct TestPrinter {
    uri: String,
    port: u16,
    jobs: Arc<Mutex<Vec<ReceivedJob>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestPrinter {
    /// Starts a printer on `127.0.0.1` at a free port.
    pub fn start(config: TestPrinterConfig) -> Result<TestPrinter> {
        let l = TcpListener::bind("127.0.0.1:0").map_err(|e| PrintError::Ipp(format!("test printer: {e}")))?;
        let port = l.local_addr().map_err(|e| PrintError::Ipp(format!("test printer: {e}")))?.port();
        let uri = format!("ipp://127.0.0.1:{port}/printers/{}", config.name);
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (j, st, u) = (jobs.clone(), stop.clone(), uri.clone());
        let thread = std::thread::Builder::new()
            .name("ipp-test-printer".into())
            .spawn(move || {
                for conn in l.incoming() {
                    if st.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(s) = conn {
                        let _ = serve(s, &config, &u, &j);
                    }
                }
            })
            .map_err(|e| PrintError::Ipp(format!("test printer: {e}")))?;
        Ok(TestPrinter { uri, port, jobs, stop, thread: Some(thread) })
    }

    /// The printer's `ipp://` URI.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The jobs received so far.
    pub fn jobs(&self) -> Vec<ReceivedJob> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for TestPrinter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Reads one HTTP request, answers it, closes.
fn serve(mut s: TcpStream, config: &TestPrinterConfig, uri: &str, jobs: &Mutex<Vec<ReceivedJob>>) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let (end, len) = loop {
        let n = s.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        got.extend_from_slice(buf.get(..n).unwrap_or(&[]));
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(got.get(..end).unwrap_or(&[])).to_string();
            let len = head
                .lines()
                .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().to_string()))
                .and_then(|v| v.parse::<usize>().ok());
            match len {
                Some(len) if len <= MAX_DOCUMENT => break (end, len),
                _ => return reply_http(&mut s, "411 Length Required", &[]),
            }
        }
        if got.len() > 64 * 1024 && !got.windows(4).any(|w| w == b"\r\n\r\n") {
            return reply_http(&mut s, "400 Bad Request", &[]);
        }
    };
    let want = end.saturating_add(4).saturating_add(len);
    while got.len() < want {
        let n = s.read(&mut buf)?;
        if n == 0 {
            break;
        }
        got.extend_from_slice(buf.get(..n).unwrap_or(&[]));
    }
    let body = got.get(end.saturating_add(4)..want.min(got.len())).unwrap_or(&[]);
    let resp = answer(body, config, uri, jobs);
    reply_http(&mut s, "200 OK", &resp.encode())
}

fn reply_http(s: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    s.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes(),
    )?;
    s.write_all(body)?;
    s.flush()
}

fn response(status: u16, request_id: u32) -> Message {
    let mut m = Message::request(0, request_id, "");
    m.code = status;
    // a response's operation group carries charset and language only
    if let Some(g) = m.groups.first_mut() {
        g.attrs.retain(|a| a.name != "printer-uri");
    }
    m
}

fn attr(tag: u8, name: &str, values: Vec<Value>) -> Attribute {
    Attribute { tag, name: name.into(), values }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

/// The IPP response to one request body.
fn answer(body: &[u8], config: &TestPrinterConfig, uri: &str, jobs: &Mutex<Vec<ReceivedJob>>) -> Message {
    let Ok((req, at)) = Message::decode_prefix(body) else {
        return response(0x0400, 0); // client-error-bad-request
    };
    let id = req.request_id;
    let op_str = |n: &str| req.attr(TAG_OPERATION, n).and_then(|a| a.values.first()).and_then(Value::as_str).map(str::to_string);
    match req.code {
        OP_GET_PRINTER_ATTRIBUTES => {
            let hmm = |v: i32| Value::Int(v);
            let cols = config
                .media
                .iter()
                .map(|m| {
                    Value::Collection(vec![
                        ("media-key".into(), vec![text(&m.name)]),
                        (
                            "media-size".into(),
                            vec![Value::Collection(vec![("x-dimension".into(), vec![hmm(m.size[0])]), ("y-dimension".into(), vec![hmm(m.size[1])])])],
                        ),
                        ("media-top-margin".into(), vec![hmm(m.margins[0])]),
                        ("media-right-margin".into(), vec![hmm(m.margins[1])]),
                        ("media-bottom-margin".into(), vec![hmm(m.margins[2])]),
                        ("media-left-margin".into(), vec![hmm(m.margins[3])]),
                    ])
                })
                .collect();
            let mut m = response(0, id);
            m.groups.push(Group {
                tag: TAG_PRINTER,
                attrs: vec![
                    attr(V_NAME_WO_LANG, "printer-name", vec![text(&config.name)]),
                    attr(V_URI, "printer-uri-supported", vec![text(uri)]),
                    attr(V_ENUM, "printer-state", vec![Value::Int(3)]),
                    attr(V_MIME, "document-format-supported", config.formats.iter().map(|f| text(f)).collect()),
                    attr(V_KEYWORD, "media-supported", config.media.iter().map(|m| text(&m.name)).collect()),
                    attr(V_KEYWORD, "media-default", config.media.first().map(|m| vec![text(&m.name)]).unwrap_or_else(|| vec![text("none")])),
                    attr(V_BEG_COLLECTION, "media-col-database", cols),
                    attr(V_KEYWORD, "media-type-supported", config.media_types.iter().map(|m| text(m)).collect()),
                    attr(V_KEYWORD, "media-source-supported", config.media_sources.iter().map(|m| text(m)).collect()),
                    attr(
                        V_BEG_COLLECTION,
                        "media-col-default",
                        vec![Value::Collection(
                            [("media-type", config.media_types.first()), ("media-source", config.media_sources.first())]
                                .into_iter()
                                .filter_map(|(k, v)| v.map(|v| (k.to_string(), vec![text(v)])))
                                .collect(),
                        )],
                    ),
                    attr(V_ENUM, "print-quality-supported", config.qualities.iter().map(|q| Value::Int(*q)).collect()),
                    attr(V_ENUM, "print-quality-default", config.qualities.first().map(|q| vec![Value::Int(*q)]).unwrap_or_default()),
                    attr(V_KEYWORD, "sides-supported", config.sides.iter().map(|m| text(m)).collect()),
                    attr(V_KEYWORD, "sides-default", config.sides.first().map(|m| vec![text(m)]).unwrap_or_default()),
                ],
            });
            m
        }
        OP_PRINT_JOB => {
            let format = op_str("document-format").unwrap_or_else(|| "application/octet-stream".into());
            if !config.formats.iter().any(|f| f.eq_ignore_ascii_case(&format)) {
                return response(0x040A, id); // client-error-document-format-not-supported
            }
            let document = body.get(at..).unwrap_or(&[]).to_vec();
            if document.is_empty() {
                return response(0x0400, id);
            }
            let copies = req.attr(TAG_JOB, "copies").and_then(|a| a.values.first()).and_then(Value::as_int).unwrap_or(1);
            let job_str = |n: &str| req.attr(TAG_JOB, n).and_then(|a| a.values.first()).and_then(Value::as_str).map(str::to_string);
            let col = req.attr(TAG_JOB, "media-col").and_then(|a| a.values.first());
            let col_str = |n: &str| col.and_then(|c| c.member(n)).and_then(Value::as_str).map(str::to_string);
            let mut list = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            let job_id = i32::try_from(list.len()).unwrap_or(i32::MAX - 1).saturating_add(1);
            list.push(ReceivedJob {
                id: job_id,
                name: op_str("job-name").unwrap_or_default(),
                user: op_str("requesting-user-name").unwrap_or_default(),
                format,
                copies,
                media_type: col_str("media-type"),
                media_source: col_str("media-source"),
                print_quality: req.attr(TAG_JOB, "print-quality").and_then(|a| a.values.first()).and_then(Value::as_int),
                sides: job_str("sides"),
                document,
            });
            let mut m = response(0, id);
            m.groups.push(Group {
                tag: TAG_JOB,
                attrs: vec![
                    attr(V_URI, "job-uri", vec![text(&format!("{uri}/{job_id}"))]),
                    attr(V_INTEGER, "job-id", vec![Value::Int(job_id)]),
                    attr(V_ENUM, "job-state", vec![Value::Int(JobState::PENDING)]),
                ],
            });
            m
        }
        OP_GET_JOB_ATTRIBUTES => {
            let want = req.attr(TAG_OPERATION, "job-id").and_then(|a| a.values.first()).and_then(Value::as_int);
            let known = want.is_some_and(|w| jobs.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|j| j.id == w));
            if !known {
                return response(0x0406, id); // client-error-not-found
            }
            let reason = if config.final_state == JobState::COMPLETED { "job-completed-successfully" } else { "aborted-by-system" };
            let mut m = response(0, id);
            m.groups.push(Group {
                tag: TAG_JOB,
                attrs: vec![
                    attr(V_ENUM, "job-state", vec![Value::Int(config.final_state)]),
                    attr(V_KEYWORD, "job-state-reasons", vec![text(reason)]),
                ],
            });
            m
        }
        _ => response(0x0501, id), // server-error-operation-not-supported
    }
}
