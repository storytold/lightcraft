//! Bounds-checked IFD parsing.

use crate::{ByteOrder, Entry, FieldType, Ifd, Result, Tiff, TiffError, Value, tags};
use std::collections::HashSet;

/// Limits applied while parsing. The defaults are generous for real files and safe for hostile ones.
#[derive(Clone, Debug)]
pub struct ParseOptions {
    /// Maximum number of IFDs visited in total (chain + children).
    pub max_ifds: usize,
    /// Maximum nesting depth of child IFDs.
    pub max_depth: usize,
    /// Maximum number of entries read from one IFD.
    pub max_entries: usize,
    /// Maximum total bytes of decoded values; `None` = `8 × input length + 1 MiB`.
    pub value_budget: Option<u64>,
    /// Follow `SubIFDs` / Exif / GPS / Interop pointers.
    pub follow_children: bool,
    /// Accept the TIFF-like magic numbers used by some raw formats (ORF `IIRO`/`IIRS`, RW2 `IIU`).
    pub accept_raw_magic: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self { max_ifds: 1024, max_depth: 10, max_entries: 8192, value_budget: None, follow_children: true, accept_raw_magic: true }
    }
}

impl ParseOptions {
    /// The value budget for `data_len` bytes of input: [`Self::value_budget`], else `8 × data_len + 1 MiB`.
    pub fn budget_for(&self, data_len: usize) -> u64 {
        self.value_budget.unwrap_or((data_len as u64).saturating_mul(8).saturating_add(1 << 20))
    }
}

/// Where a directory's entries are and how many of them the reader reads, as [`directory_entries`] found them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryEntries {
    /// Position of the first entry.
    table: usize,
    /// Entries read: the declared count, cut to the entries the data holds and to the `max_entries` limit.
    read: usize,
    /// The declared entry count.
    declared: u64,
    /// Bytes per entry: 12, or 20 in BigTIFF.
    entry_size: usize,
}

impl DirectoryEntries {
    /// Positions of the entries read, in directory order. Checked arithmetic: the iteration ends rather than
    /// overflow, whatever the layout.
    pub fn positions(&self) -> impl Iterator<Item = usize> + use<> {
        let (table, size) = (self.table, self.entry_size);
        (0..self.read).map_while(move |i| i.checked_mul(size)?.checked_add(table))
    }

    /// The number of entries read.
    pub fn read(&self) -> usize {
        self.read
    }

    /// The entry count the directory declares.
    pub fn declared(&self) -> u64 {
        self.declared
    }
}

/// The directory at `offset` as the reader walks it: its entry table and how many entries it reads. The same errors
/// as [`parse_ifd_at`] for a directory it can't read: past the data, empty, or without room for one entry.
pub fn directory_entries(data: &[u8], offset: u64, order: ByteOrder, bigtiff: bool, max_entries: usize) -> Result<DirectoryEntries> {
    let pos = usize::try_from(offset).map_err(|_| TiffError::Truncated(offset))?;
    let (n, head, entry_size) = if bigtiff {
        (order.read_u64(data, pos).ok_or(TiffError::Truncated(offset))?, 8usize, 20usize)
    } else {
        (order.read_u16(data, pos).ok_or(TiffError::Truncated(offset))? as u64, 2, 12)
    };
    let table = pos + head;
    let fit = (data.len().saturating_sub(table) / entry_size) as u64;
    let read = n.min(fit).min(max_entries as u64) as usize;
    if n == 0 {
        return Err(TiffError::Invalid(format!("empty IFD at {offset}")));
    }
    if read == 0 {
        return Err(TiffError::Truncated(offset));
    }
    Ok(DirectoryEntries { table, read, declared: n, entry_size })
}

/// A directory entry the reader can read: its tag, type and count, and where its values are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntrySpan {
    pub tag: u16,
    pub field_type: FieldType,
    pub count: u64,
    /// Position of the values in the data: the entry's own value field when they fit it, else `base` plus the offset
    /// stored there.
    pub start: u64,
    /// `count × size` bytes.
    pub len: u64,
}

