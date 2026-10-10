//! PTP/IP (CIPA DC-005): PTP over two TCP connections (commands and events), the way Wi-Fi
//! cameras tether. Packets are a 32-bit length, a 32-bit type and a payload. The client is
//! [`PtpIpTransport`]; [`serve`] makes a [`SimCamera`] answer on a listener, for tests and the
//! demo.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::data::{Reader, Writer};
use super::sim::SimCamera;
use super::transport::{Event, Response, Transport};
use super::{MAX_DATA, PtpError};

/// The PTP/IP port.
pub const PORT: u16 = 15740;
/// Protocol version 1.0.
const VERSION: u32 = 0x0001_0000;
/// How long a connection or an answer may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Packet types.
pub mod pk {
    pub const INIT_COMMAND_REQUEST: u32 = 1;
    pub const INIT_COMMAND_ACK: u32 = 2;
    pub const INIT_EVENT_REQUEST: u32 = 3;
    pub const INIT_EVENT_ACK: u32 = 4;
    pub const INIT_FAIL: u32 = 5;
    pub const OPERATION_REQUEST: u32 = 6;
    pub const OPERATION_RESPONSE: u32 = 7;
    pub const EVENT: u32 = 8;
    pub const START_DATA: u32 = 9;
    pub const DATA: u32 = 10;
    pub const CANCEL: u32 = 11;
    pub const END_DATA: u32 = 12;
    pub const PROBE_REQUEST: u32 = 13;
    pub const PROBE_RESPONSE: u32 = 14;
}

/// Write one packet.
pub fn write_packet(w: &mut impl Write, ty: u32, payload: &[u8]) -> Result<(), PtpError> {
    let len = u32::try_from(payload.len().saturating_add(8)).map_err(|_| PtpError::Protocol("packet too large".into()))?;
    let mut b = Vec::with_capacity(payload.len() + 8);
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&ty.to_le_bytes());
    b.extend_from_slice(payload);
    w.write_all(&b)?;
    Ok(())
}

/// Read one packet: (type, payload).
pub fn read_packet(r: &mut impl Read) -> Result<(u32, Vec<u8>), PtpError> {
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
    let ty = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
    if !(8..=MAX_DATA).contains(&len) {
        return Err(PtpError::Protocol(format!("PTP/IP packet length {len}")));
    }
    let mut payload = vec![0u8; len - 8];
    r.read_exact(&mut payload)?;
    Ok((ty, payload))
}

