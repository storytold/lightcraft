use std::time::Duration;

use super::camera::format_value;
use super::data::*;
use super::sim::SimCamera;
use super::transport::{Container, kind};
use super::vendor::{self, Vendor};
use super::*;

fn transports(cam: &SimCamera) -> Vec<(&'static str, Box<dyn Transport>)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = cam.clone();
    std::thread::spawn(move || {
        let _ = ptpip::serve(&listener, &server);
    });
    let ip = ptpip::PtpIpTransport::connect(&addr.to_string(), [7; 16], "test host").unwrap();
    assert_eq!(ip.camera_name, "Simulated PTP Camera");
    vec![("direct", cam.transport()), ("usb", cam.usb()), ("ptpip", Box::new(ip))]
}

#[test]
fn session_capture_download_delete_over_every_transport() {
    for (name, t) in transports(&SimCamera::new(Some(Box::new(|n| vec![n as u8; 3000])), false)) {
        let mut c = Camera::open(t, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(c.name(), "Simulated PTP Camera", "{name}");
        assert!(c.can_capture());
        c.capture().unwrap();
        let added = c.wait_capture(Duration::from_secs(5)).unwrap();
        assert_eq!(added.len(), 1, "{name}");
        let info = c.object_info(added[0]).unwrap();
        assert!(info.filename.starts_with("DSC_"), "{name}");
        let bytes = c.download(added[0]).unwrap();
        assert_eq!(bytes.len(), 3000, "{name}");
        assert_eq!(info.size, 3000);
        c.delete(added[0]).unwrap();
        assert_eq!(c.download(added[0]), Err(PtpError::Response(rc::INVALID_OBJECT_HANDLE)), "{name}");
        c.close();
    }
}

#[test]
fn readouts_and_settings() {
    let cam = SimCamera::new(None, false);
    for (name, t) in transports(&cam) {
        let mut c = Camera::open(t, None).unwrap();
        let r = c.readouts();
        let names: Vec<_> = r.iter().map(|x| x.name).collect();
        assert_eq!(names, ["shutter", "aperture", "iso", "wb", "ev", "battery"], "{name}");
        let sh = &r[0];
        assert!(sh.choices.contains(&"1/250".to_string()), "{:?}", sh.choices);
        c.set_readout("shutter", "1/125").unwrap();
        assert_eq!(cam.prop(prop::EXPOSURE_TIME), Some(PropValue::Int(80)), "{name}");
        c.set_readout("aperture", "f/8").unwrap();
        c.set_readout("iso", "ISO 800").unwrap();
        c.set_readout("wb", "Daylight").unwrap();
        c.set_readout("ev", "-0.3 EV").unwrap();
        assert_eq!(cam.prop(prop::EXPOSURE_BIAS), Some(PropValue::Int(-333)));
        assert!(c.set_readout("iso", "ISO 123").is_err());
        assert!(matches!(c.set_readout("battery", "50"), Err(PtpError::Unsupported(_))));
        let r = c.readouts();
        assert_eq!(r[1].label, "f/8");
        assert_eq!(r[3].label, "Daylight");
        // reset for the next transport
        c.set_readout("shutter", "1/250").unwrap();
    }
}

#[test]
fn shots_from_the_camera_body_are_announced() {
    let cam = SimCamera::new(None, false);
    let mut c = Camera::open(cam.usb(), None).unwrap();
    cam.shoot();
    cam.shoot();
    let evs = c.poll_events(Duration::from_millis(50)).unwrap();
    let added: Vec<_> = evs.iter().filter(|e| e.code == ev::OBJECT_ADDED).collect();
    assert_eq!(added.len(), 2);
}

#[test]
fn live_view_only_where_known() {
    let mut plain = Camera::open(SimCamera::new(None, false).transport(), None).unwrap();
    assert!(!plain.has_live_view());
    assert!(matches!(plain.live_view_frame(), Err(PtpError::Unsupported(_))));
    let jpeg = |_n: u32| vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
    let cam = SimCamera::new(Some(Box::new(jpeg)), true);
    let mut c = Camera::open(cam.usb(), None).unwrap();
    assert_eq!(c.vendor, Vendor::Nikon);
    assert!(c.has_live_view());
    assert_eq!(c.live_view_frame().unwrap(), vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]);
    c.stop_live_view();
}

