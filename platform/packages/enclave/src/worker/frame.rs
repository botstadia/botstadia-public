//! Frames between the runner and its workers: `[u32 length][u8 kind][u32
//! seat][body]`, little-endian, over a child's stdin and stdout.
//!
//! The runner (the keyholder) reads only the fixed header of a frame from a
//! worker, bounds-checked, and relays or hashes bodies it doesn't parse
//! (BOTS.md, isolation). The one body it decodes is a worker's final answer,
//! through [`decode`]'s size limit.

use std::io::{Read, Write};

/// No frame is longer: a whole bundle's models fit, with room.
pub const MAX_FRAME: usize = 320 << 20;

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    /// The seat it's about, where that means anything.
    pub seat: u32,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(kind: u8, seat: u32, body: Vec<u8>) -> Self {
        Self { kind, seat, body }
    }
}

pub fn write(w: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    let len = 5 + frame.body.len();
    if len > MAX_FRAME {
        return Err(std::io::Error::other("frame too long"));
    }
    w.write_all(&(len as u32).to_le_bytes())?;
    w.write_all(&[frame.kind])?;
    w.write_all(&frame.seat.to_le_bytes())?;
    w.write_all(&frame.body)?;
    w.flush()
}

/// The next frame, or `None` at a clean end of stream.
pub fn read(r: &mut impl Read) -> std::io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if !(5..=MAX_FRAME).contains(&len) {
        return Err(std::io::Error::other(format!("a frame of {len} bytes")));
    }
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let mut body = vec![0u8; len - 5];
    r.read_exact(&mut body)?;
    Ok(Some(Frame {
        kind: head[0],
        seat: u32::from_le_bytes([head[1], head[2], head[3], head[4]]),
        body,
    }))
}

/// A serde value as a frame body.
pub fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    bincode::serialize(value).expect("encoding a frame body")
}

/// A frame body as a serde value, refusing anything past `limit` bytes.
pub fn decode<T: serde::de::DeserializeOwned>(body: &[u8], limit: u64) -> anyhow::Result<T> {
    use bincode::Options;
    anyhow::ensure!(
        body.len() as u64 <= limit,
        "a frame body of {} bytes, past its {limit}",
        body.len()
    );
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .with_limit(limit)
        .deserialize(body)
        .map_err(|e| anyhow::anyhow!("a malformed frame: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_a_clean_end_is_none() {
        let mut buf = Vec::new();
        write(&mut buf, &Frame::new(3, 7, b"hello".to_vec())).unwrap();
        write(&mut buf, &Frame::new(1, 0, vec![])).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(
            read(&mut r).unwrap(),
            Some(Frame::new(3, 7, b"hello".to_vec()))
        );
        assert_eq!(read(&mut r).unwrap(), Some(Frame::new(1, 0, vec![])));
        assert_eq!(read(&mut r).unwrap(), None);
    }

    #[test]
    fn a_frame_claiming_too_much_or_too_little_is_refused() {
        for len in [0u32, 4, (MAX_FRAME + 1) as u32] {
            let mut buf = len.to_le_bytes().to_vec();
            buf.extend([0u8; 16]);
            assert!(read(&mut buf.as_slice()).is_err());
        }
    }

    #[test]
    fn decode_honours_its_limit() {
        let body = encode(&vec![7u8; 1000]);
        assert!(decode::<Vec<u8>>(&body, 100).is_err());
        assert_eq!(decode::<Vec<u8>>(&body, 2000).unwrap().len(), 1000);
    }
}