/// The host's (or camera's) name as PTP/IP sends it: UCS-2, zero-terminated.
fn name_bytes(name: &str) -> Vec<u8> {
    name.encode_utf16().take(39).chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

/// A PTP/IP connection to a camera.
pub struct PtpIpTransport {
    cmd: TcpStream,
    events: TcpStream,
    peer: String,
    /// The camera's name from the handshake.
    pub camera_name: String,
}

fn params_of(r: &mut Reader<'_>) -> Vec<u32> {
    let mut out = Vec::new();
    while out.len() < 5 {
        match r.u32() {
            Ok(v) => out.push(v),
            Err(_) => break,
        }
    }
    out
}

impl PtpIpTransport {
    /// Connect to `host` (`host` or `host:port`), introducing the app as `host_name` with `guid`.
    pub fn connect(host: &str, guid: [u8; 16], host_name: &str) -> Result<PtpIpTransport, PtpError> {
        let addr =
            if host.contains(':') && !host.starts_with('[') && host.matches(':').count() == 1 { host.to_string() } else { format!("{host}:{PORT}") };
        let sa = addr.to_socket_addrs()?.next().ok_or_else(|| PtpError::Io(format!("no address for {host}")))?;
        let mut cmd = TcpStream::connect_timeout(&sa, TIMEOUT)?;
        cmd.set_read_timeout(Some(TIMEOUT))?;
        cmd.set_nodelay(true)?;
        let mut p = guid.to_vec();
        p.extend(name_bytes(host_name));
        p.extend(VERSION.to_le_bytes());
        write_packet(&mut cmd, pk::INIT_COMMAND_REQUEST, &p)?;
        let (ty, ack) = read_packet(&mut cmd)?;
        if ty == pk::INIT_FAIL {
            let reason = Reader::new(&ack).u32().unwrap_or(0);
            return Err(PtpError::Io(format!("the camera refused the connection (reason {reason}); accept the computer on the camera first")));
        }
        if ty != pk::INIT_COMMAND_ACK {
            return Err(PtpError::Protocol(format!("expected InitCommandAck, got packet type {ty}")));
        }
        let mut r = Reader::new(&ack);
        let conn = r.u32()?;
        let rest = r.remaining();
        let name_units: Vec<u16> =
            rest.get(16..).unwrap_or_default().chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        let mut events = TcpStream::connect_timeout(&sa, TIMEOUT)?;
        events.set_read_timeout(Some(TIMEOUT))?;
        write_packet(&mut events, pk::INIT_EVENT_REQUEST, &conn.to_le_bytes())?;
        let (ty, _) = read_packet(&mut events)?;
        if ty != pk::INIT_EVENT_ACK {
            return Err(PtpError::Protocol(format!("expected InitEventAck, got packet type {ty}")));
        }
        Ok(PtpIpTransport { cmd, events, peer: sa.to_string(), camera_name: String::from_utf16_lossy(&name_units) })
    }
}

impl Transport for PtpIpTransport {
    fn transact(&mut self, code: u16, tid: u32, params: &[u32], data_out: Option<&[u8]>) -> Result<(Response, Vec<u8>), PtpError> {
        let mut w = Writer::default();
        w.u32(if data_out.is_some() { 2 } else { 1 }).u16(code).u32(tid);
        for p in params.iter().take(5) {
            w.u32(*p);
        }
        write_packet(&mut self.cmd, pk::OPERATION_REQUEST, &w.0)?;
        if let Some(d) = data_out {
            let mut s = Writer::default();
            s.u32(tid).u64(d.len() as u64);
            write_packet(&mut self.cmd, pk::START_DATA, &s.0)?;
            let mut e = tid.to_le_bytes().to_vec();
            e.extend_from_slice(d);
            write_packet(&mut self.cmd, pk::END_DATA, &e)?;
        }
        let mut data = Vec::new();
        loop {
            let (ty, p) = read_packet(&mut self.cmd)?;
            let mut r = Reader::new(&p);
            match ty {
                pk::START_DATA => {
                    let _tid = r.u32()?;
                    let total = r.u64()?;
                    if total > MAX_DATA as u64 {
                        return Err(PtpError::Protocol("data phase too large".into()));
                    }
                    data.reserve(total as usize);
                }
                pk::DATA | pk::END_DATA => {
                    let _tid = r.u32()?;
                    if data.len().saturating_add(r.remaining().len()) > MAX_DATA {
                        return Err(PtpError::Protocol("data phase too large".into()));
                    }
                    data.extend_from_slice(r.remaining());
                }
                pk::OPERATION_RESPONSE => {
                    let code = r.u16()?;
                    let _tid = r.u32()?;
                    return Ok((Response { code, params: params_of(&mut r) }, data));
                }
                pk::PROBE_REQUEST => write_packet(&mut self.cmd, pk::PROBE_RESPONSE, &[])?,
                other => return Err(PtpError::Protocol(format!("unexpected PTP/IP packet type {other}"))),
            }
        }
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>, PtpError> {
        // wait for bytes without consuming any, so a timeout never splits a packet
        self.events.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut one = [0u8; 1];
        match self.events.peek(&mut one) {
            Ok(0) => return Err(PtpError::Io("the camera closed the event connection".into())),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        self.events.set_read_timeout(Some(TIMEOUT))?;
        let (ty, p) = read_packet(&mut self.events)?;
        match ty {
            pk::EVENT => {
                let mut r = Reader::new(&p);
                let code = r.u16()?;
                let _tid = r.u32()?;
                Ok(Some(Event { code, params: params_of(&mut r) }))
            }
            pk::PROBE_REQUEST => {
                write_packet(&mut self.events, pk::PROBE_RESPONSE, &[])?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn describe(&self) -> String {
        format!("PTP/IP {}", self.peer)
    }
}

/// Serve `cam` over PTP/IP on `listener`: one client (command then event connection), until it
/// disconnects. Runs on the calling thread; event forwarding gets a thread of its own.
pub fn serve(listener: &TcpListener, cam: &SimCamera) -> Result<(), PtpError> {
    let (mut cmd, _) = listener.accept()?;
    let (ty, _) = read_packet(&mut cmd)?;
    if ty != pk::INIT_COMMAND_REQUEST {
        write_packet(&mut cmd, pk::INIT_FAIL, &1u32.to_le_bytes())?;
        return Err(PtpError::Protocol("expected InitCommandRequest".into()));
    }
    let mut ack = 1u32.to_le_bytes().to_vec();
    ack.extend([0u8; 16]);
    ack.extend(name_bytes("Simulated PTP Camera"));
    ack.extend(VERSION.to_le_bytes());
    write_packet(&mut cmd, pk::INIT_COMMAND_ACK, &ack)?;
    let (mut ev, _) = listener.accept()?;
    let (ty, _) = read_packet(&mut ev)?;
    if ty != pk::INIT_EVENT_REQUEST {
        return Err(PtpError::Protocol("expected InitEventRequest".into()));
    }
    write_packet(&mut ev, pk::INIT_EVENT_ACK, &[])?;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let events_cam = cam.clone();
    let events_done = done.clone();
    let forward = std::thread::spawn(move || {
        while !events_done.load(std::sync::atomic::Ordering::Relaxed) {
            let mut t = super::sim::SimCamera::transport(&events_cam);
            match t.poll_event(Duration::from_millis(20)) {
                Ok(Some(e)) => {
                    let mut w = Writer::default();
                    w.u16(e.code).u32(0);
                    for p in &e.params {
                        w.u32(*p);
                    }
                    if write_packet(&mut ev, pk::EVENT, &w.0).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    });
    let result = (|| -> Result<(), PtpError> {
        loop {
            let (ty, p) = match read_packet(&mut cmd) {
                Ok(x) => x,
                Err(PtpError::Io(_)) => return Ok(()), // the client went away
                Err(e) => return Err(e),
            };
            if ty != pk::OPERATION_REQUEST {
                return Err(PtpError::Protocol(format!("camera got packet type {ty}")));
            }
            let mut r = Reader::new(&p);
            let phase = r.u32()?;
            let code = r.u16()?;
            let tid = r.u32()?;
            let params = params_of(&mut r);
            let mut data_out = None;
            if phase == 2 {
                let mut d = Vec::new();
                loop {
                    let (ty, p) = read_packet(&mut cmd)?;
                    match ty {
                        pk::START_DATA => {}
                        pk::DATA | pk::END_DATA => {
                            d.extend_from_slice(p.get(4..).unwrap_or_default());
                            if ty == pk::END_DATA {
                                break;
                            }
                        }
                        other => return Err(PtpError::Protocol(format!("camera got packet type {other} in a data phase"))),
                    }
                }
                data_out = Some(d);
            }
            let (resp, data) = cam.handle(code, &params, data_out.as_deref());
            if !data.is_empty() {
                let mut s = Writer::default();
                s.u32(tid).u64(data.len() as u64);
                write_packet(&mut cmd, pk::START_DATA, &s.0)?;
                for (i, chunk) in data.chunks(64 * 1024).enumerate() {
                    let last = (i + 1) * 64 * 1024 >= data.len();
                    let mut b = tid.to_le_bytes().to_vec();
                    b.extend_from_slice(chunk);
                    write_packet(&mut cmd, if last { pk::END_DATA } else { pk::DATA }, &b)?;
                }
            }
            let mut w = Writer::default();
            w.u16(resp.code).u32(tid);
            for p in &resp.params {
                w.u32(*p);
            }
            write_packet(&mut cmd, pk::OPERATION_RESPONSE, &w.0)?;
        }
    })();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = forward.join();
    result
}
