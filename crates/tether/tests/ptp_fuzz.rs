//! P6.2: a camera (or something pretending to be one on the network) is untrusted. Mutated PTP
//! datasets, containers, PTP/IP packets and whole sessions with a lying camera are errors,
//! never a panic, a hang or an unbounded allocation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dac_fuzzkit::Rng;
use dac_tether::ptp::sim::SimCamera;
use dac_tether::ptp::transport::Response;
use dac_tether::ptp::{Camera, Container, DeviceInfo, Event, ObjectInfo, PropDesc, Transport, op, prop, ptpip, vendor};

fn cam() -> SimCamera {
    SimCamera::new(Some(Box::new(|n| vec![n as u8; 2000])), true)
}

/// The simulated camera's real datasets: device info, object info, property descriptions.
fn datasets(c: &SimCamera) -> Vec<Vec<u8>> {
    let mut out = vec![c.handle(op::GET_DEVICE_INFO, &[], None).1];
    let _ = c.handle(op::OPEN_SESSION, &[1], None);
    let h = c.shoot();
    out.push(c.handle(op::GET_OBJECT_INFO, &[h], None).1);
    for p in [prop::EXPOSURE_TIME, prop::F_NUMBER, prop::EXPOSURE_INDEX, prop::WHITE_BALANCE] {
        out.push(c.handle(op::GET_DEVICE_PROP_DESC, &[u32::from(p)], None).1);
    }
    out
}

#[test]
fn mutated_datasets_never_panic() {
    let seeds = datasets(&cam());
    assert!(DeviceInfo::parse(&seeds[0]).is_ok() && ObjectInfo::parse(&seeds[1]).is_ok());
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    dac_fuzzkit::run("ptp.dataset", &refs, 5000, |b| {
        if let Ok(d) = DeviceInfo::parse(b) {
            let _ = vendor::Vendor::detect(d.vendor_extension_id, &d.manufacturer, Some(0x04a9));
        }
        let _ = ObjectInfo::parse(b);
        if let Ok(p) = PropDesc::parse(b) {
            let _ = dac_tether::ptp::camera::format_value(p.code, &p.current);
        }
        let _ = vendor::jpeg_in(b);
    });
}

#[test]
fn mutated_containers_and_ptpip_packets_never_panic() {
    let c = Container::with_params(2, op::GET_OBJECT, 7, &[1, 2, 3]).encode();
    let mut pkt = Vec::new();
    ptpip::write_packet(&mut pkt, 7, &[1, 0, 0, 0, 9, 9]).unwrap();
    dac_fuzzkit::run("ptp.container", &[&c, &pkt], 5000, |b| {
        let _ = Container::header(b);
        if let Ok(c) = Container::decode(b) {
            let _ = c.params();
        }
        let _ = ptpip::read_packet(&mut &b[..]);
    });
}

/// A transport that passes everything to the simulated camera, then garbles what comes back.
struct Liar {
    inner: Box<dyn Transport>,
    rng: Arc<Mutex<Rng>>,
}

impl Transport for Liar {
    fn transact(&mut self, code: u16, tid: u32, params: &[u32], data_out: Option<&[u8]>) -> Result<(Response, Vec<u8>), dac_tether::ptp::PtpError> {
        let (mut r, data) = self.inner.transact(code, tid, params, data_out)?;
        let mut rng = self.rng.lock().unwrap();
        let data = if rng.chance(2) { dac_fuzzkit::mutate(&mut rng, &data, 4) } else { data };
        if rng.chance(6) {
            r.params = (0..rng.below(8)).map(|_| rng.next_u64() as u32).collect();
        }
        if rng.chance(10) {
            r.code = rng.next_u64() as u16;
        }
        Ok((r, data))
    }
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<Event>, dac_tether::ptp::PtpError> {
        let e = self.inner.poll_event(timeout)?;
        let mut rng = self.rng.lock().unwrap();
        Ok(if rng.chance(3) {
            Some(Event { code: rng.next_u64() as u16, params: (0..rng.below(6)).map(|_| rng.next_u64() as u32).collect() })
        } else {
            e
        })
    }
    fn describe(&self) -> String {
        "liar".into()
    }
}

#[test]
fn a_lying_camera_never_panics() {
    let rng = Arc::new(Mutex::new(Rng::new(77)));
    for _ in 0..dac_fuzzkit::iterations(150) {
        let sim = cam();
        let inner = if rng.lock().unwrap().chance(2) { sim.transport() } else { sim.usb() };
        let Ok(mut c) = Camera::open(Box::new(Liar { inner, rng: rng.clone() }), Some(0x04b0)) else { continue };
        let _ = (c.name(), c.describe(), c.can_capture(), c.has_live_view());
        let _ = c.readouts();
        let _ = c.capture();
        if let Ok(handles) = c.wait_capture(Duration::from_millis(5)) {
            for h in handles.into_iter().take(4) {
                if let Ok(info) = c.object_info(h) {
                    let _ = Camera::is_folder(&info);
                }
                let _ = c.download(h);
            }
        }
        let _ = c.poll_events(Duration::from_millis(1));
        let _ = c.set_readout("iso", "800");
        let _ = c.live_view_frame();
        c.stop_live_view();
        c.close();
    }
}
