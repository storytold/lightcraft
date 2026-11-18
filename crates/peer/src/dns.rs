//! A small, bounds-checked DNS message encoder and decoder: just what multicast DNS service
//! discovery needs (RFC 1035 wire format as used by RFC 6762 / RFC 6763).
//!
//! The decoder treats every byte as hostile: all reads are checked, name-compression pointers may
//! only point backwards and are followed at most [`MAX_JUMPS`] times, names are capped at 255
//! bytes, and section counts are capped. Anything malformed is `None`, never a panic.

use std::net::{Ipv4Addr, Ipv6Addr};

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const CLASS_IN: u16 = 1;
/// Top bit of the class in a record: "cache flush" (RFC 6762 §10.2); in a question: "unicast reply wanted".
pub const CLASS_TOP: u16 = 0x8000;
/// Flags of a response from an authoritative mDNS responder.
pub const FLAGS_RESPONSE: u16 = 0x8400;

/// Name-compression pointers followed while reading one name.
pub const MAX_JUMPS: usize = 16;
/// Longest name (RFC 1035 §3.1).
pub const MAX_NAME: usize = 255;
/// Records or questions read per section; more are ignored.
pub const MAX_PER_SECTION: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Dotted, without the trailing dot, e.g. `_x._tcp.local`.
    pub name: String,
    pub qtype: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Ptr(String),
    Srv { port: u16, target: String },
    Txt(Vec<Vec<u8>>),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub ttl: u32,
    /// Sets the cache-flush bit when encoding (unique records: SRV, TXT, A).
    pub flush: bool,
    pub data: Data,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub flags: u16,
    pub questions: Vec<Question>,
    /// Answer, authority and additional records together.
    pub records: Vec<Record>,
}

impl Message {
    pub fn is_response(&self) -> bool {
        self.flags & 0x8000 != 0
    }

    /// The wire form, or `None` if a name can't be encoded (a label over 63 bytes, a name over 255).
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        let qd = u16::try_from(self.questions.len()).ok()?;
        let an = u16::try_from(self.records.len()).ok()?;
        for n in [qd, an, 0, 0] {
            out.extend_from_slice(&n.to_be_bytes());
        }
        for q in &self.questions {
            put_name(&mut out, &q.name)?;
            out.extend_from_slice(&q.qtype.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
        }
        for r in &self.records {
            put_name(&mut out, &r.name)?;
            let (rtype, rdata) = match &r.data {
                Data::A(ip) => (TYPE_A, ip.octets().to_vec()),
                Data::Aaaa(ip) => (TYPE_AAAA, ip.octets().to_vec()),
                Data::Ptr(n) => {
                    let mut d = Vec::new();
                    put_name(&mut d, n)?;
                    (TYPE_PTR, d)
                }
                Data::Srv { port, target } => {
                    let mut d = vec![0, 0, 0, 0];
                    d.extend_from_slice(&port.to_be_bytes());
                    put_name(&mut d, target)?;
                    (TYPE_SRV, d)
                }
                Data::Txt(items) => {
                    let mut d = Vec::new();
                    for s in items {
                        d.push(u8::try_from(s.len()).ok()?);
                        d.extend_from_slice(s);
                    }
                    if d.is_empty() {
                        d.push(0);
                    }
                    (TYPE_TXT, d)
                }
                Data::Other => return None,
            };
            out.extend_from_slice(&rtype.to_be_bytes());
            let class = if r.flush { CLASS_IN | CLASS_TOP } else { CLASS_IN };
            out.extend_from_slice(&class.to_be_bytes());
            out.extend_from_slice(&r.ttl.to_be_bytes());
            out.extend_from_slice(&u16::try_from(rdata.len()).ok()?.to_be_bytes());
            out.extend_from_slice(&rdata);
        }
        Some(out)
    }

    /// A message from a datagram; anything malformed is `None`.
    pub fn decode(buf: &[u8]) -> Option<Message> {
        let mut r = Reader { buf, pos: 0 };
        let _id = r.u16()?;
        let flags = r.u16()?;
        let qd = usize::from(r.u16()?);
        let counts = [r.u16()?, r.u16()?, r.u16()?];
        let mut msg = Message { flags, ..Message::default() };
        for i in 0..qd {
            let name = r.name()?;
            let qtype = r.u16()?;
            let _class = r.u16()?;
            if i < MAX_PER_SECTION {
                msg.questions.push(Question { name, qtype });
            }
        }
        for n in counts {
            for i in 0..usize::from(n) {
                let rec = r.record()?;
                if i < MAX_PER_SECTION {
                    msg.records.push(rec);
                }
            }
        }
        Some(msg)
    }
}

