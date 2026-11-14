//! The running peer service: mDNS adverts out, adverts in, connections in, and the pairing in progress.
//!
//! Discovery is multicast DNS service discovery (RFC 6762 / RFC 6763, see [`crate::beacon`]):
//! while looking for devices or when paired devices exist, one thread announces this device every
//! couple of seconds (and a goodbye when it stops), and browses for the service type; one thread
//! reads the multicast group (answers, and queries it answers by announcing); one accepts
//! connections, and each connection gets its own short-lived thread. Every loop checks the stop flag at least twice a second, so dropping the [`Service`]
//! ends them promptly. Nothing here panics on what the network sends.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::beacon::{self, Beacon};
use crate::dns::{self, Data, Message, Question, Record};
use crate::identity::{Peer, Store, clean_name};
use crate::secure::Channel;
use crate::{Error, Result};

/// How often this device announces itself.
const BEACON_EVERY: Duration = Duration::from_secs(2);
/// How often it asks who else is there.
const QUERY_EVERY: Duration = Duration::from_millis(2500);
/// The TTL of the advertised records (short: devices come and go).
const RECORD_TTL: u32 = 10;
/// The most devices remembered; a busy network can't grow the list without bound.
const MAX_FOUND: usize = 256;
/// A device not heard from for this long is no longer listed (or online).
const GONE_AFTER: Duration = Duration::from_secs(8);
/// How long a pairing waits for the people to compare codes.
const DECIDE_WITHIN: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most incoming connections served at once; more are closed straight away, so a busy or
/// hostile network can't make this device start a thread per connection without bound.
const MAX_CONNECTIONS: usize = 16;
/// How long a pairing connection may take to say what it wants after the handshake.
const OPENING_WITHIN: Duration = Duration::from_secs(10);

/// Where a pairing is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "stage", content = "message")]
pub enum Stage {
    Connecting,
    /// Both screens show the code; waiting for this side's answer.
    Confirm,
    /// This side said yes; waiting for the other.
    Waiting,
    Paired,
    Failed(String),
}

struct Pairing {
    device_id: String,
    name: String,
    code: Option<String>,
    stage: Stage,
    decide: Option<Sender<bool>>,
}

struct Found {
    beacon: Beacon,
    ip: IpAddr,
    seen: Instant,
}

#[derive(Default)]
struct State {
    found: HashMap<String, Found>,
    pairing: Option<Pairing>,
}

struct Inner {
    stop: AtomicBool,
    finding: AtomicBool,
    /// Ask the beacon thread to send now (a change worth announcing).
    nudge: AtomicBool,
    state: Mutex<State>,
    store: Mutex<Store>,
    store_path: PathBuf,
    tcp_port: u16,
    /// Takes a paired device's connection once it has said hello (the catalog exchange).
    on_hello: Mutex<Option<HelloHandler>>,
    /// Incoming connections being served now.
    connections: AtomicUsize,
}

/// Holds one of the [`MAX_CONNECTIONS`] slots; gives it back when dropped.
struct ConnSlot(Arc<Inner>);

impl ConnSlot {
    fn take(inner: &Arc<Inner>) -> Option<ConnSlot> {
        let slot = ConnSlot(inner.clone());
        // counted first, so the drop gives it back whether or not it fits
        (inner.connections.fetch_add(1, Ordering::AcqRel) < MAX_CONNECTIONS).then_some(slot)
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What runs a paired device's connection after the hello: the paired device and the channel.
pub type HelloHandler = Arc<dyn Fn(Peer, Channel) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What Settings ▸ Sync shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub device_id: String,
    pub name: String,
    pub finding: bool,
    /// Devices looking for a pairing right now (not yet paired with this one).
    pub found: Vec<FoundDevice>,
    pub paired: Vec<PairedDevice>,
    pub pairing: Option<PairingStatus>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundDevice {
    pub device_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub device_id: String,
    pub name: String,
    /// Heard on the network in the last few seconds.
    pub online: bool,
    pub paired_at: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    pub device_id: String,
    pub name: String,
    pub code: Option<String>,
    #[serde(flatten)]
    pub stage: Stage,
}

/// The first message on a connection.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Opening {
    /// Asks to pair (the receiver must be looking for devices).
    Pair { name: String },
    /// A paired device saying hello (the catalog exchange starts from here).
    Hello,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Reply {
    Ready { name: String },
    No { why: String },
    Decision { ok: bool },
}

/// The peer service; dropping it stops every thread.
pub struct Service {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").field("tcp_port", &self.inner.tcp_port).finish_non_exhaustive()
    }
}

/// The multicast DNS socket: shared with other responders on the machine (the system's own, other
/// copies of the app), joined to the mDNS group.
fn mdns_socket(port: u16) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    let udp: UdpSocket = s.into();
    // no multicast route (no network yet): keep running, nothing is found until there is one
    if let Err(e) = udp.join_multicast_v4(&beacon::GROUP, &Ipv4Addr::UNSPECIFIED) {
        log::info!("peer: can't join the mDNS group: {e}");
    }
    udp.set_multicast_loop_v4(true)?;
    udp.set_multicast_ttl_v4(255)?;
    udp.set_read_timeout(Some(Duration::from_millis(400)))?;
    Ok(udp)
}

