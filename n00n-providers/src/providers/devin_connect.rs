//! Connect protocol framing for Devin agent streams.
//!
//! Wire format: `[flags: u8][length: u32 BE][payload]`.

use std::io::Read;

pub const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;
pub const CONNECT_COMPRESSED_FLAG: u8 = 0b0000_0001;
const MAX_CONNECT_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFrame {
    pub end_stream: bool,
    pub compressed: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
}

impl FrameBuffer {
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Returns `None` while waiting for more bytes, `Some(Ok(frame))` when complete,
    /// or `Some(Err(message))` when the declared length exceeds the cap.
    pub fn next_frame(&mut self) -> Option<Result<ConnectFrame, String>> {
        if self.buf.len() < 5 {
            return None;
        }
        let flags = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_CONNECT_FRAME_LEN {
            self.buf.drain(0..5);
            return Some(Err(format!(
                "connect frame length {len} exceeds maximum {MAX_CONNECT_FRAME_LEN}"
            )));
        }
        let Some(total) = 5usize.checked_add(len) else {
            self.buf.drain(0..5);
            return Some(Err(format!(
                "connect frame length {len} exceeds addressable buffer"
            )));
        };
        if self.buf.len() < total {
            return None;
        }
        let payload = self.buf[5..total].to_vec();
        self.buf.drain(0..total);
        Some(Ok(ConnectFrame {
            end_stream: flags & CONNECT_END_STREAM_FLAG != 0,
            compressed: flags & CONNECT_COMPRESSED_FLAG != 0,
            payload,
        }))
    }
}

pub fn encode_frame(flags: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > MAX_CONNECT_FRAME_LEN {
        return Err(format!(
            "connect frame payload length {} exceeds maximum {MAX_CONNECT_FRAME_LEN}",
            payload.len()
        ));
    }
    let len = u32::try_from(payload.len()).map_err(|_| {
        format!(
            "connect frame payload length {} does not fit in u32",
            payload.len()
        )
    })?;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Expand a Connect frame payload when the compressed flag is set (gzip).
pub fn decode_frame_payload(frame: &ConnectFrame) -> Result<Vec<u8>, String> {
    if !frame.compressed {
        return Ok(frame.payload.clone());
    }
    let mut decoder = flate2::read::GzDecoder::new(frame.payload.as_slice());
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|error| format!("connect gzip decompress: {error}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn encode_frame_roundtrip() {
        let payload = b"hello";
        let encoded = encode_frame(0, payload).expect("encode");
        let mut buf = FrameBuffer::default();
        buf.push(&encoded);
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert!(!frame.end_stream);
        assert!(!frame.compressed);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn end_stream_flag_preserved() {
        let encoded = encode_frame(CONNECT_END_STREAM_FLAG, b"").expect("encode");
        let mut buf = FrameBuffer::default();
        buf.push(&encoded);
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert!(frame.end_stream);
    }

    #[test]
    fn chunked_input_reassembles() {
        let encoded = encode_frame(0, b"chunked").expect("encode");
        let mut buf = FrameBuffer::default();
        for byte in encoded {
            buf.push(&[byte]);
        }
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert_eq!(frame.payload, b"chunked");
    }

    #[test]
    fn decode_frame_payload_gunzips_compressed_flag() {
        let plain = b"interaction text delta";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(plain).expect("gzip write");
        let gzipped = encoder.finish().expect("gzip finish");
        let frame = ConnectFrame {
            end_stream: false,
            compressed: true,
            payload: gzipped,
        };
        let decoded = decode_frame_payload(&frame).expect("decode");
        assert_eq!(decoded, plain);
    }
}
