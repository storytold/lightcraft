//! Fuzz target `peer` (`cargo xtask fuzz peer`): what another device on the network sends before it is
//! trusted. Mutated mDNS packets through `dns::Message::decode` and the beacon TXT parser, and (every
//! eighth input) mutated handshake frames written to a loopback socket that runs the responder side of
//! `Channel::handshake`. A short run in `cargo test`; time-boxed long runs under `LCC_FUZZ_SECS`.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use lcc_peer::dns::{Data, FLAGS_RESPONSE, Message, Question, Record, TYPE_PTR, TYPE_TXT};
use lcc_peer::{Channel, Identity, beacon};

fn frame(json: &str) -> Vec<u8> {
    let mut v = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
    v.extend_from_slice(json.as_bytes());
    v
}

#[test]
fn fuzz_target() {
    let b = beacon::Beacon {
        app: "X".into(),
        v: beacon::VERSION,
        device_id: "dev-1".into(),
        name: "Laptop".into(),
        key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        port: 4000,
        pairing: true,
        proto: beacon::PROTO,
    };
    let name = beacon::service_type();
    let msg = Message {
        flags: FLAGS_RESPONSE,
        questions: vec![Question { name: name.clone(), qtype: TYPE_PTR }],
        records: vec![
            Record { name: name.clone(), ttl: 120, flush: false, data: Data::Ptr(format!("dev-1.{name}")) },
            Record { name: format!("dev-1.{name}"), ttl: 120, flush: true, data: Data::Txt(b.txt()) },
            Record { name: "host.local".into(), ttl: 120, flush: true, data: Data::A(std::net::Ipv4Addr::new(10, 0, 0, 2)) },
        ],
    };
    let seeds = [
        msg.encode().unwrap(),
        Message { flags: 0, questions: vec![Question { name, qtype: TYPE_TXT }], records: vec![] }.encode().unwrap(),
        frame(r#"{"commit":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#),
        frame(r#"{"v":2,"protocol":"lcc-peer-v2","device_id":"d","name":"n","key":"AAAA","nonce":"AAAA"}"#),
    ];
    let me = Identity::generate("Fuzz").unwrap();
    let mut n = 0usize;
    lcc_fuzzkit::run_target("peer", &seeds, &[], 600, |bytes| {
        if let Some(m) = Message::decode(bytes) {
            for r in &m.records {
                if let Data::Txt(t) = &r.data {
                    let _ = beacon::Beacon::from_txt(t, 4000);
                }
            }
        }
        let parts: Vec<Vec<u8>> = bytes.split(|&c| c == 0).map(<[u8]>::to_vec).collect();
        let _ = beacon::Beacon::from_txt(&parts, 4000);
        n += 1;
        if n.is_multiple_of(8) {
            let Ok(l) = TcpListener::bind("127.0.0.1:0") else { return };
            let Ok(addr) = l.local_addr() else { return };
            let junk = bytes.to_vec();
            let t = std::thread::spawn(move || {
                if let Ok(mut s) = TcpStream::connect(addr) {
                    let _ = s.write_all(&junk);
                }
            });
            // Never a blocking accept: if the connect fails (e.g. ephemeral ports used up by
            // TIME_WAIT after millions of loopback connections) nothing would ever arrive.
            if l.set_nonblocking(true).is_ok() {
                let until = std::time::Instant::now() + Duration::from_secs(2);
                while std::time::Instant::now() < until {
                    match l.accept() {
                        Ok((s, _)) => {
                            if s.set_nonblocking(false).is_ok() {
                                let _ = Channel::handshake_within(s, &me, false, Duration::from_secs(2));
                            }
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                        Err(_) => break,
                    }
                }
            }
            let _ = t.join();
        }
    });
}
