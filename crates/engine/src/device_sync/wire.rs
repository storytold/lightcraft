//! One exchange with one device, on its own thread.
//!
//! Both sides say hello (app version, clock, the group of the open catalogue, an invitation).
//! Then, while anything moves: rounds of record batches (each side sends what changed and
//! confirms what it took), the starter asks for the files it lacks and gets them, the other side
//! does the same. Files travel in 1 MiB frames into a `.part` file that a later exchange carries
//! on from, and are checked (an original against its key, a preview by its own header) before
//! they take their place.
//!
//! The thread never touches the catalogue: it asks the session ([`Job`]) and waits. A session
//! that doesn't answer in time ends the exchange; nothing is confirmed that wasn't taken.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use lcc_peer::Channel;
use lcc_sync::pill::Depth;
use lcc_sync::proto::{self, Batch, CHUNK, Get, Hello, MAX_BLOB, MAX_GETS, Msg, Refusal};
use lcc_sync::replica::Outgoing;

use super::{Hub, Job, Link, Want, lock};
use crate::activity::Cancel;

/// How long the session may take to answer (it answers once a second).
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// The most rounds of one exchange (each moves up to a batch and some files both ways).
const MAX_CYCLES: usize = 10_000;

/// What an exchange moved.
#[derive(Default)]
struct Moved {
    sent: usize,
    received: usize,
    files: usize,
}

type Outcome = Result<Moved, String>;

fn ask<T>(hub: &Hub, make: impl FnOnce(Sender<T>) -> Job) -> Result<T, String> {
    let (tx, rx) = channel();
    hub.ask(make(tx));
    rx.recv_timeout(ANSWER_WITHIN).map_err(|_| "the app was busy; sync carries on at the next exchange".to_string())
}

fn send(ch: &mut Channel, m: &Msg) -> Result<(), String> {
    ch.send_json(m).map_err(|e| e.to_string())
}

fn recv(ch: &mut Channel) -> Result<Msg, String> {
    ch.recv_json::<Msg>().map_err(|e| e.to_string())
}

fn refusal(r: &Refusal, name: &str) -> String {
    match r {
        Refusal::Protocol => format!("{name} runs a version of the app that syncs differently: update the app on both devices"),
        Refusal::Version(v) => {
            format!("{name} runs version {v} and this device {}: both need the same version to sync", lcc_catalog::APP_VERSION)
        }
        Refusal::Clock(min) => format!("the clocks of this device and {name} differ by {min} minutes: set both to the right time"),
    }
}

/// Records how an exchange ended, and frees the device for the next one.
fn finish(hub: &Hub, device: &str, out: Outcome) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    {
        let mut links = lock(&hub.links);
        let l = links.entry(device.to_string()).or_default();
        l.running = false;
        match out {
            Ok(m) => {
                l.last = now;
                l.error = None;
                if m.sent + m.received + m.files > 0 {
                    *l = Link { running: false, last: now, error: None, sent: m.sent, received: m.received, files: m.files };
                }
            }
            // (both devices started at once: one of the two exchanges goes on)
            Err(e) if e == BUSY => {}
            Err(e) => l.error = Some(e),
        }
    }
    lock(&hub.running).remove(device);
}

const BUSY: &str = "busy";

/// Both hellos. `Ok(false)`: nothing to exchange (other groups, or none).
fn hello(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel, theirs: Option<Hello>) -> Result<bool, String> {
    let mine = ask(hub, |reply| Job::Hello { peer: peer.device_id.clone(), reply })?;
    send(ch, &Msg::Hello(mine.clone()))?;
    let theirs = match theirs {
        Some(h) => h,
        None => match recv(ch)? {
            Msg::Hello(h) => h,
            Msg::Bye { reason } if reason == BUSY => return Err(BUSY.into()),
            Msg::Bye { reason } => return Err(reason.chars().take(300).collect()),
            _ => return Err("the other device answered out of turn".into()),
        },
    };
    if let Err(r) = proto::check(&mine, &theirs) {
        return Err(refusal(&r, &peer.name));
    }
    let same = mine.group.is_some() && mine.group == theirs.group;
    hub.ask(Job::Their { peer: peer.clone(), hello: Box::new(theirs) });
    Ok(same)
}

