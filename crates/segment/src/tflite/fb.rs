//! A bounded reader of the FlatBuffers binary format, just enough for TensorFlow Lite models.
//!
//! Written from the public FlatBuffers format description ("FlatBuffers internals",
//! <https://flatbuffers.dev/internals/>, Apache-2.0, Copyright Google Inc.): little-endian scalars;
//! the buffer starts with a `u32` offset to the root table; a table starts with an `i32` that,
//! subtracted from the table's position, gives its vtable; a vtable is `u16` vtable size, `u16`
//! table size and one `u16` per field (the field's offset inside the table, 0 = absent, default
//! value); strings, vectors and sub-tables are reached through a `u32` offset relative to where
//! the offset is stored; a vector is a `u32` element count followed by the elements. No code was
//! copied from the FlatBuffers library.
//!
//! Every position is bounds-checked: a truncated or garbled buffer reads as an error, never a
//! panic or an out-of-bounds access.

use crate::{Error, Result};

fn err(what: &str) -> Error {
    Error::Message(format!("TFLite model: malformed file ({what})"))
}

fn bytes<const N: usize>(buf: &[u8], at: usize) -> Result<[u8; N]> {
    let end = at.checked_add(N).ok_or_else(|| err("offset overflow"))?;
    let s = buf.get(at..end).ok_or_else(|| err("read past the end"))?;
    let mut a = [0u8; N];
    a.copy_from_slice(s);
    Ok(a)
}

pub fn u16_at(buf: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(buf, at)?))
}

pub fn u32_at(buf: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(buf, at)?))
}

fn i32_at(buf: &[u8], at: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(bytes(buf, at)?))
}

fn u64_at(buf: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(buf, at)?))
}

/// Follow the `u32` offset stored at `at`.
fn follow(buf: &[u8], at: usize) -> Result<usize> {
    let off = u32_at(buf, at)? as usize;
    let p = at.checked_add(off).ok_or_else(|| err("offset overflow"))?;
    if p >= buf.len() {
        return Err(err("offset past the end"));
    }
    Ok(p)
}

/// A table: its position and its vtable.
#[derive(Clone, Copy)]
pub struct Table<'a> {
    buf: &'a [u8],
    pos: usize,
    vt: usize,
    vt_len: usize,
}

/// A vector: the position of its first element and its length.
#[derive(Clone, Copy)]
pub struct Vector<'a> {
    buf: &'a [u8],
    start: usize,
    len: usize,
}

impl<'a> Table<'a> {
    /// The root table of a buffer.
    pub fn root(buf: &'a [u8]) -> Result<Self> {
        if buf.len() < 8 {
            return Err(err("too short"));
        }
        Self::at(buf, follow(buf, 0)?)
    }

    fn at(buf: &'a [u8], pos: usize) -> Result<Self> {
        let soff = i32_at(buf, pos)? as i64;
        let vt = (pos as i64).checked_sub(soff).ok_or_else(|| err("vtable offset"))?;
        let vt = usize::try_from(vt).map_err(|_| err("vtable before the start"))?;
        let vt_len = u16_at(buf, vt)? as usize;
        if vt_len < 4 || !vt_len.is_multiple_of(2) || vt.checked_add(vt_len).is_none_or(|e| e > buf.len()) {
            return Err(err("vtable size"));
        }
        let table_len = u16_at(buf, vt + 2)? as usize;
        if pos.checked_add(table_len).is_none_or(|e| e > buf.len()) {
            return Err(err("table size"));
        }
        Ok(Self { buf, pos, vt, vt_len })
    }

    /// The position of field `id`'s value, `None` when absent.
    fn field(&self, id: usize) -> Result<Option<usize>> {
        let entry = 4 + 2 * id;
        if entry + 2 > self.vt_len {
            return Ok(None);
        }
        let off = u16_at(self.buf, self.vt + entry)? as usize;
        if off == 0 {
            return Ok(None);
        }
        Ok(Some(self.pos.checked_add(off).ok_or_else(|| err("field offset"))?))
    }

    pub fn u8(&self, id: usize, default: u8) -> Result<u8> {
        match self.field(id)? {
            Some(p) => self.buf.get(p).copied().ok_or_else(|| err("field past the end")),
            None => Ok(default),
        }
    }

    pub fn i8(&self, id: usize, default: i8) -> Result<i8> {
        Ok(self.u8(id, default as u8)? as i8)
    }

    pub fn bool(&self, id: usize) -> Result<bool> {
        Ok(self.u8(id, 0)? != 0)
    }

    pub fn i32(&self, id: usize, default: i32) -> Result<i32> {
        match self.field(id)? {
            Some(p) => i32_at(self.buf, p),
            None => Ok(default),
        }
    }

    pub fn u32(&self, id: usize, default: u32) -> Result<u32> {
        match self.field(id)? {
            Some(p) => u32_at(self.buf, p),
            None => Ok(default),
        }
    }

