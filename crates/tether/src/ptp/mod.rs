//! Native tethering over the Picture Transfer Protocol (PTP, ISO 15740 / PIMA 15740, public
//! standard). Transports: USB still-image class ([`usb`], via `nusb`), PTP/IP over a network
//! ([`ptpip`], CIPA DC-005) and an in-process simulated camera ([`sim`]) for tests and demos.
//! [`Camera`] speaks the generic operations (session, device info, capture, object events,
//! download, delete, device properties); [`vendor`] adds what is known per maker from public
//! specifications only.
//!
//! Sources: ISO 15740 / PIMA 15740:2000 (operation, response, event, property and data type
//! codes, dataset layouts), USB Still Image Capture Device Definition 1.0 (container format),
//! CIPA DC-005 (PTP/IP packets). Own design otherwise. No GPL code was read.

pub mod camera;
pub mod data;
pub mod ptpip;
pub mod sim;
pub mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod usb;
pub mod vendor;

pub use camera::{Camera, Readout, Readouts};
pub use data::{DeviceInfo, ObjectInfo, PropDesc, PropForm, PropValue};
pub use transport::{Container, Event, Transport};
pub use vendor::Vendor;

/// The most bytes one container or object may carry (a 2 GiB raw is far beyond any camera's).
pub const MAX_DATA: usize = 1 << 31;

/// Operation codes (ISO 15740 §10.4).
pub mod op {
    pub const GET_DEVICE_INFO: u16 = 0x1001;
    pub const OPEN_SESSION: u16 = 0x1002;
    pub const CLOSE_SESSION: u16 = 0x1003;
    pub const GET_STORAGE_IDS: u16 = 0x1004;
    pub const GET_OBJECT_HANDLES: u16 = 0x1007;
    pub const GET_OBJECT_INFO: u16 = 0x1008;
    pub const GET_OBJECT: u16 = 0x1009;
    pub const DELETE_OBJECT: u16 = 0x100B;
    pub const INITIATE_CAPTURE: u16 = 0x100E;
    pub const GET_DEVICE_PROP_DESC: u16 = 0x1014;
    pub const GET_DEVICE_PROP_VALUE: u16 = 0x1015;
    pub const SET_DEVICE_PROP_VALUE: u16 = 0x1016;
}

/// Response codes (ISO 15740 §11).
pub mod rc {
    pub const OK: u16 = 0x2001;
    pub const GENERAL_ERROR: u16 = 0x2002;
    pub const SESSION_NOT_OPEN: u16 = 0x2003;
    pub const OPERATION_NOT_SUPPORTED: u16 = 0x2005;
    pub const INVALID_OBJECT_HANDLE: u16 = 0x2009;
    pub const DEVICE_PROP_NOT_SUPPORTED: u16 = 0x200A;
    pub const STORE_FULL: u16 = 0x200C;
    pub const ACCESS_DENIED: u16 = 0x200F;
    pub const DEVICE_BUSY: u16 = 0x2019;
    pub const INVALID_DEVICE_PROP_VALUE: u16 = 0x201C;
    pub const INVALID_PARAMETER: u16 = 0x201D;
    pub const SESSION_ALREADY_OPEN: u16 = 0x201E;
}

/// Event codes (ISO 15740 §12).
pub mod ev {
    pub const OBJECT_ADDED: u16 = 0x4002;
    pub const DEVICE_PROP_CHANGED: u16 = 0x4006;
    pub const STORE_FULL: u16 = 0x400A;
    pub const CAPTURE_COMPLETE: u16 = 0x400D;
}

/// Device property codes (ISO 15740 §13).
pub mod prop {
    pub const BATTERY_LEVEL: u16 = 0x5001;
    pub const WHITE_BALANCE: u16 = 0x5005;
    /// f-number × 100.
    pub const F_NUMBER: u16 = 0x5007;
    /// Seconds × 10 000.
    pub const EXPOSURE_TIME: u16 = 0x500D;
    pub const EXPOSURE_PROGRAM_MODE: u16 = 0x500E;
    /// ISO speed.
    pub const EXPOSURE_INDEX: u16 = 0x500F;
    /// EV × 1000 (signed).
    pub const EXPOSURE_BIAS: u16 = 0x5010;
}

/// Data type codes (ISO 15740 §5.3).
pub mod dt {
    pub const INT8: u16 = 0x0001;
    pub const UINT8: u16 = 0x0002;
    pub const INT16: u16 = 0x0003;
    pub const UINT16: u16 = 0x0004;
    pub const INT32: u16 = 0x0005;
    pub const UINT32: u16 = 0x0006;
    pub const INT64: u16 = 0x0007;
    pub const UINT64: u16 = 0x0008;
    pub const STR: u16 = 0xFFFF;
}

/// The ObjectFormat code of an association (folder).
pub const FORMAT_ASSOCIATION: u16 = 0x3001;

/// What went wrong talking to a camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtpError {
    /// The camera answered with a response code other than OK.
    Response(u16),
    /// The bytes did not follow the protocol.
    Protocol(String),
    /// The link failed (USB, network).
    Io(String),
    /// No answer in time.
    Timeout,
    /// The camera or this transport can't do it.
    Unsupported(String),
}

impl std::fmt::Display for PtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtpError::Response(c) => write!(f, "the camera refused ({})", response_name(*c)),
            PtpError::Protocol(m) => write!(f, "protocol error: {m}"),
            PtpError::Io(m) => write!(f, "connection error: {m}"),
            PtpError::Timeout => write!(f, "the camera did not answer in time"),
            PtpError::Unsupported(m) => write!(f, "not supported: {m}"),
        }
    }
}

impl std::error::Error for PtpError {}

impl From<std::io::Error> for PtpError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => PtpError::Timeout,
            _ => PtpError::Io(e.to_string()),
        }
    }
}

/// A readable name for a response code.
pub fn response_name(code: u16) -> String {
    let n = match code {
        rc::OK => "OK",
        rc::GENERAL_ERROR => "general error",
        rc::SESSION_NOT_OPEN => "session not open",
        rc::OPERATION_NOT_SUPPORTED => "operation not supported",
        rc::INVALID_OBJECT_HANDLE => "invalid object",
        rc::DEVICE_PROP_NOT_SUPPORTED => "property not supported",
        rc::STORE_FULL => "card full",
        rc::ACCESS_DENIED => "access denied",
        rc::DEVICE_BUSY => "camera busy",
        rc::INVALID_DEVICE_PROP_VALUE => "value not allowed",
        rc::INVALID_PARAMETER => "invalid parameter",
        rc::SESSION_ALREADY_OPEN => "session already open",
        _ => return format!("0x{code:04X}"),
    };
    n.to_string()
}

#[cfg(test)]
mod tests;
