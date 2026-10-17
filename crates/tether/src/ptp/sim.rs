//! A simulated PTP camera, in process: it keeps a card of objects, the tether bar's properties
//! and an event queue, and answers operations as a camera would. It is reachable as a direct
//! [`Transport`] ([`SimCamera::transport`]), as a USB bulk pipe ([`SimCamera::usb`], exercising
//! the container framing) and as a PTP/IP responder ([`super::ptpip::serve`]). CI tests and the
//! app's `sim` device use it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::data::{DeviceInfo, ObjectInfo, PropDesc, PropForm, PropValue, Reader};
use super::transport::{BulkPipe, Container, Event, Response, Transport, UsbTransport, kind};
use super::vendor::{ext_id, nikon};
use super::{PtpError, dt, ev, op, prop, rc};

/// Makes the bytes of shot number `n` (1-based).
pub type ShotMaker = Box<dyn Fn(u32) -> Vec<u8> + Send>;

struct State {
    session: Option<u32>,
    objects: BTreeMap<u32, (ObjectInfo, Vec<u8>)>,
    next_handle: u32,
    shots: u32,
    events: VecDeque<Event>,
    props: BTreeMap<u16, PropDesc>,
    info: DeviceInfo,
    make: ShotMaker,
    live_view: bool,
    /// Fail the next operation with this response (tests).
    fail_next: Option<u16>,
}

/// A simulated camera (cheap to clone; clones share the camera).
#[derive(Clone)]
pub struct SimCamera(Arc<Mutex<State>>);

fn enum_desc(code: u16, data_type: u16, current: i64, values: &[i64]) -> PropDesc {
    PropDesc {
        code,
        data_type,
        writable: true,
        default: PropValue::Int(current),
        current: PropValue::Int(current),
        form: PropForm::Enum(values.iter().map(|v| PropValue::Int(*v)).collect()),
    }
}

/// A tiny but valid-looking file for shot `n` when no maker is given (not an image).
fn default_shot(n: u32) -> Vec<u8> {
    format!("simulated shot {n}\n").into_bytes()
}