impl Service {
    /// Opens (or creates) the store at `store_path` and starts listening. `default_name` names a
    /// new device. `port` is the multicast DNS port ([`beacon::PORT`], 5353; tests pass their own
    /// so they neither hear the network nor each other).
    pub fn start(store_path: PathBuf, default_name: &str, port: u16) -> Result<Service> {
        let store = Store::load_or_create(&store_path, default_name)?;
        let udp = mdns_socket(port)?;
        let tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        tcp.set_nonblocking(true)?;
        let tcp_port = tcp.local_addr()?.port();
        let inner = Arc::new(Inner {
            stop: AtomicBool::new(false),
            finding: AtomicBool::new(false),
            nudge: AtomicBool::new(true),
            state: Mutex::new(State::default()),
            store: Mutex::new(store),
            store_path,
            tcp_port,
            on_hello: Mutex::new(None),
            connections: AtomicUsize::new(0),
        });
        let mut threads = Vec::new();
        let spawn =
            |name: &str, f: Box<dyn FnOnce() + Send>| std::thread::Builder::new().name(name.into()).spawn(f).map_err(|e| Error::Io(e.to_string()));
        let send_udp = udp.try_clone()?;
        let i = inner.clone();
        threads.push(spawn("peer-announce", Box::new(move || announce_loop(&i, &send_udp, port)))?);
        let i = inner.clone();
        threads.push(spawn("peer-listen", Box::new(move || listen_loop(&i, &udp)))?);
        let i = inner.clone();
        threads.push(spawn("peer-accept", Box::new(move || accept_loop(&i, &tcp)))?);
        Ok(Service { inner, threads })
    }

    /// Looks for devices to pair with (or stops looking).
    pub fn set_finding(&self, on: bool) {
        if self.inner.finding.swap(on, Ordering::Relaxed) != on {
            self.inner.nudge.store(true, Ordering::Relaxed);
        }
    }

    pub fn finding(&self) -> bool {
        self.inner.finding.load(Ordering::Relaxed)
    }

    pub fn has_peers(&self) -> bool {
        !lock(&self.inner.store).peers.is_empty()
    }

