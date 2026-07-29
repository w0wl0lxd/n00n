//! In-memory Cursor checkpoint blob store for `KvClientMessage` / `KvServerMessage`.
//!
//! Wire (from cursor-agent 2026.07.26):
//! - Server → client: `AgentServerMessage.kv_server_message` (f4)
//!   - `GetBlobArgs { blob_id }` (f2) or `SetBlobArgs { blob_id, blob_data }` (f3)
//! - Client → server: `AgentClientMessage.kv_client_message` (f3)
//!   - `GetBlobResult { blob_data? }` (f2) or `SetBlobResult { error? }` (f3)
//! - Resume: `UserMessage.conversation_state_blob_id` (f10 bytes)

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::proto::{field_bytes, field_ld, field_varint};

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
    let mut result = Vec::new();
    if let Some(data) = blob_data {
        result.extend(field_bytes(1, data));
    }
    let mut kv = field_varint(1, u64::from(request_id));
    kv.extend(field_ld(2, &result));
    field_ld(3, &kv)
}

/// Encode `AgentClientMessage.kv_client_message` for a set-blob ack.
#[must_use]
pub(crate) fn encode_set_blob_result(request_id: u32) -> Vec<u8> {
    let mut kv = field_varint(1, u64::from(request_id));
    kv.extend(field_ld(3, &[]));
    field_ld(3, &kv)
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

pub(crate) fn parse_kv_server_message(payload: &[u8]) -> Result<Option<KvServerOp>, String> {
    use super::proto::{decode_varint, iter_fields};

    for field in iter_fields(payload) {
        let (num, wire, data) = field?;
        if num != 4 || wire != 2 {
            continue;
        }
        let mut id = 0u32;
        let mut get_blob: Option<Vec<u8>> = None;
        let mut set_blob: Option<(Vec<u8>, Vec<u8>)> = None;
        let mut rest = data;
        while !rest.is_empty() {
            let (tag, after) = decode_varint(rest)?;
            rest = after;
            let field_no = tag >> 3;
            let wire_ty = (tag & 7) as u8;
            match (field_no, wire_ty) {
                (1, 0) => {
                    let (value, after) = decode_varint(rest)?;
                    id = u32::try_from(value).map_err(|_| "kv id overflow".to_string())?;
                    rest = after;
                }
                (2, 2) => {
                    let (len, after) = decode_varint(rest)?;
                    let len = usize::try_from(len).map_err(|_| "len overflow".to_string())?;
                    if after.len() < len {
                        return Err("truncated get_blob_args".into());
                    }
                    let (args, after) = after.split_at(len);
                    rest = after;
                    for arg in iter_fields(args) {
                        let (anum, awire, adata) = arg?;
                        if anum == 1 && awire == 2 {
                            get_blob = Some(adata.to_vec());
                        }
                    }
                }
                (3, 2) => {
                    let (len, after) = decode_varint(rest)?;
                    let len = usize::try_from(len).map_err(|_| "len overflow".to_string())?;
                    if after.len() < len {
                        return Err("truncated set_blob_args".into());
                    }
                    let (args, after) = after.split_at(len);
                    rest = after;
                    let mut blob_id = Vec::new();
                    let mut blob_data = Vec::new();
                    for arg in iter_fields(args) {
                        let (anum, awire, adata) = arg?;
                        if anum == 1 && awire == 2 {
                            blob_id = adata.to_vec();
                        } else if anum == 2 && awire == 2 {
                            blob_data = adata.to_vec();
                        }
                    }
                    set_blob = Some((blob_id, blob_data));
                }
                (_, 2) => {
                    let (len, after) = decode_varint(rest)?;
                    let len = usize::try_from(len).map_err(|_| "len overflow".to_string())?;
                    if after.len() < len {
                        return Err("truncated kv field".into());
                    }
                    rest = &after[len..];
                }
                (_, 0) => {
                    let (_, after) = decode_varint(rest)?;
                    rest = after;
                }
                _ => return Err(format!("unsupported kv wire {wire_ty}")),
            }
        }
        if let Some(blob_id) = get_blob {
            return Ok(Some(KvServerOp::Get { id, blob_id }));
        }
        if let Some((blob_id, data)) = set_blob {
            return Ok(Some(KvServerOp::Set { id, blob_id, data }));
        }
        return Ok(None);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::cursor::proto::{field_bytes, field_ld, field_varint};

    #[test]
    fn store_roundtrip() {
        let mut store = CheckpointStore::new();
        store.set(b"id".to_vec(), b"data".to_vec());
        assert_eq!(store.get(b"id"), Some(b"data".as_slice()));
    }

    #[test]
    fn parse_set_blob_args() {
        let mut args = field_bytes(1, b"blob-id");
        args.extend(field_bytes(2, b"blob-data"));
        let mut kv = field_varint(1, 7);
        kv.extend(field_ld(3, &args));
        let msg = field_ld(4, &kv);
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
    }
}
