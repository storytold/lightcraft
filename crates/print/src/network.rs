//! Network printing over WS-Print and IPP on craft-print's `wsd` and `ipp` backends. Cargo feature
//! `network-print`; not built for wasm32.
//!
//! GENERIC: apart from the document the app hands over, this module is the same for every Craft app
//! (it follows `pdfcraft-print`'s `network` module) and is meant to move into craft-libs.
//!
//! The protocol, the print ticket and the job classification are craft-print's. What is here:
//!
//! * [`Station`]: one printer the person typed, behind a private [`craft_print::Registry`] that holds both
//!   network backends; the address picks the backend ([`protocol_of`]). It asks what the printer is
//!   ([`Station::wsd_info`], [`Station::ipp_info`]; reads only) and makes the tracked print
//!   ([`Station::job`]).
//! * **One press of Print is one [`PrintJob`]**, craft-print's lifecycle record, advanced by the caller
//!   (`advance(station.registry())`): it sends, follows, retries what is safe to retry, refuses to retry what
//!   could print twice and reconciles when the outcome of a send is unknown. Nothing here sends a document
//!   twice on its own.
//! * **Formats and conversion are the registry's.** A document in a format the printer lists is sent as it is;
//!   anything else goes through the app's [`DocumentConverter`] ([`Station::set_converter`]), or is
//!   [`ErrorKind::UnsupportedFormat`] before a job exists.
//!
//! WS-Eventing push for WS-Print is opt-in ([`events_from_env`], `LIGHTCRAFT_TEST_EVENTS_BIND`, or the
//! legacy `PDFCRAFT_TEST_EVENTS_BIND` the test launchers set).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

pub use craft_print::ipp;
use craft_print::ipp::{IppBackend, PrinterAttrs, PrinterUri};
use craft_print::namespaced_id;
pub use craft_print::wsd;
use craft_print::wsd::error::Op;
use craft_print::wsd::events::NotifyConfig;
use craft_print::wsd::{Client, PrinterElements, PrinterUrl, WsdBackend};
pub use craft_print::{
    AttemptError, BytesSource, CancelToken, ColourMode, Conversion, ConversionPolicy, ConvertContext, Document, DocumentConverter, DocumentFormat,
    Duplex, ErrorKind, Job, JobHint, JobId, JobKind, JobOptions, JobState, JobView, LifecycleState, Orientation, Outcome as PrintOutcome,
    OwnedDocument, Phase, PrintError, PrintJob, PrintJobConfig, PrinterDetails, Progress, Quality, Reconciled as TrackedReconciled, Registry,
    Resolution, RetryAdvice, RetrySafety, Scaling, SimpleState, Snapshot, StaleReason, Staleness, UserConfirmed, pwg,
};

/// A transport shared with worker threads: the plain-HTTP one of craft-wsd, or an in-memory fake.
pub type Transport = Arc<dyn wsd::adapter::Transport + Send + Sync>;

/// Which protocol a typed address names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// WS-Print (the default).
    Wsd,
    /// IPP (`ipp://`, `ipps://`, or an `http://` address whose path has an `/ipp/` segment).
    Ipp,
}

impl Protocol {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Wsd => "WS-Print",
            Self::Ipp => "IPP",
        }
    }
}

/// Choose the protocol by scheme and path: `ipp:`/`ipps:` is IPP, an `http:` path containing an
/// `/ipp/` segment (or ending in `/ipp`) is IPP, everything else is WS-Print.
pub fn protocol_of(address: &str) -> Protocol {
    let text = address.trim().to_ascii_lowercase();
    if text.starts_with("ipp://") || text.starts_with("ipps://") {
        return Protocol::Ipp;
    }
    let rest = text.split_once("://").map_or(text.as_str(), |(_, rest)| rest);
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    if path.contains("/ipp/") || path.ends_with("/ipp") { Protocol::Ipp } else { Protocol::Wsd }
}

