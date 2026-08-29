//! Test-only utilities for inspecting raw protobuf wire encoding.

/// One raw protobuf field, as seen on the wire.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct WireField {
    pub number: u64,
    pub wire_type: u8,
    pub data: Vec<u8>,
}

impl WireField {
    #[allow(dead_code)]
    pub fn as_varint(&self) -> Option<u64> {
        if self.wire_type != 0 {
            return None;
        }
        decode_varint(&self.data).ok().map(|(v, _)| v)
    }

    #[allow(dead_code)]
    pub fn as_string(&self) -> Option<String> {
        if self.wire_type != 2 {
            return None;
        }
        String::from_utf8(self.data.clone()).ok()
    }

    #[allow(dead_code)]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        if self.wire_type != 2 {
            return None;
        }
        Some(&self.data)
    }
}

#[allow(dead_code)]
fn decode_varint(buf: &[u8]) -> Result<(u64, &[u8]), String> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (i, &b) in buf.iter().enumerate() {
        if shift > 63 {
            return Err("varint overflow".to_string());
        }
        value |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return Ok((value, &buf[i + 1..]));
        }
        shift += 7;
    }
    Err("truncated varint".to_string())
}

/// Parse a protobuf-encoded buffer into its wire fields.
#[allow(dead_code)]
pub(crate) fn parse_wire_fields(mut buf: &[u8]) -> Result<Vec<WireField>, String> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let (tag, rest) = decode_varint(buf)?;
        let number = tag >> 3;
        let wire_type = (tag & 0x7) as u8;
        match wire_type {
            0 => {
                let varint_start = rest;
                let (_, rest2) = decode_varint(rest)?;
                let value_consumed = varint_start.len() - rest2.len();
                let value_bytes = &varint_start[..value_consumed];
                buf = rest2;
                out.push(WireField {
                    number,
                    wire_type,
                    data: value_bytes.to_vec(),
                });
            }
            1 => {
                if rest.len() < 8 {
                    return Err("truncated 64-bit field".to_string());
                }
                let (data, rest2) = rest.split_at(8);
                buf = rest2;
                out.push(WireField {
                    number,
                    wire_type,
                    data: data.to_vec(),
                });
            }
            2 => {
                let (len, rest2) = decode_varint(rest)?;
                let len = usize::try_from(len).map_err(|_| "field length overflow".to_string())?;
                if rest2.len() < len {
                    return Err("truncated length-delimited field".to_string());
                }
                let (data, rest2) = rest2.split_at(len);
                buf = rest2;
                out.push(WireField {
                    number,
                    wire_type,
                    data: data.to_vec(),
                });
            }
            5 => {
                if rest.len() < 4 {
                    return Err("truncated 32-bit field".to_string());
                }
                let (data, rest2) = rest.split_at(4);
                buf = rest2;
                out.push(WireField {
                    number,
                    wire_type,
                    data: data.to_vec(),
                });
            }
            _ => return Err(format!("unsupported wire type {wire_type}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wire_fields_roundtrips_varint_string_and_bytes() {
        // Build a minimal protobuf by hand: field 1 varint 150, field 2 string "hi",
        // field 3 bytes "bye".
        let mut buf = Vec::new();
        // tag 1 (varint) -> 0x08, value 150 -> 0x96 0x01
        buf.extend([0x08, 0x96, 0x01]);
        // tag 2 (ld) -> 0x12, length 2, "hi"
        buf.extend([0x12, 0x02, b'h', b'i']);
        // tag 3 (ld) -> 0x1a, length 3, "bye"
        buf.extend([0x1a, 0x03, b'b', b'y', b'e']);

        let fields = parse_wire_fields(&buf).expect("valid");
        assert_eq!(fields.len(), 3);

        assert_eq!(fields[0].number, 1);
        assert_eq!(fields[0].wire_type, 0);
        assert_eq!(fields[0].as_varint(), Some(150));

        assert_eq!(fields[1].number, 2);
        assert_eq!(fields[1].wire_type, 2);
        assert_eq!(fields[1].as_string().as_deref(), Some("hi"));

        assert_eq!(fields[2].number, 3);
        assert_eq!(fields[2].wire_type, 2);
        assert_eq!(fields[2].as_bytes(), Some(b"bye".as_slice()));
    }

    #[test]
    fn parse_wire_fields_rejects_truncated() {
        let mut buf = Vec::new();
        buf.extend([0x08, 0x80]); // tag 1 varint, incomplete varint
        assert!(parse_wire_fields(&buf).is_err());
    }
}