/// The entry at `pos`, when the reader reads it: a known field type, `count × size` without overflow, and values
/// inside `data`. Nothing is decoded; [`parse_ifd_at`] also drops an entry whose values exceed what is left of its
/// value budget ([`ParseOptions::budget_for`]), which this leaves to the caller.
pub fn entry_span(data: &[u8], pos: usize, order: ByteOrder, bigtiff: bool, base: u64) -> Option<EntrySpan> {
    let tag = order.read_u16(data, pos)?;
    let field_type = FieldType::from_u16(order.read_u16(data, pos.checked_add(2)?)?)?;
    let (count, field, inline) = if bigtiff {
        (order.read_u64(data, pos.checked_add(4)?)?, pos.checked_add(12)?, 8u64)
    } else {
        (order.read_u32(data, pos.checked_add(4)?)? as u64, pos.checked_add(8)?, 4)
    };
    let len = count.checked_mul(field_type.size() as u64)?;
    let start = if len <= inline {
        field as u64
    } else {
        let rel = if bigtiff { order.read_u64(data, field)? } else { order.read_u32(data, field)? as u64 };
        base.checked_add(rel)?
    };
    let end = start.checked_add(len)?;
    (end <= data.len() as u64).then_some(EntrySpan { tag, field_type, count, start, len })
}

/// Whether a directory declaring `declared` entries was read whole (`read` entries), so that its next-IFD pointer
/// follows them. The counts compare as 64-bit numbers: narrowing the declared count to `usize` instead would, on a
/// 32-bit target, let a BigTIFF declaring 4,294,967,297 entries but holding one pass for a whole directory.
fn reads_every_entry(declared: u64, read: usize) -> bool {
    u64::try_from(read).is_ok_and(|read| read == declared)
}

/// Parse the header: (byte order, bigtiff, first IFD offset).
pub(crate) fn header(data: &[u8]) -> Result<(ByteOrder, bool, u64)> {
    header_with(data, true)
}

fn header_with(data: &[u8], raw_magic: bool) -> Result<(ByteOrder, bool, u64)> {
    let order = match data.get(0..2) {
        Some(b"II") => ByteOrder::Little,
        Some(b"MM") => ByteOrder::Big,
        _ => return Err(TiffError::NotTiff),
    };
    let magic = order.read_u16(data, 2).ok_or(TiffError::NotTiff)?;
    match magic {
        42 => Ok((order, false, order.read_u32(data, 4).ok_or(TiffError::Truncated(4))? as u64)),
        43 => {
            let bytesize = order.read_u16(data, 4).ok_or(TiffError::Truncated(4))?;
            let zero = order.read_u16(data, 6).ok_or(TiffError::Truncated(6))?;
            if bytesize != 8 || zero != 0 {
                return Err(TiffError::NotTiff);
            }
            Ok((order, true, order.read_u64(data, 8).ok_or(TiffError::Truncated(8))?))
        }
        0x4f52 | 0x5352 | 0x0055 if raw_magic => Ok((order, false, order.read_u32(data, 4).ok_or(TiffError::Truncated(4))? as u64)),
        _ => Err(TiffError::NotTiff),
    }
}

pub(crate) fn parse(data: &[u8], opts: &ParseOptions) -> Result<Tiff> {
    let (order, big, first) = header_with(data, opts.accept_raw_magic)?;
    let mut ctx = Ctx::new(data, order, big, opts);
    let mut ifds = Vec::new();
    let mut off = first;
    while off != 0 {
        match ctx.ifd(off, 0, 0) {
            Ok((ifd, next)) => {
                ifds.push(ifd);
                off = next;
            }
            Err(e) => {
                if ifds.is_empty() {
                    return Err(e);
                }
                break;
            }
        }
    }
    if ifds.is_empty() {
        return Err(TiffError::Invalid("no IFDs".into()));
    }
    Ok(Tiff { order, bigtiff: big, ifds })
}

