//! The running peer service: mDNS adverts out, adverts in, connections in, and the pairing in progress.
//!
//! Discovery is multicast DNS service discovery (RFC 6762 / RFC 6763, see [`crate::beacon`]):
//! while looking for devices or when paired devices exist, one thread announces this device every
//! couple of seconds (and a goodbye when it stops), and browses for the service type; one thread
//! reads the multicast group (answers, and queries it answers by announcing); one accepts
//! connections, and each connection gets its own short-lived thread. Every loop checks the stop flag at least twice a second, so dropping the [`Service`]
//! ends them promptly. Nothing here panics on what the network sends.
//!
//! The multicast group is joined again every half minute and when a send or a read fails, on
//! every interface that can do multicast: a network that came up late, changed or woke from sleep
//! is heard again without a restart.
//!
//! What an unpaired host on the network can and can't do:
//! - Adverts are not authenticated. They only say where a device might be: a name is stored, a
//!   device is trusted, only after the handshake proved its key ([`crate::secure`]). Flooding the
//!   list of found devices pushes out the oldest unpaired entries (a few per sender), never a
//!   paired device.
//! - A connection must finish its handshake and say what it wants within a few seconds; half of
//!   the connection slots are kept for the addresses of paired devices.
//! - A pairing request shows up only after its handshake. The person can dismiss it at once
//!   ([`Service::cancel_pairing`]), and choosing a device to pair with replaces a request nobody
//!   answered. After many unfinished handshakes pairing requests are refused for a while: that is
//!   what trying connection after connection for a matching pairing code looks like.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::beacon::{self, Beacon};
use crate::dns::{self, Data, Message, Question, Record};
use crate::identity::{Peer, Store, clean_name};
use crate::secure::{Channel, HANDSHAKE_WITHIN};
use crate::{Error, Result};

/// How often this device announces itself.
const BEACON_EVERY: Duration = Duration::from_secs(2);
/// How often it asks who else is there.
const QUERY_EVERY: Duration = Duration::from_millis(2500);
/// The TTL of the advertised records (short: devices come and go).
const RECORD_TTL: u32 = 10;
/// The most devices remembered; a busy network can't grow the list without bound.
const MAX_FOUND: usize = 256;
/// The most unpaired devices remembered from one sender's adverts (several copies of the app on
/// one machine are a handful): one host can't fill the list by itself.
const MAX_PER_SOURCE: usize = 8;
/// A device not heard from for this long is no longer listed (or online).
const GONE_AFTER: Duration = Duration::from_secs(8);
/// How long a pairing waits for the people to compare codes.
const DECIDE_WITHIN: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most incoming connections served at once; more are closed straight away, so a busy or
/// hostile network can't make this device start a thread per connection without bound.
const MAX_CONNECTIONS: usize = 16;
/// Of those, the most that may come from addresses no paired device was heard at: the rest stay
/// free for paired devices whatever else is going on.
const MAX_UNKNOWN_CONNECTIONS: usize = 8;
/// How long an incoming connection has for its handshake and to say what it wants: a host that
/// connects and stays silent gives its slot back after this.
const PRE_AUTH_WITHIN: Duration = Duration::from_secs(3);
/// How long a pairing connection may take to say what it wants after the handshake.
const OPENING_WITHIN: Duration = Duration::from_secs(10);
/// How often the multicast group is joined again.
const REJOIN_EVERY: Duration = Duration::from_secs(30);
/// The shortest time between two joins, however many sends and reads fail.
const REJOIN_NO_SOONER: Duration = Duration::from_secs(2);
/// The most interfaces the group is joined on (the system allows about 20 per socket).
const MAX_INTERFACES: usize = 16;
/// How often a paired device that is online is asked whether it is still paired (and its name).
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// A paired device advertising another name than the stored one is asked sooner than that.
const CHECK_RENAMED_AFTER: Duration = Duration::from_secs(10);
/// Incoming connections that learned this device's hello and then didn't ask to pair, within
/// [`DECIDE_WITHIN`], after which pairing requests are refused: the pairing code has a million
/// values and each such connection may be one guess at it.
const MAX_ABANDONED: usize = 16;

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
    /// Which pairing this is: a thread working on an older one leaves the current one alone.
    id: u64,
    device_id: String,
    name: String,
    code: Option<String>,
    stage: Stage,
    /// The person's answer; `false` also cancels after a yes.
    decide: Option<Sender<bool>>,
    /// The other device asked.
    incoming: bool,
}

impl Pairing {
    fn active(&self) -> bool {
        matches!(self.stage, Stage::Connecting | Stage::Confirm | Stage::Waiting)
    }
}

struct Found {
    beacon: Beacon,
    /// Where to connect (the advert's address record, else its sender).
    ip: IpAddr,
    /// Who sent the advert.
    from: IpAddr,
    seen: Instant,
}

impl Found {
    fn fresh(&self) -> bool {
        self.seen.elapsed() < GONE_AFTER
    }
}

#[derive(Default)]
struct State {
    found: HashMap<String, Found>,
    pairing: Option<Pairing>,
    pairings_started: u64,
    /// Paired devices that answered "not paired": they unpaired this device.
    not_paired_there: HashSet<String>,
    /// When each paired device was last asked whether it is still paired.
    checked: HashMap<String, Instant>,
}

