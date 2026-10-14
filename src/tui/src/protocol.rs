//! Bounded transport for the existing version-two local UI protocol.
use serde_json::Value;
use std::io::{self, BufRead};

pub const VERSION: u64 = 2;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const QUEUE_CAPACITY: usize = 32;

/// Read one frame without allocating beyond the configured limit. A transport
/// error terminates the connection; an oversized frame must never be resumed.
pub fn read_frame(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Value>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Incomplete protocol frame",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(available.len());
        if length > limit.saturating_sub(bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Protocol frame exceeds limit",
            ));
        }
        bytes.extend_from_slice(&available[..length]);
        reader.consume(length + usize::from(newline.is_some()));
        if newline.is_some() {
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid protocol JSON"))?;
            if !value.is_object() || value["version"].as_u64() != Some(VERSION) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unsupported protocol version",
                ));
            }
            return Ok(Some(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn fragmented_frames_preserve_boundaries() {
        let mut reader = BufReader::with_capacity(
            3,
            Cursor::new(b"{\"version\":2}\n{\"version\":2,\"op\":\"quit\"}\n"),
        );
        assert_eq!(read_frame(&mut reader, 100).unwrap().unwrap()["version"], 2);
        assert_eq!(read_frame(&mut reader, 100).unwrap().unwrap()["op"], "quit");
        assert!(read_frame(&mut reader, 100).unwrap().is_none());
    }

    #[test]
    fn rejects_oversize_before_reading_entire_input() {
        let mut reader = BufReader::with_capacity(4, Cursor::new(vec![b'x'; 1000]));
        assert_eq!(
            read_frame(&mut reader, 8).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(reader.get_ref().position() <= 12);
    }

    #[test]
    fn accepts_exact_limit_and_rejects_truncated_or_invalid_frames() {
        assert!(
            read_frame(&mut Cursor::new(b"{\"version\":2}\n"), 13)
                .unwrap()
                .is_some()
        );
        for bytes in [
            b"{\"version\":1}\n".as_slice(),
            b"[]\n",
            b"bad\n",
            b"{\"version\":2}",
        ] {
            assert!(read_frame(&mut Cursor::new(bytes), 100).is_err());
        }
    }
}