/// Parse one IFD at absolute `offset` whose value offsets (and child IFD pointers) are relative to
/// `base` (absolute position = `base + stored offset`). Returns the IFD and the absolute offset of the
/// next IFD (0 when there is none). Used for maker notes, which often use their own offset base.
pub fn parse_ifd_at(data: &[u8], offset: u64, order: ByteOrder, base: u64, bigtiff: bool, opts: &ParseOptions) -> Result<(Ifd, u64)> {
    let mut ctx = Ctx::new(data, order, bigtiff, opts);
    ctx.ifd(offset, base, 0)
}

struct Ctx<'a> {
    data: &'a [u8],
    order: ByteOrder,
    big: bool,
    opts: &'a ParseOptions,
    visited: HashSet<u64>,
    budget: u64,
}

impl<'a> Ctx<'a> {
    fn new(data: &'a [u8], order: ByteOrder, big: bool, opts: &'a ParseOptions) -> Self {
        let budget = opts.budget_for(data.len());
        Self { data, order, big, opts, visited: HashSet::new(), budget }
    }

    fn read_off(&self, pos: usize) -> Option<u64> {
        if self.big { self.order.read_u64(self.data, pos) } else { self.order.read_u32(self.data, pos).map(|v| v as u64) }
    }

    /// Returns (ifd, absolute next offset or 0).
    fn ifd(&mut self, offset: u64, base: u64, depth: usize) -> Result<(Ifd, u64)> {
        if depth > self.opts.max_depth {
            return Err(TiffError::Limit("IFD nesting depth"));
        }
        if self.visited.len() >= self.opts.max_ifds {
            return Err(TiffError::Limit("IFD count"));
        }
        if !self.visited.insert(offset) {
            return Err(TiffError::Invalid(format!("IFD loop at {offset}")));
        }
        let dir = directory_entries(self.data, offset, self.order, self.big, self.opts.max_entries)?;
        let mut entries = Vec::with_capacity(dir.read);
        for p in dir.positions() {
            if let Some(e) = self.entry(p, base) {
                entries.push(e);
            }
        }
        entries.sort_by_key(|e| e.tag);
        entries.dedup_by_key(|e| e.tag);
        // the next-IFD pointer follows the last entry, read only when every declared entry was (checked: a BigTIFF
        // count can be anything up to u64::MAX)
        let next = if reads_every_entry(dir.declared, dir.read) {
            match dir.read.checked_mul(dir.entry_size).and_then(|n| n.checked_add(dir.table)).and_then(|p| self.read_off(p)) {
                Some(0) | None => 0,
                Some(v) => base.checked_add(v).unwrap_or(0),
            }
        } else {
            0
        };
        let mut ifd = Ifd { offset, entries, ..Default::default() };
        if self.opts.follow_children {
            self.children(&mut ifd, base, depth);
        }
        Ok((ifd, next))
    }

    fn children(&mut self, ifd: &mut Ifd, base: u64, depth: usize) {
        if let Some(v) = ifd.value(tags::SUB_IFDS) {
            let offs = v.to_u64_vec();
            for o in offs {
                if let Some(abs) = base.checked_add(o)
                    && let Ok((sub, _)) = self.ifd(abs, base, depth + 1)
                {
                    ifd.sub_ifds.push(sub);
                }
            }
        }
        let child = |tag: u16, ctx: &mut Self| -> Option<Box<Ifd>> {
            let o = ifd.value(tag)?.get_u64(0)?;
            let abs = base.checked_add(o)?;
            ctx.ifd(abs, base, depth + 1).ok().map(|(i, _)| Box::new(i))
        };
        let exif = child(tags::EXIF_IFD, self);
        let gps = child(tags::GPS_IFD, self);
        let interop = child(tags::INTEROP_IFD, self);
        ifd.exif = exif;
        ifd.gps = gps;
        ifd.interop = interop;
    }

