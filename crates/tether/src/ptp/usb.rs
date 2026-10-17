//! PTP over USB with `nusb` (pure Rust): finds still-image class interfaces (class 6, subclass 1,
//! protocol 1), claims one and moves containers over its bulk endpoints; events come on the
//! interrupt endpoint. Platform notes (udev rule, macOS `ptpcamerad`, Windows WinUSB) are in
//! `docs/tethering.md`.

use std::io::Read;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, In, Interrupt, Out};

use super::PtpError;
use super::transport::{BulkPipe, Transport, UsbTransport};
use super::vendor::Vendor;

/// The still-image interface class (USB class codes).
const CLASS_IMAGE: u8 = 6;
/// Read size of one bulk IN transfer.
const READ_SIZE: usize = 512 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

/// A camera on the USB bus.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UsbCamera {
    /// `usb:<bus>:<address>`.
    pub id: String,
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub vendor: Vendor,
}

fn usb_err(e: impl std::fmt::Display) -> PtpError {
    PtpError::Io(e.to_string())
}

fn bus_of(d: &nusb::DeviceInfo) -> String {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        d.busnum().to_string()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        d.bus_id().to_string()
    }
}

fn id_of(d: &nusb::DeviceInfo) -> String {
    format!("usb:{}:{}", bus_of(d), d.device_address())
}

fn still_image_interface(d: &nusb::DeviceInfo) -> Option<u8> {
    d.interfaces().find(|i| i.class() == CLASS_IMAGE && i.subclass() == 1).map(|i| i.interface_number())
}

/// The PTP cameras connected over USB.
pub fn list() -> Result<Vec<UsbCamera>, PtpError> {
    let devices = nusb::list_devices().wait().map_err(usb_err)?;
    Ok(devices
        .filter(|d| still_image_interface(d).is_some())
        .map(|d| {
            let vendor = Vendor::detect(0, d.manufacturer_string().unwrap_or_default(), Some(d.vendor_id()));
            let name = match (d.manufacturer_string(), d.product_string()) {
                (_, Some(p)) if p.to_lowercase().contains(&vendor.name().to_lowercase()) => p.to_string(),
                (Some(m), Some(p)) => format!("{m} {p}"),
                (_, Some(p)) => p.to_string(),
                _ => format!("{} camera {:04x}:{:04x}", vendor.name(), d.vendor_id(), d.product_id()),
            };
            UsbCamera { id: id_of(&d), name, vendor_id: d.vendor_id(), product_id: d.product_id(), vendor }
        })
        .collect())
}

/// Open the camera `id` (`usb:<bus>:<address>`, or `usb` for the first one found).
pub fn open(id: &str) -> Result<(Box<dyn Transport>, u16), PtpError> {
    let devices = nusb::list_devices().wait().map_err(usb_err)?;
    let mut found = None;
    for d in devices {
        if still_image_interface(&d).is_some() && (id == "usb" || id_of(&d) == id) {
            found = Some(d);
            break;
        }
    }
    let d = found.ok_or_else(|| PtpError::Io(format!("no PTP camera at {id} (is it on, in PTP/PC mode, and connected?)")))?;
    let iface_no = still_image_interface(&d).ok_or_else(|| PtpError::Io("no still-image interface".into()))?;
    let dev = d.open().wait().map_err(|e| {
        PtpError::Io(format!("can't open the camera: {e} (Linux: install the udev rule; macOS: quit apps that hold the camera; Windows: WinUSB driver — see docs/tethering.md)"))
    })?;
    let iface = dev
        .detach_and_claim_interface(iface_no)
        .wait()
        .map_err(|e| PtpError::Io(format!("can't claim the camera: {e} (another app may be using it)")))?;
    let desc = iface.descriptor().ok_or_else(|| PtpError::Io("no interface descriptor".into()))?;
    let (mut bin, mut bout, mut intr) = (None, None, None);
    for ep in desc.endpoints() {
        let addr = ep.address();
        let is_in = addr & 0x80 != 0;
        match (ep.transfer_type(), is_in) {
            (TransferType::Bulk, true) => bin = Some(addr),
            (TransferType::Bulk, false) => bout = Some(addr),
            (TransferType::Interrupt, true) => intr = Some(addr),
            _ => {}
        }
    }
    let (Some(bin), Some(bout)) = (bin, bout) else { return Err(PtpError::Io("the camera has no bulk endpoints".into())) };
    let reader = iface.endpoint::<Bulk, In>(bin).map_err(usb_err)?.reader(READ_SIZE).with_read_timeout(TIMEOUT);
    let out = iface.endpoint::<Bulk, Out>(bout).map_err(usb_err)?;
    let events = match intr {
        Some(a) => iface.endpoint::<Interrupt, In>(a).ok(),
        None => None,
    };
    let pipe = NusbPipe { reader, out, events, label: format!("USB {}", id_of(&d)) };
    Ok((Box::new(UsbTransport { pipe }), d.vendor_id()))
}

struct NusbPipe {
    reader: nusb::io::EndpointRead<Bulk>,
    out: nusb::Endpoint<Bulk, Out>,
    events: Option<nusb::Endpoint<Interrupt, In>>,
    label: String,
}

impl BulkPipe for NusbPipe {
    fn write(&mut self, b: &[u8]) -> Result<(), PtpError> {
        let mps = self.out.max_packet_size().max(1);
        let mut buf = Buffer::new(b.len());
        buf.extend_from_slice(b);
        self.out.transfer_blocking(buf, TIMEOUT).status.map_err(usb_err)?;
        if b.len().is_multiple_of(mps) {
            // a transfer that fills whole packets ends with a zero-length packet
            self.out.transfer_blocking(Buffer::new(0), TIMEOUT).status.map_err(usb_err)?;
        }
        Ok(())
    }
    fn read(&mut self) -> Result<Vec<u8>, PtpError> {
        let mut out = Vec::new();
        let mut r = self.reader.until_short_packet();
        r.read_to_end(&mut out)?;
        r.consume_end().map_err(|e| PtpError::Protocol(format!("{e:?}")))?;
        Ok(out)
    }
    fn read_interrupt(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, PtpError> {
        let Some(ep) = self.events.as_mut() else {
            std::thread::sleep(timeout.min(Duration::from_millis(50)));
            return Ok(None);
        };
        if ep.pending() == 0 {
            let mps = ep.max_packet_size().max(1);
            let len = 64usize.div_ceil(mps) * mps;
            let buf = ep.allocate(len);
            ep.submit(buf);
        }
        match ep.wait_next_complete(timeout) {
            Some(c) => {
                let n = c.actual_len;
                let b = c.status.map(|_| c.buffer.into_vec()).map_err(usb_err)?;
                Ok(Some(b.get(..n).unwrap_or_default().to_vec()))
            }
            None => Ok(None),
        }
    }
    fn describe(&self) -> String {
        self.label.clone()
    }
}
