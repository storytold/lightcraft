//! The transaction layer every link implements, and the USB container framing (USB Still Image
//! Capture Device Definition §7: length, type, code, transaction id, payload) with a transport
//! built on any bulk pipe ([`BulkPipe`]): the real USB device and the simulated one share it.

use std::time::Duration;

use super::{MAX_DATA, PtpError};

/// An event from the camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub code: u16,
    pub params: Vec<u32>,
}

/// The camera's answer to one operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub code: u16,
    pub params: Vec<u32>,
}

/// One link to a camera: runs whole transactions and delivers events.
pub trait Transport: Send {
    /// Run operation `code` with `params` (transaction `tid`), sending `data_out` in the data
    /// phase when given; returns the response and the data the camera sent (empty when none).
    fn transact(&mut self, code: u16, tid: u32, params: &[u32], data_out: Option<&[u8]>) -> Result<(Response, Vec<u8>), PtpError>;
    /// The next event, waiting at most `timeout` (`None`: none came).
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>, PtpError>;
    /// A short description ("USB 3-4", "PTP/IP 192.168.1.20", "simulated").
    fn describe(&self) -> String;
}

/// USB container types.
pub mod kind {
    pub const COMMAND: u16 = 1;
    pub const DATA: u16 = 2;
    pub const RESPONSE: u16 = 3;
    pub const EVENT: u16 = 4;
}

/// The size of a container header.
pub const HEADER: usize = 12;

/// A USB PTP container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub kind: u16,
    pub code: u16,
    pub tid: u32,
    pub payload: Vec<u8>,
}

impl Container {
    pub fn with_params(kind: u16, code: u16, tid: u32, params: &[u32]) -> Container {
        Container { kind, code, tid, payload: params.iter().take(5).flat_map(|p| p.to_le_bytes()).collect() }
    }
    pub fn encode(&self) -> Vec<u8> {
        let len = u32::try_from(HEADER.saturating_add(self.payload.len())).unwrap_or(u32::MAX);
        let mut b = Vec::with_capacity(HEADER + self.payload.len());
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&self.kind.to_le_bytes());
        b.extend_from_slice(&self.code.to_le_bytes());
        b.extend_from_slice(&self.tid.to_le_bytes());
        b.extend_from_slice(&self.payload);
        b
    }
    /// The header of `b`: (declared total length, kind, code, tid).
    pub fn header(b: &[u8]) -> Result<(usize, u16, u16, u32), PtpError> {
        let h = b.get(..HEADER).ok_or_else(|| PtpError::Protocol("container shorter than its header".into()))?;
        let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
        if !(HEADER..=MAX_DATA).contains(&len) {
            return Err(PtpError::Protocol(format!("container length {len}")));
        }
        Ok((len, u16::from_le_bytes([h[4], h[5]]), u16::from_le_bytes([h[6], h[7]]), u32::from_le_bytes([h[8], h[9], h[10], h[11]])))
    }
    /// A whole container. Bytes past the declared length are ignored; fewer is an error.
    pub fn decode(b: &[u8]) -> Result<Container, PtpError> {
        let (len, kind, code, tid) = Container::header(b)?;
        let payload = b.get(HEADER..len).ok_or_else(|| PtpError::Protocol("container cut short".into()))?.to_vec();
        Ok(Container { kind, code, tid, payload })
    }
    pub fn params(&self) -> Vec<u32> {
        self.payload.chunks_exact(4).take(5).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }
}

/// A bulk pipe pair plus an interrupt pipe: what the USB still-image class gives.
pub trait BulkPipe: Send {
    /// Send one transfer (ended by a short or zero-length packet).
    fn write(&mut self, b: &[u8]) -> Result<(), PtpError>;
    /// Receive one transfer (up to a short packet).
    fn read(&mut self) -> Result<Vec<u8>, PtpError>;
    /// One interrupt transfer (an event container), waiting at most `timeout`.
    fn read_interrupt(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, PtpError>;
    fn describe(&self) -> String;
}

/// PTP over a bulk pipe (USB).
pub struct UsbTransport<P: BulkPipe> {
    pub pipe: P,
}

impl<P: BulkPipe> UsbTransport<P> {
    /// Read one container that may span several transfers.
    fn read_container(&mut self) -> Result<Container, PtpError> {
        let mut buf = self.pipe.read()?;
        let (len, ..) = Container::header(&buf)?;
        while buf.len() < len {
            let more = self.pipe.read()?;
            if more.is_empty() {
                return Err(PtpError::Protocol("container cut short".into()));
            }
            buf.extend_from_slice(&more);
        }
        Container::decode(&buf)
    }
}

impl<P: BulkPipe> Transport for UsbTransport<P> {
    fn transact(&mut self, code: u16, tid: u32, params: &[u32], data_out: Option<&[u8]>) -> Result<(Response, Vec<u8>), PtpError> {
        self.pipe.write(&Container::with_params(kind::COMMAND, code, tid, params).encode())?;
        if let Some(d) = data_out {
            self.pipe.write(&Container { kind: kind::DATA, code, tid, payload: d.to_vec() }.encode())?;
        }
        let mut data = Vec::new();
        loop {
            let c = self.read_container()?;
            match c.kind {
                kind::DATA => data = c.payload,
                kind::RESPONSE => return Ok((Response { code: c.code, params: c.params() }, data)),
                other => return Err(PtpError::Protocol(format!("unexpected container type {other}"))),
            }
        }
    }
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>, PtpError> {
        let Some(b) = self.pipe.read_interrupt(timeout)? else { return Ok(None) };
        let c = Container::decode(&b)?;
        Ok(Some(Event { code: c.code, params: c.params() }))
    }
    fn describe(&self) -> String {
        self.pipe.describe()
    }
}