impl State {
    fn current(&mut self, id: u64) -> Option<&mut Pairing> {
        self.pairing.as_mut().filter(|p| p.id == id)
    }

    fn begin(&mut self, device_id: &str, name: &str, incoming: bool) -> u64 {
        self.pairings_started = self.pairings_started.wrapping_add(1);
        let id = self.pairings_started;
        self.pairing = Some(Pairing {
            id,
            device_id: device_id.to_string(),
            name: name.to_string(),
            code: None,
            stage: Stage::Connecting,
            decide: None,
            incoming,
        });
        id
    }
}

struct Inner {
    stop: AtomicBool,
    finding: AtomicBool,
    /// Ask the beacon thread to send now (a change worth announcing).
    nudge: AtomicBool,
    /// Ask it to join the multicast group again (a send or a read failed).
    rejoin: AtomicBool,
    state: Mutex<State>,
    store: Mutex<Store>,
    store_path: PathBuf,
    tcp_port: u16,
    /// Takes a paired device's connection once it has said hello (the catalog exchange).
    on_hello: Mutex<Option<HelloHandler>>,
    /// Incoming connections being served now.
    connections: AtomicUsize,
    /// Those of them from addresses no paired device was heard at.
    unknown_connections: AtomicUsize,
    /// When incoming handshakes failed, lately.
    abandoned: Mutex<VecDeque<Instant>>,
    /// A paired device is being asked whether it is still paired.
    checking: AtomicBool,
    /// The store that couldn't be used at start and was moved aside.
    lost: Option<PathBuf>,
}

impl Inner {
    fn new(store: Store, store_path: PathBuf, tcp_port: u16, lost: Option<PathBuf>) -> Inner {
        Inner {
            stop: AtomicBool::new(false),
            finding: AtomicBool::new(false),
            nudge: AtomicBool::new(true),
            rejoin: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            store: Mutex::new(store),
            store_path,
            tcp_port,
            on_hello: Mutex::new(None),
            connections: AtomicUsize::new(0),
            unknown_connections: AtomicUsize::new(0),
            abandoned: Mutex::new(VecDeque::new()),
            checking: AtomicBool::new(false),
            lost,
        }
    }
}

/// Holds one of the [`MAX_CONNECTIONS`] slots; gives it back when dropped.
struct ConnSlot {
    inner: Arc<Inner>,
    unknown: bool,
}

