//! In-memory Cursor checkpoint blob store for `KvClientMessage` / `KvServerMessage`.
//!
//! Wire (from cursor-agent 2026.07.26):
//! - Server → client: `AgentServerMessage.kv_server_message` (f4) wraps a `KvServerMessage`
//!   - `KvServerMessage.get_blob` (f2) → `GetBlobArgs { blob_id }` (f1)
//!   - `KvServerMessage.set_blob` (f3) → `SetBlobArgs { blob_id, blob_data }` (f1, f2)
//! - Client → server: `AgentClientMessage.kv_client_message` (f3) wraps a `KvClientMessage`
//!   - `KvClientMessage.get_blob_result` (f2) → `GetBlobResult { blob_data? }` (f1)
//!   - `KvClientMessage.set_blob_result` (f3) → `SetBlobResult { error? }` (f1)
//! - Resume: `UserMessage.conversation_state_blob_id` (f10 bytes)

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use prost::Message;

use super::proto::{GetBlobResult, KvClientMessage, KvServerMessage, SetBlobResult, field_ld};

#[derive(Debug, Default)]
pub(crate) struct CheckpointStore {
    blobs: HashMap<Vec<u8>, Vec<u8>>,
}

impl CheckpointStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&mut self, blob_id: Vec<u8>, data: Vec<u8>) {
        self.blobs.insert(blob_id, data);
    }

    pub(crate) fn get(&self, blob_id: &[u8]) -> Option<&[u8]> {
        self.blobs.get(blob_id).map(Vec::as_slice)
    }
}

pub(crate) type SharedCheckpointStore = Arc<Mutex<CheckpointStore>>;

pub(crate) fn shared_store() -> SharedCheckpointStore {
    Arc::new(Mutex::new(CheckpointStore::new()))
}

/// Encode `AgentClientMessage.kv_client_message` for a get-blob response.
#[must_use]
pub(crate) fn encode_get_blob_result(request_id: u32, blob_data: Option<&[u8]>) -> Vec<u8> {
    let result = GetBlobResult {
        blob_data: blob_data.map_or_else(Vec::new, <[u8]>::to_vec),
    };
    let mut result_bytes = result.encode_to_vec();
    if matches!(blob_data, Some(data) if data.is_empty()) {
        result_bytes.extend_from_slice(&field_ld(1, &[]));
    }
    let kv = KvClientMessage {
        id: u64::from(request_id),
        get_blob_result: None,
        set_blob_result: None,
    };
    let mut kv_bytes = kv.encode_to_vec();
    kv_bytes.extend_from_slice(&field_ld(2, &result_bytes));
    field_ld(3, &kv_bytes)
}

/// Encode `AgentClientMessage.kv_client_message` for a set-blob ack.
#[must_use]
pub(crate) fn encode_set_blob_result(request_id: u32) -> Vec<u8> {
    let result = SetBlobResult {
        error: String::new(),
    };
    let kv = KvClientMessage {
        id: u64::from(request_id),
        get_blob_result: None,
        set_blob_result: Some(result),
    };
    field_ld(3, &kv.encode_to_vec())
}

/// Parse `kv_server_message` payload into get/set ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KvServerOp {
    Get {
        id: u32,
        blob_id: Vec<u8>,
    },
    Set {
        id: u32,
        blob_id: Vec<u8>,
        data: Vec<u8>,
    },
}