/// Check an address against the protocol's own URL policy (a numeric loopback, link-local or private
/// address; no TLS for IPP).
pub fn validate(address: &str) -> Result<Protocol, PrintError> {
    let protocol = protocol_of(address);
    match protocol {
        Protocol::Wsd => PrinterUrl::parse(address).map(|_| ()).map_err(PrintError::from)?,
        Protocol::Ipp => PrinterUri::parse(address).map(|_| ()).map_err(PrintError::from)?,
    }
    Ok(protocol)
}

/// The WS-Eventing listener the test environment asks for: `LIGHTCRAFT_TEST_EVENTS_BIND`, else the legacy
/// `PDFCRAFT_TEST_EVENTS_BIND` (for instance `127.0.0.1:0`). `None` when neither is set or both are empty. A
/// bind address that cannot be advertised (unspecified) is an error.
pub fn events_from_env() -> Option<Result<NotifyConfig, PrintError>> {
    let text = std::env::var("LIGHTCRAFT_TEST_EVENTS_BIND").or_else(|_| std::env::var("PDFCRAFT_TEST_EVENTS_BIND")).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(events_config(text))
}

/// [`events_from_env`] for a given text (`ip:port`). The advertised address is the bind address (with port 0,
/// the port the system picked).
pub fn events_config(bind: &str) -> Result<NotifyConfig, PrintError> {
    let addr: SocketAddr =
        bind.parse().map_err(|e| PrintError::new(ErrorKind::InvalidRequest, format!("the events bind address {bind:?} is not ip:port: {e}")))?;
    if addr.ip().is_unspecified() {
        return Err(PrintError::new(ErrorKind::InvalidRequest, "the events bind address must name an address the printer can call, not 0.0.0.0"));
    }
    let advertise = format!("http://{}:{}/", addr.ip(), if addr.port() == 0 { 1 } else { addr.port() });
    NotifyConfig::new(addr, &advertise)
}

/// One network printer behind a registry that holds both network backends.
pub struct Station {
    protocol: Protocol,
    address: String,
    id: String,
    registry: Registry,
    transport: Transport,
    wsd: WsdBackend,
    events: bool,
    /// What the printer calls itself, once a query has read it; for the words shown to the person.
    display_name: Mutex<String>,
}

impl Station {
    /// Open the printer at `address` (no connection is made). `events`: the opt-in WS-Eventing listener for a
    /// WS-Print printer (ignored for IPP, which polls).
    pub fn open(address: &str, transport: Option<Transport>, events: Option<NotifyConfig>) -> Result<Self, PrintError> {
        let protocol = validate(address)?;
        let transport = transport.unwrap_or_else(wsd::adapter::default_transport);
        let mut backend = WsdBackend::new().with_transport(Arc::clone(&transport));
        let with_events = events.is_some() && protocol == Protocol::Wsd;
        if let Some(config) = events.filter(|_| protocol == Protocol::Wsd) {
            backend = backend.with_events(config)?;
        }
        let ipp_backend = IppBackend::new().with_transport(Arc::clone(&transport));
        let (native, name) = match protocol {
            Protocol::Wsd => (backend.add(address)?, "wsd"),
            Protocol::Ipp => (ipp_backend.add(address)?, "ipp"),
        };
        let registry = Registry::new();
        registry.register(Box::new(backend.clone())).map_err(|e| PrintError::new(ErrorKind::Failed, e.to_string()))?;
        registry.register(Box::new(ipp_backend)).map_err(|e| PrintError::new(ErrorKind::Failed, e.to_string()))?;
        Ok(Self {
            protocol,
            id: namespaced_id(name, &native),
            address: native,
            registry,
            transport,
            wsd: backend,
            events: with_events,
            display_name: Mutex::default(),
        })
    }

    /// Use `converter` for a document the printer does not list.
    pub fn set_converter(&self, converter: Arc<dyn DocumentConverter>) {
        self.registry.set_converter(converter);
    }

    /// The registry's choices around conversion (target formats in order, the resolution to approach).
    pub fn set_conversion_policy(&self, policy: ConversionPolicy) {
        self.registry.set_conversion_policy(policy);
    }

    /// Remember the name the printer reported (shown in status lines instead of its address).
    pub fn set_display_name(&self, name: &str) {
        *self.display_name.lock().unwrap_or_else(PoisonError::into_inner) = name.trim().to_owned();
    }

