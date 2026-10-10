//! PTP datasets: little-endian readers and writers, and the DeviceInfo, ObjectInfo and
//! DevicePropDesc datasets (ISO 15740 §5.5). Every read is bounds-checked: a short or hostile
//! dataset is a [`PtpError::Protocol`], never a panic.

use super::{PtpError, dt};

/// Arrays longer than this in a dataset are refused (no camera lists a million operations).
const MAX_ARRAY: usize = 65_536;

/// A cursor over dataset bytes.
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

fn short(what: &str) -> PtpError {
    PtpError::Protocol(format!("dataset too short ({what})"))
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }
    pub fn remaining(&self) -> &'a [u8] {
        self.b.get(self.at..).unwrap_or_default()
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], PtpError> {
        let end = self.at.checked_add(n).ok_or_else(|| short("length"))?;
        let s = self.b.get(self.at..end).ok_or_else(|| short("field"))?;
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], PtpError> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, PtpError> {
        Ok(self.arr::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16, PtpError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, PtpError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, PtpError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    /// A PTP string: a count of UCS-2 units (including the terminating zero), then the units.
    pub fn string(&mut self) -> Result<String, PtpError> {
        let n = usize::from(self.u8()?);
        let raw = self.take(n.saturating_mul(2))?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        Ok(String::from_utf16_lossy(&units))
    }
    pub fn array_u16(&mut self) -> Result<Vec<u16>, PtpError> {
        let n = self.u32()? as usize;
        if n > MAX_ARRAY {
            return Err(PtpError::Protocol(format!("array of {n} entries")));
        }
        (0..n).map(|_| self.u16()).collect()
    }
    pub fn array_u32(&mut self) -> Result<Vec<u32>, PtpError> {
        let n = self.u32()? as usize;
        if n > MAX_ARRAY {
            return Err(PtpError::Protocol(format!("array of {n} entries")));
        }
        (0..n).map(|_| self.u32()).collect()
    }
    /// A value of data type `ty`.
    pub fn value(&mut self, ty: u16) -> Result<PropValue, PtpError> {
        Ok(match ty {
            dt::INT8 => PropValue::Int(i64::from(self.u8()? as i8)),
            dt::UINT8 => PropValue::Int(i64::from(self.u8()?)),
            dt::INT16 => PropValue::Int(i64::from(self.u16()? as i16)),
            dt::UINT16 => PropValue::Int(i64::from(self.u16()?)),
            dt::INT32 => PropValue::Int(i64::from(self.u32()? as i32)),
            dt::UINT32 => PropValue::Int(i64::from(self.u32()?)),
            dt::INT64 | dt::UINT64 => PropValue::Int(self.u64()? as i64),
            dt::STR => PropValue::Str(self.string()?),
            other => return Err(PtpError::Unsupported(format!("property data type 0x{other:04X}"))),
        })
    }
}

/// Builds dataset bytes.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// A PTP string (at most 254 characters; longer text is cut).
    pub fn string(&mut self, s: &str) -> &mut Self {
        if s.is_empty() {
            return self.u8(0);
        }
        let units: Vec<u16> = s.encode_utf16().take(254).collect();
        self.u8(u8::try_from(units.len() + 1).unwrap_or(255));
        for u in units {
            self.u16(u);
        }
        self.u16(0)
    }
    pub fn array_u16(&mut self, a: &[u16]) -> &mut Self {
        self.u32(u32::try_from(a.len()).unwrap_or(u32::MAX));
        for v in a {
            self.u16(*v);
        }
        self
    }
    pub fn array_u32(&mut self, a: &[u32]) -> &mut Self {
        self.u32(u32::try_from(a.len()).unwrap_or(u32::MAX));
        for v in a {
            self.u32(*v);
        }
        self
    }
    /// `v` encoded as data type `ty` (integers wrap to the type's width, as the wire does).
    pub fn value(&mut self, ty: u16, v: &PropValue) -> Result<&mut Self, PtpError> {
        match (ty, v) {
            (dt::INT8 | dt::UINT8, PropValue::Int(i)) => Ok(self.u8(*i as u8)),
            (dt::INT16 | dt::UINT16, PropValue::Int(i)) => Ok(self.u16(*i as u16)),
            (dt::INT32 | dt::UINT32, PropValue::Int(i)) => Ok(self.u32(*i as u32)),
            (dt::INT64 | dt::UINT64, PropValue::Int(i)) => Ok(self.u64(*i as u64)),
            (dt::STR, PropValue::Str(s)) => Ok(self.string(s)),
            _ => Err(PtpError::Unsupported(format!("value {v:?} for data type 0x{ty:04X}"))),
        }
    }
}

/// A device property value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropValue {
    Int(i64),
    Str(String),
}

impl PropValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            PropValue::Int(i) => Some(*i),
            PropValue::Str(_) => None,
        }
    }
}

/// The DeviceInfo dataset (ISO 15740 §5.5.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub standard_version: u16,
    pub vendor_extension_id: u32,
    pub vendor_extension_version: u16,
    pub vendor_extension_desc: String,
    pub functional_mode: u16,
    pub operations: Vec<u16>,
    pub events: Vec<u16>,
    pub properties: Vec<u16>,
    pub capture_formats: Vec<u16>,
    pub image_formats: Vec<u16>,
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
}

