use super::*;
use crate::identity::{Identity, unique_name};

/// Whether multicast to the mDNS group loops back on this machine (it doesn't in some sandboxes).
fn multicast_loops_back(port: u16) -> bool {
    let Ok(rx) = mdns_socket(port) else { return false };
    hears_the_group(&rx, port)
}

/// Whether a datagram sent to the group reaches `rx`.
fn hears_the_group(rx: &UdpSocket, port: u16) -> bool {
    let Ok(tx) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return false };
    let mut buf = [0u8; 64];
    for _ in 0..5 {
        let _ = tx.send_to(b"probe", (beacon::GROUP, port));
        if rx.recv_from(&mut buf).is_ok_and(|(n, _)| &buf[..n] == b"probe") {
            return true;
        }
    }
    false
}

fn wait_within(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait(what: &str, f: impl FnMut() -> bool) {
    wait_within(what, Duration::from_secs(20), f);
}

/// "At once": seconds, where waiting for the other side's answer or timeout would be two minutes.
const AT_ONCE: Duration = Duration::from_secs(6);

/// A multicast port of this test's own, so parallel tests (and test runs) don't hear each other.
fn test_port(k: u16) -> u16 {
    40000 + (std::process::id() % 1900) as u16 * 10 + k
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lcc-peer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Puts a device in `a`'s list of found devices, as an advert would, without the network (the
/// entry doesn't age, so the test doesn't depend on multicast working here).
fn found_at(a: &Service, beacon: Beacon) {
    let seen = Instant::now() + Duration::from_secs(3600);
    lock(&a.inner.state).found.insert(beacon.device_id.clone(), Found { beacon, ip: LOCALHOST, from: LOCALHOST, seen });
}

/// `a` finds `b` (what `b` advertises right now).
fn introduce(a: &Service, b: &Service) {
    found_at(a, my_beacon(&b.inner).unwrap());
}

fn stage_of(s: &Service) -> Option<Stage> {
    s.status().pairing.map(|p| p.stage)
}

fn failure_of(s: &Service) -> Option<String> {
    match stage_of(s) {
        Some(Stage::Failed(why)) => Some(why),
        _ => None,
    }
}

fn start(dir: &std::path::Path, name: &str, k: u16) -> Service {
    let s = Service::start(dir.join(format!("{}.json", name.to_lowercase())), name, test_port(k)).unwrap();
    s.set_finding(true);
    s
}

/// `a` asks `b`; both show the code.
fn pair_to_the_code(a: &Service, b: &Service) {
    introduce(a, b);
    a.pair(&b.device_id()).unwrap();
    wait("both to show the code", || stage_of(a) == Some(Stage::Confirm) && stage_of(b) == Some(Stage::Confirm));
}

fn pair_fully(a: &Service, b: &Service) {
    pair_to_the_code(a, b);
    assert_eq!(a.status().pairing.unwrap().code, b.status().pairing.unwrap().code);
    a.confirm(true).unwrap();
    b.confirm(true).unwrap();
    wait("both paired", || stage_of(a) == Some(Stage::Paired) && stage_of(b) == Some(Stage::Paired));
    a.clear_pairing();
    b.clear_pairing();
    introduce(b, a);
}

/// Two devices, paired with each other over the loopback interface.
fn paired(tag: &str, k: u16) -> (Service, Service, PathBuf) {
    let dir = temp_dir(tag);
    let a = start(&dir, "Desk", k);
    let b = start(&dir, "Tablet", k);
    pair_fully(&a, &b);
    (a, b, dir)
}

fn paired_entry(s: &Service, device_id: &str) -> Option<PairedDevice> {
    s.status().paired.into_iter().find(|p| p.device_id == device_id)
}

/// Two services on this machine find each other over mDNS and pair; then each knows the
/// other as paired, and a third party's attempt is refused while they aren't looking.
#[test]
fn two_devices_find_each_other_and_pair() {
    // macOS LAN pairing check: discovery times out on the macOS CI runner; checked in person on
    // real Macs instead (see "Checked in person, not on CI" in docs/ci.md)
    if cfg!(target_os = "macos") && std::env::var_os("CI").is_some() {
        lcc_testkit::skip!(System, "LAN discovery on the macOS CI runner (macOS LAN pairing check, tested in person)");
        return;
    }
    let dir = temp_dir("svc");
    let port = test_port(0);
    if !multicast_loops_back(port) {
        lcc_testkit::skip!(System, "multicast doesn't loop back here");
        return;
    }
    let a = Service::start(dir.join("a.json"), "Desk", port).unwrap();
    let b = Service::start(dir.join("b.json"), "Tablet", port).unwrap();
    a.set_finding(true);
    b.set_finding(true);
    let b_id = b.status().device_id;
    let a_id = a.status().device_id;
    wait("a to see b", || a.status().found.iter().any(|f| f.device_id == b_id));
    a.pair(&b_id).unwrap();
    wait("both to show the code", || stage_of(&a) == Some(Stage::Confirm) && stage_of(&b) == Some(Stage::Confirm));
    let (ca, cb) = (a.status().pairing.unwrap().code, b.status().pairing.unwrap().code);
    assert!(ca.is_some());
    assert_eq!(ca, cb);
    a.confirm(true).unwrap();
    b.confirm(true).unwrap();
    wait("both paired", || a.status().paired.iter().any(|p| p.device_id == b_id) && b.status().paired.iter().any(|p| p.device_id == a_id));
    // the pairing is remembered
    drop(a);
    let store = Store::load_or_create(&dir.join("a.json"), "x").unwrap();
    assert!(store.peer(&b_id).is_some());
    drop(b);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_code_pairs_nobody() {
    let dir = temp_dir("no");
    let a = start(&dir, "Desk", 1);
    let b = start(&dir, "Tablet", 1);
    pair_to_the_code(&a, &b);
    // b refuses first: once a confirms, b's pairing may already have moved on.
    b.confirm(false).unwrap();
    let _ = a.confirm(true);
    wait("a to fail", || failure_of(&a).is_some());
    assert!(a.status().paired.is_empty());
    assert!(b.status().paired.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pairing works over a plain connection (no multicast needed), both sides store each other, and
/// the stored name is the one the device gave over the authenticated channel, not the advert's.
#[test]
fn a_pairing_stores_the_authenticated_name_on_both_sides() {
    let dir = temp_dir("names");
    let a = start(&dir, "Desk", 2);
    let b = start(&dir, "Tablet", 2);
    let (a_id, b_id) = (a.device_id(), b.device_id());
    // someone advertises b's id and address under another name
    let mut advert = my_beacon(&b.inner).unwrap();
    advert.name = "Evil twin".into();
    found_at(&a, advert);
    a.pair(&b_id).unwrap();
    wait("both to show the code", || stage_of(&a) == Some(Stage::Confirm) && stage_of(&b) == Some(Stage::Confirm));
    let (pa, pb) = (a.status().pairing.unwrap(), b.status().pairing.unwrap());
    assert_eq!(pa.code, pb.code);
    assert!(!pa.incoming && pb.incoming);
    assert_eq!(pa.name, "Tablet", "the name shown with the code is the authenticated one");
    a.confirm(true).unwrap();
    assert_eq!(stage_of(&b), Some(Stage::Confirm), "one yes pairs nobody");
    b.confirm(true).unwrap();
    wait("both paired", || stage_of(&a) == Some(Stage::Paired) && stage_of(&b) == Some(Stage::Paired));
    assert_eq!(paired_entry(&a, &b_id).unwrap().name, "Tablet");
    assert_eq!(paired_entry(&b, &a_id).unwrap().name, "Desk");
    assert!(Store::load_or_create(&dir.join("desk.json"), "x").unwrap().peer(&b_id).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The advert that lists a device proves nothing: if another device answers at that address, or
/// a known device answers with another key, the pairing fails before any code is shown.
#[test]
fn pairing_checks_who_answered() {
    let dir = temp_dir("who");
    let a = start(&dir, "Desk", 3);
    let b = start(&dir, "Tablet", 3);
    let b_id = b.device_id();
    // an advert for a device that doesn't exist, pointing at b
    let mut ghost = my_beacon(&b.inner).unwrap();
    ghost.device_id = "00ff00ff00ff00ff".into();
    found_at(&a, ghost);
    a.pair("00ff00ff00ff00ff").unwrap();
    wait("the pairing to fail", || failure_of(&a).is_some());
    assert!(failure_of(&a).unwrap().contains("another device answered"), "{:?}", failure_of(&a));
    assert!(a.status().pairing.unwrap().code.is_none());
    assert!(a.status().paired.is_empty() && b.status().paired.is_empty());
    a.clear_pairing();

    // a knows b under a key that isn't b's: neither direction pairs over it
    let stranger = Identity::generate("S").unwrap().public_key().unwrap();
    lock(&a.inner.store).add_peer(Peer { device_id: b_id.clone(), name: "Tablet".into(), public_key: stranger.clone(), paired_at: 1 });
    lock(&a.inner.state).not_paired_there.insert(b_id.clone());
    introduce(&a, &b);
    a.pair(&b_id).unwrap();
    wait("the pairing to fail", || failure_of(&a).is_some());
    assert!(failure_of(&a).unwrap().contains("unpair it first"), "{:?}", failure_of(&a));
    a.clear_pairing();
    wait("b to be free again", || b.status().pairing.is_none_or(|p| !matches!(p.stage, Stage::Connecting | Stage::Confirm | Stage::Waiting)));
    b.clear_pairing();
    introduce(&b, &a);
    b.pair(&a.device_id()).unwrap();
    wait("the pairing to fail", || failure_of(&b).is_some());
    assert!(failure_of(&b).unwrap().contains("another key"), "{:?}", failure_of(&b));
    assert_eq!(lock(&a.inner.store).peer(&b_id).unwrap().public_key, stranger, "the stored key is untouched");
    assert!(b.status().paired.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A side that declines, cancels or goes away is seen by the other side at once, while that
/// side is still looking at the code: not when it answers, and not after the two-minute wait.
#[test]
fn a_declined_or_cancelled_pairing_is_seen_at_once() {
    let dir = temp_dir("abort");
    let a = start(&dir, "Desk", 4);
    let b = start(&dir, "Tablet", 4);

    // b declines; a never answered
    pair_to_the_code(&a, &b);
    b.confirm(false).unwrap();
    wait_within("a to see the refusal", AT_ONCE, || failure_of(&a).is_some());
    assert!(failure_of(&a).unwrap().contains("the other device cancelled"), "{:?}", failure_of(&a));
    wait("b to end", || failure_of(&b).is_some());
    a.clear_pairing();
    b.clear_pairing();

    // a takes back its choice: gone from its own status immediately, b told
    pair_to_the_code(&a, &b);
    assert!(a.cancel_pairing());
    assert!(a.status().pairing.is_none());
    assert!(!a.cancel_pairing());
    wait_within("b to see the cancel", AT_ONCE, || failure_of(&b).is_some());
    b.clear_pairing();

    // b dismisses the request it never asked for
    pair_to_the_code(&a, &b);
    assert!(b.status().pairing.unwrap().incoming);
    assert!(b.cancel_pairing());
    wait_within("a to see the dismissal", AT_ONCE, || failure_of(&a).is_some());
    a.clear_pairing();

    // a says yes, then cancels while b still looks at the code
    pair_to_the_code(&a, &b);
    a.confirm(true).unwrap();
    wait("a to wait for b", || stage_of(&a) == Some(Stage::Waiting));
    assert!(a.cancel_pairing());
    wait_within("b to see the cancel", AT_ONCE, || failure_of(&b).is_some());
    assert!(b.confirm(true).is_err(), "a yes after the other side's cancel pairs nobody");
    b.clear_pairing();

    // a goes away altogether
    pair_to_the_code(&a, &b);
    drop(a);
    wait_within("b to see a leave", AT_ONCE, || failure_of(&b).is_some());
    assert!(b.status().paired.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host that asks to pair and then sits there doesn't keep the person from pairing with the
/// device they want: their own choice replaces the unanswered request. Their own pairing, in
/// turn, isn't replaced by a request.
#[test]
fn the_persons_choice_replaces_an_unanswered_request() {
    let dir = temp_dir("slot");
    let a = start(&dir, "Desk", 5);
    let b = start(&dir, "Tablet", 5);
    let c = start(&dir, "Squatter", 5);
    let (b_id, c_id) = (b.device_id(), c.device_id());
    introduce(&c, &a);
    c.pair(&a.device_id()).unwrap();
    wait("a to show c's request", || a.status().pairing.is_some_and(|p| p.device_id == c_id && p.incoming && p.stage == Stage::Confirm));

    introduce(&a, &b);
    a.pair(&b_id).unwrap();
    wait_within("c to be told", AT_ONCE, || failure_of(&c).is_some());
    wait("a and b to show the code", || {
        a.status().pairing.is_some_and(|p| p.device_id == b_id && !p.incoming && p.stage == Stage::Confirm) && stage_of(&b) == Some(Stage::Confirm)
    });

    // c tries again while a's own pairing is going on: refused, a's pairing untouched
    c.clear_pairing();
    c.pair(&a.device_id()).unwrap();
    wait("c to be refused", || failure_of(&c).is_some());
    assert!(failure_of(&c).unwrap().contains("someone else"), "{:?}", failure_of(&c));
    assert!(matches!(a.pair(&c_id), Err(Error::Protocol(_)) | Err(Error::Busy)), "a's own pairing isn't replaced by another choice either");
    assert!(a.status().pairing.is_some_and(|p| p.device_id == b_id && p.stage == Stage::Confirm));
    a.confirm(true).unwrap();
    b.confirm(true).unwrap();
    wait("a and b paired", || paired_entry(&a, &b_id).is_some() && stage_of(&b) == Some(Stage::Paired));
    assert!(paired_entry(&a, &c_id).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// One side unpairs. The other side learns it from the next authenticated connection, in either
/// direction, as a distinct error; shows the device as not paired there; and lists it among the
/// found devices again, so either side can pair anew.
#[test]
fn an_unpair_on_one_side_is_noticed_and_can_be_repaired() {
    let (a, b, dir) = paired("unpair", 6);
    let (a_id, b_id) = (a.device_id(), b.device_id());
    assert!(b.connect(&a_id).is_ok());
    assert!(b.status().found.iter().all(|f| f.device_id != a_id), "a device paired on both sides isn't listed as found");
    assert!(b.online_peers().iter().any(|p| p.device_id == a_id));

    assert!(a.unpair(&b_id).unwrap());
    // from the side that still thinks it is paired
    assert_eq!(b.connect(&a_id).unwrap_err(), Error::NotPaired);
    assert_eq!(b.check(&a_id).unwrap_err(), Error::NotPaired);
    let entry = paired_entry(&b, &a_id).unwrap();
    assert!(entry.not_paired_there && entry.online);
    assert_eq!(b.not_paired_there(), vec![a_id.clone()]);
    assert!(b.online_peers().is_empty(), "nothing to sync with until it is paired again");
    let listed = b.status().found.into_iter().find(|f| f.device_id == a_id).expect("listed as found again");
    assert!(listed.paired && listed.compatible);
    // from the side that unpaired
    introduce(&a, &b);
    assert_eq!(a.connect(&b_id).unwrap_err(), Error::NotPaired);
    assert!(a.status().found.iter().any(|f| f.device_id == b_id && !f.paired));
    assert!(Error::NotPaired.to_string().contains("pair them again"));

    // the side that was unpaired starts the new pairing
    pair_fully(&b, &a);
    assert!(b.not_paired_there().is_empty());
    assert!(!paired_entry(&b, &a_id).unwrap().not_paired_there);
    assert!(b.connect(&a_id).is_ok());
    assert!(a.connect(&b_id).is_ok());

    // and the other way round: the side that unpaired starts it
    assert!(a.unpair(&b_id).unwrap());
    assert_eq!(b.check(&a_id).unwrap_err(), Error::NotPaired);
    pair_fully(&a, &b);
    assert!(b.not_paired_there().is_empty());
    assert!(b.connect(&a_id).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The service asks its paired devices now and then whether they are still paired, so an unpair
/// on the other side shows up without anyone starting a sync.
#[test]
fn an_unpair_on_the_other_side_shows_up_by_itself() {
    let (a, b, dir) = paired("selfcheck", 7);
    let (a_id, b_id) = (a.device_id(), b.device_id());
    assert!(a.unpair(&b_id).unwrap());
    // as if the last check was long ago
    lock(&b.inner.state).checked.clear();
    wait("b to notice", || paired_entry(&b, &a_id).is_some_and(|p| p.not_paired_there));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A paired device's stored name follows the name it gives over the authenticated channel, in
/// both directions, and is saved; a name in an advert changes nothing by itself.
#[test]
fn a_paired_devices_new_name_is_stored_once_authenticated() {
    let (a, b, dir) = paired("rename", 8);
    let (a_id, b_id) = (a.device_id(), b.device_id());
    assert_eq!(a.rename("Studio").unwrap(), "Studio");
    // b connects: a's answer carries its name
    drop(b.connect(&a_id).unwrap());
    assert_eq!(paired_entry(&b, &a_id).unwrap().name, "Studio");
    assert_eq!(Store::load_or_create(&dir.join("tablet.json"), "x").unwrap().peer(&a_id).unwrap().name, "Studio");
    // b's hello carries b's name
    assert_eq!(b.rename("Slate").unwrap(), "Slate");
    b.check(&a_id).unwrap();
    wait("a to store b's name", || paired_entry(&a, &b_id).is_some_and(|p| p.name == "Slate"));
    assert_eq!(Store::load_or_create(&dir.join("desk.json"), "x").unwrap().peer(&b_id).unwrap().name, "Slate");

    // an advert under a's id with another name: not stored, and the check it prompts asks a itself
    let mut spoof = my_beacon(&a.inner).unwrap();
    spoof.name = "Evil".into();
    {
        let paired: HashSet<String> = [a_id.clone()].into();
        let mut st = lock(&b.inner.state);
        take_answers(&mut st.found, &advert(&spoof, &beacon::service_type(), 10), LOCALHOST, &beacon::service_type(), &b_id, &paired, Instant::now());
        assert_eq!(st.found.get(&a_id).unwrap().beacon.name, "Evil");
    }
    assert_eq!(paired_entry(&b, &a_id).unwrap().name, "Studio");
    b.check(&a_id).unwrap();
    assert_eq!(paired_entry(&b, &a_id).unwrap().name, "Studio");
    // hostile names over the authenticated channel are cleaned like any other
    note_name(&b.inner, &a_id, &format!("\u{7}{}", "x".repeat(500)));
    assert_eq!(paired_entry(&b, &a_id).unwrap().name.chars().count(), crate::identity::MAX_NAME);
    note_name(&b.inner, &a_id, "\u{1}\u{2}  ");
    assert_eq!(paired_entry(&b, &a_id).unwrap().name.chars().count(), crate::identity::MAX_NAME, "an empty name is ignored");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A device on another version of the protocol is listed, marked, and refused with the error
/// that says what to do.
#[test]
fn a_device_on_another_protocol_version_is_refused_up_front() {
    let (a, b, dir) = paired("version", 9);
    let b_id = b.device_id();
    let mut old = my_beacon(&b.inner).unwrap();
    old.proto = 1;
    found_at(&a, old.clone());
    assert_eq!(a.connect(&b_id).unwrap_err(), Error::Version);
    assert_eq!(a.check(&b_id).unwrap_err(), Error::Version);
    old.device_id = "0123456789abcdef".into();
    found_at(&a, old);
    assert_eq!(a.pair("0123456789abcdef").unwrap_err(), Error::Version);
    assert!(a.status().pairing.is_none());
    assert!(a.status().found.iter().any(|f| f.device_id == "0123456789abcdef" && !f.compatible));
    assert!(Error::Version.to_string().contains("same app version"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_shared_device_name_is_reported() {
    let dir = temp_dir("collide");
    let a = start(&dir, "Desk", 10);
    let b = start(&dir, "Tablet", 10);
    introduce(&a, &b);
    assert!(!a.name_collides() && !a.status().name_collides);
    b.rename(" desk").unwrap();
    introduce(&a, &b);
    assert!(a.name_collides() && a.status().name_collides);
    assert!(a.taken_names().contains(&"desk".to_string()));
    assert_eq!(unique_name("Desk", &a.taken_names()), "Desk 2");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A damaged store doesn't keep the service from starting: it is set aside and reported.
#[test]
fn a_damaged_store_is_set_aside_at_start() {
    let dir = temp_dir("lost");
    let path = dir.join("peers.json");
    std::fs::write(&path, b"{\"identity\": 7").unwrap();
    let s = Service::start(path.clone(), "Desk", test_port(11)).unwrap();
    assert_eq!(s.pairings_lost(), Some(dir.join("peers.json.corrupt")));
    assert!(s.status().pairings_lost);
    assert!(dir.join("peers.json.corrupt").is_file());
    let id = s.device_id();
    drop(s);
    let s = Service::start(path, "Desk", test_port(11)).unwrap();
    assert!(s.pairings_lost().is_none() && !s.status().pairings_lost);
    assert_eq!(s.device_id(), id);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What an unpaired device may send after a good handshake: nothing it sends panics the
/// service, takes the pairing slot for longer than it stays, or is answered as if it were paired.
#[test]
fn hostile_openings_are_refused() {
    let dir = temp_dir("openings");
    let s = start(&dir, "Desk", 12);
    let connect = || {
        let me = Identity::generate("Stranger").unwrap();
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, s.inner.tcp_port)).unwrap();
        let mut ch = Channel::handshake(stream, &me, true).unwrap();
        ch.set_deadline(Duration::from_secs(10));
        ch
    };
    let junk = [
        serde_json::json!({"t": "bogus"}),
        serde_json::json!({"t": "pair"}),
        serde_json::json!({"t": "pair", "name": 5}),
        serde_json::json!({"name": "x"}),
        serde_json::json!([1, 2, 3]),
        serde_json::json!("pair"),
        serde_json::json!({"t": "decision", "ok": true}),
        serde_json::json!({"t": "ready", "name": "x"}),
    ];
    for opening in junk {
        let mut ch = connect();
        ch.send_json(&opening).unwrap();
        assert!(ch.recv().is_err(), "{opening} was answered");
    }
    // binary data, garbage, and silence where the opening belongs
    let mut ch = connect();
    ch.send_bytes(b"{\"t\":\"hello\"}").unwrap();
    assert!(ch.recv().is_err());
    let mut ch = connect();
    ch.send(b"\xff\xfe").unwrap();
    assert!(ch.recv().is_err());
    let mut ch = connect();
    let waited = Instant::now();
    assert!(ch.recv().is_err());
    assert!(waited.elapsed() < Duration::from_secs(8), "a silent connection is dropped after a few seconds");

    // a paired device's openings from a device that isn't paired
    for opening in [
        serde_json::json!({"t": "hello"}),
        serde_json::json!({"t": "hello", "name": "x".repeat(60_000)}),
        serde_json::json!({"t": "check", "name": "Tablet"}),
    ] {
        let mut ch = connect();
        ch.send_json(&opening).unwrap();
        assert!(matches!(ch.recv_json::<Reply>(), Ok(Reply::NotPaired)));
    }
    assert!(s.status().paired.is_empty() && s.status().pairing.is_none());
    // none of those asked to pair after learning the code: each counts as a possible guess at it
    wait("them to be counted", || lock(&s.inner.abandoned).len() >= 14);
    lock(&s.inner.abandoned).clear();

    // a pairing request with a hostile name, then nothing: shown cleaned, gone when the host goes
    let mut ch = connect();
    ch.send_json(&serde_json::json!({"t": "pair", "name": format!("\u{7}\u{1b}[31m{}", "y".repeat(5000))})).unwrap();
    assert!(matches!(ch.recv_json::<Reply>(), Ok(Reply::Ready { .. })));
    wait("the request to show", || stage_of(&s) == Some(Stage::Confirm));
    let shown = s.status().pairing.unwrap();
    assert!(shown.incoming && shown.name.chars().count() <= crate::identity::MAX_NAME && !shown.name.chars().any(char::is_control));
    // answers out of turn end it
    ch.send_json(&serde_json::json!({"t": "ready", "name": "x"})).unwrap();
    wait_within("the request to end", AT_ONCE, || failure_of(&s).is_some());
    s.clear_pairing();
    let mut ch = connect();
    ch.send_json(&serde_json::json!({"t": "pair", "name": "Host"})).unwrap();
    assert!(matches!(ch.recv_json::<Reply>(), Ok(Reply::Ready { .. })));
    wait("the request to show", || stage_of(&s) == Some(Stage::Confirm));
    drop(ch);
    wait_within("the request to end", AT_ONCE, || failure_of(&s).is_some());
    // every one of those connections gave its slot back
    wait("the slots to come back", || s.inner.connections.load(Ordering::Acquire) == 0 && s.inner.unknown_connections.load(Ordering::Acquire) == 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Opening connection after connection and dropping each (what guessing at a pairing code looks
/// like from the device being guessed at) gets pairing requests refused for a while.
#[test]
fn many_unfinished_handshakes_get_pairing_requests_refused() {
    use std::io::Write as _;
    let dir = temp_dir("abandoned");
    let a = start(&dir, "Desk", 13);
    let b = start(&dir, "Tablet", 13);
    assert!(!too_many_abandoned(&b.inner));
    for _ in 0..MAX_ABANDONED {
        // a commitment, b's hello comes back, and the "guess" is dropped
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, b.inner.tcp_port)).unwrap();
        let commit =
            serde_json::to_vec(&serde_json::json!({"proto": crate::secure::PROTOCOL, "commit": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}))
                .unwrap();
        s.write_all(&(commit.len() as u32).to_be_bytes()).unwrap();
        s.write_all(&commit).unwrap();
        let mut hello = [0u8; 4];
        std::io::Read::read_exact(&mut s, &mut hello).unwrap();
    }
    wait("b to count them", || too_many_abandoned(&b.inner));
    introduce(&a, &b);
    a.pair(&b.device_id()).unwrap();
    wait("a to be refused", || failure_of(&a).is_some());
    assert!(failure_of(&a).unwrap().contains("unfinished"), "{:?}", failure_of(&a));
    assert!(b.status().pairing.is_none());
    // an old build's hello is not a guess: it isn't counted
    let before = lock(&b.inner.abandoned).len();
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, b.inner.tcp_port)).unwrap();
    let v1 = br#"{"device":"ab","key":"k","eph":"e"}"#;
    s.write_all(&(v1.len() as u32).to_be_bytes()).unwrap();
    s.write_all(v1).unwrap();
    let mut end = [0u8; 1];
    let _ = std::io::Read::read(&mut s, &mut end);
    assert_eq!(lock(&b.inner.abandoned).len(), before);
    // once they are forgotten, pairing works again
    lock(&b.inner.abandoned).clear();
    a.clear_pairing();
    pair_to_the_code(&a, &b);
    let _ = std::fs::remove_dir_all(&dir);
}

fn advert_of(id: &str, pairing: bool, ttl: u32) -> Message {
    let b = Beacon {
        app: lcc_brand::ENV_PREFIX.into(),
        v: beacon::VERSION,
        device_id: id.into(),
        name: "Tablet".into(),
        key: "k".into(),
        port: 4000,
        pairing,
        proto: beacon::PROTO,
    };
    advert(&b, &beacon::service_type(), ttl)
}

/// A sender address of its own for each `i`.
fn source(i: usize) -> IpAddr {
    Ipv4Addr::new(10, 1, (i / 250) as u8, (i % 250) as u8 + 1).into()
}

#[test]
fn answers_fill_and_empty_the_found_list() {
    let stype = beacon::service_type();
    let src: IpAddr = Ipv4Addr::new(10, 0, 0, 9).into();
    let now = Instant::now();
    let none = HashSet::new();
    let mut found = HashMap::new();
    take_answers(&mut found, &advert_of("ab12", true, 10), src, &stype, "ff", &none, now);
    let f = found.get("ab12").unwrap();
    assert!(f.beacon.pairing);
    assert_eq!(f.beacon.port, 4000);
    // our own advert is not a found device
    take_answers(&mut found, &advert_of("ff", true, 10), src, &stype, "ff", &none, now);
    assert!(!found.contains_key("ff"));
    // a goodbye removes it
    take_answers(&mut found, &advert_of("ab12", true, 0), src, &stype, "ff", &none, now);
    assert!(found.is_empty());
    // the list is capped
    for i in 0..(MAX_FOUND + 20) {
        take_answers(&mut found, &advert_of(&format!("{i:x}"), false, 10), source(i), &stype, "ff", &none, now);
    }
    assert_eq!(found.len(), MAX_FOUND);
}

/// A host sending adverts for made-up devices can't push the real ones off the list: paired
/// devices stay whatever arrives, one sender holds only a few entries, and a full list makes room
/// for a new device by dropping the entry not heard from for longest.
#[test]
fn a_flood_of_adverts_leaves_room_for_real_devices() {
    let stype = beacon::service_type();
    let start = Instant::now();
    let at = |i: usize| start + Duration::from_millis(i as u64);
    let paired: HashSet<String> = ["aa01".to_string()].into();
    let mut found = HashMap::new();
    take_answers(&mut found, &advert_of("aa01", false, 10), source(0), &stype, "ff", &paired, at(0));

    // one sender, many devices: a handful are kept, the latest among them
    let flooder: IpAddr = Ipv4Addr::new(10, 9, 9, 9).into();
    for i in 0..1000 {
        take_answers(&mut found, &advert_of(&format!("f{i:x}"), true, 10), flooder, &stype, "ff", &paired, at(i));
    }
    assert_eq!(found.values().filter(|f| f.from == flooder).count(), MAX_PER_SOURCE);
    assert!(found.contains_key(&format!("f{:x}", 999)));
    assert!(found.contains_key("aa01"));

    // many senders (forged addresses): the list fills, the paired device stays
    for i in 0..(MAX_FOUND * 3) {
        take_answers(&mut found, &advert_of(&format!("e{i:x}"), true, 10), source(i + 1), &stype, "ff", &paired, at(1000 + i));
    }
    assert_eq!(found.len(), MAX_FOUND);
    assert!(found.contains_key("aa01"), "a paired device is never pushed out");

    // a real device announces into the full list: it gets in
    let real: IpAddr = Ipv4Addr::new(192, 168, 1, 7).into();
    take_answers(&mut found, &advert_of("bb02", true, 10), real, &stype, "ff", &paired, at(5000));
    assert!(found.contains_key("bb02"));
    assert_eq!(found.len(), MAX_FOUND);
    // and stays while it keeps announcing between the made-up ones
    for i in 0..(MAX_FOUND * 3) {
        take_answers(&mut found, &advert_of(&format!("d{i:x}"), true, 10), source(i + 1), &stype, "ff", &paired, at(5001 + i));
        if i % 100 == 0 {
            take_answers(&mut found, &advert_of("bb02", true, 10), real, &stype, "ff", &paired, at(5001 + i));
        }
    }
    assert!(found.contains_key("bb02") && found.contains_key("aa01"));
    assert_eq!(found.len(), MAX_FOUND);

    // a paired device gets in even when everything listed is paired too
    let all: HashSet<String> = found.keys().cloned().chain(["cc03".to_string()]).collect();
    take_answers(&mut found, &advert_of("cc03", false, 10), real, &stype, "ff", &all, at(9000));
    assert!(found.contains_key("cc03"));
    // an unpaired one doesn't push a paired one out
    take_answers(&mut found, &advert_of("dd04", true, 10), source(7), &stype, "ff", &all, at(9001));
    assert!(!found.contains_key("dd04"));
    assert_eq!(found.len(), MAX_FOUND + 1);
}

#[test]
fn hostile_answers_are_ignored() {
    let stype = beacon::service_type();
    let src: IpAddr = Ipv4Addr::LOCALHOST.into();
    let instance = format!("ab.{stype}");
    let mut found = HashMap::new();
    let cases = [
        // TXT without an SRV, SRV without a TXT, an SRV for another service type, a TXT of junk
        vec![Record { name: instance.clone(), ttl: 10, flush: true, data: Data::Txt(vec![b"id=ab".to_vec()]) }],
        vec![Record { name: instance.clone(), ttl: 10, flush: true, data: Data::Srv { port: 1, target: "x.local".into() } }],
        vec![
            Record { name: "ab._other._tcp.local".into(), ttl: 10, flush: true, data: Data::Srv { port: 1, target: "x".into() } },
            Record { name: "ab._other._tcp.local".into(), ttl: 10, flush: true, data: advert_of("ab", true, 10).records[2].data.clone() },
        ],
        vec![
            Record { name: instance.clone(), ttl: 10, flush: true, data: Data::Srv { port: 1, target: "x".into() } },
            Record { name: instance.clone(), ttl: 10, flush: true, data: Data::Txt(vec![vec![0xff; 255]; 40]) },
        ],
    ];
    for records in cases {
        let msg = Message { flags: dns::FLAGS_RESPONSE, questions: vec![], records };
        take_answers(&mut found, &msg, src, &stype, "ff", &HashSet::new(), Instant::now());
    }
    assert!(found.is_empty());
}

#[test]
fn the_group_is_joined_again_on_a_schedule_and_soon_after_a_failure() {
    let now = Instant::now() + Duration::from_secs(3600);
    let ago = |secs: u64| Some(now - Duration::from_secs(secs));
    assert!(rejoin_due(None, false, now), "never joined");
    assert!(!rejoin_due(ago(1), false, now));
    assert!(!rejoin_due(ago(29), false, now));
    assert!(rejoin_due(ago(30), false, now));
    assert!(rejoin_due(ago(3), true, now), "a failed send or read brings it forward");
    assert!(!rejoin_due(ago(1), true, now), "but not into a tight loop");
    assert!(!rejoin_due(Some(now + Duration::from_secs(5)), true, now), "a clock that went backwards is no reason");
}

/// Joining again neither fails nor piles up memberships, and the group is heard afterwards:
/// also when the membership was lost in between (the interface went away and came back).
#[test]
fn joining_the_group_again_keeps_it_audible() {
    let port = test_port(14);
    let audible = multicast_loops_back(port);
    let rx = mdns_socket(port).unwrap();
    let mut joined = vec![Ipv4Addr::UNSPECIFIED];
    let first = join_group(&rx, &mut joined);
    for _ in 0..20 {
        assert_eq!(join_group(&rx, &mut joined), first);
    }
    assert!(first <= MAX_INTERFACES + 1);
    let mut unique = joined.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), joined.len());
    if !audible {
        lcc_testkit::skip!(System, "multicast doesn't loop back here");
        return;
    }
    assert!(first >= 1);
    assert!(hears_the_group(&rx, port));
    // every membership gone, as after the interface was taken away
    for ip in joined.iter().rev() {
        let _ = rx.leave_multicast_v4(&beacon::GROUP, ip);
    }
    assert!(join_group(&rx, &mut joined) >= 1);
    assert!(hears_the_group(&rx, port));
}

fn bare_inner(tag: &str) -> Arc<Inner> {
    let path = std::env::temp_dir().join(format!("lcc-peer-{tag}-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Store::load_or_create(&path, "Test").unwrap();
    Arc::new(Inner::new(store, path, 0, None))
}

/// Past the cap, a new connection is closed at once instead of getting a thread.
#[test]
fn connections_past_the_cap_are_closed() {
    use std::io::Read;
    let inner = bare_inner("cap");
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let addr = tcp.local_addr().unwrap();
    let held: Vec<ConnSlot> = (0..MAX_CONNECTIONS).map(|i| ConnSlot::take(&inner, i >= MAX_UNKNOWN_CONNECTIONS).unwrap()).collect();
    assert!(ConnSlot::take(&inner, true).is_none());
    assert!(ConnSlot::take(&inner, false).is_none());
    let i = inner.clone();
    let t = std::thread::spawn(move || accept_loop(&i, &tcp));

    let mut c = TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut b = [0u8; 1];
    // closed by the service: EOF (or a reset), not a timeout
    match c.read(&mut b) {
        Ok(n) => assert_eq!(n, 0),
        Err(e) => assert!(!matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "{e}"),
    }

    drop(held);
    assert_eq!(inner.connections.load(Ordering::Acquire), 0);
    assert_eq!(inner.unknown_connections.load(Ordering::Acquire), 0);
    // with room again, a connection is served: a garbage handshake gets dropped, and the slot comes back
    let mut c = TcpStream::connect(addr).unwrap();
    use std::io::Write;
    c.write_all(&[0xff; 4]).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = c.read(&mut b);
    wait("slot returned", || inner.connections.load(Ordering::Acquire) == 0);
    inner.stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
    let _ = std::fs::remove_file(&inner.store_path);
}

/// Hosts no paired device was heard from can hold only part of the connection slots, and each
/// only for a few seconds without a handshake: paired devices still get through.
#[test]
fn idle_strangers_cant_hold_every_connection_slot() {
    use std::io::Read;
    let inner = bare_inner("strangers");
    // the slots: strangers fill their share, the rest stays for paired devices' addresses
    let strangers: Vec<ConnSlot> = (0..MAX_UNKNOWN_CONNECTIONS).map(|_| ConnSlot::take(&inner, false).unwrap()).collect();
    assert!(ConnSlot::take(&inner, false).is_none(), "the strangers' share is used up");
    let paired: Vec<ConnSlot> = (0..(MAX_CONNECTIONS - MAX_UNKNOWN_CONNECTIONS)).map(|_| ConnSlot::take(&inner, true).unwrap()).collect();
    assert!(ConnSlot::take(&inner, true).is_none());
    drop(paired);
    assert!(ConnSlot::take(&inner, false).is_none(), "a failed try doesn't use up a slot, and paired slots aren't the strangers'");
    assert!(ConnSlot::take(&inner, true).is_some());
    drop(strangers);
    assert_eq!((inner.connections.load(Ordering::Acquire), inner.unknown_connections.load(Ordering::Acquire)), (0, 0));

    // which address counts as a paired device's: one a paired device was heard at, lately
    let paired_ip: IpAddr = Ipv4Addr::new(192, 168, 1, 7).into();
    let other_ip: IpAddr = Ipv4Addr::new(192, 168, 1, 8).into();
    lock(&inner.store).add_peer(Peer { device_id: "aa01".into(), name: "Tablet".into(), public_key: "k".into(), paired_at: 1 });
    {
        let stype = beacon::service_type();
        let paired: HashSet<String> = ["aa01".to_string()].into();
        let mut st = lock(&inner.state);
        take_answers(&mut st.found, &advert_of("aa01", false, 10), paired_ip, &stype, "ff", &paired, Instant::now());
        take_answers(&mut st.found, &advert_of("bb02", true, 10), other_ip, &stype, "ff", &paired, Instant::now());
    }
    assert!(heard_paired_at(&inner, paired_ip));
    assert!(!heard_paired_at(&inner, other_ip));
    assert!(!heard_paired_at(&inner, LOCALHOST));

    // a stranger that connects and says nothing is dropped after the few pre-handshake seconds
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let addr = tcp.local_addr().unwrap();
    let i = inner.clone();
    let t = std::thread::spawn(move || accept_loop(&i, &tcp));
    let started = Instant::now();
    let mut idle: Vec<TcpStream> = (0..MAX_UNKNOWN_CONNECTIONS).map(|_| TcpStream::connect(addr).unwrap()).collect();
    wait("the strangers to be served", || inner.unknown_connections.load(Ordering::Acquire) == MAX_UNKNOWN_CONNECTIONS);
    let mut b = [0u8; 1];
    for c in &mut idle {
        c.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        match c.read(&mut b) {
            Ok(n) => assert_eq!(n, 0),
            Err(e) => assert!(!matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "{e}"),
        }
    }
    assert!(started.elapsed() < PRE_AUTH_WITHIN + Duration::from_secs(5), "took {:?}", started.elapsed());
    wait("the slots to come back", || inner.connections.load(Ordering::Acquire) == 0 && inner.unknown_connections.load(Ordering::Acquire) == 0);
    inner.stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
    let _ = std::fs::remove_file(&inner.store_path);
}
