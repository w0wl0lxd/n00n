//! Test-only utilities for inspecting raw protobuf wire encoding.

/// One raw protobuf field, as seen on the wire.
#[derive(Debug, Clone)]
pub(crate) struct WireField {
    pub number: u64,
    pub wire_type: u8,
    pub data: Vec<u8>,
}

impl WireField {
    pub fn as_varint(&self) -> Option<u64> {
        if self.wire_type != 0 {
            return None;
        }
        decode_varint(&self.data).ok().map(|(v, _)| v)
    }

    pub fn as_string(&self) -> Option<String> {
        if self.wire_type != 2 {
            return None;
        }
        String::from_utf8(self.data.clone()).ok()
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        if self.wire_type != 2 {
            return None;
        }
        Some(&self.data)
    }
}

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
pub(crate) fn parse_wire_fields(mut buf: &[u8]) -> Result<Vec<WireField>, String> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let start = buf;
        let (tag, rest) = decode_varint(buf)?;
        let number = tag >> 3;
        let wire_type = (tag & 0x7) as u8;
        let _ = start; // only rest is needed after decoding the tag
        match wire_type {
            0 => {
                let field_start = rest;
                let (_, rest2) = decode_varint(rest)?;
                let value_consumed = field_start.len() - rest2.len();
                let value_bytes = &field_start[..value_consumed];
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
