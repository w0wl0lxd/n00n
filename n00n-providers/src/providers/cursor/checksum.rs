//! Cursor `x-cursor-checksum` (Jyh timestamp cipher + machine id).
//!
//! Algorithm matches the public RE in `eisbaw/cursor_api_demo` (`generateCursorChecksum`).
//!
//! This module is not yet wired into the Cursor provider; it's prepared for
//! future native integration to replace the cursor-agent subprocess approach.

#![allow(dead_code)]

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OpenFlags;
use uuid::Uuid;

const MACHINE_ID_KEY: &str = "storage.serviceMachineId";
const CHECKSUM_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const JYH_SEED: u8 = 165;
const TIMESTAMP_CHUNK_MS: u128 = 1_000_000;
/// Named fallback when the host clock is before the Unix epoch.
const CLOCK_BEFORE_EPOCH_MS: u128 = 0;

/// Cursor wire-protocol pad appended when hashing a token into a synthetic machine id.
///
/// This is **not** an application secret. The public Cursor checksum algorithm
/// (`generateCursorChecksum` / eisbaw) uses the literal ASCII `machineId` as the
/// fallback pad when no IDE `storage.serviceMachineId` is available. Changing it
/// would break `x-cursor-checksum` interop with Cursor's servers.
// codeql[rust/hard-coded-cryptographic-value]
const CURSOR_MACHINE_ID_FALLBACK_PAD: &str = "machineId";

/// Read `storage.serviceMachineId` from the Cursor IDE state db.
pub(crate) fn read_machine_id_from(path: &Path) -> Option<String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let value: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            [MACHINE_ID_KEY],
            |row| row.get(0),
        )
        .ok()?;
    if value.is_empty() { None } else { Some(value) }
}

/// SHA-256 hex of `input` concatenated with a Cursor wire-protocol pad (64 chars).
///
/// `protocol_pad` is an interop constant from Cursor's public checksum algorithm,
/// not a randomly chosen application salt.
#[must_use]
pub(crate) fn hashed_64_hex(input: &str, protocol_pad: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher.update(protocol_pad.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// UUID v5 (DNS namespace) of the auth token — Cursor `x-session-id`.
#[must_use]
pub(crate) fn session_id_from_token(token: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, token.as_bytes()).to_string()
}

/// Build `x-cursor-checksum` for the given unix-ms timestamp and machine id.
#[must_use]
pub(crate) fn checksum_at_millis(timestamp_ms: u128, machine_id: &str) -> String {
    let chunk = timestamp_ms / TIMESTAMP_CHUNK_MS;
    let mut bytes = [
        ((chunk >> 40) & 0xff) as u8,
        ((chunk >> 32) & 0xff) as u8,
        ((chunk >> 24) & 0xff) as u8,
        ((chunk >> 16) & 0xff) as u8,
        ((chunk >> 8) & 0xff) as u8,
        (chunk & 0xff) as u8,
    ];
    let mut t = JYH_SEED;
    for i in 0..6u8 {
        let byte = &mut bytes[usize::from(i)];
        *byte = (*byte ^ t).wrapping_add(i);
        t = *byte;
    }
    format!("{}{machine_id}", urlsafe_b64_nopad(&bytes))
}

/// Current-time checksum using IDE machine id, or a token-derived fallback.
#[must_use]
pub(crate) fn generate_checksum(token: &str, machine_id: Option<&str>) -> String {
    let machine = machine_id.map_or_else(
        || hashed_64_hex(token, CURSOR_MACHINE_ID_FALLBACK_PAD),
        ToOwned::to_owned,
    );
    let now_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis(),
        Err(_) => CLOCK_BEFORE_EPOCH_MS,
    };
    checksum_at_millis(now_ms, &machine)
}

/// Resolve machine id from the IDE state db when present.
#[must_use]
pub(crate) fn resolve_machine_id() -> Option<String> {
    super::auth::ide_vscdb_path().and_then(|path| read_machine_id_from(&path))
}

/// SHA-256 hex of the bare token (`x-client-key`).
///
/// Cursor's wire format hashes the token alone (no protocol pad). Implemented
/// as a direct digest so we never pass an empty "salt" into a hasher API —
/// that empty pad is an interop constant, not a secret, but it trips CodeQL's
/// hard-coded cryptographic-value heuristic.
#[must_use]
pub(crate) fn client_key_from_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn urlsafe_b64_nopad(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let c = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(CHECKSUM_ALPHABET[(a >> 2) as usize] as char);
        out.push(CHECKSUM_ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(CHECKSUM_ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        }
        if i + 2 < bytes.len() {
            out.push(CHECKSUM_ALPHABET[(c & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_deterministic_for_fixed_timestamp() {
        let a = checksum_at_millis(1_700_000_000_000, "machine-abc");
        let b = checksum_at_millis(1_700_000_000_000, "machine-abc");
        assert_eq!(a, b);
        assert!(a.ends_with("machine-abc"));
        assert!(a.len() > "machine-abc".len());
    }

    #[test]
    fn checksum_changes_with_machine_id() {
        let a = checksum_at_millis(1_700_000_000_000, "a");
        let b = checksum_at_millis(1_700_000_000_000, "b");
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_is_stable_uuid_v5() {
        let a = session_id_from_token("tok");
        let b = session_id_from_token("tok");
        assert_eq!(a, b);
        assert_ne!(a, session_id_from_token("other"));
        assert!(Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn hashed_64_hex_is_64_chars() {
        assert_eq!(hashed_64_hex("x", CURSOR_MACHINE_ID_FALLBACK_PAD).len(), 64);
    }

    #[test]
    fn client_key_matches_bare_token_sha256() {
        use sha2::{Digest, Sha256};
        let expected = format!("{:x}", Sha256::digest(b"tok"));
        assert_eq!(client_key_from_token("tok"), expected);
        assert_eq!(client_key_from_token("tok").len(), 64);
    }

    #[test]
    fn checksum_matches_eisbaw_fixed_timestamp() {
        // Python eisbaw generate_cursor_checksum at ms=1_700_000_000_000.
        assert_eq!(
            checksum_at_millis(1_700_000_000_000, "machine-abc"),
            "paaotEjtmachine-abc"
        );
    }
}
