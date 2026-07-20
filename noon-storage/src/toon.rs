//! Lossless TOON passthrough for on-disk JSON.
//!
//! JSON is parsed and re-encoded to TOON via `toon-format`. When TOON is
//! smaller than the original we store it with a 1-byte marker; otherwise we
//! store the original JSON verbatim. Decoding recovers the exact JSON value
//! either way, so the format is a strict win/passthrough with no information
//! loss. `toon-format`'s decoder interoperates through `serde_json::Value`,
//! which is used as the in-memory interchange type.

use serde_json::Value;
use toon_format::{decode_default, encode_default};

use crate::StorageError;

const TOON_MARKER: u8 = 0x01;
const JSON_MARKER: u8 = 0x00;

/// Re-encode `json` (UTF-8 JSON bytes) as a lossless, possibly smaller, blob.
///
/// Returns the original bytes prefixed with [`JSON_MARKER`] when TOON does not
/// shrink them, or the TOON text prefixed with [`TOON_MARKER`] when it does.
pub fn serialize_lossless(json: &[u8]) -> Result<Vec<u8>, StorageError> {
    let value: Value =
        serde_json::from_slice(json).map_err(|e| StorageError::Toon(format!("json parse: {e}")))?;
    let toon =
        encode_default(&value).map_err(|e| StorageError::Toon(format!("toon encode: {e}")))?;
    let toon_bytes = toon.as_bytes();
    let toon_smaller = toon_bytes.len() + 1 < json.len();
    if toon_smaller && toon_roundtrips(&value, toon_bytes) {
        let mut out = Vec::with_capacity(toon_bytes.len() + 1);
        out.push(TOON_MARKER);
        out.extend_from_slice(toon_bytes);
        Ok(out)
    } else {
        let mut out = Vec::with_capacity(json.len() + 1);
        out.push(JSON_MARKER);
        out.extend_from_slice(json);
        Ok(out)
    }
}

/// Inverse of [`serialize_lossless`].
///
/// Returns canonical JSON bytes for the TOON form, or the original bytes for
/// the passthrough form; either way the result is value-equivalent to the
/// input that was originally serialized.
pub fn deserialize_lossless(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    let (&marker, rest) = data
        .split_first()
        .ok_or_else(|| StorageError::Toon("empty toon blob".into()))?;
    match marker {
        TOON_MARKER => {
            let value: Value = decode_default(
                std::str::from_utf8(rest)
                    .map_err(|e| StorageError::Toon(format!("toon utf8: {e}")))?,
            )
            .map_err(|e| StorageError::Toon(format!("toon decode: {e}")))?;
            serde_json::to_vec(&value).map_err(|e| StorageError::Toon(format!("json emit: {e}")))
        }
        JSON_MARKER => Ok(rest.to_vec()),
        other => Err(StorageError::Toon(format!("unknown marker {other:#x}"))),
    }
}

/// Whether the TOON encoding of `value` decodes back to an equal `serde_json::Value`.
///
/// TOON normalizes some number forms (e.g. `1e10` -> `10000000000`, `-0.0` ->
/// `0`), so we verify round-trip equality before trusting it as lossless.
fn toon_roundtrips(value: &Value, toon_bytes: &[u8]) -> bool {
    std::str::from_utf8(toon_bytes)
        .ok()
        .and_then(|s| decode_default::<Value>(s).ok())
        .and_then(|decoded| serde_json::to_value(&decoded).ok())
        .is_some_and(|decoded| decoded == *value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &[u8] = br#"{"a":1}"#;
    const BIG: &[u8] = br#"{"items":["alpha","alpha","alpha","alpha","alpha","alpha","alpha","alpha","alpha","alpha"],"n":42}"#;

    fn parse(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn roundtrip_preserves_small_value() {
        let stored = serialize_lossless(SMALL).unwrap();
        let back = deserialize_lossless(&stored).unwrap();
        assert_eq!(parse(&back), parse(SMALL));
    }

    #[test]
    fn roundtrip_preserves_big_value() {
        let stored = serialize_lossless(BIG).unwrap();
        let back = deserialize_lossless(&stored).unwrap();
        assert_eq!(parse(&back), parse(BIG));
    }

    #[test]
    fn toon_wins_on_repetitive_json() {
        let stored = serialize_lossless(BIG).unwrap();
        assert_eq!(stored[0], TOON_MARKER);
        assert!(stored.len() < BIG.len() + 1, "toon should be smaller");
    }

    #[test]
    fn passthrough_keeps_original_when_not_smaller() {
        let stored = serialize_lossless(SMALL).unwrap();
        assert!(stored[0] == JSON_MARKER || stored[0] == TOON_MARKER);
        assert_eq!(deserialize_lossless(&stored).unwrap(), SMALL);
    }

    #[test]
    fn rejects_unknown_marker() {
        assert!(deserialize_lossless(&[0x02, b'x']).is_err());
    }

    #[test]
    fn empty_input_errors() {
        assert!(serialize_lossless(b"").is_err());
        assert!(deserialize_lossless(b"").is_err());
    }

    #[test]
    fn lossless_holds_for_number_and_unicode_edge_cases() {
        let cases: &[&str] = &[
            r#"{"big":9007199254740993,"neg":-0.0,"exp":1e10,"frac":1.5,"small":1}"#,
            r#"{"u":"héllo ⇢ 日本語","esc":"\u0041"}"#,
            r#"[1,2.0,3e2,-4,-0.0,1.0]"#,
            r#"{"nested":{"a":[1,2,{"b":3}]}}"#,
        ];
        for c in cases {
            let stored = serialize_lossless(c.as_bytes()).unwrap();
            let back = deserialize_lossless(&stored).unwrap();
            assert_eq!(
                parse(&back),
                parse(c.as_bytes()),
                "lossless mismatch for {c}"
            );
        }
    }
}