    fn entry(&mut self, p: usize, base: u64) -> Option<Entry> {
        let span = entry_span(self.data, p, self.order, self.big, base)?;
        if span.len > self.budget {
            return None;
        }
        self.budget -= span.len;
        let bytes = self.data.get(usize::try_from(span.start).ok()?..usize::try_from(span.start + span.len).ok()?)?;
        Some(Entry { tag: span.tag, value: decode(self.order, span.field_type, bytes), offset: span.start })
    }
}

/// Decode `bytes` (exactly `count × size` long, or the first values of an entry) as values of type `typ`.
pub fn decode(o: ByteOrder, typ: FieldType, b: &[u8]) -> Value {
    fn arr<const N: usize>(c: &[u8]) -> [u8; N] {
        let mut a = [0u8; N];
        a.copy_from_slice(c);
        a
    }
    match typ {
        FieldType::Byte => Value::Byte(b.to_vec()),
        FieldType::Undefined => Value::Undefined(b.to_vec()),
        FieldType::Ascii => {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            Value::Ascii(String::from_utf8_lossy(&b[..end]).into_owned())
        }
        FieldType::SByte => Value::SByte(b.iter().map(|&x| x as i8).collect()),
        FieldType::Short => Value::Short(b.as_chunks::<2>().0.iter().map(|c| o.u16(arr(c))).collect()),
        FieldType::SShort => Value::SShort(b.as_chunks::<2>().0.iter().map(|c| o.u16(arr(c)) as i16).collect()),
        FieldType::Long => Value::Long(b.as_chunks::<4>().0.iter().map(|c| o.u32(arr(c))).collect()),
        FieldType::Ifd => Value::Ifd(b.as_chunks::<4>().0.iter().map(|c| o.u32(arr(c))).collect()),
        FieldType::SLong => Value::SLong(b.as_chunks::<4>().0.iter().map(|c| o.u32(arr(c)) as i32).collect()),
        FieldType::Float => Value::Float(b.as_chunks::<4>().0.iter().map(|c| f32::from_bits(o.u32(arr(c)))).collect()),
        FieldType::Rational => Value::Rational(b.as_chunks::<8>().0.iter().map(|c| (o.u32(arr(&c[..4])), o.u32(arr(&c[4..])))).collect()),
        FieldType::SRational => {
            Value::SRational(b.as_chunks::<8>().0.iter().map(|c| (o.u32(arr(&c[..4])) as i32, o.u32(arr(&c[4..])) as i32)).collect())
        }
        FieldType::Double => Value::Double(b.as_chunks::<8>().0.iter().map(|c| f64::from_bits(o.u64(arr(c)))).collect()),
        FieldType::Long8 => Value::Long8(b.as_chunks::<8>().0.iter().map(|c| o.u64(arr(c))).collect()),
        FieldType::Ifd8 => Value::Ifd8(b.as_chunks::<8>().0.iter().map(|c| o.u64(arr(c))).collect()),
        FieldType::SLong8 => Value::SLong8(b.as_chunks::<8>().0.iter().map(|c| o.u64(arr(c)) as i64).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BigTIFF whose first IFD, at 80, declares `declared` entries but holds one, followed by a next-IFD pointer
    /// `next`; a valid one-entry IFD sits at 16.
    fn bigtiff_chain(order: ByteOrder, declared: u64, next: u64) -> Vec<u8> {
        let ifd = |d: &mut Vec<u8>, count: u64, tag: u16, next: u64| {
            order.put_u64(d, count);
            order.put_u16(d, tag);
            order.put_u16(d, 3);
            order.put_u64(d, 1);
            order.put_u16(d, 8);
            d.extend([0; 6]);
            order.put_u64(d, next);
        };
        let mut d = if order == ByteOrder::Little { b"II".to_vec() } else { b"MM".to_vec() };
        order.put_u16(&mut d, 43);
        order.put_u16(&mut d, 8);
        order.put_u16(&mut d, 0);
        order.put_u64(&mut d, 80);
        ifd(&mut d, 1, 0x0101, 0);
        d.resize(80, 0);
        ifd(&mut d, declared, 0x0100, next);
        d
    }

    #[test]
    fn only_a_directory_read_whole_has_a_next_pointer() {
        // the comparison itself, on every target: a declared count past u32::MAX never matches a small read count
        for (declared, read, whole) in [
            (0, 0, true),
            (1, 1, true),
            (8192, 8192, true),
            (8193, 8192, false),
            (1, 0, false),
            ((1 << 32) + 1, 1, false),
            ((2 << 32) + 1, 1, false),
            ((1 << 63) + 1, 1, false),
            (u64::MAX, 1, false),
        ] {
            assert_eq!(reads_every_entry(declared, read), whole, "{declared} declared, {read} read");
        }
    }

    #[test]
    fn a_bigtiff_count_past_u32_max_never_follows_the_next_pointer() {
        // the whole chain: on 32-bit targets (wasm32) the old cast truncated these counts to 1 and followed the pointer
        for order in [ByteOrder::Little, ByteOrder::Big] {
            for declared in [(1u64 << 32) + 1, (2u64 << 32) + 1, (1u64 << 63) + 1] {
                let d = bigtiff_chain(order, declared, 16);
                let tiff = Tiff::parse(&d).unwrap();
                assert_eq!(tiff.ifds.len(), 1, "{order:?} {declared}");
                assert_eq!(tiff.ifds[0].u64(0x0100), Some(8));
                assert_eq!(parse_ifd_at(&d, 80, order, 0, true, &ParseOptions::default()).unwrap().1, 0, "{order:?} {declared}");
            }
            // control: the same file declaring its one entry follows the pointer to the IFD at 16
            let whole = Tiff::parse(&bigtiff_chain(order, 1, 16)).unwrap();
            assert_eq!(whole.ifds.len(), 2, "{order:?}");
            assert_eq!(whole.ifds[1].u64(0x0101), Some(8));
        }
    }

    #[test]
    fn directory_positions_never_overflow() {
        // a hand-built layout the reader never produces: the iterator stops instead of overflowing
        let hostile = DirectoryEntries { table: usize::MAX, read: 2, declared: 2, entry_size: 12 };
        assert_eq!(hostile.positions().collect::<Vec<_>>(), vec![usize::MAX]);
        assert_eq!(hostile.positions().nth(1), None);
        let wide = DirectoryEntries { table: usize::MAX - 30, read: 4, declared: 4, entry_size: 12 };
        assert_eq!(wide.positions().collect::<Vec<_>>(), vec![usize::MAX - 30, usize::MAX - 18, usize::MAX - 6]);
        // a directory read from data lists its entries
        let mut data = Vec::new();
        ByteOrder::Little.put_u16(&mut data, 2);
        data.extend([0; 24]);
        let dir = directory_entries(&data, 0, ByteOrder::Little, false, 8192).unwrap();
        assert_eq!(dir.positions().collect::<Vec<_>>(), vec![2, 14]);
    }

    #[test]
    fn a_bigtiff_directory_declaring_u64_max_entries_parses_without_overflow() {
        // 44 bytes: BigTIFF header (first IFD at 16), an entry count of u64::MAX, one 20-byte entry
        let mut data = b"II\x2b\x00\x08\x00\x00\x00".to_vec();
        data.extend(16u64.to_le_bytes());
        data.extend(u64::MAX.to_le_bytes());
        data.extend(0x0100u16.to_le_bytes());
        data.extend(3u16.to_le_bytes());
        data.extend(1u64.to_le_bytes());
        data.extend([8, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(data.len(), 44);
        let tiff = Tiff::parse(&data).unwrap();
        assert_eq!(tiff.ifds.len(), 1);
        assert_eq!(tiff.ifds[0].u64(0x0100), Some(8));
    }
}