/// The device that starts: connects and leads the rounds.
pub(super) fn start(svc: &lcc_peer::Service, hub: &Arc<Hub>, peer: lcc_peer::Peer) {
    let out = match svc.connect(&peer.device_id) {
        Ok(mut ch) => lead(hub, &peer, &mut ch),
        Err(e) => Err(e.to_string()),
    };
    finish(hub, &peer.device_id, out);
}

fn lead(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel) -> Outcome {
    let mut moved = Moved::default();
    if !hello(hub, peer, ch, None)? {
        let _ = send(ch, &Msg::Bye { reason: String::new() });
        return Ok(moved);
    }
    // what of theirs was taken and not yet confirmed to them
    let mut ack: Option<u64> = None;
    let mut acked: Option<u64> = None;
    for _ in 0..MAX_CYCLES {
        let mut busy = false;
        let mut any = false;
        // records, until neither side has more
        loop {
            let (mut batch, out): (Batch, Outgoing) = ask(hub, |reply| Job::Prepare { peer: peer.device_id.clone(), reply })?;
            batch.ack = ack.take();
            let (sent, more) = (batch.items.len(), batch.more);
            let upto = batch.upto;
            send(ch, &Msg::Batch(batch))?;
            let reply = match recv(ch)? {
                Msg::Batch(b) => b,
                Msg::Bye { reason } => return Err(reason.chars().take(300).collect()),
                _ => return Err("the other device answered out of turn".into()),
            };
            if reply.ack == Some(upto) {
                hub.ask(Job::Confirm { peer: peer.device_id.clone(), out });
                moved.sent += sent;
            } else if sent > 0 {
                // the other side couldn't take it now (a photo is being edited there): later
                busy = true;
            }
            let (got, their_more, their_upto) = (reply.items.len(), reply.more, reply.upto);
            match ask(hub, |r| Job::Apply { peer: peer.clone(), batch: Box::new(reply), reply: r })? {
                Ok(()) => {
                    moved.received += got;
                    if acked != Some(their_upto) {
                        ack = Some(their_upto);
                        acked = Some(their_upto);
                    }
                }
                Err(_) => busy = true,
            }
            any |= sent + got > 0;
            if busy || (sent == 0 && !more && got == 0 && !their_more && ack.is_none()) {
                break;
            }
        }
        // files: mine, then theirs
        let wants: Vec<Want> = ask(hub, |reply| Job::Wants { peer: peer.device_id.clone(), reply })?;
        let mine = fetch(hub, peer, ch, wants)?;
        send(ch, &Msg::Turn)?;
        let theirs = serve_until_turn(hub, peer, ch)?;
        moved.files += mine;
        if busy || (!any && mine + theirs == 0) {
            break;
        }
    }
    let _ = send(ch, &Msg::Bye { reason: String::new() });
    Ok(moved)
}

/// The device that was called: answers until the other says goodbye.
pub(super) fn answer(hub: &Arc<Hub>, peer: lcc_peer::Peer, mut ch: Channel) {
    // the other device started while this one was starting with it: its exchange is turned away
    // by the device with the lower id, so exactly one goes on
    let first = match recv(&mut ch) {
        Ok(Msg::Hello(h)) => h,
        _ => return,
    };
    {
        let mut running = lock(&hub.running);
        if running.contains(&peer.device_id) && hub.me < peer.device_id {
            drop(running);
            let _ = send(&mut ch, &Msg::Bye { reason: BUSY.into() });
            return;
        }
        running.insert(peer.device_id.clone());
    }
    lock(&hub.links).entry(peer.device_id.clone()).or_default().running = true;
    let out = follow(hub, &peer, &mut ch, first);
    finish(hub, &peer.device_id, out);
}