    pub fn u64(&self, id: usize, default: u64) -> Result<u64> {
        match self.field(id)? {
            Some(p) => u64_at(self.buf, p),
            None => Ok(default),
        }
    }

    pub fn f32(&self, id: usize, default: f32) -> Result<f32> {
        match self.field(id)? {
            Some(p) => Ok(f32::from_bits(u32_at(self.buf, p)?)),
            None => Ok(default),
        }
    }

    pub fn table(&self, id: usize) -> Result<Option<Table<'a>>> {
        match self.field(id)? {
            Some(p) => Ok(Some(Table::at(self.buf, follow(self.buf, p)?)?)),
            None => Ok(None),
        }
    }

    /// A vector of `elem`-byte elements (empty when absent).
    pub fn vector(&self, id: usize, elem: usize) -> Result<Vector<'a>> {
        let Some(p) = self.field(id)? else {
            return Ok(Vector { buf: self.buf, start: 0, len: 0 });
        };
        let at = follow(self.buf, p)?;
        let len = u32_at(self.buf, at)? as usize;
        let start = at + 4;
        let end = len.checked_mul(elem).and_then(|n| n.checked_add(start)).ok_or_else(|| err("vector size"))?;
        if end > self.buf.len() {
            return Err(err("vector past the end"));
        }
        Ok(Vector { buf: self.buf, start, len })
    }

    pub fn string(&self, id: usize) -> Result<String> {
        let v = self.vector(id, 1)?;
        Ok(String::from_utf8_lossy(v.bytes()).into_owned())
    }
}

impl<'a> Vector<'a> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The raw bytes of a byte vector.
    pub fn bytes(&self) -> &'a [u8] {
        self.buf.get(self.start..self.start + self.len).unwrap_or(&[])
    }

    pub fn i32(&self, i: usize) -> Result<i32> {
        if i >= self.len {
            return Err(err("vector index"));
        }
        i32_at(self.buf, self.start + 4 * i)
    }

    /// All elements of an `int` vector.
    pub fn i32s(&self) -> Result<Vec<i32>> {
        (0..self.len).map(|i| self.i32(i)).collect()
    }

    /// Element `i` of a vector of tables.
    pub fn table(&self, i: usize) -> Result<Table<'a>> {
        if i >= self.len {
            return Err(err("vector index"));
        }
        Table::at(self.buf, follow(self.buf, self.start + 4 * i)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// root → table with fields: 0 = u8 7, 1 = i32 -5 (absent 2), 3 = vector [1, 2, 3] of i32.
    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; 64];
        // root offset → table at 16
        b[0..4].copy_from_slice(&16u32.to_le_bytes());
        // vtable at 4: size 12, table size 16, fields at 4, 8, 0, 12
        for (i, v) in [12u16, 16, 4, 8, 0, 12].iter().enumerate() {
            b[4 + 2 * i..6 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        // table at 16: soffset = 16 - 4 = 12
        b[16..20].copy_from_slice(&12i32.to_le_bytes());
        b[20] = 7;
        b[24..28].copy_from_slice(&(-5i32).to_le_bytes());
        // vector offset at 28 → 40
        b[28..32].copy_from_slice(&12u32.to_le_bytes());
        b[40..44].copy_from_slice(&3u32.to_le_bytes());
        for (i, v) in [1i32, 2, 3].iter().enumerate() {
            b[44 + 4 * i..48 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    #[test]
    fn reads_fields_and_defaults() {
        let b = sample();
        let t = Table::root(&b).unwrap();
        assert_eq!(t.u8(0, 0).unwrap(), 7);
        assert_eq!(t.i32(1, 0).unwrap(), -5);
        assert_eq!(t.i32(2, 42).unwrap(), 42);
        assert_eq!(t.vector(3, 4).unwrap().i32s().unwrap(), vec![1, 2, 3]);
        // fields past the vtable are absent
        assert_eq!(t.i32(9, 11).unwrap(), 11);
        assert_eq!(t.vector(9, 4).unwrap().len(), 0);
        assert!(t.vector(3, 4).unwrap().i32(3).is_err());
    }

    #[test]
    fn bad_offsets_are_errors() {
        let b = sample();
        // truncated anywhere: never a panic
        for n in 0..b.len() {
            let t = Table::root(&b[..n]);
            if let Ok(t) = t {
                let _ = t.u8(0, 0);
                let _ = t.i32(1, 0);
                let _ = t.vector(3, 4).and_then(|v| v.i32s());
            }
        }
        // a vector length that runs past the end
        let mut bad = sample();
        bad[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Table::root(&bad).unwrap().vector(3, 4).is_err());
        // a vtable before the start of the buffer
        let mut bad = sample();
        bad[16..20].copy_from_slice(&100i32.to_le_bytes());
        assert!(Table::root(&bad).is_err());
        // a vtable that claims more bytes than the buffer holds
        let mut bad = sample();
        bad[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(Table::root(&bad).is_err());
    }
}