    pub fn status(&self) -> Status {
        let store = lock(&self.inner.store).clone();
        let mut st = lock(&self.inner.state);
        st.found.retain(|_, f| f.seen.elapsed() < GONE_AFTER);
        let mut found: Vec<FoundDevice> = st
            .found
            .values()
            .filter(|f| f.beacon.pairing && store.peer(&f.beacon.device_id).is_none())
            .map(|f| FoundDevice { device_id: f.beacon.device_id.clone(), name: f.beacon.name.clone() })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name).then(a.device_id.cmp(&b.device_id)));
        let paired = store
            .peers
            .iter()
            .map(|p| PairedDevice {
                device_id: p.device_id.clone(),
                name: p.name.clone(),
                online: st.found.contains_key(&p.device_id),
                paired_at: p.paired_at,
            })
            .collect();
        let pairing = st.pairing.as_ref().map(|p| PairingStatus {
            device_id: p.device_id.clone(),
            name: p.name.clone(),
            code: p.code.clone(),
            stage: p.stage.clone(),
        });
        Status { device_id: store.identity.device_id.clone(), name: store.identity.name.clone(), finding: self.finding(), found, paired, pairing }
    }

    /// Starts pairing with a device that is looking for one.
    pub fn pair(&self, device_id: &str) -> Result<()> {
        let (addr, name) = {
            let mut st = lock(&self.inner.state);
            if st.pairing.as_ref().is_some_and(|p| matches!(p.stage, Stage::Connecting | Stage::Confirm | Stage::Waiting)) {
                return Err(Error::Busy);
            }
            let f = st
                .found
                .get(device_id)
                .filter(|f| f.beacon.pairing)
                .ok_or_else(|| Error::Protocol("that device isn't looking for a pairing now".into()))?;
            let addr = SocketAddr::new(f.ip, f.beacon.port);
            let name = f.beacon.name.clone();
            st.pairing = Some(Pairing { device_id: device_id.to_string(), name: name.clone(), code: None, stage: Stage::Connecting, decide: None });
            (addr, name)
        };
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new().name("peer-pair".into()).spawn(move || {
            let r = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(Error::from).and_then(|s| {
                let me = lock(&inner.store).identity.clone();
                let mut ch = Channel::handshake(s, &me, true)?;
                ch.set_deadline(OPENING_WITHIN);
                ch.send_json(&Opening::Pair { name: me.name.clone() })?;
                match ch.recv_json::<Reply>()? {
                    Reply::Ready { .. } => {}
                    Reply::No { why } => return Err(Error::Protocol(why)),
                    Reply::Decision { .. } => return Err(Error::Protocol("unexpected answer".into())),
                }
                decide(&inner, ch, &name)
            });
            finish(&inner, r);
        });
        if let Err(e) = spawned {
            let e = Error::Io(e.to_string());
            finish(&self.inner, Err(e.clone()));
            return Err(e);
        }
        Ok(())
    }

    /// Sets what runs a paired device's incoming connection after its hello.
    pub fn set_hello_handler(&self, handler: HelloHandler) {
        *lock(&self.inner.on_hello) = Some(handler);
    }

    /// The paired devices heard on the network in the last few seconds.
    pub fn online_peers(&self) -> Vec<Peer> {
        let store = lock(&self.inner.store).clone();
        let st = lock(&self.inner.state);
        store.peers.into_iter().filter(|p| st.found.get(&p.device_id).is_some_and(|f| f.seen.elapsed() < GONE_AFTER)).collect()
    }

    /// This device's id.
    pub fn device_id(&self) -> String {
        lock(&self.inner.store).identity.device_id.clone()
    }

    /// Connects to a paired device that is online, proves both sides and says hello; the channel is
    /// then ready for the catalog exchange. Blocks (seconds at most): call it off the UI thread.
    pub fn connect(&self, device_id: &str) -> Result<Channel> {
        let peer = lock(&self.inner.store).peer(device_id).cloned().ok_or_else(|| Error::Protocol("not paired with that device".into()))?;
        let addr = {
            let st = lock(&self.inner.state);
            let f = st
                .found
                .get(device_id)
                .filter(|f| f.seen.elapsed() < GONE_AFTER)
                .ok_or_else(|| Error::Protocol(format!("{} isn't on the network", peer.name)))?;
            SocketAddr::new(f.ip, f.beacon.port)
        };
        let me = lock(&self.inner.store).identity.clone();
        let s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        let mut ch = Channel::handshake(s, &me, true)?;
        if ch.peer_device != peer.device_id || ch.peer_key != peer.public_key {
            return Err(Error::Protocol("the device on the network isn't the paired one".into()));
        }
        ch.set_deadline(Duration::from_secs(30));
        ch.send_json(&Opening::Hello)?;
        match ch.recv_json::<Reply>()? {
            Reply::Ready { .. } => {
                // the other side proved it is the paired device: large frames and per-read waits
                ch.allow_large_frames();
                ch.set_timeout(Some(Duration::from_secs(30)))?;
                Ok(ch)
            }
            Reply::No { why } => Err(Error::Protocol(why)),
            Reply::Decision { .. } => Err(Error::Protocol("unexpected answer".into())),
        }
    }

    /// The person's answer to "do both screens show this code?".
    pub fn confirm(&self, accept: bool) -> Result<()> {
        let st = lock(&self.inner.state);
        let tx = st.pairing.as_ref().and_then(|p| p.decide.clone()).ok_or_else(|| Error::Protocol("no pairing is waiting for an answer".into()))?;
        drop(st);
        tx.send(accept).map_err(|_| Error::Protocol("the pairing has ended".into()))
    }

    /// Forgets a finished or failed pairing (the status no longer shows it).
    pub fn clear_pairing(&self) {
        let mut st = lock(&self.inner.state);
        if st.pairing.as_ref().is_some_and(|p| matches!(p.stage, Stage::Paired | Stage::Failed(_))) {
            st.pairing = None;
        }
    }

    pub fn rename(&self, name: &str) -> Result<String> {
        let mut store = lock(&self.inner.store);
        store.identity.name = clean_name(name, &store.identity.name.clone());
        store.save(&self.inner.store_path)?;
        self.inner.nudge.store(true, Ordering::Relaxed);
        Ok(store.identity.name.clone())
    }

    pub fn unpair(&self, device_id: &str) -> Result<bool> {
        let mut store = lock(&self.inner.store);
        let removed = store.remove_peer(device_id);
        if removed {
            store.save(&self.inner.store_path)?;
        }
        Ok(removed)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            if t.join().is_err() {
                log::warn!("peer: a service thread panicked");
            }
        }
    }
}