impl DeviceInfo {
    pub fn parse(b: &[u8]) -> Result<DeviceInfo, PtpError> {
        let mut r = Reader::new(b);
        Ok(DeviceInfo {
            standard_version: r.u16()?,
            vendor_extension_id: r.u32()?,
            vendor_extension_version: r.u16()?,
            vendor_extension_desc: r.string()?,
            functional_mode: r.u16()?,
            operations: r.array_u16()?,
            events: r.array_u16()?,
            properties: r.array_u16()?,
            capture_formats: r.array_u16()?,
            image_formats: r.array_u16()?,
            manufacturer: r.string()?,
            model: r.string()?,
            device_version: r.string()?,
            serial_number: r.string()?,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u16(self.standard_version).u32(self.vendor_extension_id).u16(self.vendor_extension_version);
        w.string(&self.vendor_extension_desc).u16(self.functional_mode);
        w.array_u16(&self.operations).array_u16(&self.events).array_u16(&self.properties);
        w.array_u16(&self.capture_formats).array_u16(&self.image_formats);
        w.string(&self.manufacturer).string(&self.model).string(&self.device_version).string(&self.serial_number);
        w.0
    }
    pub fn supports(&self, op: u16) -> bool {
        self.operations.contains(&op)
    }
    /// "Manufacturer Model", trimmed.
    pub fn name(&self) -> String {
        let m = self.model.trim();
        let mf = self.manufacturer.trim();
        if m.to_lowercase().starts_with(&mf.to_lowercase()) || mf.is_empty() { m.to_string() } else { format!("{mf} {m}") }
    }
}

/// The ObjectInfo dataset (ISO 15740 §5.5.2), the fields tethering uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectInfo {
    pub storage_id: u32,
    pub format: u16,
    pub size: u32,
    pub parent: u32,
    pub filename: String,
    pub capture_date: String,
}

impl ObjectInfo {
    pub fn parse(b: &[u8]) -> Result<ObjectInfo, PtpError> {
        let mut r = Reader::new(b);
        let storage_id = r.u32()?;
        let format = r.u16()?;
        let _protection = r.u16()?;
        let size = r.u32()?;
        let _thumb_format = r.u16()?;
        for _ in 0..6 {
            r.u32()?; // thumb size, thumb w/h, image w/h, bit depth
        }
        let parent = r.u32()?;
        let _assoc_type = r.u16()?;
        let _assoc_desc = r.u32()?;
        let _seq = r.u32()?;
        let filename = r.string()?;
        let capture_date = r.string().unwrap_or_default();
        Ok(ObjectInfo { storage_id, format, size, parent, filename, capture_date })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(self.storage_id).u16(self.format).u16(0).u32(self.size).u16(0);
        for _ in 0..6 {
            w.u32(0);
        }
        w.u32(self.parent).u16(0).u32(0).u32(0);
        w.string(&self.filename).string(&self.capture_date).string("").string("");
        w.0
    }
}

/// The allowed values of a property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropForm {
    None,
    Range { min: i64, max: i64, step: i64 },
    Enum(Vec<PropValue>),
}

/// The DevicePropDesc dataset (ISO 15740 §5.5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropDesc {
    pub code: u16,
    pub data_type: u16,
    pub writable: bool,
    pub default: PropValue,
    pub current: PropValue,
    pub form: PropForm,
}

impl PropDesc {
    pub fn parse(b: &[u8]) -> Result<PropDesc, PtpError> {
        let mut r = Reader::new(b);
        let code = r.u16()?;
        let data_type = r.u16()?;
        let writable = r.u8()? == 1;
        let default = r.value(data_type)?;
        let current = r.value(data_type)?;
        let form = match r.u8().unwrap_or(0) {
            1 => PropForm::Range {
                min: r.value(data_type)?.as_int().unwrap_or(0),
                max: r.value(data_type)?.as_int().unwrap_or(0),
                step: r.value(data_type)?.as_int().unwrap_or(0),
            },
            2 => {
                let n = usize::from(r.u16()?);
                PropForm::Enum((0..n).map(|_| r.value(data_type)).collect::<Result<_, _>>()?)
            }
            _ => PropForm::None,
        };
        Ok(PropDesc { code, data_type, writable, default, current, form })
    }
    pub fn encode(&self) -> Result<Vec<u8>, PtpError> {
        let mut w = Writer::default();
        w.u16(self.code).u16(self.data_type).u8(u8::from(self.writable));
        w.value(self.data_type, &self.default)?.value(self.data_type, &self.current)?;
        match &self.form {
            PropForm::None => {
                w.u8(0);
            }
            PropForm::Range { min, max, step } => {
                w.u8(1);
                for v in [min, max, step] {
                    w.value(self.data_type, &PropValue::Int(*v))?;
                }
            }
            PropForm::Enum(vals) => {
                w.u8(2).u16(u16::try_from(vals.len()).unwrap_or(u16::MAX));
                for v in vals.iter().take(usize::from(u16::MAX)) {
                    w.value(self.data_type, v)?;
                }
            }
        }
        Ok(w.0)
    }
    /// The allowed values, listed (a range is stepped, at most 256 values).
    pub fn choices(&self) -> Vec<PropValue> {
        match &self.form {
            PropForm::None => Vec::new(),
            PropForm::Enum(v) => v.clone(),
            PropForm::Range { min, max, step } => {
                let step = (*step).max(1);
                let mut out = Vec::new();
                let mut v = *min;
                while v <= *max && out.len() < 256 {
                    out.push(PropValue::Int(v));
                    v = v.saturating_add(step);
                }
                out
            }
        }
    }
    pub fn allows(&self, v: &PropValue) -> bool {
        match &self.form {
            PropForm::None => true,
            PropForm::Enum(vals) => vals.contains(v),
            PropForm::Range { min, max, .. } => v.as_int().is_some_and(|i| i >= *min && i <= *max),
        }
    }
}
