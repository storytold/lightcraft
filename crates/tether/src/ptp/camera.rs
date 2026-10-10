//! A camera session over any [`Transport`]: the generic PTP operations tethering needs, and the
//! exposure readouts of the tether bar (shutter, aperture, ISO, white balance, exposure
//! compensation, battery).

use std::time::{Duration, Instant};

use serde::Serialize;

use super::data::{DeviceInfo, ObjectInfo, PropDesc, PropValue, Writer};
use super::transport::{Event, Transport};
use super::vendor::{self, Vendor};
use super::{FORMAT_ASSOCIATION, MAX_DATA, PtpError, ev, op, prop, rc};

/// The session id the app opens.
const SESSION_ID: u32 = 1;

/// A connected camera with an open session.
pub struct Camera {
    link: Box<dyn Transport>,
    tid: u32,
    pub info: DeviceInfo,
    pub vendor: Vendor,
    live_view: bool,
}

/// One readout of the tether bar.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Readout {
    /// `shutter`, `aperture`, `iso`, `wb`, `ev`, `battery`.
    pub name: &'static str,
    /// The raw PTP value.
    pub value: Option<i64>,
    /// As shown ("1/250", "f/5.6", "ISO 400", "Daylight").
    pub label: String,
    pub writable: bool,
    /// The settable values, as labels (empty: not settable or no list).
    pub choices: Vec<String>,
}

/// The readouts the camera offers, in tether bar order.
pub type Readouts = Vec<Readout>;

/// The tether bar's properties: (name, PTP code).
pub const READOUTS: [(&str, u16); 6] = [
    ("shutter", prop::EXPOSURE_TIME),
    ("aperture", prop::F_NUMBER),
    ("iso", prop::EXPOSURE_INDEX),
    ("wb", prop::WHITE_BALANCE),
    ("ev", prop::EXPOSURE_BIAS),
    ("battery", prop::BATTERY_LEVEL),
];

/// The PTP code of a readout name.
pub fn prop_code(name: &str) -> Option<u16> {
    READOUTS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, c)| *c)
}

/// A property value as shown.
pub fn format_value(code: u16, v: &PropValue) -> String {
    let Some(i) = v.as_int() else {
        return match v {
            PropValue::Str(s) => s.clone(),
            PropValue::Int(_) => String::new(),
        };
    };
    match code {
        prop::EXPOSURE_TIME => {
            if i <= 0 {
                "Bulb".into()
            } else if i >= 10_000 {
                let s = i as f64 / 10_000.0;
                if (s - s.round()).abs() < 0.05 { format!("{}\"", s.round()) } else { format!("{s:.1}\"") }
            } else if i >= 3_000 {
                format!("{:.1}\"", i as f64 / 10_000.0)
            } else {
                format!("1/{}", (10_000.0 / i as f64).round())
            }
        }
        prop::F_NUMBER => {
            let f = i as f64 / 100.0;
            if f >= 10.0 || (f - f.round()).abs() < 0.01 { format!("f/{}", f.round()) } else { format!("f/{f:.1}") }
        }
        prop::EXPOSURE_INDEX => {
            if i == 0xFFFF {
                "ISO Auto".into()
            } else {
                format!("ISO {i}")
            }
        }
        prop::WHITE_BALANCE => match i {
            1 => "Manual".into(),
            2 => "Auto".into(),
            3 => "One-push".into(),
            4 => "Daylight".into(),
            5 => "Fluorescent".into(),
            6 => "Tungsten".into(),
            7 => "Flash".into(),
            _ => format!("WB {i:#06X}"),
        },
        prop::EXPOSURE_BIAS => {
            let e = (i as i16) as f64 / 1000.0;
            if e.abs() < 0.01 { "0 EV".into() } else { format!("{e:+.1} EV") }
        }
        prop::BATTERY_LEVEL => format!("{i}%"),
        _ => i.to_string(),
    }
}