fn my_beacon(inner: &Inner) -> Option<Beacon> {
    let store = lock(&inner.store);
    Some(Beacon {
        app: lcc_brand::ENV_PREFIX.to_string(),
        v: beacon::VERSION,
        device_id: store.identity.device_id.clone(),
        name: store.identity.name.clone(),
        key: store.identity.public_key().ok()?,
        port: inner.tcp_port,
        pairing: inner.finding.load(Ordering::Relaxed),
        proto: beacon::PROTO,
    })
}

/// The IPv4 address other devices reach this one at (the route to the mDNS group), if any.
fn local_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((beacon::GROUP, beacon::PORT)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// The records that advertise `b`: PTR (type → instance), SRV (instance → host, port), TXT, and A
/// (host → address) when the address is known. `ttl` 0 is the goodbye.
fn advert(b: &Beacon, stype: &str, ttl: u32) -> Message {
    let instance = format!("{}.{stype}", b.device_id);
    let host = format!("{}.local", b.device_id);
    let mut records = vec![
        Record { name: stype.to_string(), ttl, flush: false, data: Data::Ptr(instance.clone()) },
        Record { name: instance.clone(), ttl, flush: true, data: Data::Srv { port: b.port, target: host.clone() } },
        Record { name: instance, ttl, flush: true, data: Data::Txt(b.txt()) },
    ];
    if let Some(ip) = local_ip() {
        records.push(Record { name: host, ttl, flush: true, data: Data::A(ip) });
    }
    Message { flags: dns::FLAGS_RESPONSE, questions: vec![], records }
}

fn send(udp: &UdpSocket, port: u16, msg: &Message) {
    let Some(bytes) = msg.encode() else {
        log::debug!("peer: an mDNS message couldn't be encoded");
        return;
    };
    if let Err(e) = udp.send_to(&bytes, SocketAddr::from((beacon::GROUP, port))) {
        log::debug!("peer: mDNS message not sent: {e}");
    }
}

fn announce_loop(inner: &Inner, udp: &UdpSocket, port: u16) {
    let stype = beacon::service_type();
    let query = Message { flags: 0, questions: vec![Question { name: stype.clone(), qtype: dns::TYPE_PTR }], records: vec![] };
    let mut last_announce: Option<Instant> = None;
    let mut last_query: Option<Instant> = None;
    // what was last announced, for the goodbye
    let mut announced: Option<Beacon> = None;
    while !inner.stop.load(Ordering::Relaxed) {
        let wanted = inner.finding.load(Ordering::Relaxed) || !lock(&inner.store).peers.is_empty();
        if wanted {
            let due = last_announce.is_none_or(|t| t.elapsed() >= BEACON_EVERY) || inner.nudge.swap(false, Ordering::Relaxed);
            if due {
                if let Some(b) = my_beacon(inner) {
                    send(udp, port, &advert(&b, &stype, RECORD_TTL));
                    announced = Some(b);
                }
                last_announce = Some(Instant::now());
            }
            if last_query.is_none_or(|t| t.elapsed() >= QUERY_EVERY) {
                send(udp, port, &query);
                last_query = Some(Instant::now());
            }
        } else if let Some(old) = announced.take() {
            send(udp, port, &advert(&old, &stype, 0));
            last_announce = None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(old) = announced {
        send(udp, port, &advert(&old, &stype, 0));
    }
}

fn listen_loop(inner: &Inner, udp: &UdpSocket) {
    let stype = beacon::service_type();
    let mut buf = vec![0u8; beacon::MAX_LEN];
    while !inner.stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = udp.recv_from(&mut buf) else { continue };
        let Some(msg) = buf.get(..n).and_then(Message::decode) else { continue };
        if msg.is_response() {
            let me = lock(&inner.store).identity.device_id.to_ascii_lowercase();
            let mut st = lock(&inner.state);
            take_answers(&mut st.found, &msg, from.ip(), &stype, &me, Instant::now());
        } else if msg.questions.iter().any(|q| q.name.eq_ignore_ascii_case(&stype) && matches!(q.qtype, dns::TYPE_PTR | 255)) {
            // someone browses for us: answer by announcing now
            inner.nudge.store(true, Ordering::Relaxed);
        }
    }
}

/// Adds the devices a response advertises to `found` (and removes those saying goodbye).
fn take_answers(found: &mut HashMap<String, Found>, msg: &Message, from: IpAddr, stype: &str, me: &str, now: Instant) {
    let suffix = format!(".{}", stype.to_ascii_lowercase());
    let label = |instance: &str| instance.strip_suffix(suffix.as_str()).map(str::to_string);
    let mut srv: HashMap<String, (u16, String, u32)> = HashMap::new();
    let mut txt: HashMap<String, &Vec<Vec<u8>>> = HashMap::new();
    let mut addr: HashMap<String, IpAddr> = HashMap::new();
    for r in &msg.records {
        let name = r.name.to_ascii_lowercase();
        match &r.data {
            Data::Srv { port, target } if name.ends_with(&suffix) => {
                srv.insert(name, (*port, target.to_ascii_lowercase(), r.ttl));
            }
            Data::Txt(t) if name.ends_with(&suffix) => {
                txt.insert(name, t);
            }
            Data::A(ip) if !ip.is_unspecified() => {
                addr.insert(name, IpAddr::V4(*ip));
            }
            Data::Aaaa(ip) if !ip.is_unspecified() => {
                addr.entry(name).or_insert(IpAddr::V6(*ip));
            }
            Data::Ptr(instance) if r.ttl == 0 && name == stype.to_ascii_lowercase() => {
                if let Some(id) = label(&instance.to_ascii_lowercase()) {
                    found.remove(&id);
                }
            }
            _ => {}
        }
    }
    for (instance, (port, target, ttl)) in srv {
        if ttl == 0 {
            if let Some(id) = label(&instance) {
                found.remove(&id);
            }
            continue;
        }
        let Some(b) = txt.get(&instance).and_then(|t| Beacon::from_txt(t, port)) else { continue };
        if b.device_id == me {
            continue;
        }
        if found.len() >= MAX_FOUND && !found.contains_key(&b.device_id) {
            continue;
        }
        let ip = addr.get(&target).copied().unwrap_or(from);
        found.insert(b.device_id.clone(), Found { beacon: b, ip, seen: now });
    }
}

fn accept_loop(inner: &Arc<Inner>, tcp: &TcpListener) {
    while !inner.stop.load(Ordering::Relaxed) {
        match tcp.accept() {
            Ok((stream, from)) => {
                let Some(slot) = ConnSlot::take(inner) else {
                    log::info!("peer: too many connections; closed the one from {from}");
                    drop(stream);
                    continue;
                };
                let spawned = std::thread::Builder::new().name("peer-conn".into()).spawn(move || {
                    let i = slot.0.clone();
                    let _slot = slot;
                    if let Err(e) = serve(&i, stream) {
                        log::info!("peer: connection from {from}: {e}");
                    }
                });
                if let Err(e) = spawned {
                    log::warn!("peer: no thread for a connection: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => {
                log::debug!("peer: accept: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// An incoming connection.
fn serve(inner: &Arc<Inner>, stream: TcpStream) -> Result<()> {
    stream.set_nonblocking(false)?;
    let me = lock(&inner.store).identity.clone();
    let mut ch = Channel::handshake(stream, &me, false)?;
    match ch.recv_json::<Opening>()? {
        Opening::Pair { name } => {
            let name = clean_name(&name, "Device");
            if !inner.finding.load(Ordering::Relaxed) {
                return ch.send_json(&Reply::No { why: "the other device isn't looking for devices".into() });
            }
            {
                let mut st = lock(&inner.state);
                if st.pairing.as_ref().is_some_and(|p| matches!(p.stage, Stage::Connecting | Stage::Confirm | Stage::Waiting)) {
                    drop(st);
                    return ch.send_json(&Reply::No { why: "the other device is pairing with someone else".into() });
                }
                st.pairing =
                    Some(Pairing { device_id: ch.peer_device.clone(), name: name.clone(), code: None, stage: Stage::Connecting, decide: None });
            }
            if let Err(e) = ch.send_json(&Reply::Ready { name: me.name.clone() }) {
                // the slot was taken above: free it, or no other pairing could ever start
                finish(inner, Err(e.clone()));
                return Err(e);
            }
            let r = decide(inner, ch, &name);
            finish(inner, r);
            Ok(())
        }
        Opening::Hello => {
            let known = lock(&inner.store).peer(&ch.peer_device).is_some_and(|p| p.public_key == ch.peer_key);
            if !known {
                return ch.send_json(&Reply::No { why: "not paired".into() });
            }
            let Some(peer) = lock(&inner.store).peer(&ch.peer_device).cloned() else {
                return ch.send_json(&Reply::No { why: "not paired".into() });
            };
            let handler = lock(&inner.on_hello).clone();
            // proved paired: from here the catalog exchange may send large frames, at its own pace
            ch.allow_large_frames();
            ch.set_timeout(Some(Duration::from_secs(10)))?;
            ch.send_json(&Reply::Ready { name: me.name })?;
            if let Some(h) = handler {
                h(peer, ch);
            }
            Ok(())
        }
    }
}

/// Shows the code, waits for this side's answer, swaps answers with the other side and, if both
/// said yes, stores the pairing. Returns the paired device.
fn decide(inner: &Inner, mut ch: Channel, name: &str) -> Result<Peer> {
    let (tx, rx): (Sender<bool>, Receiver<bool>) = channel();
    {
        let mut st = lock(&inner.state);
        let p = st.pairing.as_mut().ok_or_else(|| Error::Protocol("the pairing was cancelled".into()))?;
        p.code = Some(ch.code.clone());
        p.stage = Stage::Confirm;
        p.decide = Some(tx);
    }
    let started = Instant::now();
    let ok = loop {
        if inner.stop.load(Ordering::Relaxed) || started.elapsed() > DECIDE_WITHIN {
            break false;
        }
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(v) => break v,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break false,
        }
    };
    if let Some(p) = lock(&inner.state).pairing.as_mut() {
        p.stage = Stage::Waiting;
        p.decide = None;
    }
    ch.send_json(&Reply::Decision { ok })?;
    if !ok {
        return Err(Error::Protocol("pairing cancelled".into()));
    }
    let remaining = DECIDE_WITHIN.saturating_sub(started.elapsed()).max(Duration::from_secs(5));
    ch.set_deadline(remaining);
    match ch.recv_json::<Reply>()? {
        Reply::Decision { ok: true } => {}
        Reply::Decision { ok: false } => return Err(Error::Protocol("the other device cancelled the pairing".into())),
        _ => return Err(Error::Protocol("unexpected answer".into())),
    }
    let peer = Peer { device_id: ch.peer_device.clone(), name: name.to_string(), public_key: ch.peer_key.clone(), paired_at: now_secs() };
    let mut store = lock(&inner.store);
    store.add_peer(peer.clone());
    store.save(&inner.store_path)?;
    Ok(peer)
}

fn finish(inner: &Inner, r: Result<Peer>) {
    let mut st = lock(&inner.state);
    if let Some(p) = st.pairing.as_mut() {
        p.decide = None;
        p.stage = match r {
            Ok(_) => Stage::Paired,
            Err(e) => Stage::Failed(e.to_string()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether multicast to the mDNS group loops back on this machine (it doesn't in some sandboxes).
    fn multicast_loops_back(port: u16) -> bool {
        let Ok(rx) = mdns_socket(port) else { return false };
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

    fn wait(what: &str, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
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
        let dir = std::env::temp_dir().join(format!("lcc-peer-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // a port of its own, so parallel test runs don't hear each other
        let port = 40000 + (std::process::id() % 20000) as u16;
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
        wait("both to show the code", || {
            let (sa, sb) = (a.status(), b.status());
            sa.pairing.as_ref().is_some_and(|p| p.stage == Stage::Confirm) && sb.pairing.as_ref().is_some_and(|p| p.stage == Stage::Confirm)
        });
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
        // macOS LAN pairing check: discovery times out on the macOS CI runner; checked in person on
        // real Macs instead (see "Checked in person, not on CI" in docs/ci.md)
        if cfg!(target_os = "macos") && std::env::var_os("CI").is_some() {
            lcc_testkit::skip!(System, "LAN discovery on the macOS CI runner (macOS LAN pairing check, tested in person)");
            return;
        }
        let dir = std::env::temp_dir().join(format!("lcc-peer-no-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let port = 40000 + ((std::process::id() + 7) % 20000) as u16;
        if !multicast_loops_back(port) {
            lcc_testkit::skip!(System, "multicast doesn't loop back here");
            return;
        }
        let a = Service::start(dir.join("a.json"), "Desk", port).unwrap();
        let b = Service::start(dir.join("b.json"), "Tablet", port).unwrap();
        a.set_finding(true);
        b.set_finding(true);
        let b_id = b.status().device_id;
        wait("a to see b", || a.status().found.iter().any(|f| f.device_id == b_id));
        a.pair(&b_id).unwrap();
        wait("b to show the code", || b.status().pairing.as_ref().is_some_and(|p| p.stage == Stage::Confirm));
        // both show the code before anyone answers, as two people pairing see it
        wait("a to show the code", || a.status().pairing.as_ref().is_some_and(|p| p.stage == Stage::Confirm));
        // b refuses first: once a confirms, b's pairing may already have moved on.
        b.confirm(false).unwrap();
        let _ = a.confirm(true);
        wait("a to fail", || a.status().pairing.as_ref().is_some_and(|p| matches!(p.stage, Stage::Failed(_))));
        assert!(a.status().paired.is_empty());
        assert!(b.status().paired.is_empty());
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

    #[test]
    fn answers_fill_and_empty_the_found_list() {
        let stype = beacon::service_type();
        let src: IpAddr = Ipv4Addr::new(10, 0, 0, 9).into();
        let now = Instant::now();
        let mut found = HashMap::new();
        take_answers(&mut found, &advert_of("ab12", true, 10), src, &stype, "ff", now);
        let f = found.get("ab12").unwrap();
        assert!(f.beacon.pairing);
        assert_eq!(f.beacon.port, 4000);
        // our own advert is not a found device
        take_answers(&mut found, &advert_of("ff", true, 10), src, &stype, "ff", now);
        assert!(!found.contains_key("ff"));
        // a goodbye removes it
        take_answers(&mut found, &advert_of("ab12", true, 0), src, &stype, "ff", now);
        assert!(found.is_empty());
        // the list is capped
        for i in 0..(MAX_FOUND + 20) {
            take_answers(&mut found, &advert_of(&format!("{i:x}"), false, 10), src, &stype, "ff", now);
        }
        assert_eq!(found.len(), MAX_FOUND);
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
            take_answers(&mut found, &msg, src, &stype, "ff", Instant::now());
        }
        assert!(found.is_empty());
    }

    fn bare_inner(tag: &str) -> Arc<Inner> {
        let path = std::env::temp_dir().join(format!("lcc-peer-{tag}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::load_or_create(&path, "Test").unwrap();
        Arc::new(Inner {
            stop: AtomicBool::new(false),
            finding: AtomicBool::new(false),
            nudge: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            store: Mutex::new(store),
            store_path: path,
            tcp_port: 0,
            on_hello: Mutex::new(None),
            connections: AtomicUsize::new(0),
        })
    }

    /// Past the cap, a new connection is closed at once instead of getting a thread.
    #[test]
    fn connections_past_the_cap_are_closed() {
        use std::io::Read;
        let inner = bare_inner("cap");
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        tcp.set_nonblocking(true).unwrap();
        let addr = tcp.local_addr().unwrap();
        let held: Vec<ConnSlot> = (0..MAX_CONNECTIONS).map(|_| ConnSlot::take(&inner).unwrap()).collect();
        assert!(ConnSlot::take(&inner).is_none());
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
}
