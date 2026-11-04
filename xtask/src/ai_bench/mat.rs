//! A reader for MATLAB level-5 MAT-files (the format of the WIDER FACE ground truth), written from MathWorks' published
//! file-format description: numeric arrays, char arrays and cell arrays, compressed or not, little-endian. Sizes are
//! checked against the data, nesting is bounded, and anything unexpected is an error.

use std::io::Read;

const MAX_DEPTH: usize = 16;
/// Largest decompressed variable.
const MAX_INFLATED: u64 = 1 << 30;
/// Largest single dimension of a matrix (every real variable is far smaller; a hostile one must not size an allocation).
const MAX_DIM: usize = 1 << 30;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A numeric matrix, column-major, as f64.
    Num {
        dims: Vec<usize>,
        data: Vec<f64>,
    },
    Char(String),
    Cell(Vec<Value>),
    /// Any other class (struct, sparse, objects): present but not read.
    Other,
}

impl Value {
    pub fn cells(&self) -> Option<&[Value]> {
        match self {
            Value::Cell(items) => Some(items),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Char(text) => Some(text),
            _ => None,
        }
    }

    /// Rows of a numeric matrix (stored column-major); an empty matrix is no rows.
    pub fn rows(&self) -> Option<Vec<Vec<f64>>> {
        let Value::Num { dims, data } = self else { return None };
        let (r, c) = (dims.first().copied().unwrap_or(0), dims.get(1).copied().unwrap_or(1));
        if r.checked_mul(c) != Some(data.len()) {
            return None;
        }
        if data.is_empty() {
            return Some(Vec::new());
        }
        Some((0..r).map(|i| (0..c).filter_map(|j| data.get(i + j * r).copied()).collect()).collect())
    }

    /// All numbers of a numeric matrix.
    pub fn numbers(&self) -> Option<&[f64]> {
        match self {
            Value::Num { data, .. } => Some(data),
            _ => None,
        }
    }
}

fn bad(what: &str) -> String {
    format!("malformed MAT-file: {what}")
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&self, len: usize) -> Option<&'a [u8]> {
        self.b.get(self.at..self.at.checked_add(len)?)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let word = self.bytes(4).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or_else(|| bad("truncated tag"))?;
        self.at += 4;
        Ok(u32::from_le_bytes(word))
    }

    fn done(&self) -> bool {
        self.at >= self.b.len()
    }

    /// The next data element: its type and bytes.
    fn element(&mut self) -> Result<(u32, &'a [u8]), String> {
        let first = self.u32()?;
        if first >> 16 != 0 {
            // small element: type and size in one word, data in the next four bytes
            let (ty, size) = (first & 0xffff, (first >> 16) as usize);
            let data = self.bytes(size.min(4)).ok_or_else(|| bad("truncated small element"))?;
            self.at += 4;
            return Ok((ty, data));
        }
        let size = self.u32()? as usize;
        let data = self.bytes(size).ok_or_else(|| bad("an element runs past the end of the file"))?;
        let end = self.at + size;
        // compressed elements are not padded; everything else is padded to 8 bytes
        self.at = if first == 15 { end } else { end.checked_next_multiple_of(8).ok_or_else(|| bad("size overflow"))? };
        Ok((first, data))
    }
}

/// The values of a numeric data element of MAT type `ty`.
fn numbers(ty: u32, d: &[u8]) -> Result<Vec<f64>, String> {
    fn le<const N: usize>(d: &[u8]) -> impl Iterator<Item = [u8; N]> + '_ {
        d.as_chunks::<N>().0.iter().copied()
    }
    Ok(match ty {
        1 => d.iter().map(|b| f64::from(*b as i8)).collect(),
        2 => d.iter().map(|b| f64::from(*b)).collect(),
        3 => le::<2>(d).map(|c| f64::from(i16::from_le_bytes(c))).collect(),
        4 => le::<2>(d).map(|c| f64::from(u16::from_le_bytes(c))).collect(),
        5 => le::<4>(d).map(|c| f64::from(i32::from_le_bytes(c))).collect(),
        6 => le::<4>(d).map(|c| f64::from(u32::from_le_bytes(c))).collect(),
        7 => le::<4>(d).map(|c| f64::from(f32::from_le_bytes(c))).collect(),
        9 => le::<8>(d).map(f64::from_le_bytes).collect(),
        12 => le::<8>(d).map(|c| i64::from_le_bytes(c) as f64).collect(),
        13 => le::<8>(d).map(|c| u64::from_le_bytes(c) as f64).collect(),
        _ => return Err(bad(&format!("numeric data of type {ty}"))),
    })
}

