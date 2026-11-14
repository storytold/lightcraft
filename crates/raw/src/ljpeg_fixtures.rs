//! Test fixtures: hand-built lossless JPEG streams (the subsampled YCbCr coding Sony uses) for the ARW tests.

/// Independent 4×4, 16-bit Sony 4:2:0 stream. Hand-specified differences in MCU order.
pub(crate) fn fixture_420() -> Vec<u8> {
    fixture_subsampled(4, 4, 0x22, &[-31768, 1, 1000, 1, -16384, -16284, 1, 1, 1, 1, 1, 2, 100, 1, 1000, 1, 100, 100, 1, 1, 1, 1, 1, 2])
}

/// A 16-bit three-component frame of `width × height` pixels with the given luma `sampling` (`0x21` / `0x22`)
/// and one shared 5-bit Huffman table, whose differences are written as given (MCU order).
pub(crate) fn fixture_subsampled(width: u8, height: u8, sampling: u8, differences: &[i32]) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8, 0xff, 0xc3, 0, 17, 16, 0, height, 0, width, 3, 1, sampling, 0, 2, 0x11, 0, 3, 0x11, 0];
    out.extend_from_slice(&[0xff, 0xc4, 0, 36, 0]);
    let mut counts = [0; 16];
    counts[4] = 17;
    out.extend_from_slice(&counts);
    out.extend(0..=16);
    out.extend_from_slice(&[0xff, 0xda, 0, 12, 3, 1, 0, 2, 0, 3, 0, 1, 0, 0]);
    let mut bw = BitWriter { out, acc: 0, n: 0 };
    for &d in differences {
        let s = ssss_of(d);
        bw.put(s as u32, 5);
        if s > 0 && s < 16 {
            let v = if d < 0 { d - 1 } else { d };
            bw.put(v as u32 & ((1 << s) - 1), s as u32);
        }
    }
    bw.flush();
    bw.out.extend_from_slice(&[0xff, 0xd9]);
    bw.out
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (v as u64 & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            let b = (self.acc >> (self.n - 8)) as u8;
            self.out.push(b);
            if b == 0xff {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
    }
}

fn ssss_of(diff: i32) -> u8 {
    if diff == 0 {
        0
    } else if diff == 32768 || diff == -32768 {
        16
    } else {
        32 - (diff.unsigned_abs()).leading_zeros() as u8
    }
}