impl Camera {
    /// Read the device info and open a session.
    pub fn open(link: Box<dyn Transport>, usb_vendor: Option<u16>) -> Result<Camera, PtpError> {
        let mut c = Camera { link, tid: 0, info: DeviceInfo::default(), vendor: Vendor::Other, live_view: false };
        // GetDeviceInfo is allowed outside a session (transaction id 0)
        let (r, data) = c.link.transact(op::GET_DEVICE_INFO, 0, &[], None)?;
        if r.code != rc::OK {
            return Err(PtpError::Response(r.code));
        }
        c.info = DeviceInfo::parse(&data)?;
        c.vendor = Vendor::detect(c.info.vendor_extension_id, &c.info.manufacturer, usb_vendor);
        match c.run(op::OPEN_SESSION, &[SESSION_ID], None) {
            Ok(_) | Err(PtpError::Response(rc::SESSION_ALREADY_OPEN)) => {}
            Err(e) => return Err(e),
        }
        Ok(c)
    }

    fn next_tid(&mut self) -> u32 {
        self.tid = self.tid.wrapping_add(1).max(1);
        self.tid
    }

    /// Run an operation; an answer other than OK is an error.
    pub fn run(&mut self, code: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Vec<u8>, PtpError> {
        let tid = self.next_tid();
        let (r, data) = self.link.transact(code, tid, params, data_out)?;
        if r.code != rc::OK {
            return Err(PtpError::Response(r.code));
        }
        Ok(data)
    }

    pub fn name(&self) -> String {
        self.info.name()
    }
    pub fn describe(&self) -> String {
        self.link.describe()
    }

    /// Whether the camera can be fired from the app.
    pub fn can_capture(&self) -> bool {
        self.info.supports(op::INITIATE_CAPTURE)
    }

    /// Fire the shutter (InitiateCapture on the default storage and format).
    pub fn capture(&mut self) -> Result<(), PtpError> {
        if !self.can_capture() {
            return Err(PtpError::Unsupported(format!(
                "{} does not offer remote capture over generic PTP; shoot on the camera (shots still download)",
                self.name()
            )));
        }
        self.run(op::INITIATE_CAPTURE, &[0, 0], None).map(|_| ())
    }

    /// Events that arrived, waiting at most `timeout` for the first and then taking what is
    /// queued (at most 64).
    pub fn poll_events(&mut self, timeout: Duration) -> Result<Vec<Event>, PtpError> {
        let mut out = Vec::new();
        let mut wait = timeout;
        while out.len() < 64 {
            match self.link.poll_event(wait)? {
                Some(e) => out.push(e),
                None => break,
            }
            wait = Duration::from_millis(1);
        }
        Ok(out)
    }

    /// The objects a capture made: object-added events until capture-complete (or `timeout`).
    pub fn wait_capture(&mut self, timeout: Duration) -> Result<Vec<u32>, PtpError> {
        let until = Instant::now() + timeout;
        let mut added = Vec::new();
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(e) = self.link.poll_event(left.min(Duration::from_millis(200)))? else { continue };
            match e.code {
                ev::OBJECT_ADDED => added.extend(e.params.first().copied()),
                ev::CAPTURE_COMPLETE => break,
                ev::STORE_FULL => return Err(PtpError::Response(rc::STORE_FULL)),
                _ => {}
            }
        }
        Ok(added)
    }

    pub fn object_info(&mut self, handle: u32) -> Result<ObjectInfo, PtpError> {
        ObjectInfo::parse(&self.run(op::GET_OBJECT_INFO, &[handle], None)?)
    }

    /// Whether `handle` is a folder (associations are never downloaded).
    pub fn is_folder(info: &ObjectInfo) -> bool {
        info.format == FORMAT_ASSOCIATION
    }