    /// The name the printer reported, or empty when no query has read one.
    pub fn display_name(&self) -> String {
        self.display_name.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// The canonical address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The registry's id of the printer (`wsd:<url>` or `ipp:<uri>`).
    pub fn printer_id(&self) -> &str {
        &self.id
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Is the WS-Eventing listener on for this printer?
    pub fn events_enabled(&self) -> bool {
        self.events
    }

    /// The listener and its subscription as plain text for a status line, when it is on.
    pub fn events_summary(&self) -> Option<String> {
        self.wsd.events_status().map(|s| format!("{s:?}"))
    }

    /// WS-Print: what the printer is and lists (`GetPrinterElements`). Sends no job.
    pub fn wsd_info(&self) -> Result<PrinterElements, PrintError> {
        let url = PrinterUrl::parse(&self.address).map_err(PrintError::from)?;
        let elements = wsd::printer_info(&Client::new(Arc::clone(&self.transport), url)).map_err(|e| e.print_error(Op::Query))?;
        Ok(elements)
    }

    /// IPP: the printer's attributes (`Get-Printer-Attributes`). Sends no job.
    pub fn ipp_info(&self) -> Result<PrinterAttrs, PrintError> {
        let uri = PrinterUri::parse(&self.address).map_err(PrintError::from)?;
        let attrs = ipp::Client::new(Arc::clone(&self.transport), uri).get_printer_attributes(&[]).map_err(PrintError::from)?;
        Ok(attrs)
    }

    /// A tracked print of the print-ready `pdf`: one object for the whole print, advanced by the caller.
    /// `job.printer` is set to this printer; `consent` is the person's ticked box.
    pub fn job(&self, pdf: Vec<u8>, job: Job, options: JobOptions, consent: UserConfirmed) -> PrintJob {
        let job = Job { printer: Some(self.id.clone()), ..job };
        PrintJob::new(job, BytesSource::new(DocumentFormat::Pdf, pdf))
            .with_options(options)
            .with_consent(consent)
            .with_config(PrintJobConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_is_chosen_by_scheme_and_path() {
        for (address, expected) in [
            ("ipp://127.0.0.1:3921/ipp/print", Protocol::Ipp),
            ("ipps://127.0.0.1/ipp/print", Protocol::Ipp),
            ("http://127.0.0.1:3921/d/inkjet1/ipp/print", Protocol::Ipp),
            ("http://127.0.0.1:3921/ipp", Protocol::Ipp),
            ("HTTP://127.0.0.1:3921/IPP/print", Protocol::Ipp),
            ("http://127.0.0.1:3921/d/inkjet1/WebServices/PrinterService", Protocol::Wsd),
            ("http://192.168.1.50:5357/service", Protocol::Wsd),
            ("127.0.0.1:3921/x", Protocol::Wsd),
        ] {
            assert_eq!(protocol_of(address), expected, "{address}");
        }
    }

    #[test]
    fn both_urls_validate_and_open_without_connecting() {
        assert_eq!(validate("http://127.0.0.1:3921/d/inkjet1/WebServices/PrinterService").ok(), Some(Protocol::Wsd));
        assert_eq!(validate("ipp://127.0.0.1:3921/ipp/print").ok(), Some(Protocol::Ipp));
        assert!(validate("ipp://printer.example/ipp/print").is_err(), "host names are refused");
        let ipp = Station::open("ipp://127.0.0.1:3921/ipp/print", None, None).expect("ipp");
        assert!(ipp.printer_id().starts_with("ipp:"));
        let wsd = Station::open("http://127.0.0.1:3921/d/inkjet1/WebServices/PrinterService", None, None).expect("wsd");
        assert!(wsd.printer_id().starts_with("wsd:"));
        assert!(!wsd.events_enabled());
    }

    #[test]
    fn events_config_needs_a_callable_address() {
        assert!(events_config("127.0.0.1:0").is_ok());
        assert!(events_config("0.0.0.0:9000").is_err());
        assert!(events_config("nope").is_err());
    }
}