impl ConnSlot {
    /// `known`: the connection comes from an address a paired device was heard at.
    fn take(inner: &Arc<Inner>, known: bool) -> Option<ConnSlot> {
        let slot = ConnSlot { inner: inner.clone(), unknown: !known };
        // counted first, so the drop gives it back whether or not it fits
        let all = inner.connections.fetch_add(1, Ordering::AcqRel);
        let unknown = if known { 0 } else { inner.unknown_connections.fetch_add(1, Ordering::AcqRel) };
        (all < MAX_CONNECTIONS && unknown < MAX_UNKNOWN_CONNECTIONS).then_some(slot)
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.inner.connections.fetch_sub(1, Ordering::AcqRel);
        if self.unknown {
            self.inner.unknown_connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Marks a check of a paired device as running; lets the next one start when dropped.
struct Checking(Arc<Inner>);

impl Drop for Checking {
    fn drop(&mut self) {
        self.0.checking.store(false, Ordering::Relaxed);
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

/// What another device said went wrong, short enough to show.
fn brief(why: &str) -> String {
    why.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// What Settings ▸ Sync shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub device_id: String,
    pub name: String,
    pub finding: bool,
    /// Devices looking for a pairing right now: not yet paired with this one, or paired here but
    /// no longer on their side ([`FoundDevice::paired`]).
    pub found: Vec<FoundDevice>,
    pub paired: Vec<PairedDevice>,
    pub pairing: Option<PairingStatus>,
    /// This device's name is also a paired or found device's name ([`Service::name_collides`]).
    pub name_collides: bool,
    /// The stored pairings couldn't be read at start and were set aside ([`Service::pairings_lost`]).
    pub pairings_lost: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundDevice {
    pub device_id: String,
    pub name: String,
    /// Still paired on this side: the other side unpaired, pairing again repairs it.
    pub paired: bool,
    /// It speaks this build's protocol; if not, both devices need the same version of the app.
    pub compatible: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub device_id: String,
    pub name: String,
    /// Heard on the network in the last few seconds.
    pub online: bool,
    pub paired_at: u64,
    /// It answered that it isn't paired with this device (any more): unpaired on the other side.
    pub not_paired_there: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    pub device_id: String,
    pub name: String,
    pub code: Option<String>,
    #[serde(flatten)]
    pub stage: Stage,
    /// The other device asked for this pairing (this side didn't choose it).
    pub incoming: bool,
}

/// The first message on a connection.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Opening {
    /// Asks to pair (the receiver must be looking for devices).
    Pair { name: String },
    /// A paired device saying hello (the catalog exchange starts from here).
    Hello {
        #[serde(default)]
        name: String,
    },
    /// A paired device asking whether it is still paired, and telling its name; nothing follows.
    Check {
        #[serde(default)]
        name: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Reply {
    Ready {
        name: String,
    },
    No {
        why: String,
    },
    /// This device isn't paired with the one that connected (any more).
    NotPaired,
    /// A side's answer to the code; `ok: false` at any time ends the pairing (declined,
    /// cancelled, timed out).
    Decision {
        ok: bool,
    },
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

/// The IPv4 addresses of the interfaces that are up and can do multicast.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn interface_addrs() -> Vec<Ipv4Addr> {
    use nix::net::if_::InterfaceFlags;
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else { return Vec::new() };
    addrs
        .filter(|a| a.flags.contains(InterfaceFlags::IFF_UP | InterfaceFlags::IFF_MULTICAST))
        .filter_map(|a| a.address.as_ref().and_then(|s| s.as_sockaddr_in()).map(|s| s.ip()))
        .filter(|ip| !ip.is_unspecified())
        .take(MAX_INTERFACES)
        .collect()
}

/// Elsewhere the interfaces can't be listed without code outside Rust: the default one is used.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
fn interface_addrs() -> Vec<Ipv4Addr> {
    Vec::new()
}

/// Joins the mDNS group again: on the default interface, on the one the route to the group
/// leaves by, and on every interface that can do multicast (where they can be listed). `joined`
/// is what the last call joined; those memberships are dropped first, so one that went stale (the
/// interface changed underneath it) is replaced. Returns how many interfaces are joined now.
fn join_group(udp: &UdpSocket, joined: &mut Vec<Ipv4Addr>) -> usize {
    // the specific ones first: leaving "any interface" drops whichever membership comes first
    for ip in joined.drain(..).rev() {
        let _ = udp.leave_multicast_v4(&beacon::GROUP, &ip);
    }
    let mut wanted = vec![Ipv4Addr::UNSPECIFIED];
    for ip in local_ip().into_iter().chain(interface_addrs()) {
        if !wanted.contains(&ip) && wanted.len() <= MAX_INTERFACES {
            wanted.push(ip);
        }
    }
    for ip in wanted {
        match udp.join_multicast_v4(&beacon::GROUP, &ip) {
            Ok(()) => joined.push(ip),
            // the default interface is usually one of the listed ones: already joined
            Err(e) => log::debug!("peer: mDNS group not joined on {ip}: {e}"),
        }
    }
    joined.len()
}

/// Whether to join the group again now: every [`REJOIN_EVERY`], and soon after a failure.
fn rejoin_due(last: Option<Instant>, failed: bool, now: Instant) -> bool {
    let Some(last) = last else { return true };
    let since = now.saturating_duration_since(last);
    since >= REJOIN_EVERY || (failed && since >= REJOIN_NO_SOONER)
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
    // no multicast route (no network yet): keep running; the announce thread joins again later
    if let Err(e) = udp.join_multicast_v4(&beacon::GROUP, &Ipv4Addr::UNSPECIFIED) {
        log::info!("peer: can't join the mDNS group yet: {e}");
    }
    udp.set_multicast_loop_v4(true)?;
    udp.set_multicast_ttl_v4(255)?;
    udp.set_read_timeout(Some(Duration::from_millis(400)))?;
    Ok(udp)
}

impl Service {
    /// Opens (or creates) the store at `store_path` and starts listening. `default_name` names a
    /// new device. `port` is the multicast DNS port ([`beacon::PORT`], 5353; tests pass their own
    /// so they neither hear the network nor each other). A store that can't be used is moved
    /// aside and replaced ([`Service::pairings_lost`]).
    pub fn start(store_path: PathBuf, default_name: &str, port: u16) -> Result<Service> {
        let (store, lost) = Store::load_or_recover(&store_path, default_name)?;
        let udp = mdns_socket(port)?;
        let tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        tcp.set_nonblocking(true)?;
        let tcp_port = tcp.local_addr()?.port();
        let inner = Arc::new(Inner::new(store, store_path, tcp_port, lost));
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
        st.found.retain(|_, f| f.fresh());
        let mut found: Vec<FoundDevice> = st
            .found
            .values()
            .filter(|f| f.beacon.pairing)
            .filter_map(|f| {
                let paired = store.peer(&f.beacon.device_id).is_some();
                // a device still paired on both sides has nothing to pair
                (!paired || st.not_paired_there.contains(&f.beacon.device_id)).then(|| FoundDevice {
                    device_id: f.beacon.device_id.clone(),
                    name: f.beacon.name.clone(),
                    paired,
                    compatible: f.beacon.proto == beacon::PROTO,
                })
            })
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
                not_paired_there: st.not_paired_there.contains(&p.device_id),
            })
            .collect();
        let pairing = st.pairing.as_ref().map(|p| PairingStatus {
            device_id: p.device_id.clone(),
            name: p.name.clone(),
            code: p.code.clone(),
            stage: p.stage.clone(),
            incoming: p.incoming,
        });
        let others: Vec<String> = st.found.values().map(|f| f.beacon.name.clone()).collect();
        Status {
            device_id: store.identity.device_id.clone(),
            name: store.identity.name.clone(),
            finding: self.finding(),
            found,
            paired,
            pairing,
            name_collides: store.name_collides(&others),
            pairings_lost: self.inner.lost.is_some(),
        }
    }

    /// Starts pairing with a device that is looking for one. A pairing request from another
    /// device that is still open gives way to this choice ([`Error::Busy`] only while a pairing
    /// this side started is going on).
    ///
    /// The device that answers must be the chosen one, and one this device already knows must
    /// still have the key it was paired under; otherwise the pairing fails.
    pub fn pair(&self, device_id: &str) -> Result<()> {
        let known = lock(&self.inner.store).peer(device_id).cloned();
        let (addr, advertised, id) = {
            let mut st = lock(&self.inner.state);
            let f = st
                .found
                .get(device_id)
                .filter(|f| f.beacon.pairing && f.fresh())
                .ok_or_else(|| Error::Protocol("that device isn't looking for a pairing now".into()))?;
            if f.beacon.proto != beacon::PROTO {
                return Err(Error::Version);
            }
            let addr = SocketAddr::new(f.ip, f.beacon.port);
            let advertised = f.beacon.name.clone();
            match st.pairing.as_ref() {
                Some(p) if p.active() && p.incoming => {
                    // the person chose a device over the other device's request: the request ends
                    if let Some(tx) = &p.decide {
                        let _ = tx.send(false);
                    }
                }
                Some(p) if p.active() => return Err(Error::Busy),
                _ => {}
            }
            let id = st.begin(device_id, &advertised, false);
            (addr, advertised, id)
        };
        let inner = self.inner.clone();
        let chosen = device_id.to_string();
        let spawned = std::thread::Builder::new().name("peer-pair".into()).spawn(move || {
            let r = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(Error::from).and_then(|s| {
                let me = lock(&inner.store).identity.clone();
                let mut ch = Channel::handshake(s, &me, true)?;
                // the advert that put the device in the list proves nothing; the handshake does
                if ch.peer_device != chosen {
                    return Err(Error::Protocol("another device answered where the chosen one was found".into()));
                }
                if known.as_ref().is_some_and(|k| k.public_key != ch.peer_key) {
                    return Err(Error::Protocol("that device isn't the one paired under its name before; unpair it first, then pair again".into()));
                }
                ch.set_deadline(OPENING_WITHIN);
                ch.send_json(&Opening::Pair { name: me.name.clone() })?;
                let name = match ch.recv_json::<Reply>()? {
                    Reply::Ready { name } => clean_name(&name, &advertised),
                    Reply::No { why } => return Err(Error::Protocol(brief(&why))),
                    Reply::NotPaired | Reply::Decision { .. } => return Err(Error::Protocol("unexpected answer".into())),
                };
                decide(&inner, id, ch, &name)
            });
            finish(&inner, id, r);
        });
        if let Err(e) = spawned {
            let e = Error::Io(e.to_string());
            finish(&self.inner, id, Err(e.clone()));
            return Err(e);
        }
        Ok(())
    }

    /// Sets what runs a paired device's incoming connection after its hello.
    pub fn set_hello_handler(&self, handler: HelloHandler) {
        *lock(&self.inner.on_hello) = Some(handler);
    }

    /// The paired devices heard on the network in the last few seconds, without those that
    /// answered that they aren't paired with this device any more.
    pub fn online_peers(&self) -> Vec<Peer> {
        let store = lock(&self.inner.store).clone();
        let st = lock(&self.inner.state);
        store
            .peers
            .into_iter()
            .filter(|p| st.found.get(&p.device_id).is_some_and(Found::fresh) && !st.not_paired_there.contains(&p.device_id))
            .collect()
    }

    /// The paired devices that answered that they aren't paired with this device (any more): they
    /// unpaired it. They are listed among the found devices again while they look for a pairing.
    pub fn not_paired_there(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock(&self.inner.state).not_paired_there.iter().cloned().collect();
        ids.sort();
        ids
    }

    /// This device's id.
    pub fn device_id(&self) -> String {
        lock(&self.inner.store).identity.device_id.clone()
    }

    /// Where a paired device is, if it is online and speaks this build's protocol.
    fn paired_at(&self, device_id: &str) -> Result<(Peer, SocketAddr)> {
        let peer = lock(&self.inner.store).peer(device_id).cloned().ok_or(Error::NotPaired)?;
        let st = lock(&self.inner.state);
        let f = st.found.get(device_id).filter(|f| f.fresh()).ok_or_else(|| Error::Protocol(format!("{} isn't on the network", peer.name)))?;
        if f.beacon.proto != beacon::PROTO {
            return Err(Error::Version);
        }
        let addr = SocketAddr::new(f.ip, f.beacon.port);
        Ok((peer, addr))
    }

    /// Connects to a paired device that is online, proves both sides and says hello; the channel is
    /// then ready for the catalog exchange. Blocks (seconds at most): call it off the UI thread.
    ///
    /// [`Error::NotPaired`] when this device doesn't know that one, or that one answered that it
    /// doesn't know this one (it then shows as [`PairedDevice::not_paired_there`]);
    /// [`Error::Version`] when it runs another version of the protocol.
    pub fn connect(&self, device_id: &str) -> Result<Channel> {
        let (peer, addr) = self.paired_at(device_id)?;
        let mut ch = open_paired(&self.inner, &peer, addr, HANDSHAKE_WITHIN)?;
        ch.set_deadline(Duration::from_secs(30));
        let name = lock(&self.inner.store).identity.name.clone();
        greet(&self.inner, &mut ch, &peer, &Opening::Hello { name })?;
        // the other side proved it is the paired device: large frames and per-read waits
        ch.allow_large_frames();
        ch.set_timeout(Some(Duration::from_secs(30)))?;
        Ok(ch)
    }

    /// Asks a paired device that is online whether it is still paired with this one, and learns
    /// its current name; nothing else happens on either side. The same errors as
    /// [`Service::connect`]. The service does this by itself about once a minute per device.
    /// Blocks (seconds at most).
    pub fn check(&self, device_id: &str) -> Result<()> {
        let (peer, addr) = self.paired_at(device_id)?;
        check(&self.inner, &peer, addr)
    }

    /// The person's answer to "do both screens show this code?".
    pub fn confirm(&self, accept: bool) -> Result<()> {
        let st = lock(&self.inner.state);
        let tx = st.pairing.as_ref().and_then(|p| p.decide.clone()).ok_or_else(|| Error::Protocol("no pairing is waiting for an answer".into()))?;
        drop(st);
        tx.send(accept).map_err(|_| Error::Protocol("the pairing has ended".into()))
    }

    /// Ends the pairing in progress, whatever stage it is in, and takes it off the status at
    /// once: dismisses a request from another device, or takes back this side's choice (even
    /// after its yes, while the other side hasn't answered). The other device is told right away.
    /// `false` when there was no pairing.
    pub fn cancel_pairing(&self) -> bool {
        let mut st = lock(&self.inner.state);
        let Some(p) = st.pairing.take() else { return false };
        if let Some(tx) = &p.decide {
            let _ = tx.send(false);
        }
        true
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
        store.save_merged(&self.inner.store_path, None)?;
        self.inner.nudge.store(true, Ordering::Relaxed);
        Ok(store.identity.name.clone())
    }

    /// The names of the paired devices and of the devices found on the network now: what this
    /// device's name should differ from ([`crate::identity::unique_name`]).
    pub fn taken_names(&self) -> Vec<String> {
        let mut names: Vec<String> = lock(&self.inner.store).peers.iter().map(|p| p.name.clone()).collect();
        let st = lock(&self.inner.state);
        for f in st.found.values().filter(|f| f.fresh()) {
            if !names.contains(&f.beacon.name) {
                names.push(f.beacon.name.clone());
            }
        }
        names
    }

    /// Whether this device's name is also the name of a paired device or of a device found on the
    /// network now (case and surrounding blanks aside).
    pub fn name_collides(&self) -> bool {
        let others: Vec<String> = lock(&self.inner.state).found.values().filter(|f| f.fresh()).map(|f| f.beacon.name.clone()).collect();
        lock(&self.inner.store).name_collides(&others)
    }

    /// The store that couldn't be used when the service started and was moved aside
    /// (`peers.json.corrupt`): this device has a new identity and every pairing is lost.
    pub fn pairings_lost(&self) -> Option<PathBuf> {
        self.inner.lost.clone()
    }

    pub fn unpair(&self, device_id: &str) -> Result<bool> {
        let removed = {
            let mut store = lock(&self.inner.store);
            let removed = store.remove_peer(device_id);
            if removed {
                store.save_merged(&self.inner.store_path, Some(device_id))?;
            }
            removed
        };
        let mut st = lock(&self.inner.state);
        st.not_paired_there.remove(device_id);
        st.checked.remove(device_id);
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

/// Connects to a paired device and proves both sides: whoever answers must hold the paired key.
fn open_paired(inner: &Inner, peer: &Peer, addr: SocketAddr, within: Duration) -> Result<Channel> {
    let me = lock(&inner.store).identity.clone();
    let s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    let ch = Channel::handshake_within(s, &me, true, within)?;
    if ch.peer_device != peer.device_id || ch.peer_key != peer.public_key {
        return Err(Error::Protocol("the device on the network isn't the paired one".into()));
    }
    Ok(ch)
}

/// Says what a connection to a paired device is for and reads its answer. The answer comes from
/// the authenticated device: "ready" carries its current name, which is stored; "not paired"
/// marks it as unpaired on its side.
fn greet(inner: &Inner, ch: &mut Channel, peer: &Peer, opening: &Opening) -> Result<()> {
    ch.send_json(opening)?;
    match ch.recv_json::<Reply>()? {
        Reply::Ready { name } => {
            lock(&inner.state).not_paired_there.remove(&peer.device_id);
            note_name(inner, &peer.device_id, &name);
            Ok(())
        }
        Reply::NotPaired => {
            // only while still paired here: an unpair meanwhile leaves nothing to mark
            if lock(&inner.store).peer(&peer.device_id).is_some() {
                lock(&inner.state).not_paired_there.insert(peer.device_id.clone());
            }
            Err(Error::NotPaired)
        }
        Reply::No { why } => Err(Error::Protocol(brief(&why))),
        Reply::Decision { .. } => Err(Error::Protocol("unexpected answer".into())),
    }
}

/// Asks a paired device whether it is still paired (see [`Service::check`]).
fn check(inner: &Inner, peer: &Peer, addr: SocketAddr) -> Result<()> {
    let mut ch = open_paired(inner, peer, addr, PRE_AUTH_WITHIN)?;
    ch.set_deadline(PRE_AUTH_WITHIN);
    let name = lock(&inner.store).identity.name.clone();
    greet(inner, &mut ch, peer, &Opening::Check { name })
}

/// Stores the name a paired device gave over the authenticated channel, if it is a new one.
fn note_name(inner: &Inner, device_id: &str, name: &str) {
    let name = clean_name(name, "");
    if name.is_empty() {
        return;
    }
    let mut store = lock(&inner.store);
    if store.rename_peer(device_id, &name)
        && let Err(e) = store.save_merged(&inner.store_path, None)
    {
        log::warn!("peer: a paired device's new name wasn't saved: {e}");
    }
}

/// Starts a check of one paired device that is due for it, if none is running: a device not
/// asked for a while, or one advertising another name than the stored one (the advert only
/// prompts the question; the name that is stored is the one the device answers with).
fn check_one_due(inner: &Arc<Inner>) {
    if inner.checking.load(Ordering::Relaxed) {
        return;
    }
    let peers = lock(&inner.store).peers.clone();
    let due = {
        let mut st = lock(&inner.state);
        st.checked.retain(|id, _| peers.iter().any(|p| &p.device_id == id));
        let due = peers.into_iter().find_map(|p| {
            // one that said "not paired" isn't asked again: pairing anew or its own hello clears that
            if st.not_paired_there.contains(&p.device_id) {
                return None;
            }
            let f = st.found.get(&p.device_id).filter(|f| f.fresh() && f.beacon.proto == beacon::PROTO)?;
            let wait = if f.beacon.name == p.name { CHECK_EVERY } else { CHECK_RENAMED_AFTER };
            let due = st.checked.get(&p.device_id).is_none_or(|t| t.elapsed() >= wait);
            due.then(|| (SocketAddr::new(f.ip, f.beacon.port), p))
        });
        if let Some((_, p)) = &due {
            st.checked.insert(p.device_id.clone(), Instant::now());
        }
        due
    };
    let Some((addr, peer)) = due else { return };
    inner.checking.store(true, Ordering::Relaxed);
    let running = Checking(inner.clone());
    let spawned = std::thread::Builder::new().name("peer-check".into()).spawn(move || {
        if let Err(e) = check(&running.0, &peer, addr) {
            log::debug!("peer: check of {}: {e}", peer.device_id);
        }
    });
    if let Err(e) = spawned {
        log::debug!("peer: no thread for a check: {e}");
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

/// Sends `msg` to the group; `false` when it couldn't be sent (no route: the network changed).
fn send(udp: &UdpSocket, port: u16, msg: &Message) -> bool {
    let Some(bytes) = msg.encode() else {
        log::debug!("peer: an mDNS message couldn't be encoded");
        return true;
    };
    match udp.send_to(&bytes, SocketAddr::from((beacon::GROUP, port))) {
        Ok(_) => true,
        Err(e) => {
            log::debug!("peer: mDNS message not sent: {e}");
            false
        }
    }
}

fn announce_loop(inner: &Arc<Inner>, udp: &UdpSocket, port: u16) {
    let stype = beacon::service_type();
    let query = Message { flags: 0, questions: vec![Question { name: stype.clone(), qtype: dns::TYPE_PTR }], records: vec![] };
    let mut last_announce: Option<Instant> = None;
    let mut last_query: Option<Instant> = None;
    let mut last_check = Instant::now();
    // what was last announced, for the goodbye
    let mut announced: Option<Beacon> = None;
    // the socket's group memberships (it was made with the default one), and when they were renewed
    let mut joined = vec![Ipv4Addr::UNSPECIFIED];
    let mut last_join: Option<Instant> = None;
    let say = |msg: &Message| {
        if !send(udp, port, msg) {
            inner.rejoin.store(true, Ordering::Relaxed);
        }
    };
    while !inner.stop.load(Ordering::Relaxed) {
        let wanted = inner.finding.load(Ordering::Relaxed) || !lock(&inner.store).peers.is_empty();
        if wanted {
            if rejoin_due(last_join, inner.rejoin.load(Ordering::Relaxed), Instant::now()) {
                inner.rejoin.store(false, Ordering::Relaxed);
                join_group(udp, &mut joined);
                last_join = Some(Instant::now());
            }
            let due = last_announce.is_none_or(|t| t.elapsed() >= BEACON_EVERY) || inner.nudge.swap(false, Ordering::Relaxed);
            if due {
                if let Some(b) = my_beacon(inner) {
                    say(&advert(&b, &stype, RECORD_TTL));
                    announced = Some(b);
                }
                last_announce = Some(Instant::now());
            }
            if last_query.is_none_or(|t| t.elapsed() >= QUERY_EVERY) {
                say(&query);
                last_query = Some(Instant::now());
            }
            if last_check.elapsed() >= Duration::from_secs(1) {
                last_check = Instant::now();
                check_one_due(inner);
            }
        } else if let Some(old) = announced.take() {
            say(&advert(&old, &stype, 0));
            last_announce = None;
            // idle from here: whatever the network does meanwhile, join afresh when wanted again
            last_join = None;
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
        let (n, from) = match udp.recv_from(&mut buf) {
            Ok(got) => got,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
            Err(e) => {
                // the interface went away under the socket: join again, and don't spin meanwhile
                log::debug!("peer: mDNS read: {e}");
                inner.rejoin.store(true, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let Some(msg) = buf.get(..n).and_then(Message::decode) else { continue };
        if msg.is_response() {
            let (me, paired) = {
                let store = lock(&inner.store);
                (store.identity.device_id.to_ascii_lowercase(), store.peers.iter().map(|p| p.device_id.clone()).collect::<HashSet<String>>())
            };
            let mut st = lock(&inner.state);
            take_answers(&mut st.found, &msg, from.ip(), &stype, &me, &paired, Instant::now());
        } else if msg.questions.iter().any(|q| q.name.eq_ignore_ascii_case(&stype) && matches!(q.qtype, dns::TYPE_PTR | 255)) {
            // someone browses for us: answer by announcing now
            inner.nudge.store(true, Ordering::Relaxed);
        }
    }
}

/// Removes the entry seen longest ago among those `which` selects; `false` if there is none.
fn forget_oldest(found: &mut HashMap<String, Found>, which: impl Fn(&Found) -> bool) -> bool {
    let oldest = found.iter().filter(|(_, f)| which(f)).min_by_key(|(_, f)| f.seen).map(|(id, _)| id.clone());
    oldest.is_some_and(|id| found.remove(&id).is_some())
}

/// Adds the devices a response advertises to `found` (and removes those saying goodbye).
///
/// Adverts are not authenticated, so the list is kept useful under a flood of made-up ones:
/// `paired` devices are never pushed out, one sender holds at most [`MAX_PER_SOURCE`] unpaired
/// entries, and when the list is full the unpaired entry seen longest ago makes room.
fn take_answers(found: &mut HashMap<String, Found>, msg: &Message, from: IpAddr, stype: &str, me: &str, paired: &HashSet<String>, now: Instant) {
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
    let unpaired = |f: &Found| !paired.contains(&f.beacon.device_id);
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
        if !found.contains_key(&b.device_id) {
            let is_paired = paired.contains(&b.device_id);
            if !is_paired && found.values().filter(|f| f.from == from && unpaired(f)).count() >= MAX_PER_SOURCE {
                forget_oldest(found, |f| f.from == from && unpaired(f));
            }
            if found.len() >= MAX_FOUND {
                // those that left without a goodbye go first
                found.retain(|id, f| paired.contains(id) || now.saturating_duration_since(f.seen) < GONE_AFTER);
            }
            if found.len() >= MAX_FOUND && !forget_oldest(found, unpaired) && !is_paired {
                continue;
            }
        }
        let ip = addr.get(&target).copied().unwrap_or(from);
        found.insert(b.device_id.clone(), Found { beacon: b, ip, from, seen: now });
    }
}

/// Whether a paired device was heard at `ip` lately.
fn heard_paired_at(inner: &Inner, ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    let paired: HashSet<String> = lock(&inner.store).peers.iter().map(|p| p.device_id.clone()).collect();
    lock(&inner.state).found.values().any(|f| (f.ip == ip || f.from == ip) && f.fresh() && paired.contains(&f.beacon.device_id))
}

fn accept_loop(inner: &Arc<Inner>, tcp: &TcpListener) {
    while !inner.stop.load(Ordering::Relaxed) {
        match tcp.accept() {
            Ok((stream, from)) => {
                let Some(slot) = ConnSlot::take(inner, heard_paired_at(inner, from.ip())) else {
                    log::info!("peer: too many connections; closed the one from {from}");
                    drop(stream);
                    continue;
                };
                let spawned = std::thread::Builder::new().name("peer-conn".into()).spawn(move || {
                    let i = slot.inner.clone();
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

/// Notes an incoming connection that got as far as learning what the pairing code would be (this
/// device's hello) and then neither asked to pair nor turned out to be a paired device: the
/// handshake wasn't finished, no opening came, or it claimed to be paired and wasn't.
fn note_abandoned(inner: &Inner) {
    let mut q = lock(&inner.abandoned);
    if q.len() >= MAX_ABANDONED * 4 {
        q.pop_front();
    }
    q.push_back(Instant::now());
}

/// Whether so many incoming connections ended that way lately that a pairing code may have been guessed at.
fn too_many_abandoned(inner: &Inner) -> bool {
    let mut q = lock(&inner.abandoned);
    while q.front().is_some_and(|t| t.elapsed() > DECIDE_WITHIN) {
        q.pop_front();
    }
    q.len() >= MAX_ABANDONED
}

/// The paired device on the other end of `ch`, by id and proven key. Anyone else is told
/// "not paired" and gets [`Error::NotPaired`].
fn known_peer(inner: &Inner, ch: &mut Channel) -> Result<Peer> {
    let peer = lock(&inner.store).peer(&ch.peer_device).filter(|p| p.public_key == ch.peer_key).cloned();
    match peer {
        Some(p) => {
            // it connected as a paired device, so it has this one paired
            lock(&inner.state).not_paired_there.remove(&p.device_id);
            Ok(p)
        }
        None => {
            // a connection that went through the handshake and then didn't ask to pair
            note_abandoned(inner);
            ch.send_json(&Reply::NotPaired)?;
            Err(Error::NotPaired)
        }
    }
}

/// An incoming connection.
fn serve(inner: &Arc<Inner>, stream: TcpStream) -> Result<()> {
    stream.set_nonblocking(false)?;
    let me = lock(&inner.store).identity.clone();
    let mut ch = match Channel::handshake_within(stream, &me, false, PRE_AUTH_WITHIN) {
        Ok(ch) => ch,
        Err(e) => {
            if e != Error::Version {
                note_abandoned(inner);
            }
            return Err(e);
        }
    };
    // the opening is due within what is left of those few seconds (the channel keeps the deadline)
    let opening = match ch.recv_json::<Opening>() {
        Ok(opening) => opening,
        Err(e) => {
            // the code was known to the other side since the handshake: this may have been a guess too
            note_abandoned(inner);
            return Err(e);
        }
    };
    match opening {
        Opening::Pair { name } => serve_pairing(inner, ch, &me.name, &name),
        Opening::Hello { name } => {
            let peer = known_peer(inner, &mut ch)?;
            note_name(inner, &peer.device_id, &name);
            let peer = lock(&inner.store).peer(&peer.device_id).cloned().unwrap_or(peer);
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
        Opening::Check { name } => {
            let peer = known_peer(inner, &mut ch)?;
            note_name(inner, &peer.device_id, &name);
            ch.send_json(&Reply::Ready { name: me.name })
        }
    }
}

/// A pairing request: takes the one pairing slot (only now, after the handshake) unless this
/// device isn't looking, is busy, or has reason to refuse.
fn serve_pairing(inner: &Inner, mut ch: Channel, my_name: &str, name: &str) -> Result<()> {
    let name = clean_name(name, "Device");
    let no = |ch: &mut Channel, why: &str| ch.send_json(&Reply::No { why: why.into() });
    if !inner.finding.load(Ordering::Relaxed) {
        return no(&mut ch, "the other device isn't looking for devices");
    }
    if too_many_abandoned(inner) {
        return no(&mut ch, "the other device saw too many unfinished connection attempts; try again in a few minutes");
    }
    if lock(&inner.store).peer(&ch.peer_device).is_some_and(|p| p.public_key != ch.peer_key) {
        return no(&mut ch, "the other device has this one paired under another key; unpair it there first, then pair again");
    }
    let id = {
        let mut st = lock(&inner.state);
        if st.pairing.as_ref().is_some_and(Pairing::active) {
            drop(st);
            return no(&mut ch, "the other device is pairing with someone else");
        }
        st.begin(&ch.peer_device, &name, true)
    };
    if let Err(e) = ch.send_json(&Reply::Ready { name: my_name.to_string() }) {
        // the slot was taken above: free it, or no other pairing could ever start
        finish(inner, id, Err(e.clone()));
        return Err(e);
    }
    let r = decide(inner, id, ch, &name);
    finish(inner, id, r);
    Ok(())
}

/// Shows the code, waits for both sides' answers and, if both said yes, stores the pairing.
/// Returns the paired device.
///
/// The channel is watched the whole time: the other side's "no" (declined, cancelled, timed out)
/// or its closed connection ends the pairing here at once, not when this side answers. This
/// side's "no", at any time before the pairing is stored, is sent at once too.
fn decide(inner: &Inner, id: u64, mut ch: Channel, name: &str) -> Result<Peer> {
    let cancelled = || Error::Protocol("pairing cancelled".into());
    let (tx, rx): (Sender<bool>, Receiver<bool>) = channel();
    {
        let mut st = lock(&inner.state);
        let p = st.current(id).ok_or_else(cancelled)?;
        p.name = name.to_string();
        p.code = Some(ch.code.clone());
        p.stage = Stage::Confirm;
        p.decide = Some(tx);
    }
    let started = Instant::now();
    let (mut mine, mut theirs) = (false, false);
    while !(mine && theirs) {
        if inner.stop.load(Ordering::Relaxed) || started.elapsed() > DECIDE_WITHIN {
            let _ = ch.send_json(&Reply::Decision { ok: false });
            return Err(Error::Protocol("the pairing timed out".into()));
        }
        match rx.try_recv() {
            Ok(true) if !mine => {
                mine = true;
                if let Some(p) = lock(&inner.state).current(id) {
                    p.stage = Stage::Waiting;
                }
                ch.send_json(&Reply::Decision { ok: true })?;
            }
            Ok(true) | Err(TryRecvError::Empty) => {}
            // declined or cancelled here (the sender is gone when the pairing was replaced or dismissed)
            Ok(false) | Err(TryRecvError::Disconnected) => {
                let _ = ch.send_json(&Reply::Decision { ok: false });
                return Err(cancelled());
            }
        }
        if mine && theirs {
            break;
        }
        match ch.wait_readable(Duration::from_millis(150)) {
            Ok(false) => {}
            Ok(true) => {
                // something is coming: it has a few seconds to arrive whole
                ch.set_deadline(Duration::from_secs(5));
                match ch.recv_json::<Reply>()? {
                    Reply::Decision { ok: true } => theirs = true,
                    Reply::Decision { ok: false } => return Err(Error::Protocol("the other device cancelled the pairing".into())),
                    _ => return Err(Error::Protocol("unexpected answer".into())),
                }
            }
            Err(_) => return Err(Error::Protocol("the other device ended the pairing".into())),
        }
    }
    let peer = Peer { device_id: ch.peer_device.clone(), name: name.to_string(), public_key: ch.peer_key.clone(), paired_at: now_secs() };
    let mut store = lock(&inner.store);
    store.add_peer(peer.clone());
    store.save_merged(&inner.store_path, None)?;
    Ok(peer)
}

/// Shows how pairing `id` ended, unless another pairing took its place meanwhile.
fn finish(inner: &Inner, id: u64, r: Result<Peer>) {
    let mut st = lock(&inner.state);
    if let Ok(peer) = &r {
        // paired (again) on both sides, whatever the status shows now
        st.not_paired_there.remove(&peer.device_id);
    }
    if let Some(p) = st.current(id) {
        p.decide = None;
        p.stage = match r {
            Ok(_) => Stage::Paired,
            Err(e) => Stage::Failed(e.to_string()),
        };
    }
}

#[cfg(test)]
mod tests;