fn follow(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel, first: Hello) -> Outcome {
    let mut moved = Moved::default();
    let same = hello(hub, peer, ch, Some(first))?;
    let mut last: Option<(Outgoing, usize)> = None;
    for _ in 0..MAX_CYCLES.saturating_mul(64) {
        match recv(ch) {
            Ok(Msg::Batch(b)) if same => {
                if let Some((out, n)) = last.take()
                    && b.ack == Some(out.upto)
                {
                    hub.ask(Job::Confirm { peer: peer.device_id.clone(), out });
                    moved.sent += n;
                }
                let (got, upto) = (b.items.len(), b.upto);
                let taken = ask(hub, |reply| Job::Apply { peer: peer.clone(), batch: Box::new(b), reply })?.is_ok();
                if taken {
                    moved.received += got;
                }
                let (mut batch, out): (Batch, Outgoing) = ask(hub, |reply| Job::Prepare { peer: peer.device_id.clone(), reply })?;
                batch.ack = taken.then_some(upto);
                let n = batch.items.len();
                send(ch, &Msg::Batch(batch))?;
                last = Some((out, n));
            }
            Ok(Msg::Gets { wants }) if same => serve(hub, peer, ch, wants)?,
            Ok(Msg::Turn) if same => {
                let wants: Vec<Want> = ask(hub, |reply| Job::Wants { peer: peer.device_id.clone(), reply })?;
                moved.files += fetch(hub, peer, ch, wants)?;
                send(ch, &Msg::Turn)?;
            }
            Ok(Msg::Bye { .. }) => return Ok(moved),
            Ok(_) => return Err("the other device spoke out of turn".into()),
            // the other side hung up after its goodbye was lost, or the network dropped
            Err(e) => return if moved.sent + moved.received + moved.files > 0 || !same { Ok(moved) } else { Err(e) },
        }
    }
    Ok(moved)
}

/// Answers the other side's requests for files until it hands the turn back. Says how many went.
fn serve_until_turn(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel) -> Result<usize, String> {
    let mut n = 0;
    for _ in 0..MAX_CYCLES {
        match recv(ch)? {
            Msg::Gets { wants } => {
                n += wants.len();
                serve(hub, peer, ch, wants)?;
            }
            Msg::Turn => return Ok(n),
            Msg::Bye { reason } => return Err(reason.chars().take(300).collect()),
            _ => return Err("the other device spoke out of turn".into()),
        }
    }
    Ok(n)
}

fn part_of(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// The hash of a file's bytes as the catalogue keys photos by it.
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = lcc_preview::Hasher128::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finish().to_string());
        }
        h.update(buf.get(..n).unwrap_or(&[]));
    }
}