/// Parse the bare `KvServerMessage` payload (the contents of `AgentServerMessage.kv_server_message`, field 4).
pub(crate) fn parse_kv_server_message(payload: &[u8]) -> Result<Option<KvServerOp>, String> {
    let msg = KvServerMessage::decode(payload).map_err(|e| e.to_string())?;
    let id = u32::try_from(msg.id).map_err(|_| "kv id overflow".to_string())?;
    if let Some(get_blob) = msg.get_blob {
        return Ok(Some(KvServerOp::Get {
            id,
            blob_id: get_blob.blob_id,
        }));
    }
    if let Some(set_blob) = msg.set_blob {
        return Ok(Some(KvServerOp::Set {
            id,
            blob_id: set_blob.blob_id,
            data: set_blob.blob_data,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::cursor::proto::{GetBlobArgs, KvServerMessage, SetBlobArgs};
    use crate::providers::proto_test_util::parse_wire_fields;

    #[test]
    fn store_roundtrip() {
        let mut store = CheckpointStore::new();
        store.set(b"id".to_vec(), b"data".to_vec());
        assert_eq!(store.get(b"id"), Some(b"data".as_slice()));
    }

    #[test]
    fn parse_set_blob_args() {
        let args = SetBlobArgs {
            blob_id: b"blob-id".to_vec(),
            blob_data: b"blob-data".to_vec(),
        };
        let kv = KvServerMessage {
            id: 7,
            get_blob: None,
            set_blob: Some(args),
        };
        let msg = kv.encode_to_vec();
        let op = parse_kv_server_message(&msg).expect("parse").expect("op");
        assert_eq!(
            op,
            KvServerOp::Set {
                id: 7,
                blob_id: b"blob-id".to_vec(),
                data: b"blob-data".to_vec(),
            }
        );
    }

    #[test]
    fn encode_get_blob_result_wraps_client_message_field_3() {
        let frame = encode_get_blob_result(3, Some(b"payload"));
        // outer field 3 = kv_client_message
        assert_eq!(frame[0] & 0x07, 2);
        assert_eq!(frame[0] >> 3, 3);
        let outer = parse_wire_fields(&frame).expect("valid wrapper");
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].number, 3);
        let decoded = KvClientMessage::decode(outer[0].as_bytes().unwrap()).expect("decode kv");
        assert_eq!(decoded.id, 3);
        assert!(decoded.get_blob_result.is_some());
    }

    #[test]
    fn get_blob_result_wire_format_with_data() {
        let frame = encode_get_blob_result(7, Some(b"payload"));
        let outer = parse_wire_fields(&frame).expect("valid wrapper");
        assert_eq!(outer.iter().map(|f| f.number).collect::<Vec<_>>(), vec![3]);
        let fields = parse_wire_fields(outer[0].as_bytes().unwrap()).expect("valid kv");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let result = parse_wire_fields(fields[1].as_bytes().unwrap()).expect("result");
        assert_eq!(result[0].number, 1);
        assert_eq!(result[0].as_bytes(), Some(b"payload".as_slice()));
    }

    #[test]
    fn get_blob_result_wire_format_without_data() {
        let frame = encode_get_blob_result(7, None);
        let outer = parse_wire_fields(&frame).expect("valid wrapper");
        assert_eq!(outer.iter().map(|f| f.number).collect::<Vec<_>>(), vec![3]);
        let fields = parse_wire_fields(outer[0].as_bytes().unwrap()).expect("valid kv");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let result = parse_wire_fields(fields[1].as_bytes().unwrap()).expect("result");
        assert!(result.is_empty());
    }

    #[test]
    fn set_blob_result_wire_format() {
        let frame = encode_set_blob_result(5);
        let outer = parse_wire_fields(&frame).expect("valid wrapper");
        assert_eq!(outer.iter().map(|f| f.number).collect::<Vec<_>>(), vec![3]);
        let fields = parse_wire_fields(outer[0].as_bytes().unwrap()).expect("valid kv");
        assert_eq!(
            fields.iter().map(|f| f.number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        let result = parse_wire_fields(fields[1].as_bytes().unwrap()).expect("result");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_kv_server_message_rejects_truncated() {
        let mut buf = KvServerMessage {
            id: 7,
            get_blob: Some(GetBlobArgs {
                blob_id: b"id".to_vec(),
            }),
            set_blob: None,
        }
        .encode_to_vec();
        buf.pop();
        assert!(parse_kv_server_message(&buf).is_err());
    }
}
