//! Connect envelope framing for Cursor `agent.v1.AgentService`.
//!
//! Wire format: `[flags: u8][length: u32 BE][payload]`.

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
            return Some(Err(format!(
                "connect frame length {len} exceeds maximum {MAX_CONNECT_FRAME_LEN}"
            )));
        }
        let Some(total) = 5usize.checked_add(len) else {
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
pub(crate) fn encode_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
    let capped = payload.len().min(MAX_CONNECT_FRAME_LEN);
    // MAX_CONNECT_FRAME_LEN is 16 MiB, so this always fits in u32.
    #[allow(clippy::cast_possible_truncation)]
    let len = capped as u32;
    let payload = &payload[..capped];
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn encode_frame_roundtrip() {
        let payload = b"hello";
        let encoded = encode_frame(0, payload);
        let mut buf = FrameBuffer::default();
        buf.push(&encoded);
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert!(!frame.end_stream);
        assert!(!frame.compressed);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn end_stream_flag_preserved() {
        let encoded = encode_frame(CONNECT_END_STREAM_FLAG, b"");
        let mut buf = FrameBuffer::default();
        buf.push(&encoded);
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert!(frame.end_stream);
    }

    #[test]
    fn chunked_input_reassembles() {
        let encoded = encode_frame(0, b"chunked");
        let mut buf = FrameBuffer::default();
        for byte in encoded {
            buf.push(&[byte]);
        }
        let frame = buf.next_frame().expect("frame").expect("ok");
        assert_eq!(frame.payload, b"chunked");
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
}