/// Asks `peer` for files and takes them in. Says how many arrived whole.
fn fetch(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel, wants: Vec<Want>) -> Result<usize, String> {
    if wants.is_empty() {
        return Ok(0);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let task = hub.activity.start("sync", &format!("Syncing photos with {}", peer.name), Cancel::Flag(cancel.clone()));
    task.progress(0, wants.len() as u64);
    let gets: Vec<Get> = wants
        .iter()
        .map(|w| Get { key: w.key.clone(), depth: w.depth, offset: std::fs::metadata(part_of(&w.dest)).map(|m| m.len()).unwrap_or(0) })
        .collect();
    send(ch, &Msg::Gets { wants: gets.clone() })?;
    let mut done = 0usize;
    for (i, (w, g)) in wants.iter().zip(&gets).enumerate() {
        let (size, offset) = match recv(ch)? {
            Msg::NotHere { later, .. } => {
                if !later {
                    hub.ask(Job::Missing { peer: peer.device_id.clone(), key: w.key.clone() });
                }
                task.progress(i as u64 + 1, wants.len() as u64);
                continue;
            }
            Msg::Blob { key, depth, size, offset } if key == w.key && depth == w.depth && size <= MAX_BLOB && offset <= size && (offset == 0 || offset == g.offset) => {
                (size, offset)
            }
            Msg::Bye { reason } => return Err(reason.chars().take(300).collect()),
            _ => return Err("the other device sent something that wasn't asked for".into()),
        };
        lock(&hub.moving).insert(w.key.clone());
        let got = receive(ch, w, size, offset, &cancel);
        lock(&hub.moving).remove(&w.key);
        got?;
        hub.ask(Job::Got { key: w.key.clone(), depth: w.depth });
        done += 1;
        task.progress(i as u64 + 1, wants.len() as u64);
    }
    Ok(done)
}

/// Reads one file's frames into its `.part` file, checks it and puts it in place.
fn receive(ch: &mut Channel, w: &Want, size: u64, offset: u64, cancel: &AtomicBool) -> Result<(), String> {
    let name = w.dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let io = |e: std::io::Error| format!("couldn't write `{name}`: {e}");
    if let Some(parent) = w.dest.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let part = part_of(&w.dest);
    let mut f = OpenOptions::new().create(true).write(true).truncate(offset == 0).open(&part).map_err(io)?;
    f.set_len(offset).map_err(io)?;
    f.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut at = offset;
    while at < size {
        if cancel.load(Ordering::Relaxed) {
            // (what arrived stays, and the next exchange carries on from there)
            return Err("syncing photos was cancelled".into());
        }
        let frame = ch.recv().map_err(|e| e.to_string())?;
        if frame.is_empty() || frame.len() > CHUNK || frame.len() as u64 > size - at {
            return Err("the other device sent a file that doesn't match its size".into());
        }
        f.write_all(&frame).map_err(io)?;
        at += frame.len() as u64;
    }
    f.sync_all().map_err(io)?;
    drop(f);
    let good = match w.depth {
        Depth::Full => hash_file(&part).map_err(io)? == w.key,
        Depth::Preview => crate::smart::is_valid(&part),
        Depth::None => false,
    };
    if !good {
        let _ = std::fs::remove_file(&part);
        return Err(format!("`{name}` arrived damaged; it is fetched again at the next exchange"));
    }
    std::fs::rename(&part, &w.dest).map_err(io)
}

/// Sends the files `peer` asked for (those it may have and that are here), in order.
fn serve(hub: &Hub, peer: &lcc_peer::Peer, ch: &mut Channel, wants: Vec<Get>) -> Result<(), String> {
    if wants.len() > MAX_GETS {
        return Err("the other device asked for too much at once".into());
    }
    let asked: Vec<(String, Depth)> = wants.iter().map(|g| (g.key.clone(), g.depth)).collect();
    let found = ask(hub, |reply| Job::Locate { peer: peer.device_id.clone(), wants: asked, reply })?;
    let mut buf = vec![0u8; CHUNK];
    for (i, g) in wants.iter().enumerate() {
        let not_here = |later: bool| Msg::NotHere { key: g.key.clone(), depth: g.depth, later };
        let path = match found.get(i) {
            Some(Ok(p)) => p,
            Some(Err(later)) => {
                send(ch, &not_here(*later))?;
                continue;
            }
            None => {
                send(ch, &not_here(false))?;
                continue;
            }
        };
        let opened = File::open(path).and_then(|f| f.metadata().map(|m| (f, m.len())));
        let Ok((mut f, size)) = opened else {
            // (an original on a drive that isn't connected, say)
            send(ch, &not_here(false))?;
            continue;
        };
        if size > MAX_BLOB {
            send(ch, &not_here(false))?;
            continue;
        }
        let offset = if g.offset < size { g.offset } else { 0 };
        if f.seek(SeekFrom::Start(offset)).is_err() {
            send(ch, &not_here(false))?;
            continue;
        }
        send(ch, &Msg::Blob { key: g.key.clone(), depth: g.depth, size, offset })?;
        let mut at = offset;
        while at < size {
            let want = (size - at).min(CHUNK as u64) as usize;
            let slice = buf.get_mut(..want).ok_or("buffer")?;
            // (a file that shrank while it was read: the exchange ends, and the next one starts it over)
            f.read_exact(slice).map_err(|e| format!("couldn't read a file to send: {e}"))?;
            ch.send(slice).map_err(|e| e.to_string())?;
            at += want as u64;
        }
    }
    Ok(())
}