fn put_name(out: &mut Vec<u8>, name: &str) -> Option<()> {
    let mut len = 1usize;
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let b = label.as_bytes();
        if b.len() > 63 {
            return None;
        }
        len = len.checked_add(b.len() + 1)?;
        if len > MAX_NAME {
            return None;
        }
        out.push(u8::try_from(b.len()).ok()?);
        out.extend_from_slice(b);
    }
    out.push(0);
    Some(())
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes(4)?;
        Some(u32::from_be_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
    }

    /// A possibly compressed name at the current position; the position moves past it.
    fn name(&mut self) -> Option<String> {
        let (name, end) = read_name(self.buf, self.pos)?;
        self.pos = end;
        Some(name)
    }

    fn record(&mut self) -> Option<Record> {
        let name = self.name()?;
        let rtype = self.u16()?;
        let class = self.u16()?;
        let ttl = self.u32()?;
        let len = usize::from(self.u16()?);
        let start = self.pos;
        let rdata = self.bytes(len)?;
        let data = match rtype {
            TYPE_A => {
                let o: [u8; 4] = rdata.try_into().ok()?;
                Data::A(Ipv4Addr::from(o))
            }
            TYPE_AAAA => {
                let o: [u8; 16] = rdata.try_into().ok()?;
                Data::Aaaa(Ipv6Addr::from(o))
            }
            TYPE_PTR => {
                let (n, end) = read_name(self.buf, start)?;
                (end <= self.pos).then_some(())?;
                Data::Ptr(n)
            }
            TYPE_SRV => {
                let port = u16::from_be_bytes([*rdata.get(4)?, *rdata.get(5)?]);
                let (target, end) = read_name(self.buf, start.checked_add(6)?)?;
                (end <= self.pos).then_some(())?;
                Data::Srv { port, target }
            }
            TYPE_TXT => {
                let mut items = Vec::new();
                let mut i = 0usize;
                while i < rdata.len() {
                    let n = usize::from(*rdata.get(i)?);
                    let s = rdata.get(i + 1..i + 1 + n)?;
                    if !s.is_empty() {
                        items.push(s.to_vec());
                    }
                    i += 1 + n;
                }
                Data::Txt(items)
            }
            _ => Data::Other,
        };
        Some(Record { name, ttl, flush: class & CLASS_TOP != 0, data })
    }
}

