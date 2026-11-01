//! Bounded, buffered copies used by verified file transfers.

use std::io::{self, Read, Write};

pub(crate) fn copy_buffered<R: Read, W: Write>(reader: &mut R, writer: W) -> io::Result<u64> {
    let mut writer = std::io::BufWriter::with_capacity(1 << 20, writer);
    let copied = io::copy(reader, &mut writer)?;
    writer.flush()?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::copy_buffered;
    use std::io::{self, Cursor, Read, Write};

    struct ChunkedReader<R> {
        inner: R,
        max_read: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let len = buf.len().min(self.max_read);
            self.inner.read(&mut buf[..len])
        }
    }

    #[derive(Default)]
    struct RecordingWriter {
        writes: Vec<usize>,
        bytes: Vec<u8>,
        max_write: usize,
        fail_flush: bool,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let len = if self.max_write == 0 { buf.len() } else { buf.len().min(self.max_write) };
            self.writes.push(len);
            self.bytes.extend_from_slice(&buf[..len]);
            Ok(len)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush { Err(io::Error::other("injected flush failure")) } else { Ok(()) }
        }
    }

    fn patterned_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|n| n as u8).collect()
    }

    #[test]
    fn copy_buffer_batches_destination_writes() {
        let source = patterned_bytes((1 << 20) + 17);
        let mut reader = ChunkedReader { inner: Cursor::new(source), max_read: 8192 };
        let mut writer = RecordingWriter::default();

        let copied = copy_buffered(&mut reader, &mut writer).unwrap();

        assert_eq!(copied, (1 << 20) as u64 + 17);
        assert!(writer.writes.len() <= 2, "writes were {:?}", writer.writes);
    }

    #[test]
    fn copy_buffer_preserves_partial_tail() {
        let source = patterned_bytes((1 << 20) + 17);
        let mut reader = ChunkedReader { inner: Cursor::new(source.clone()), max_read: 8192 };
        let mut writer = RecordingWriter::default();

        copy_buffered(&mut reader, &mut writer).unwrap();

        assert_eq!(writer.bytes, source);
    }

    #[test]
    fn copy_buffer_propagates_flush_failure() {
        let mut reader = ChunkedReader { inner: Cursor::new(b"buffered bytes"), max_read: 3 };
        let writer = RecordingWriter { fail_flush: true, ..RecordingWriter::default() };

        let err = copy_buffered(&mut reader, writer).unwrap_err();

        assert_eq!(err.to_string(), "injected flush failure");
    }

    #[test]
    fn copy_buffer_handles_short_writes() {
        let source = patterned_bytes((1 << 20) + 17);
        let mut reader = ChunkedReader { inner: Cursor::new(source.clone()), max_read: 8192 };
        let mut writer = RecordingWriter { max_write: 127, ..RecordingWriter::default() };

        copy_buffered(&mut reader, &mut writer).unwrap();

        assert_eq!(writer.bytes, source);
        assert!(writer.writes.iter().all(|len| *len <= 127));
    }

    #[test]
    fn copy_buffer_preserves_empty_and_small_inputs() {
        for source in [Vec::new(), b"small file".to_vec()] {
            let mut reader = ChunkedReader { inner: Cursor::new(source.clone()), max_read: 3 };
            let mut writer = RecordingWriter::default();

            assert_eq!(copy_buffered(&mut reader, &mut writer).unwrap(), source.len() as u64);
            assert_eq!(writer.bytes, source);
        }
    }
}
