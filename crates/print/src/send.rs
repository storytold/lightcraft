//! Sending a print to an IPP printer: pick the format the printer takes (PDF, else one JPEG job
//! per page), submit with the copy count, then follow the job until it finishes or `wait` runs out.
//!
//! Sources: RFC 8011 (Print-Job, Get-Job-Attributes, job states). Own design.

use std::time::{Duration, Instant};

use lcc_layout::Document;
use lcc_layout::render::PhotoSource;

use crate::ipp::{self, JobState};
use crate::output::{PdfOptions, to_jpeg_icc, to_pdf};
use crate::text::ShapedText;
use crate::{PrintError, PrintSettings, Result};

/// How a job went to the printer.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    /// `application/pdf` or `image/jpeg`.
    pub format: String,
    /// One job id per submitted document.
    pub jobs: Vec<i32>,
    pub pages: usize,
    pub copies: u32,
    /// The last job's state when the wait ended (`None` when the printer did not say).
    pub state: Option<JobState>,
    pub warnings: Vec<String>,
}

/// Who sends, and how long to follow the job.
#[derive(Clone, Debug)]
pub struct SendOptions {
    pub user: String,
    pub title: String,
    pub pdf: PdfOptions,
    /// How long to poll the job state after submitting (zero: don't poll).
    pub wait: Duration,
    pub poll: Duration,
}

/// Renders `doc` and prints it on `uri`.
///
/// The printer is asked which formats it takes; when it can't be asked, PDF is sent. A job the
/// printer aborts or cancels is an error.
pub fn send_to_printer(
    uri: &str,
    doc: &Document,
    settings: &PrintSettings,
    photos: &dyn PhotoSource,
    text: &ShapedText,
    opt: &SendOptions,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<Sent> {
    ipp::IppUri::parse(uri)?;
    let copies = settings.job.copies.clamp(1, 999);
    let mut warnings = Vec::new();
    let formats = match ipp::printer_info(uri) {
        Ok(info) => Some(info),
        Err(e) => {
            warnings.push(format!("could not ask the printer for its formats ({e}); sending PDF"));
            None
        }
    };
    let pdf_ok = formats.as_ref().is_none_or(|i| i.accepts("application/pdf"));
    let jpeg_ok = formats.as_ref().is_some_and(|i| i.accepts("image/jpeg"));
    let (format, files, pages) = if pdf_ok {
        let out = to_pdf(doc, settings, photos, text, &opt.pdf, progress)?;
        warnings.extend(out.warnings);
        ("application/pdf", out.files, out.pages)
    } else if jpeg_ok {
        let out = to_jpeg_icc(doc, settings, photos, text, opt.pdf.photo_profile.as_deref().map(Vec::as_slice), progress)?;
        warnings.extend(out.warnings);
        ("image/jpeg", out.files, out.pages)
    } else {
        let list = formats.map(|i| i.formats.join(", ")).unwrap_or_default();
        return Err(PrintError::Ipp(format!("the printer takes neither PDF nor JPEG (it takes: {list})")));
    };
    // the printer's own options, less those it says it doesn't offer
    let driver = match &formats {
        Some(info) => {
            let (d, w) = settings.job.driver.supported_by(&info.options);
            warnings.extend(w);
            d
        }
        None => settings.job.driver.clone(),
    };
    let mut jobs = Vec::new();
    let n = files.len();
    for (i, bytes) in files.iter().enumerate() {
        let name = if n > 1 { format!("{} ({}/{n})", opt.title, i + 1) } else { opt.title.clone() };
        jobs.push(ipp::print_job_with(uri, &opt.user, &name, format, copies, &driver, bytes)?);
    }
    let state = match jobs.last() {
        Some(&id) if id > 0 && !opt.wait.is_zero() => follow(uri, &opt.user, id, opt.wait, opt.poll),
        _ => None,
    };
    if let Some(s) = &state
        && (s.state == JobState::ABORTED || s.state == JobState::CANCELED)
    {
        let why = if s.message.is_empty() { s.reasons.join(", ") } else { s.message.clone() };
        return Err(PrintError::Ipp(format!("the printer {} the job: {why}", s.label())));
    }
    Ok(Sent { format: format.to_string(), jobs, pages, copies, state, warnings })
}

/// Polls the job state until it is final or `wait` passes; `None` when the printer won't say.
fn follow(uri: &str, user: &str, id: i32, wait: Duration, poll: Duration) -> Option<JobState> {
    let until = Instant::now() + wait;
    let mut last = None;
    loop {
        match ipp::job_state(uri, user, id) {
            Ok(s) => {
                let done = s.is_final();
                last = Some(s);
                if done {
                    return last;
                }
            }
            Err(_) => return last,
        }
        if Instant::now() >= until {
            return last;
        }
        std::thread::sleep(poll.min(until.saturating_duration_since(Instant::now())));
    }
}