/// A miMATRIX element's body.
fn matrix(body: &[u8], depth: usize) -> Result<(String, Value), String> {
    if depth > MAX_DEPTH {
        return Err(bad("nested too deeply"));
    }
    if body.is_empty() {
        return Ok((String::new(), Value::Num { dims: vec![0, 0], data: Vec::new() }));
    }
    let mut r = Reader { b: body, at: 0 };
    let (_, flags) = r.element()?;
    let class = flags.first().copied().ok_or_else(|| bad("no array flags"))?;
    let complex = flags.get(1).is_some_and(|f| f & 0x08 != 0);
    let (dty, dims) = r.element()?;
    let dims: Vec<usize> = numbers(dty, dims)?.into_iter().map(|d| if d.is_finite() && d >= 0.0 { d as usize } else { 0 }).collect();
    if dims.iter().any(|d| *d > MAX_DIM) {
        return Err(bad("a dimension larger than any real variable"));
    }
    let count = dims.iter().try_fold(1usize, |n, d| n.checked_mul(*d)).ok_or_else(|| bad("dimensions overflow"))?;
    let (_, name) = r.element()?;
    let name = String::from_utf8_lossy(name).into_owned();
    let value = match class {
        1 => {
            // every item takes at least a tag (8 bytes), which bounds a hostile count by the data
            if count > body.len() / 8 {
                return Err(bad("a cell array larger than its data"));
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let (ty, b) = r.element()?;
                if ty != 14 {
                    return Err(bad("a cell that is not a matrix"));
                }
                items.push(matrix(b, depth + 1)?.1);
            }
            Value::Cell(items)
        }
        4 => {
            let (ty, d) = r.element()?;
            let text = match ty {
                16 => String::from_utf8_lossy(d).into_owned(),
                _ => numbers(ty, d)?.into_iter().map(|c| char::from_u32(c as u32).unwrap_or('\u{fffd}')).collect(),
            };
            Value::Char(text)
        }
        6..=15 => {
            let (ty, d) = r.element()?;
            let data = numbers(ty, d)?;
            if data.len() != count {
                return Err(bad("a matrix whose data does not match its dimensions"));
            }
            if complex && !r.done() {
                r.element()?;
            }
            Value::Num { dims, data }
        }
        _ => Value::Other,
    };
    Ok((name, value))
}