/// Reads a name starting at `pos`; returns it (dotted, lowercase-preserving) and the position just
/// past it in the original stream. Pointers must point strictly backwards.
fn read_name(buf: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut wire_len = 1usize;
    let mut end = None;
    let mut jumps = 0usize;
    loop {
        let len = *buf.get(pos)?;
        match len & 0xC0 {
            0x00 => {
                if len == 0 {
                    return Some((out, end.unwrap_or(pos + 1)));
                }
                let n = usize::from(len);
                let label = buf.get(pos + 1..pos + 1 + n)?;
                wire_len = wire_len.checked_add(n + 1)?;
                if wire_len > MAX_NAME {
                    return None;
                }
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(&String::from_utf8_lossy(label));
                pos += 1 + n;
            }
            0xC0 => {
                let lo = *buf.get(pos + 1)?;
                let target = (usize::from(len & 0x3F) << 8) | usize::from(lo);
                jumps += 1;
                if jumps > MAX_JUMPS || target >= pos {
                    return None;
                }
                if end.is_none() {
                    end = Some(pos + 2);
                }
                pos = target;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Message {
        Message {
            flags: FLAGS_RESPONSE,
            questions: vec![Question { name: "_x._tcp.local".into(), qtype: TYPE_PTR }],
            records: vec![
                Record { name: "_x._tcp.local".into(), ttl: 10, flush: false, data: Data::Ptr("ab._x._tcp.local".into()) },
                Record { name: "ab._x._tcp.local".into(), ttl: 10, flush: true, data: Data::Srv { port: 4000, target: "ab.local".into() } },
                Record { name: "ab._x._tcp.local".into(), ttl: 10, flush: true, data: Data::Txt(vec![b"v=1".to_vec(), b"id=ab".to_vec()]) },
                Record { name: "ab.local".into(), ttl: 10, flush: true, data: Data::A(Ipv4Addr::new(192, 168, 1, 2)) },
                Record { name: "ab.local".into(), ttl: 10, flush: true, data: Data::Aaaa(Ipv6Addr::LOCALHOST) },
            ],
        }
    }

    #[test]
    fn messages_round_trip_and_unencodable_names_are_refused() {
        // round trips
        {
            let m = sample();
            assert_eq!(Message::decode(&m.encode().unwrap()), Some(m));
        }
        // reads compressed names
        {
            // header, one answer: "a.local" PTR -> pointer to the owner name
            let mut b = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
            b.extend_from_slice(&[1, b'a', 5, b'l', b'o', b'c', b'a', b'l', 0]);
            b.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 10, 0, 2, 0xC0, 12]);
            let m = Message::decode(&b).unwrap();
            assert_eq!(m.records[0].data, Data::Ptr("a.local".into()));
        }
        // unencodable names are refused
        {
            let long = "a".repeat(64);
            let m = Message { questions: vec![Question { name: long, qtype: TYPE_PTR }], ..Message::default() };
            assert_eq!(m.encode(), None);
            let deep = vec!["abcdefghij"; 30].join(".");
            let m = Message { questions: vec![Question { name: deep, qtype: TYPE_PTR }], ..Message::default() };
            assert_eq!(m.encode(), None);
        }
    }

    /// Malformed datagrams are `None` (or harmlessly parsed), never a panic or a hang.
    #[test]
    fn hostile_packets_do_not_panic() {
        let header = |qd: u16, an: u16| {
            let mut v = vec![0, 0, 0x84, 0];
            v.extend_from_slice(&qd.to_be_bytes());
            v.extend_from_slice(&an.to_be_bytes());
            v.extend_from_slice(&[0, 0, 0, 0]);
            v
        };
        let with = |mut h: Vec<u8>, tail: &[u8]| {
            h.extend_from_slice(tail);
            h
        };
        let good = sample().encode().unwrap();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", vec![]),
            ("short header", vec![0, 0, 0x84]),
            ("counts beyond data", header(5, 5)),
            ("pointer loop to itself", with(header(1, 0), &[0xC0, 12, 0, 12, 0, 1])),
            ("forward pointer", with(header(1, 0), &[0xC0, 40, 0, 12, 0, 1])),
            ("two pointers ping-pong", with(header(1, 0), &[1, b'a', 0xC0, 12, 0, 12, 0, 1])),
            ("reserved label bits", with(header(1, 0), &[0x40, 0, 12, 0, 1])),
            ("label past end", with(header(1, 0), &[60, b'a'])),
            ("rdlength past end", with(header(0, 1), &[0, 0, 1, 0, 1, 0, 0, 0, 1, 0xFF, 0xFF, 1])),
            ("A of wrong size", with(header(0, 1), &[0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 3, 1, 2, 3])),
            ("SRV too short", with(header(0, 1), &[0, 0, 33, 0, 1, 0, 0, 0, 1, 0, 2, 0, 0])),
            ("TXT item past rdata", with(header(0, 1), &[0, 0, 16, 0, 1, 0, 0, 0, 1, 0, 2, 9, b'a'])),
            ("PTR name runs past rdata", with(header(0, 1), &[0, 0, 12, 0, 1, 0, 0, 0, 1, 0, 1, 3, b'a', b'b', b'c', 0])),
            ("max counts", header(u16::MAX, u16::MAX)),
            ("all ones", vec![0xFF; 600]),
        ];
        for (what, bytes) in &cases {
            assert_eq!(Message::decode(bytes), None, "{what}");
        }
        // every truncation of a good packet, and every single-byte corruption, is handled
        for n in 0..good.len() {
            assert_eq!(Message::decode(&good[..n]), None, "truncated at {n}");
        }
        for i in 0..good.len() {
            for v in [0x00, 0x3F, 0xC0, 0xFF] {
                let mut b = good.clone();
                b[i] = v;
                let _ = Message::decode(&b);
            }
        }
        // a long chain of backward pointers stops at the jump limit
        let mut chain = header(1, 0);
        chain.extend_from_slice(&[1, b'a', 0]);
        for k in 0..40u16 {
            let at = 12 + 3 + k * 2 - if k == 0 { 3 } else { 2 };
            chain.extend_from_slice(&(0xC000 | at).to_be_bytes());
        }
        let _ = Message::decode(&chain);
    }
}