impl SimCamera {
    /// A camera that identifies as "Simulated PTP Camera". `nikon_live_view`: also offer the
    /// Nikon live view operations (and identify with Nikon's extension id).
    pub fn new(make: Option<ShotMaker>, nikon_live_view: bool) -> SimCamera {
        let mut operations = vec![
            op::GET_DEVICE_INFO,
            op::OPEN_SESSION,
            op::CLOSE_SESSION,
            op::GET_STORAGE_IDS,
            op::GET_OBJECT_HANDLES,
            op::GET_OBJECT_INFO,
            op::GET_OBJECT,
            op::DELETE_OBJECT,
            op::INITIATE_CAPTURE,
            op::GET_DEVICE_PROP_DESC,
            op::GET_DEVICE_PROP_VALUE,
            op::SET_DEVICE_PROP_VALUE,
        ];
        if nikon_live_view {
            operations.extend([nikon::START_LIVE_VIEW, nikon::GET_LIVE_VIEW_IMAGE, nikon::END_LIVE_VIEW]);
        }
        let props: BTreeMap<u16, PropDesc> = [
            enum_desc(prop::EXPOSURE_TIME, dt::UINT32, 40, &[10_000, 5_000, 1_000, 250, 125, 80, 40, 20, 10, 5]),
            enum_desc(prop::F_NUMBER, dt::UINT16, 560, &[280, 400, 560, 800, 1100, 1600]),
            enum_desc(prop::EXPOSURE_INDEX, dt::UINT16, 100, &[100, 200, 400, 800, 1600, 3200, 6400]),
            enum_desc(prop::WHITE_BALANCE, dt::UINT16, 2, &[2, 4, 6, 5, 7, 1]),
            enum_desc(prop::EXPOSURE_BIAS, dt::INT16, 0, &[-2000, -1000, -333, 0, 333, 1000, 2000]),
            PropDesc {
                code: prop::BATTERY_LEVEL,
                data_type: dt::UINT8,
                writable: false,
                default: PropValue::Int(100),
                current: PropValue::Int(87),
                form: PropForm::Range { min: 0, max: 100, step: 1 },
            },
        ]
        .into_iter()
        .map(|d| (d.code, d))
        .collect();
        let info = DeviceInfo {
            standard_version: 100,
            vendor_extension_id: if nikon_live_view { ext_id::NIKON } else { ext_id::MICROSOFT },
            vendor_extension_version: 100,
            vendor_extension_desc: "microsoft.com: 1.0".into(),
            functional_mode: 0,
            operations,
            events: vec![ev::OBJECT_ADDED, ev::CAPTURE_COMPLETE, ev::DEVICE_PROP_CHANGED, ev::STORE_FULL],
            properties: props.keys().copied().collect(),
            capture_formats: vec![0x3801],
            image_formats: vec![0x3801, FORMAT_RAW],
            manufacturer: "Simulated".into(),
            model: "Simulated PTP Camera".into(),
            device_version: "1.0".into(),
            serial_number: "SIM0001".into(),
        };
        SimCamera(Arc::new(Mutex::new(State {
            session: None,
            objects: BTreeMap::new(),
            next_handle: 0x1000,
            shots: 0,
            events: VecDeque::new(),
            props,
            info,
            make: make.unwrap_or_else(|| Box::new(default_shot)),
            live_view: false,
            fail_next: None,
        })))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Press the shutter on the camera body: a shot lands on the card and the events say so.
    pub fn shoot(&self) -> u32 {
        let mut s = self.state();
        s.shots = s.shots.saturating_add(1);
        let n = s.shots;
        let bytes = (s.make)(n);
        let handle = s.next_handle;
        s.next_handle = s.next_handle.saturating_add(1);
        let info = ObjectInfo {
            storage_id: 0x0001_0001,
            format: 0x3801,
            size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            parent: 0,
            filename: format!("DSC_{n:04}.JPG"),
            capture_date: "20261011T120000".into(),
        };
        s.objects.insert(handle, (info, bytes));
        s.events.push_back(Event { code: ev::OBJECT_ADDED, params: vec![handle] });
        s.events.push_back(Event { code: ev::CAPTURE_COMPLETE, params: vec![] });
        handle
    }

    /// Handles of the objects on the card.
    pub fn card(&self) -> Vec<u32> {
        self.state().objects.keys().copied().collect()
    }

    /// The current raw value of a property.
    pub fn prop(&self, code: u16) -> Option<PropValue> {
        self.state().props.get(&code).map(|d| d.current.clone())
    }

    /// Make the next operation fail with `code`.
    pub fn fail_next(&self, code: u16) {
        self.state().fail_next = Some(code);
    }

    /// Answer one operation.
    pub fn handle(&self, code: u16, params: &[u32], data_out: Option<&[u8]>) -> (Response, Vec<u8>) {
        match self.answer(code, params, data_out) {
            Ok(data) => (Response { code: rc::OK, params: vec![] }, data),
            Err(c) => (Response { code: c, params: vec![] }, Vec::new()),
        }
    }

    fn answer(&self, code: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Vec<u8>, u16> {
        let p = |i: usize| params.get(i).copied().unwrap_or(0);
        {
            let mut s = self.state();
            if let Some(f) = s.fail_next.take() {
                return Err(f);
            }
            if code == op::GET_DEVICE_INFO {
                return Ok(s.info.encode());
            }
            if code == op::OPEN_SESSION {
                if s.session.is_some() {
                    return Err(rc::SESSION_ALREADY_OPEN);
                }
                if p(0) == 0 {
                    return Err(rc::INVALID_PARAMETER);
                }
                s.session = Some(p(0));
                return Ok(Vec::new());
            }
            if s.session.is_none() {
                return Err(rc::SESSION_NOT_OPEN);
            }
            if !s.info.operations.contains(&code) {
                return Err(rc::OPERATION_NOT_SUPPORTED);
            }
        }
        match code {
            op::CLOSE_SESSION => {
                self.state().session = None;
                Ok(Vec::new())
            }
            op::GET_STORAGE_IDS => {
                let mut w = super::data::Writer::default();
                w.array_u32(&[0x0001_0001]);
                Ok(w.0)
            }
            op::GET_OBJECT_HANDLES => {
                let mut w = super::data::Writer::default();
                w.array_u32(&self.card());
                Ok(w.0)
            }
            op::GET_OBJECT_INFO => self.state().objects.get(&p(0)).map(|(i, _)| i.encode()).ok_or(rc::INVALID_OBJECT_HANDLE),
            op::GET_OBJECT => self.state().objects.get(&p(0)).map(|(_, b)| b.clone()).ok_or(rc::INVALID_OBJECT_HANDLE),
            op::DELETE_OBJECT => self.state().objects.remove(&p(0)).map(|_| Vec::new()).ok_or(rc::INVALID_OBJECT_HANDLE),
            op::INITIATE_CAPTURE => {
                self.shoot();
                Ok(Vec::new())
            }
            op::GET_DEVICE_PROP_DESC => {
                let s = self.state();
                let d = s.props.get(&(p(0) as u16)).ok_or(rc::DEVICE_PROP_NOT_SUPPORTED)?;
                d.encode().map_err(|_| rc::GENERAL_ERROR)
            }
            op::GET_DEVICE_PROP_VALUE => {
                let s = self.state();
                let d = s.props.get(&(p(0) as u16)).ok_or(rc::DEVICE_PROP_NOT_SUPPORTED)?;
                let mut w = super::data::Writer::default();
                w.value(d.data_type, &d.current).map_err(|_| rc::GENERAL_ERROR)?;
                Ok(w.0)
            }
            op::SET_DEVICE_PROP_VALUE => {
                let mut s = self.state();
                let pc = p(0) as u16;
                let d = s.props.get_mut(&pc).ok_or(rc::DEVICE_PROP_NOT_SUPPORTED)?;
                if !d.writable {
                    return Err(rc::ACCESS_DENIED);
                }
                let v = Reader::new(data_out.unwrap_or_default()).value(d.data_type).map_err(|_| rc::INVALID_DEVICE_PROP_VALUE)?;
                if !d.allows(&v) {
                    return Err(rc::INVALID_DEVICE_PROP_VALUE);
                }
                d.current = v;
                s.events.push_back(Event { code: ev::DEVICE_PROP_CHANGED, params: vec![u32::from(pc)] });
                Ok(Vec::new())
            }
            nikon::START_LIVE_VIEW => {
                self.state().live_view = true;
                Ok(Vec::new())
            }
            nikon::END_LIVE_VIEW => {
                self.state().live_view = false;
                Ok(Vec::new())
            }
            nikon::GET_LIVE_VIEW_IMAGE => {
                let s = self.state();
                if !s.live_view {
                    return Err(rc::ACCESS_DENIED);
                }
                // a vendor header, then the frame
                let n = s.shots.saturating_add(1);
                let mut b = vec![0u8; 64];
                b.extend((s.make)(n));
                Ok(b)
            }
            _ => Err(rc::OPERATION_NOT_SUPPORTED),
        }
    }

    fn next_event(&self) -> Option<Event> {
        self.state().events.pop_front()
    }

    /// The camera as a direct transport.
    pub fn transport(&self) -> Box<dyn Transport> {
        Box::new(SimTransport(self.clone()))
    }

    /// The camera behind a USB bulk pipe (container framing on the wire).
    pub fn usb(&self) -> Box<dyn Transport> {
        Box::new(UsbTransport { pipe: SimPipe { cam: self.clone(), out: VecDeque::new(), pending: None } })
    }
}

/// The ObjectFormat the simulated camera lists for raws (undefined non-image is 0x3000; this
/// stands in for a maker raw).
const FORMAT_RAW: u16 = 0xB101;

struct SimTransport(SimCamera);

fn wait_event(cam: &SimCamera, timeout: Duration) -> Option<Event> {
    let until = Instant::now() + timeout;
    loop {
        if let Some(e) = cam.next_event() {
            return Some(e);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2).min(timeout));
    }
}

impl Transport for SimTransport {
    fn transact(&mut self, code: u16, _tid: u32, params: &[u32], data_out: Option<&[u8]>) -> Result<(Response, Vec<u8>), PtpError> {
        Ok(self.0.handle(code, params, data_out))
    }
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>, PtpError> {
        Ok(wait_event(&self.0, timeout))
    }
    fn describe(&self) -> String {
        "simulated".into()
    }
}

/// The camera's side of a USB link: parses the host's containers, queues its answers, split
/// into 512-byte transfers like a high-speed bulk endpoint.
struct SimPipe {
    cam: SimCamera,
    out: VecDeque<Vec<u8>>,
    /// A command waiting for its data phase.
    pending: Option<Container>,
}

impl SimPipe {
    fn answer(&mut self, cmd: &Container, data: Option<&[u8]>) {
        let (r, d) = self.cam.handle(cmd.code, &cmd.params(), data);
        if !d.is_empty() {
            let bytes = Container { kind: kind::DATA, code: cmd.code, tid: cmd.tid, payload: d }.encode();
            for chunk in bytes.chunks(512) {
                self.out.push_back(chunk.to_vec());
            }
        }
        self.out.push_back(Container::with_params(kind::RESPONSE, r.code, cmd.tid, &r.params).encode());
    }
}

impl BulkPipe for SimPipe {
    fn write(&mut self, b: &[u8]) -> Result<(), PtpError> {
        let c = Container::decode(b)?;
        match c.kind {
            kind::COMMAND if c.code == op::SET_DEVICE_PROP_VALUE => {
                self.pending = Some(c);
            }
            kind::COMMAND => self.answer(&c, None),
            kind::DATA => {
                let cmd = self.pending.take().ok_or_else(|| PtpError::Protocol("data without a command".into()))?;
                self.answer(&cmd, Some(&c.payload));
            }
            other => return Err(PtpError::Protocol(format!("host sent container type {other}"))),
        }
        Ok(())
    }
    fn read(&mut self) -> Result<Vec<u8>, PtpError> {
        self.out.pop_front().ok_or(PtpError::Timeout)
    }
    fn read_interrupt(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, PtpError> {
        Ok(wait_event(&self.cam, timeout).map(|e| Container::with_params(kind::EVENT, e.code, 0, &e.params).encode()))
    }
    fn describe(&self) -> String {
        "simulated USB".into()
    }
}