/// The variables in a MAT-file's bytes, by name.
pub fn parse(bytes: &[u8]) -> Result<Vec<(String, Value)>, String> {
    if bytes.len() < 128 {
        return Err(bad("shorter than its header"));
    }
    if bytes.get(126..128) != Some(b"IM") {
        return Err(bad("not a little-endian level-5 MAT-file (v7.3 HDF5 files are not supported)"));
    }
    let mut r = Reader { b: bytes, at: 128 };
    let mut out = Vec::new();
    while !r.done() {
        let (ty, body) = r.element()?;
        match ty {
            14 => out.push(matrix(body, 0)?),
            15 => {
                let mut inflated = Vec::new();
                flate2::read::ZlibDecoder::new(body)
                    .take(MAX_INFLATED)
                    .read_to_end(&mut inflated)
                    .map_err(|e| bad(&format!("compressed variable: {e}")))?;
                let (ity, ibody) = Reader { b: &inflated, at: 0 }.element()?;
                if ity == 14 {
                    out.push(matrix(ibody, 0)?);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The variable `name` in `vars`.
pub fn var<'a>(vars: &'a [(String, Value)], name: &str) -> Result<&'a Value, String> {
    vars.iter().find(|(n, _)| n == name).map(|(_, v)| v).ok_or_else(|| format!("the MAT-file has no variable `{name}`"))
}

/// Builders for synthetic MAT-files, shared with the ground-truth tests.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn element(ty: u32, data: &[u8]) -> Vec<u8> {
        let mut out = ty.to_le_bytes().to_vec();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(8) * 8, 0);
        out
    }

    fn header(class: u8, dims: &[i32], name: &str) -> Vec<u8> {
        let mut out = element(6, &[class, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(element(5, &dims.iter().flat_map(|d| d.to_le_bytes()).collect::<Vec<_>>()));
        out.extend(element(1, name.as_bytes()));
        out
    }

    /// A double matrix as a miMATRIX element (column-major `data`).
    pub(crate) fn double(name: &str, rows: i32, cols: i32, data: &[f64]) -> Vec<u8> {
        let mut body = header(6, &[rows, cols], name);
        body.extend(element(9, &data.iter().flat_map(|d| d.to_le_bytes()).collect::<Vec<_>>()));
        element(14, &body)
    }

    pub(crate) fn chars(name: &str, text: &str) -> Vec<u8> {
        let mut body = header(4, &[1, text.len() as i32], name);
        body.extend(element(16, text.as_bytes()));
        element(14, &body)
    }

    pub(crate) fn cell(name: &str, items: &[Vec<u8>]) -> Vec<u8> {
        let mut body = header(1, &[items.len() as i32, 1], name);
        for item in items {
            body.extend_from_slice(item);
        }
        element(14, &body)
    }

    /// A MAT-file: the 128-byte header, then `variables`.
    pub(crate) fn file(variables: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 128];
        out[126] = b'I';
        out[127] = b'M';
        for v in variables {
            out.extend_from_slice(v);
        }
        out
    }

    #[test]
    fn reads_numbers_text_and_nested_cells() {
        let bytes = file(&[double("m", 2, 2, &[1.0, 2.0, 3.0, 4.0]), cell("c", &[chars("", "ab"), cell("", &[double("", 1, 1, &[7.0])])])]);
        let vars = parse(&bytes).unwrap();
        // column-major [1 3; 2 4] has the rows [1, 3] and [2, 4]
        assert_eq!(var(&vars, "m").unwrap().rows(), Some(vec![vec![1.0, 3.0], vec![2.0, 4.0]]));
        let c = var(&vars, "c").unwrap().cells().unwrap();
        assert_eq!(c[0].text(), Some("ab"));
        assert_eq!(c[1].cells().unwrap()[0].numbers(), Some(&[7.0][..]));
        assert!(var(&vars, "absent").is_err());
    }

    #[test]
    fn a_matrix_without_data_has_no_rows_whatever_its_dimensions_claim() {
        // dims [N, 0] hold no data; building N empty rows from a hostile N would exhaust memory
        let empty = Value::Num { dims: vec![1 << 20, 0], data: Vec::new() };
        assert_eq!(empty.rows(), Some(Vec::new()));
        // a dimension no real variable has is refused at parse time
        let hostile = file(&[double("m", i32::MAX, 0, &[])]);
        assert!(parse(&hostile).is_err());
        // an ordinary empty matrix still parses and has no rows
        let vars = parse(&file(&[double("m", 0, 0, &[])])).unwrap();
        assert_eq!(var(&vars, "m").unwrap().rows(), Some(Vec::new()));
    }

    #[test]
    fn malformed_files_are_errors() {
        assert!(parse(b"too short").is_err());
        let mut wrong_endian = file(&[]);
        wrong_endian[126] = b'M';
        wrong_endian[127] = b'I';
        assert!(parse(&wrong_endian).is_err());
        let good = file(&[double("m", 1, 2, &[1.0, 2.0])]);
        for cut in [130, 140, good.len() - 1] {
            assert!(parse(&good[..cut]).is_err(), "cut at {cut}");
        }
        // a cell array that claims far more items than it has bytes
        let mut hostile = header(1, &[i32::MAX, 1], "c");
        hostile.extend(element(14, &[]));
        assert!(parse(&file(&[element(14, &hostile)])).is_err());
    }
}