#[test]
fn camera_errors_are_errors() {
    let cam = SimCamera::new(None, false);
    let mut c = Camera::open(cam.transport(), None).unwrap();
    cam.fail_next(rc::DEVICE_BUSY);
    let e = c.capture().unwrap_err();
    assert_eq!(e, PtpError::Response(rc::DEVICE_BUSY));
    assert!(e.to_string().contains("camera busy"));
    // a second session open is tolerated
    assert!(Camera::open(cam.transport(), None).is_ok());
}

#[test]
fn datasets_round_trip() {
    let d = DeviceInfo { manufacturer: "Nikon Corporation".into(), model: "Z 6".into(), operations: vec![1, 2], ..Default::default() };
    assert_eq!(DeviceInfo::parse(&d.encode()).unwrap(), d);
    assert_eq!(d.name(), "Nikon Corporation Z 6");
    let o = ObjectInfo { storage_id: 5, format: 0x3801, size: 99, parent: 3, filename: "ÄB.NEF".into(), capture_date: "x".into() };
    assert_eq!(ObjectInfo::parse(&o.encode()).unwrap(), o);
    let p = PropDesc {
        code: prop::EXPOSURE_BIAS,
        data_type: dt::INT16,
        writable: true,
        default: PropValue::Int(0),
        current: PropValue::Int(-1000),
        form: PropForm::Range { min: -3000, max: 3000, step: 333 },
    };
    assert_eq!(PropDesc::parse(&p.encode().unwrap()).unwrap(), p);
    assert_eq!(p.choices().len(), 19);
}

#[test]
fn hostile_bytes_never_panic() {
    // truncated datasets and containers at every length
    let d = DeviceInfo { model: "M".into(), operations: vec![1; 10], ..Default::default() }.encode();
    for n in 0..d.len() {
        assert!(DeviceInfo::parse(&d[..n]).is_err());
    }
    let c = Container::with_params(kind::RESPONSE, rc::OK, 1, &[1, 2]).encode();
    for n in 0..c.len() {
        assert!(Container::decode(&c[..n]).is_err());
    }
    // absurd lengths
    let mut huge = c.clone();
    huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Container::decode(&huge).is_err());
    let mut arr = Writer::default();
    arr.u32(u32::MAX);
    assert!(Reader::new(&arr.0).array_u16().is_err());
    assert!(PropDesc::parse(&[0x0D, 0x50, 0x99, 0x99, 1]).is_err());
    let mut bad = std::io::Cursor::new(vec![3, 0, 0, 0, 6, 0, 0, 0]);
    assert!(ptpip::read_packet(&mut bad).is_err());
    let mut bad = std::io::Cursor::new(vec![0xFF, 0xFF, 0xFF, 0xFF, 6, 0, 0, 0]);
    assert!(ptpip::read_packet(&mut bad).is_err());
}

#[test]
fn labels() {
    assert_eq!(format_value(prop::EXPOSURE_TIME, &PropValue::Int(40)), "1/250");
    assert_eq!(format_value(prop::EXPOSURE_TIME, &PropValue::Int(20_000)), "2\"");
    assert_eq!(format_value(prop::EXPOSURE_TIME, &PropValue::Int(5_000)), "0.5\"");
    assert_eq!(format_value(prop::F_NUMBER, &PropValue::Int(560)), "f/5.6");
    assert_eq!(format_value(prop::F_NUMBER, &PropValue::Int(1100)), "f/11");
    assert_eq!(format_value(prop::EXPOSURE_INDEX, &PropValue::Int(400)), "ISO 400");
    assert_eq!(format_value(prop::EXPOSURE_BIAS, &PropValue::Int(-333)), "-0.3 EV");
    assert_eq!(format_value(prop::EXPOSURE_BIAS, &PropValue::Int(0xFD2F)), "-0.7 EV");
}

#[test]
fn vendors() {
    assert_eq!(Vendor::detect(vendor::ext_id::CANON, "", None), Vendor::Canon);
    assert_eq!(Vendor::detect(6, "SONY", None), Vendor::Sony);
    assert_eq!(Vendor::detect(6, "", Some(vendor::usb_id::FUJIFILM)), Vendor::Fujifilm);
    assert_eq!(Vendor::detect(6, "Acme", Some(1)), Vendor::Other);
    assert_eq!(vendor::jpeg_in(&[0, 0, 0xFF, 0xD8, 0xFF, 1]), Some(&[0xFF, 0xD8, 0xFF, 1][..]));
    assert_eq!(vendor::jpeg_in(&[0, 1, 2]), None);
}
