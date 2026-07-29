//! Connect protocol framing for Cursor agent streams.
//!
//! Wire format: `[flags: u8][length: u32 BE][payload]`.

#![allow(dead_code)]

use std::io::Read;

const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;
const CONNECT_COMPRESSED_FLAG: u8 = 0b0000_0001;
const MAX_CONNECT_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectFrame {
    pub end_stream: bool,
    pub compressed: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub(crate) struct FrameBuffer {
    buf: Vec<u8>,
}

impl FrameBuffer {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Returns `None` while waiting for more bytes, `Some(Ok(frame))` when complete,
    /// or `Some(Err(message))` when the declared length exceeds the cap.
    pub(crate) fn next_frame(&mut self) -> Option<Result<ConnectFrame, String>> {
        if self.buf.len() < 5 {
            return None;
        }
        let flags = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_CONNECT_FRAME_LEN {
            // Consume the bad header so callers looping on next_frame cannot spin forever.
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

#[must_use]
pub(crate) fn encode_frame(flags: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > MAX_CONNECT_FRAME_LEN {
        return Err(format!(
            "connect frame payload length {} exceeds maximum {MAX_CONNECT_FRAME_LEN}",
            payload.len()
        ));
    }
    // MAX_CONNECT_FRAME_LEN is 16 MiB, so this always fits in u32.
    #[allow(clippy::cast_possible_truncation)]
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Expand a Connect frame payload when the compressed flag is set (gzip).
pub(crate) fn decode_frame_payload(frame: &ConnectFrame) -> Result<Vec<u8>, String> {
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
    use test_case::test_case;

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
    fn encode_frame_rejects_oversized_payload() {
        let payload = vec![0u8; MAX_CONNECT_FRAME_LEN + 1];
        let err = encode_frame(0, &payload).expect_err("oversize");
        assert!(err.contains("exceeds maximum"));
    }

    #[test_case(16 * 1024 * 1024 + 1, "exceeds maximum")]
    fn oversized_length_rejected(over: u32, _note: &str) {
        let mut buf = FrameBuffer::default();
        let mut header = [0u8; 5];
        header[1..].copy_from_slice(&over.to_be_bytes());
        buf.push(&header);
        match buf.next_frame().expect("result") {
            Err(e) => assert!(e.contains("exceeds maximum")),
            Ok(_) => panic!("expected oversize rejection"),
        }
    }

    #[test]
    fn fuzz_random_bytes_never_panic() {
        let mut buf = FrameBuffer::default();
        for i in 0..500 {
            let n = (fastrand::u8(..) as usize % 32) + 1;
            let mut chunk = vec![0u8; n];
            for byte in &mut chunk {
                *byte = fastrand::u8(..);
            }
            buf.push(&chunk);
            loop {
                if buf.next_frame().is_none() {
                    break;
                }
            }
            if i % 25 == 24 {
                buf = FrameBuffer::default();
            }
        }
    }

    #[test]
    fn fuzz_valid_frames_roundtrip_random_payloads() {
        for size in [0usize, 1, 7, 63, 256, 1024] {
            let mut payload = vec![0u8; size];
            for byte in &mut payload {
                *byte = fastrand::u8(..);
            }
            let flags = fastrand::u8(..) & 0b11;
            let encoded = encode_frame(flags, &payload).expect("encode");
            let mut buf = FrameBuffer::default();
            let mut offset = 0;
            while offset < encoded.len() {
                let take = ((fastrand::u8(..) as usize) % 17).max(1);
                let end = (offset + take).min(encoded.len());
                buf.push(&encoded[offset..end]);
                offset = end;
            }
            let frame = buf.next_frame().expect("complete").expect("valid frame");
            assert_eq!(frame.payload, payload);
            assert_eq!(frame.end_stream, flags & CONNECT_END_STREAM_FLAG != 0);
            assert_eq!(frame.compressed, flags & CONNECT_COMPRESSED_FLAG != 0);
            assert!(buf.next_frame().is_none());
        }
    }

    #[test]
    fn oversized_header_is_consumed_so_loop_cannot_spin() {
        let mut buf = FrameBuffer::default();
        #[allow(clippy::cast_possible_truncation)] // MAX_CONNECT_FRAME_LEN is 16 MiB
        let over = (MAX_CONNECT_FRAME_LEN as u32).saturating_add(1);
        let mut header = [0u8; 5];
        header[1..].copy_from_slice(&over.to_be_bytes());
        buf.push(&header);
        assert!(buf.next_frame().expect("err").is_err());
        // Second call must not keep returning the same error forever.
        assert!(buf.next_frame().is_none());
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

    #[test]
    fn decode_frame_payload_rejects_corrupt_gzip() {
        let frame = ConnectFrame {
            end_stream: false,
            compressed: true,
            payload: b"not-gzip".to_vec(),
        };
        let err = decode_frame_payload(&frame).expect_err("corrupt");
        assert!(err.contains("gzip"));
    }
}