    /// The object's bytes.
    pub fn download(&mut self, handle: u32) -> Result<Vec<u8>, PtpError> {
        let b = self.run(op::GET_OBJECT, &[handle], None)?;
        if b.len() > MAX_DATA {
            return Err(PtpError::Protocol("object too large".into()));
        }
        Ok(b)
    }

    pub fn delete(&mut self, handle: u32) -> Result<(), PtpError> {
        self.run(op::DELETE_OBJECT, &[handle, 0], None).map(|_| ())
    }

    pub fn prop_desc(&mut self, code: u16) -> Result<PropDesc, PtpError> {
        PropDesc::parse(&self.run(op::GET_DEVICE_PROP_DESC, &[u32::from(code)], None)?)
    }

    /// Set a property, checked against its description first.
    pub fn set_prop(&mut self, code: u16, v: &PropValue) -> Result<(), PtpError> {
        let d = self.prop_desc(code)?;
        if !d.writable {
            return Err(PtpError::Unsupported(format!("{} is read-only on this camera", format_value(code, v))));
        }
        if !d.allows(v) {
            return Err(PtpError::Response(rc::INVALID_DEVICE_PROP_VALUE));
        }
        let mut w = Writer::default();
        w.value(d.data_type, v)?;
        self.run(op::SET_DEVICE_PROP_VALUE, &[u32::from(code)], Some(&w.0)).map(|_| ())
    }

    /// Set the readout `name` to a value given as its label ("1/250", "f/8") or raw number.
    pub fn set_readout(&mut self, name: &str, value: &str) -> Result<(), PtpError> {
        let code = prop_code(name).ok_or_else(|| PtpError::Unsupported(format!("unknown setting `{name}`")))?;
        let d = self.prop_desc(code)?;
        let want = value.trim();
        let v = d
            .choices()
            .into_iter()
            .find(|c| format_value(code, c).eq_ignore_ascii_case(want))
            .or_else(|| want.parse::<i64>().ok().map(PropValue::Int))
            .ok_or_else(|| PtpError::Unsupported(format!("`{want}` is not a {name} this camera offers")))?;
        self.set_prop(code, &v)
    }

    /// The readouts the camera supports.
    pub fn readouts(&mut self) -> Readouts {
        let mut out = Vec::new();
        for (name, code) in READOUTS {
            if !self.info.properties.contains(&code) {
                continue;
            }
            let Ok(d) = self.prop_desc(code) else { continue };
            out.push(Readout {
                name,
                value: d.current.as_int(),
                label: format_value(code, &d.current),
                writable: d.writable,
                choices: if d.writable { d.choices().iter().map(|c| format_value(code, c)).collect() } else { Vec::new() },
            });
        }
        out
    }

    /// Whether live view is known for this camera.
    pub fn has_live_view(&self) -> bool {
        self.vendor.live_view_ops().is_some_and(|(start, frame, _)| self.info.supports(start) && self.info.supports(frame))
    }

    /// One live view frame (a JPEG).
    pub fn live_view_frame(&mut self) -> Result<Vec<u8>, PtpError> {
        let Some((start, frame, _)) = self.vendor.live_view_ops().filter(|_| self.has_live_view()) else {
            return Err(PtpError::Unsupported(format!("live view on {}", self.name())));
        };
        if !self.live_view {
            self.run(start, &[], None)?;
            self.live_view = true;
        }
        let b = self.run(frame, &[], None)?;
        vendor::jpeg_in(&b).map(<[u8]>::to_vec).ok_or_else(|| PtpError::Protocol("live view frame without an image".into()))
    }

    pub fn stop_live_view(&mut self) {
        if let Some((_, _, end)) = self.vendor.live_view_ops().filter(|_| self.live_view) {
            let _ = self.run(end, &[], None);
        }
        self.live_view = false;
    }

    /// End the session (errors ignored: the camera may already be gone).
    pub fn close(mut self) {
        self.stop_live_view();
        let _ = self.run(op::CLOSE_SESSION, &[], None);
    }
}
