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

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected reader failure"))
        }
    }

    struct ReaderThenFailure<R> {
        inner: R,
        first_read: bool,
        first_read_len: usize,
    }

    impl<R: Read> Read for ReaderThenFailure<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.first_read {
                self.first_read = false;
                self.first_read_len = self.inner.read(buf)?;
                Ok(self.first_read_len)
            } else {
                Err(io::Error::other("injected reader failure after data"))
            }
        }
    }

    #[derive(Default)]
    struct RecordingWriter {
        writes: Vec<usize>,
        bytes: Vec<u8>,
        largest_write_request: usize,
        max_write: usize,
        flush_calls: usize,
        fail_write: bool,
        fail_flush: bool,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.largest_write_request = self.largest_write_request.max(buf.len());
            if self.fail_write {
                return Err(io::Error::other("injected destination write failure"));
            }
            let len = if self.max_write == 0 { buf.len() } else { buf.len().min(self.max_write) };
            self.writes.push(len);
            self.bytes.extend_from_slice(&buf[..len]);
            Ok(len)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
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
        assert!(writer.largest_write_request <= 1 << 20, "largest request was {} bytes", writer.largest_write_request);
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
    fn copy_buffer_propagates_reader_failure() {
        let mut reader = FailingReader;
        let mut writer = RecordingWriter::default();

        let err = copy_buffered(&mut reader, &mut writer).unwrap_err();

        assert_eq!(err.to_string(), "injected reader failure");
        assert!(writer.writes.is_empty());
    }

    #[test]
    fn copy_buffer_propagates_reader_failure_after_data() {
        let mut reader = ReaderThenFailure { inner: Cursor::new(b"first chunk"), first_read: true, first_read_len: 0 };
        let mut writer = RecordingWriter::default();

        let err = copy_buffered(&mut reader, &mut writer).unwrap_err();

        assert!(reader.first_read_len > 0);
        assert_eq!(err.to_string(), "injected reader failure after data");
    }

    #[test]
    fn copy_buffer_propagates_destination_write_failure() {
        let source = patterned_bytes((1 << 20) + 17);
        let mut reader = ChunkedReader { inner: Cursor::new(source), max_read: 8192 };
        let mut writer = RecordingWriter { fail_write: true, ..RecordingWriter::default() };

        let err = copy_buffered(&mut reader, &mut writer).unwrap_err();

        assert_eq!(err.to_string(), "injected destination write failure");
        assert_eq!(writer.largest_write_request, 1 << 20);
        assert_eq!(writer.flush_calls, 0, "write failure must occur during io::copy, before explicit flush");
    }

    #[test]
    fn copy_buffer_propagates_buffered_tail_write_failure() {
        let source = b"small buffered tail";
        let mut reader = Cursor::new(source);
        let mut writer = RecordingWriter { fail_write: true, ..RecordingWriter::default() };

        let err = copy_buffered(&mut reader, &mut writer).unwrap_err();

        assert_eq!(err.to_string(), "injected destination write failure");
        assert_eq!(reader.position(), source.len() as u64);
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
