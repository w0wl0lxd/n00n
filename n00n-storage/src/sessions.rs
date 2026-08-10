//! Session persistence with append-only, zstd-compressed JSONL logs.
//!
//! Each session is stored as a canonical `{id}.jsonl`, with one or more zstd frames.
//! The format is crash-safe: on load, any trailing partial frame is discarded.
//! `SessionLog` tracks cursor state to enable O(delta) incremental saves.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Error as IoError, ErrorKind, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::UNIX_EPOCH;

use tracing::warn;

use crate::id::{n00nId, n00nIdParseError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zstd::stream::{Decoder, Encoder};

use crate::{
    StateDir, StorageError, atomic_write, atomic_write_permissions, atomic_write_streaming,
    now_epoch,
};

const SESSION_VERSION: u32 = 1;
const LOG_FORMAT_VERSION: u32 = 3;
const COMPRESS_LEVEL: i32 = 3;
const MAX_INCREMENTAL_FRAMES: u64 = 16_384;
const MAX_SESSION_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_SESSION_DECODED_BYTES: usize = 512 * 1024 * 1024;
const MAX_SCAN_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCAN_DECODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_ZSTD_WINDOW_LOG: u32 = 27;
const ZSTD_WINDOW_TOO_LARGE_ERROR_CODE: usize = 16;
const TRANSCRIPT_RECORD_TYPE: &str = "transcript";
pub const SESSIONS_DIR: &str = "sessions";
const CWD_INDEX_FILE: &str = "cwd_latest.json";
const SCAN_CACHE_FILE: &str = "scan_cache_v3.json";
const SCAN_CACHE_FILE_V2: &str = "scan_cache_v2.json";
const DEFAULT_TITLE: &str = "New session";
const MAX_TITLE_LEN: usize = 60;
const MAX_SNIPPET_BYTES: usize = 256;
const MAX_FIRST_MESSAGE_LINE_BYTES: usize = 64 * 1024;
const MAX_FIRST_MESSAGE_TEXT_BYTES: usize = 1024;
const MAX_FIRST_MESSAGE_BYTES: usize = 256 * 1024;
pub const SESSION_STATE_SCHEMA_VERSION: u32 = 1;
const MAX_PLUGIN_STATE_ENTRIES: usize = 64;
const MAX_PLUGIN_STATE_NAME_BYTES: usize = 128;
pub const MAX_PLUGIN_STATE_BYTES: usize = 256 * 1024;
const MAX_SESSION_STATE_BYTES: usize = 1024 * 1024;
const META_RECORD_PREFIX: &str = r#"{"t":"meta""#;
const MSG_RECORD_PREFIX: &str = r#"{"t":"msg""#;
const OPENAI_RESPONSE_CHAIN_SUFFIX: &str = "openai-response.json";
const OPENAI_RESPONSE_CHAIN_LOCK_SUFFIX: &str = "openai-response.lock";
const OPENAI_RESPONSE_CHAIN_FILE_MODE: u32 = 0o600;
pub const OPENAI_RESPONSE_CHAIN_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("incompatible session version {found} (expected {expected})")]
    VersionMismatch { found: u32, expected: u32 },
    #[error("session ID mismatch: log owns {log_id}, got {given_id}")]
    IdMismatch { log_id: n00nId, given_id: n00nId },
    #[error("session log {path} has header id {raw_id:?} that is not a valid id: {source}")]
    CorruptHeaderId {
        path: String,
        raw_id: String,
        source: n00nIdParseError,
    },
    #[error("cursor ahead of session (log has {saved}, session has {actual}); compact required")]
    CursorAhead { saved: usize, actual: usize },
    #[error("session log record in {path} exceeds the {limit}-byte decoded record limit")]
    RecordTooLarge { path: String, limit: usize },
    #[error("session log {path} exceeds the configured zstd window-log limit {window_log}")]
    DecoderWindowLimitExceeded { path: String, window_log: u32 },
    #[error("decoded session log {path} exceeds the {limit}-byte load budget")]
    DecodedBudgetExceeded { path: String, limit: usize },
    #[error("session log contains an unknown record type")]
    UnknownRecord,
    #[error("session record exceeds the {maximum}-byte limit")]
    RecordTooLargeWrite { maximum: usize },
}

/// Per-model token breakdown entry. Mirrors the four usage counters tracked by
/// the active provider; kept storage-local to avoid a circular dependency on
/// `n00n-providers`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokenUsage {
    #[serde(default)]
    pub input: u32,
    #[serde(default)]
    pub output: u32,
    #[serde(default)]
    pub cache_creation: u32,
    #[serde(default)]
    pub cache_read: u32,
}

impl StoredTokenUsage {
    #[must_use]
    pub fn total_input(&self) -> u32 {
        self.input + self.cache_read + self.cache_creation
    }

    #[must_use]
    pub fn total(&self) -> u32 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

impl std::ops::AddAssign for StoredTokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input += rhs.input;
        self.output += rhs.output;
        self.cache_creation += rhs.cache_creation;
        self.cache_read += rhs.cache_read;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredImageMediaType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredImageSource {
    pub media_type: StoredImageMediaType,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMcpPrompt {
    pub qualified_name: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub arguments: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredDelivery {
    #[default]
    TurnEnd,
    Steering,
    Immediate,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredSessionLifecycle {
    Queued,
    Bootstrapping,
    Running,
    WaitingInput,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
    #[default]
    Idle,
}

impl StoredSessionLifecycle {
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Bootstrapping | Self::Running | Self::WaitingInput
        )
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredQueuedMessage {
    pub text: String,
    pub images: Vec<StoredImageSource>,
    /// `None` means this field was absent in an older session snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<StoredMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<StoredThinking>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workflow: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control: bool,
    #[serde(default, skip_serializing_if = "is_default_delivery")]
    pub delivery: StoredDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<StoredMcpPrompt>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires fn(&T) -> bool
fn is_default_delivery(delivery: &StoredDelivery) -> bool {
    *delivery == StoredDelivery::TurnEnd
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStateScope {
    #[default]
    Session,
    Root,
}

impl StoredStateScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Root => "root",
        }
    }

    fn from_stored(value: &str) -> Option<Self> {
        match value {
            "session" => Some(Self::Session),
            "root" => Some(Self::Root),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoredPluginState {
    schema_version: Option<u64>,
    raw: serde_json::Value,
}

impl StoredPluginState {
    fn new(schema_version: u32, payload: serde_json::Value) -> Self {
        let mut fields = serde_json::Map::new();
        fields.insert(
            "schema_version".to_owned(),
            serde_json::Value::from(schema_version),
        );
        fields.insert("payload".to_owned(), payload);
        Self {
            schema_version: Some(u64::from(schema_version)),
            raw: serde_json::Value::Object(fields),
        }
    }

    fn from_raw(raw: serde_json::Value) -> Self {
        let schema_version = raw
            .get("schema_version")
            .and_then(serde_json::Value::as_u64);
        Self {
            schema_version,
            raw,
        }
    }

    fn payload(&self) -> Option<&serde_json::Value> {
        self.raw.get("payload")
    }
}

impl Serialize for StoredPluginState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StoredPluginState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::from_raw(raw))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StoredPluginScopes {
    scopes: Option<BTreeMap<String, StoredPluginState>>,
    malformed: Option<serde_json::Value>,
}

impl Default for StoredPluginScopes {
    fn default() -> Self {
        Self {
            scopes: Some(BTreeMap::new()),
            malformed: None,
        }
    }
}

impl StoredPluginScopes {
    fn set(
        &mut self,
        plugin: &str,
        scope: StoredStateScope,
        schema_version: u32,
        payload: serde_json::Value,
    ) -> Result<(), SessionStateError> {
        let Some(scopes) = self.scopes.as_mut() else {
            return Err(SessionStateError::InvalidPluginContainer {
                plugin: plugin.to_owned(),
            });
        };
        if let Some(state) = scopes.get_mut(scope.as_str()) {
            let serde_json::Value::Object(fields) = &mut state.raw else {
                return Err(SessionStateError::InvalidPluginState {
                    plugin: plugin.to_owned(),
                    scope,
                });
            };
            fields.insert(
                "schema_version".to_owned(),
                serde_json::Value::from(schema_version),
            );
            fields.insert("payload".to_owned(), payload);
            state.schema_version = Some(u64::from(schema_version));
        } else {
            scopes.insert(
                scope.as_str().to_owned(),
                StoredPluginState::new(schema_version, payload),
            );
        }
        Ok(())
    }
}

impl Serialize for StoredPluginScopes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match (&self.scopes, &self.malformed) {
            (Some(scopes), _) => scopes.serialize(serializer),
            (None, Some(raw)) => raw.serialize(serializer),
            (None, None) => serde_json::Value::Null.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for StoredPluginScopes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let serde_json::Value::Object(fields) = raw else {
            return Ok(Self {
                scopes: None,
                malformed: Some(raw),
            });
        };
        let mut scopes = BTreeMap::new();
        for (scope, state) in fields {
            scopes.insert(scope, StoredPluginState::from_raw(state));
        }
        Ok(Self {
            scopes: Some(scopes),
            malformed: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredSessionStateSnapshotV1 {
    schema_version: u32,
    state_revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    plugins: BTreeMap<String, StoredPluginScopes>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
enum StoredSessionStateSnapshotInner {
    Supported(StoredSessionStateSnapshotV1),
    Unsupported {
        schema_version: u64,
        raw: serde_json::Value,
    },
    Malformed {
        raw: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSessionStateSnapshot {
    inner: StoredSessionStateSnapshotInner,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredPluginStateEntry<'a> {
    pub plugin: &'a str,
    pub scope: StoredStateScope,
    pub payload: &'a serde_json::Value,
}

impl Default for StoredSessionStateSnapshot {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Serialize for StoredSessionStateSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.inner {
            StoredSessionStateSnapshotInner::Supported(snapshot) => snapshot.serialize(serializer),
            StoredSessionStateSnapshotInner::Unsupported { raw, .. }
            | StoredSessionStateSnapshotInner::Malformed { raw } => raw.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for StoredSessionStateSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = serde_json::Value::deserialize(deserializer)?;
        let schema_version = raw
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                D::Error::custom("session-state snapshot requires numeric schema_version")
            })?;
        let bytes = serde_json::to_vec(&raw).map_err(D::Error::custom)?.len();
        if bytes > MAX_SESSION_STATE_BYTES {
            return Err(D::Error::custom(SessionStateError::SnapshotTooLarge {
                bytes,
                maximum: MAX_SESSION_STATE_BYTES,
            }));
        }
        if schema_version != u64::from(SESSION_STATE_SCHEMA_VERSION) {
            return Ok(Self {
                inner: StoredSessionStateSnapshotInner::Unsupported {
                    schema_version,
                    raw,
                },
            });
        }
        let snapshot: StoredSessionStateSnapshotV1 =
            serde_json::from_value(raw).map_err(D::Error::custom)?;
        validate_supported_snapshot(&snapshot).map_err(D::Error::custom)?;
        Ok(Self {
            inner: StoredSessionStateSnapshotInner::Supported(snapshot),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionStateError {
    #[error("unsupported session-state schema version {found} (expected {expected})")]
    UnsupportedSchemaVersion { found: u64, expected: u32 },
    #[error("session-state snapshot envelope is malformed")]
    InvalidEnvelope,
    #[error("session state has {found} plugin entries (maximum {maximum})")]
    TooManyPlugins { found: usize, maximum: usize },
    #[error("invalid session-state plugin name {plugin:?}")]
    InvalidPluginName { plugin: String },
    #[error("session-state plugin {plugin:?} has malformed scoped-state container")]
    InvalidPluginContainer { plugin: String },
    #[error("session-state plugin {plugin:?} has no scoped state")]
    MissingPluginState { plugin: String },
    #[error("session-state plugin {plugin:?} has malformed {scope:?} state")]
    InvalidPluginState {
        plugin: String,
        scope: StoredStateScope,
    },
    #[error("session-state plugin {plugin:?} has unsupported stored scope {scope:?}")]
    InvalidStoredScope { plugin: String, scope: String },
    #[error("session state for plugin {plugin:?} is {bytes} bytes (maximum {maximum})")]
    PluginStateTooLarge {
        plugin: String,
        bytes: usize,
        maximum: usize,
    },
    #[error("session state is {bytes} bytes (maximum {maximum})")]
    SnapshotTooLarge { bytes: usize, maximum: usize },
    #[error("unsupported state version {found} for plugin {plugin:?} (expected {expected})")]
    UnsupportedPluginVersion {
        plugin: String,
        found: u64,
        expected: u32,
    },
    #[error("session-state revision cannot regress from {current} to {requested}")]
    StateRevisionRegression { current: u64, requested: u64 },
    #[error("failed to measure serialized session state: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl StoredSessionStateSnapshot {
    #[must_use]
    pub fn new(state_revision: u64) -> Self {
        Self {
            inner: StoredSessionStateSnapshotInner::Supported(StoredSessionStateSnapshotV1 {
                schema_version: SESSION_STATE_SCHEMA_VERSION,
                state_revision,
                plugins: BTreeMap::new(),
                extra: BTreeMap::new(),
            }),
        }
    }

    #[must_use]
    pub fn state_revision(&self) -> Option<u64> {
        match &self.inner {
            StoredSessionStateSnapshotInner::Supported(snapshot) => Some(snapshot.state_revision),
            StoredSessionStateSnapshotInner::Unsupported { .. }
            | StoredSessionStateSnapshotInner::Malformed { .. } => None,
        }
    }

    /// Advances the state revision without allowing regression.
    ///
    /// # Errors
    /// Returns a typed error for unsupported envelopes, revision regression, or exceeded bounds.
    pub fn set_state_revision(&mut self, state_revision: u64) -> Result<(), SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        if state_revision < snapshot.state_revision {
            return Err(SessionStateError::StateRevisionRegression {
                current: snapshot.state_revision,
                requested: state_revision,
            });
        }
        let current_size = serde_json::to_vec(snapshot).map(|v| v.len()).ok();
        let mut candidate = snapshot.clone();
        candidate.state_revision = state_revision;
        let new_size = serde_json::to_vec(&candidate).map(|v| v.len()).ok();
        if new_size.is_some_and(|new| current_size.is_some_and(|cur| new <= cur)) {
            self.inner = StoredSessionStateSnapshotInner::Supported(candidate);
            return Ok(());
        }
        validate_supported_snapshot(&candidate)?;
        self.inner = StoredSessionStateSnapshotInner::Supported(candidate);
        Ok(())
    }

    /// Adds or replaces one exact plugin scope after enforcing snapshot bounds.
    ///
    /// # Errors
    /// Returns a typed error for unsupported envelopes, invalid names, or exceeded bounds.
    pub fn set_plugin_state(
        &mut self,
        plugin: &str,
        schema_version: u32,
        scope: StoredStateScope,
        payload: serde_json::Value,
    ) -> Result<(), SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        let mut candidate = snapshot.clone();
        validate_plugin_name(plugin)?;
        candidate
            .plugins
            .entry(plugin.to_owned())
            .or_default()
            .set(plugin, scope, schema_version, payload)?;
        validate_supported_snapshot(&candidate)?;
        self.inner = StoredSessionStateSnapshotInner::Supported(candidate);
        Ok(())
    }

    /// Removes one exact plugin scope while preserving every sibling entry.
    ///
    /// # Errors
    /// Returns a typed error for unsupported envelopes, invalid names, or exceeded bounds.
    pub fn remove_plugin_state(
        &mut self,
        plugin: &str,
        scope: StoredStateScope,
    ) -> Result<(), SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        validate_plugin_name(plugin)?;
        let mut candidate = snapshot.clone();
        if candidate
            .plugins
            .get(plugin)
            .is_some_and(|stored_scopes| stored_scopes.scopes.is_none())
        {
            return Err(SessionStateError::InvalidPluginContainer {
                plugin: plugin.to_owned(),
            });
        }
        let remove_plugin = candidate
            .plugins
            .get_mut(plugin)
            .and_then(|stored_scopes| stored_scopes.scopes.as_mut())
            .is_some_and(|scopes| {
                scopes.remove(scope.as_str());
                scopes.is_empty()
            });
        if remove_plugin {
            candidate.plugins.remove(plugin);
        }
        validate_supported_snapshot(&candidate)?;
        self.inner = StoredSessionStateSnapshotInner::Supported(candidate);
        Ok(())
    }

    /// Returns plugin names that contain the exact stored scope, including opaque state versions.
    ///
    /// # Errors
    /// Returns a typed error when the snapshot envelope version is unsupported.
    pub fn plugin_names_with_scope(
        &self,
        scope: StoredStateScope,
    ) -> Result<Vec<&str>, SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        Ok(snapshot
            .plugins
            .iter()
            .filter_map(|(plugin, stored_scopes)| {
                stored_scopes
                    .scopes
                    .as_ref()
                    .is_some_and(|scopes| scopes.contains_key(scope.as_str()))
                    .then_some(plugin.as_str())
            })
            .collect())
    }

    /// Enumerates well-formed entries matching a supported plugin state version.
    ///
    /// Malformed entries, unknown scopes, and other plugin state versions remain preserved but are
    /// omitted from the result.
    ///
    /// # Errors
    /// Returns a typed error when the snapshot envelope version is unsupported.
    pub fn plugin_entries_for_apply(
        &self,
        schema_version: u32,
    ) -> Result<Vec<StoredPluginStateEntry<'_>>, SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        let mut entries = Vec::new();
        for (plugin, stored_scopes) in &snapshot.plugins {
            let Some(scopes) = stored_scopes.scopes.as_ref() else {
                continue;
            };
            for (scope_name, state) in scopes {
                let Some(scope) = StoredStateScope::from_stored(scope_name) else {
                    continue;
                };
                let (Some(found), Some(payload)) = (state.schema_version, state.payload()) else {
                    continue;
                };
                if found == u64::from(schema_version) {
                    entries.push(StoredPluginStateEntry {
                        plugin,
                        scope,
                        payload,
                    });
                }
            }
        }
        Ok(entries)
    }

    /// Validates persisted state before any plugin can apply it.
    ///
    /// These limits bound state accepted by plugins. The session reader separately caps each
    /// decompressed outer record before JSON deserialization.
    ///
    /// # Errors
    /// Returns a typed error for unsupported envelopes or exceeded bounds.
    pub fn validate_for_apply(&self) -> Result<(), SessionStateError> {
        match &self.inner {
            StoredSessionStateSnapshotInner::Supported(snapshot) => {
                validate_supported_snapshot(snapshot)
            }
            StoredSessionStateSnapshotInner::Unsupported { .. }
            | StoredSessionStateSnapshotInner::Malformed { .. } => {
                Err(self.unsupported_schema_error())
            }
        }
    }

    /// Returns a compatible plugin payload without dropping unknown entries.
    ///
    /// # Errors
    /// Returns a typed error when the snapshot, plugin version, or scope cannot be applied.
    pub fn plugin_payload_for_apply(
        &self,
        plugin: &str,
        schema_version: u32,
        scope: StoredStateScope,
    ) -> Result<Option<&serde_json::Value>, SessionStateError> {
        let StoredSessionStateSnapshotInner::Supported(snapshot) = &self.inner else {
            return Err(self.unsupported_schema_error());
        };
        validate_plugin_name(plugin)?;
        let Some(stored_scopes) = snapshot.plugins.get(plugin) else {
            return Ok(None);
        };
        let Some(scopes) = stored_scopes.scopes.as_ref() else {
            return Err(SessionStateError::InvalidPluginContainer {
                plugin: plugin.to_owned(),
            });
        };
        let Some(state) = scopes.get(scope.as_str()) else {
            return Ok(None);
        };
        validate_stored_plugin_state_size(plugin, state)?;
        let payload = state.payload();
        let (Some(found), Some(payload)) = (state.schema_version, payload) else {
            return Err(SessionStateError::InvalidPluginState {
                plugin: plugin.to_owned(),
                scope,
            });
        };
        if found != u64::from(schema_version) {
            return Err(SessionStateError::UnsupportedPluginVersion {
                plugin: plugin.to_owned(),
                found,
                expected: schema_version,
            });
        }
        Ok(Some(payload))
    }

    fn unsupported_schema_error(&self) -> SessionStateError {
        let found = match &self.inner {
            StoredSessionStateSnapshotInner::Supported(snapshot) => {
                u64::from(snapshot.schema_version)
            }
            StoredSessionStateSnapshotInner::Unsupported { schema_version, .. } => *schema_version,
            StoredSessionStateSnapshotInner::Malformed { .. } => {
                return SessionStateError::InvalidEnvelope;
            }
        };
        SessionStateError::UnsupportedSchemaVersion {
            found,
            expected: SESSION_STATE_SCHEMA_VERSION,
        }
    }
}

fn validate_supported_snapshot(
    snapshot: &StoredSessionStateSnapshotV1,
) -> Result<(), SessionStateError> {
    let entry_count = snapshot
        .plugins
        .values()
        .map(|stored_scopes| match &stored_scopes.scopes {
            Some(scopes) => scopes.len(),
            None => 1,
        })
        .sum::<usize>();
    if entry_count > MAX_PLUGIN_STATE_ENTRIES {
        return Err(SessionStateError::TooManyPlugins {
            found: entry_count,
            maximum: MAX_PLUGIN_STATE_ENTRIES,
        });
    }
    for (plugin, stored_scopes) in &snapshot.plugins {
        validate_plugin_name(plugin)?;
        let Some(scopes) = stored_scopes.scopes.as_ref() else {
            let Some(raw) = stored_scopes.malformed.as_ref() else {
                return Err(SessionStateError::InvalidPluginContainer {
                    plugin: plugin.clone(),
                });
            };
            validate_plugin_state_size(plugin, raw)?;
            continue;
        };
        for state in scopes.values() {
            validate_stored_plugin_state_size(plugin, state)?;
        }
    }
    let bytes = serde_json::to_vec(snapshot)?.len();
    if bytes > MAX_SESSION_STATE_BYTES {
        return Err(SessionStateError::SnapshotTooLarge {
            bytes,
            maximum: MAX_SESSION_STATE_BYTES,
        });
    }
    Ok(())
}

fn validate_plugin_name(plugin: &str) -> Result<(), SessionStateError> {
    let valid = !plugin.is_empty()
        && plugin.len() <= MAX_PLUGIN_STATE_NAME_BYTES
        && plugin
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if !valid {
        return Err(SessionStateError::InvalidPluginName {
            plugin: plugin.to_owned(),
        });
    }
    Ok(())
}

fn validate_plugin_state_size(
    plugin: &str,
    value: &serde_json::Value,
) -> Result<(), SessionStateError> {
    validate_plugin_state_bytes(plugin, serde_json::to_vec(value)?.len())
}

fn validate_stored_plugin_state_size(
    plugin: &str,
    state: &StoredPluginState,
) -> Result<(), SessionStateError> {
    let serde_json::Value::Object(fields) = &state.raw else {
        return validate_plugin_state_size(plugin, &state.raw);
    };
    let Some(payload) = fields.get("payload") else {
        return validate_plugin_state_size(plugin, &state.raw);
    };
    let payload_bytes = serde_json::to_vec(payload)?.len();
    let opaque = fields
        .iter()
        .filter(|(name, _)| {
            name.as_str() != "payload"
                && (name.as_str() != "schema_version" || state.schema_version.is_none())
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let opaque_bytes = if opaque.is_empty() {
        0
    } else {
        serde_json::to_vec(&opaque)?.len()
    };
    validate_plugin_state_bytes(plugin, payload_bytes.saturating_add(opaque_bytes))
}

fn validate_plugin_state_bytes(plugin: &str, bytes: usize) -> Result<(), SessionStateError> {
    if bytes > MAX_PLUGIN_STATE_BYTES {
        return Err(SessionStateError::PluginStateTooLarge {
            plugin: plugin.to_owned(),
            bytes,
            maximum: MAX_PLUGIN_STATE_BYTES,
        });
    }
    Ok(())
}

fn deserialize_state_snapshot_option<'de, D>(
    deserializer: D,
) -> Result<Option<StoredSessionStateSnapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let bytes = serde_json::to_vec(&raw)
        .map_err(serde::de::Error::custom)?
        .len();
    if bytes > MAX_SESSION_STATE_BYTES {
        return Err(serde::de::Error::custom(
            SessionStateError::SnapshotTooLarge {
                bytes,
                maximum: MAX_SESSION_STATE_BYTES,
            },
        ));
    }
    match serde_json::from_value(raw.clone()) {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(error) => {
            warn!(
                category = ?error.classify(),
                "quarantining malformed session-state snapshot"
            );
            Ok(Some(StoredSessionStateSnapshot {
                inner: StoredSessionStateSnapshotInner::Malformed { raw },
            }))
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<n00nId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<n00nId>,
    #[serde(default, skip_serializing_if = "StoredSessionLifecycle::is_idle")]
    pub lifecycle: StoredSessionLifecycle,
    #[serde(default)]
    pub mode: Option<StoredMode>,
    #[serde(default)]
    pub plan_path: Option<String>,
    #[serde(default)]
    pub plan_written: bool,
    #[serde(default)]
    pub session_rules: Vec<StoredRule>,
    #[serde(default)]
    pub context_size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_draft: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_messages: Vec<String>,
    /// Full queued-message snapshots, including messages hidden by the paint gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_submissions: Vec<StoredQueuedMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<StoredSubagent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<StoredThinking>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workflow: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub usage_by_model: HashMap<String, StoredTokenUsage>,
    /// Fusion dual-lane cost breakdown when `--fusion` / `always_fusion` was on.
    #[serde(
        default,
        deserialize_with = "deserialize_fusion_usage_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub fusion: Option<StoredFusionUsage>,
    #[serde(
        default,
        deserialize_with = "deserialize_state_snapshot_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub state_snapshot: Option<StoredSessionStateSnapshot>,
    /// Monotonic snapshot ordering used by write-behind persistence.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredFusionUsage {
    #[serde(default)]
    pub lead_cost: f64,
    #[serde(default)]
    pub sidekick_cost: f64,
    #[serde(default)]
    pub lead_usage: StoredTokenUsage,
    #[serde(default)]
    pub sidekick_usage: StoredTokenUsage,
    #[serde(default)]
    pub delegation_count: u32,
    #[serde(default)]
    pub compact_count: u32,
    #[serde(default)]
    pub final_lane: String,
}

/// Parses a JSON `Value` into `StoredFusionUsage`, dropping corrupt/empty records.
///
/// Older session files occasionally wrote `lead_cost` or `sidekick_cost` as objects
/// (e.g., maps from the model) instead of plain `f64`s. We tolerate those so the
/// whole meta record is not discarded.
fn fusion_usage_from_value(value: &serde_json::Value) -> Option<StoredFusionUsage> {
    if value.is_null() {
        return None;
    }
    match serde_json::from_value::<StoredFusionUsage>(value.clone()) {
        Ok(usage) if usage == StoredFusionUsage::default() => None,
        Ok(usage) => Some(usage),
        Err(e) => {
            warn!(
                error = %e,
                value_type = %match value {
                    serde_json::Value::Object(_) => "object",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Null => "null",
                },
                "rejected malformed fusion usage during session restore"
            );
            None
        }
    }
}

fn deserialize_fusion_usage_option<'de, D>(
    deserializer: D,
) -> Result<Option<StoredFusionUsage>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.as_ref().and_then(fusion_usage_from_value))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOpenAiResponseChain {
    pub response_id: String,
    pub message_count: usize,
    pub tools_hash: String,
    pub messages_hash: String,
    pub auth_scope_hash: String,
    pub expires_at: u64,
}

pub struct OpenAiResponseChainLock {
    file: File,
}

impl OpenAiResponseChainLock {
    /// Create another handle to the held lock for a blocking task.
    ///
    /// # Errors
    /// Returns an error if the lock handle cannot be duplicated.
    pub fn try_clone(&self) -> Result<Self, StorageError> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranscriptEntry<M> {
    Message(M),
    GeneratedMessage(M),
    Compaction {
        entries: Vec<TranscriptEntry<M>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generated_summary: Option<M>,
    },
}

#[must_use]
pub fn active_messages_from_transcript<M: Clone>(transcript: &[TranscriptEntry<M>]) -> Vec<M> {
    transcript
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Message(message) | TranscriptEntry::GeneratedMessage(message) => {
                Some(message.clone())
            }
            TranscriptEntry::Compaction { .. } => None,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session<M, U, T> {
    pub version: u32,
    pub id: n00nId,
    pub title: String,
    pub cwd: String,
    pub model: String,
    pub messages: Vec<M>,
    #[serde(default)]
    pub transcript: Vec<TranscriptEntry<M>>,
    #[serde(skip)]
    transcript_revision: Option<u64>,
    pub token_usage: U,
    #[serde(default = "HashMap::new")]
    pub tool_outputs: HashMap<String, T>,
    #[serde(default = "HashMap::new", skip_serializing_if = "HashMap::is_empty")]
    pub subagent_messages: HashMap<String, Vec<M>>,
    #[serde(flatten)]
    pub meta: SessionMeta,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: n00nId,
    pub title: String,
    #[serde(default)]
    pub display_title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<n00nId>,
    pub updated_at: u64,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredMode {
    Build,
    Plan,
    Research,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub effect: StoredEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThinkingParseError {
    #[error(
        "unknown thinking value {0:?} (use off, adaptive, minimal, low, medium, high, xhigh, max, or a token budget)"
    )]
    Unknown(String),
    #[error("thinking budget must be greater than zero")]
    BudgetZero,
}

/// Floor for every token budget sent to a provider; some APIs reject smaller values.
pub const MIN_THINKING_BUDGET: u32 = 1024;

/// Thinking effort level. Declaration order is intensity order: the `Ord`
/// derive and [`Effort::ALL`] rely on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Self; 6] = [
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Percentage of the model's max thinking budget this level spends.
    #[must_use]
    pub const fn percent(self) -> u32 {
        match self {
            Self::Minimal => 10,
            Self::Low => 20,
            Self::Medium => 40,
            Self::High => 60,
            Self::XHigh => 80,
            Self::Max => 100,
        }
    }

    /// `percent` of `max`, clamped to `[MIN_THINKING_BUDGET, max]`.
    /// A `max` below the floor is raised to it.
    #[must_use]
    pub fn budget(self, max: u32) -> u32 {
        let max = max.max(MIN_THINKING_BUDGET);
        let computed = u64::from(max) * u64::from(self.percent()) / 100;
        let tokens = if let Ok(t) = u32::try_from(computed) {
            t
        } else {
            warn!(effort = %self, max, computed, "thinking budget overflow, using max");
            max
        };
        tokens.clamp(MIN_THINKING_BUDGET, max)
    }

    /// Inverse of [`Self::budget`]: the lowest level whose percentage covers
    /// `n` tokens out of `max`. Budgets at or above `max` map to `Max`.
    #[must_use]
    pub fn from_budget(n: u32, max: u32) -> Self {
        let pct = u64::from(n).saturating_mul(100) / u64::from(max.max(1));
        Self::ALL
            .into_iter()
            .find(|e| u64::from(e.percent()) >= pct)
            .unwrap_or_else(|| {
                warn!(n, max, pct, "no thinking level matches budget, using Max");
                Self::Max
            })
    }

    /// Nearest level a provider accepts: exact match keeps `self`, otherwise
    /// the closest lower supported level, otherwise the lowest supported.
    /// An empty `supported` list returns `self` unchanged (dynamic model
    /// listings may not declare supported efforts).
    #[must_use]
    pub fn snap(self, supported: &[Self]) -> Self {
        if supported.is_empty() || supported.contains(&self) {
            return self;
        }
        if let Some(level) = supported.iter().rev().find(|&&e| e < self).copied() {
            level
        } else {
            warn!(effort = %self, supported = ?supported, "no lower thinking level found, using lowest supported");
            supported[0]
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Effort {
    type Err = ThinkingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|e| e.as_str() == s)
            .ok_or_else(|| ThinkingParseError::Unknown(s.to_string()))
    }
}

/// Serializable identifier for a built-in effort dialect, resolved to the
/// actual dialect by `n00n_providers::effort_dialect_for`. Lives here so both
/// `n00n-config` (providers.toml) and `n00n-providers` (dynamic provider
/// script JSON) can deserialize it without a cross-dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffortDialectId {
    Standard,
    OpenaiExtended,
    PreferHigh,
    HighOnly,
    Glm,
    DeepSeek,
    AnthropicAdaptive,
    TensorX,
}

/// One toggle object written to a request body based on the thinking state.
/// `on` is merged for Effort/Budget, `adaptive` for Adaptive (falling back to
/// `on`), `off` is set for Off. `budget_key` nests the resolved budget inside
/// this toggle's object when no explicit budget path is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToggleEntry {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_key: Option<String>,
}

/// Where thinking values go in a request body. When set on a model it
/// overrides the base provider's hardcoded layout. Supports multiple toggle
/// objects, dot-separated nested paths (`reasoning.effort`), budgets nested
/// inside a toggle (Anthropic's `budget_tokens`), and budget caps (Google's
/// family-specific limits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ThinkingFieldConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_max: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toggles: Vec<ToggleEntry>,
}

/// Per-model request body manipulation. Three operations run in order:
/// `defaults` (fills absent keys), `replace` (deep-merges, overwriting), and
/// `filter` (strips keys). Every provider guards its conversation field, so
/// none of the three can touch `messages`, `input`, or `contents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BodyOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredReasoningMode {
    Standard,
    Pro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredReasoningContext {
    Auto,
    CurrentTurn,
    AllTurns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum StoredThinking {
    Off,
    Adaptive,
    Effort {
        level: Effort,
    },
    Budget {
        tokens: u32,
    },
    #[serde(rename = "with_extras")]
    WithExtras {
        level: Effort,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_mode: Option<StoredReasoningMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_context: Option<StoredReasoningContext>,
    },
}

impl StoredThinking {
    /// The one string-to-thinking parser: `/thinking`, `always_thinking`
    /// config, and the Lua agent API all delegate here.
    ///
    /// # Errors
    /// Returns `ThinkingParseError` if the input is not a valid thinking setting.
    pub fn parse_setting(input: &str) -> Result<Self, ThinkingParseError> {
        match input.trim() {
            "off" => Ok(Self::Off),
            "adaptive" => Ok(Self::Adaptive),
            other => {
                if let Ok(level) = other.parse::<Effort>() {
                    return Ok(Self::Effort { level });
                }
                match other.parse::<u32>() {
                    Ok(0) => Err(ThinkingParseError::BudgetZero),
                    Ok(n) => Ok(Self::Budget { tokens: n }),
                    Err(_) => Err(ThinkingParseError::Unknown(other.to_string())),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSubagent {
    pub tool_use_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

pub trait TitleSource {
    fn first_user_text(&self) -> Option<&str>;
}

/// A pasted code block bakes `\n` into a title and skews width-based padding
/// in single-line UI like the picker, so every title entry point calls this.
#[must_use]
pub fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn generate_title<M: TitleSource>(messages: &[M]) -> String {
    let first_user_text = messages.iter().find_map(|m| m.first_user_text());

    let Some(text) = first_user_text.map(str::trim).filter(|t| !t.is_empty()) else {
        return DEFAULT_TITLE.into();
    };
    truncate_label(text)
}

const SUBTASK_TITLE_PREFIXES: &[(&str, &str)] = &[
    ("team:", "team"),
    ("workflow:", "workflow"),
    ("task:", "task"),
];

const SUBTASK_JSON_KEYS: &[&str] = &["goal", "description", "prompt", "name", "title", "input"];

fn truncate_label(text: &str) -> String {
    let text = normalize_title(text);
    if text.len() <= MAX_TITLE_LEN {
        return text;
    }
    let boundary = text.floor_char_boundary(MAX_TITLE_LEN);
    let truncated = &text[..boundary];
    match truncated.rfind(' ') {
        Some(pos) if pos > MAX_TITLE_LEN / 2 => format!("{}…", &truncated[..pos]),
        _ => format!("{truncated}…"),
    }
}

fn strip_tool_directive(text: &str) -> Option<(&'static str, &str)> {
    let text = text.strip_prefix("Use the ")?;
    let end = text.find(" tool now")?;
    let requested_kind = &text[..end];
    let (_, kind) = SUBTASK_TITLE_PREFIXES
        .iter()
        .find(|(_, kind)| *kind == requested_kind)?;
    let after = &text[end + " tool now".len()..];
    if after
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
    {
        return None;
    }
    let body = after.split_once("\n\n").map_or(after, |(_, body)| body);
    Some((kind, body.trim_start()))
}

fn snippet_from_json(value: &serde_json::Value) -> Option<String> {
    fn find_string(value: &serde_json::Value) -> Option<String> {
        value
            .as_str()
            .map(std::string::ToString::to_string)
            .filter(|s| !s.is_empty())
            .map(|s| cap_text(&s, MAX_SNIPPET_BYTES))
    }

    for key in SUBTASK_JSON_KEYS {
        if let Some(v) = value.get(*key) {
            if let Some(s) = find_string(v) {
                return Some(s);
            }
            for key2 in SUBTASK_JSON_KEYS {
                if let Some(nested) = v.get(*key2).and_then(find_string) {
                    return Some(nested);
                }
            }
        }
    }

    value
        .as_object()
        .and_then(|obj| obj.values().find_map(find_string))
}

fn cap_text(text: &str, max_bytes: usize) -> String {
    let cap = text.len().min(max_bytes);
    let boundary = text.floor_char_boundary(cap);
    text[..boundary].to_string()
}

fn snippet_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(snippet) = snippet_from_json(&value)
    {
        return Some(cap_text(&snippet, MAX_SNIPPET_BYTES));
    }
    let first = trimmed
        .split_once('\n')
        .map_or(trimmed, |(line, _)| line)
        .trim();
    if first.is_empty() {
        None
    } else {
        Some(cap_text(first, MAX_SNIPPET_BYTES))
    }
}

fn classify_and_display(title: &str, first_message: Option<&str>) -> (String, String) {
    let title_norm = normalize_title(title);
    let title_norm_lower = title_norm.to_ascii_lowercase();
    let message = first_message.map(str::trim).filter(|t| !t.is_empty());

    for (prefix, kind) in SUBTASK_TITLE_PREFIXES {
        if title_norm_lower.starts_with(prefix) {
            let rest = title_norm[prefix.len()..].trim();
            let display = if rest.is_empty() {
                message
                    .and_then(snippet_from_text)
                    .unwrap_or_else(|| title_norm.clone())
            } else {
                rest.to_string()
            };
            return (truncate_label(&display), kind.to_string());
        }
    }

    if let Some(text) = message
        && let Some((kind, body)) = strip_tool_directive(text)
    {
        let display = snippet_from_text(body).unwrap_or_else(|| body.to_string());
        return (truncate_label(&display), kind.to_string());
    }

    let display = if title_norm == DEFAULT_TITLE || title_norm.starts_with("Use the ") {
        message
            .and_then(snippet_from_text)
            .unwrap_or_else(|| title_norm.clone())
    } else {
        title_norm
    };
    (truncate_label(&display), "main".to_string())
}

// -- JSONL record types --

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
#[allow(clippy::large_enum_variant)]
enum LogRecord<M, U, T> {
    #[serde(rename = "header")]
    Header {
        v: u32,
        id: n00nId,
        #[serde(default)]
        model: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        created_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<n00nId>,
    },
    #[serde(rename = "msg")]
    Msg { d: M },
    #[serde(rename = "out")]
    Out { id: String, d: T },
    #[serde(rename = "sub_msg")]
    SubMsg { sub: String, d: M },
    #[serde(rename = "transcript")]
    Transcript { d: TranscriptEntry<M> },
    #[serde(rename = "meta")]
    Meta {
        title: String,
        token_usage: U,
        updated_at: u64,
        #[serde(default)]
        log_appends: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<Vec<TranscriptEntry<M>>>,
        #[serde(flatten)]
        meta: SessionMeta,
    },
    #[serde(other)]
    Unknown,
}

// -- SessionLog: append-only persistence --

pub struct SessionLog {
    session_id: n00nId,
    dir: PathBuf,
    file: File,
    saved_messages: MessageCursor,
    saved_transcript: MessageCursor,
    saved_tool_ids: HashSet<String>,
    saved_sub_msg_counts: HashMap<String, usize>,
    appended_frames: u64,
    saved_transcript_revision: Option<u64>,
    /// Serialized trailing meta record; lets `append` persist meta-only
    /// changes (title, draft, `updated_at`) instead of dropping them.
    saved_meta: Vec<u8>,
    saved_title: String,
}

struct MessageCursor {
    identities: Vec<Vec<u8>>,
}

impl MessageCursor {
    fn capture<M: Serialize>(messages: &[M]) -> Result<Self, SessionError> {
        let identities = messages
            .iter()
            .map(|message| serde_json::to_vec(message).map_err(StorageError::from))
            .collect::<Result<_, _>>()?;
        Ok(Self { identities })
    }

    fn len(&self) -> usize {
        self.identities.len()
    }

    fn is_prefix_of(&self, current: &Self) -> bool {
        current.identities.starts_with(&self.identities)
    }
}

fn sub_msg_snapshot<M>(map: &HashMap<String, Vec<M>>) -> HashMap<String, usize> {
    map.iter().map(|(k, v)| (k.clone(), v.len())).collect()
}

#[derive(Serialize)]
struct TranscriptRecord<'a, M> {
    t: &'static str,
    d: &'a TranscriptEntry<M>,
}

impl SessionLog {
    /// # Errors
    /// Returns `SessionError` if the session file cannot be created or written.
    pub fn create<M, U, T>(dir: &Path, session: &Session<M, U, T>) -> Result<Self, SessionError>
    where
        M: Serialize + Clone,
        U: Serialize,
        T: Serialize,
    {
        let file = write_session_file(dir, session)?;
        update_cwd_index(dir, &session.cwd, session.id)?;
        Self::cursor_from(dir, session, file, 0)
    }

    /// # Errors
    /// Returns `SessionError` if the session file cannot be found, read, or parsed,
    /// or if the session ID does not match.
    pub fn open<M, U, T>(
        dir: &Path,
        session_id: n00nId,
    ) -> Result<(Session<M, U, T>, Self), SessionError>
    where
        M: Serialize + DeserializeOwned + Clone + Default,
        U: Serialize + DeserializeOwned + Default,
        T: Serialize + DeserializeOwned,
    {
        let path = locate_session_file(dir, session_id)
            .ok_or_else(|| SessionError::from(StorageError::NotFound(session_id.to_string())))?;
        let (session, saw_legacy_transcript, recovered_tail, log_appends) =
            parse_records::<M, U, T>(&path)?;

        if session.id != session_id {
            return Err(SessionError::IdMismatch {
                log_id: session.id,
                given_id: session_id,
            });
        }

        let rewrite = saw_legacy_transcript || recovered_tail;
        let file = if rewrite {
            write_session_file(dir, &session)?
        } else {
            OpenOptions::new()
                .read(true)
                .append(true)
                .open(&path)
                .map_err(StorageError::from)?
        };
        let appended_frames = if rewrite { 0 } else { log_appends };
        let log = Self::cursor_from(dir, &session, file, appended_frames)?;
        update_cwd_index(dir, &session.cwd, session.id)?;
        Ok((session, log))
    }

    #[must_use]
    pub fn session_id(&self) -> n00nId {
        self.session_id
    }

    /// # Errors
    /// Returns `SessionError` if the session ID does not match, the cursor is ahead,
    /// or the append operation fails.
    pub fn append<M, U, T>(&mut self, session: &Session<M, U, T>) -> Result<(), SessionError>
    where
        M: Serialize + Clone,
        U: Serialize,
        T: Serialize,
    {
        self.require_same_id(session)?;

        if session.title != self.saved_title {
            let dir = self.dir.clone();
            return self.compact(&dir, session);
        }

        let current_messages = MessageCursor::capture(&session.messages)?;
        if !self.saved_messages.is_prefix_of(&current_messages) {
            let dir = self.dir.clone();
            return self.compact(&dir, session);
        }

        let current_transcript = self.updated_transcript_cursor(session)?;
        if self.transcript_replaced(current_transcript.as_ref()) {
            let dir = self.dir.clone();
            return self.compact(&dir, session);
        }

        if self.cursor_ahead(session) {
            return Err(SessionError::CursorAhead {
                saved: self.saved_messages.len(),
                actual: session.messages.len(),
            });
        }

        if self.appended_frames >= MAX_INCREMENTAL_FRAMES {
            let dir = self.dir.clone();
            return self.compact(&dir, session);
        }

        let mut buf = Vec::new();
        let mut new_tool_ids = Vec::new();

        for msg in &session.messages[self.saved_messages.len()..] {
            append_record(&mut buf, &LogRecord::<&M, &U, &T>::Msg { d: msg })?;
        }

        for (id, output) in &session.tool_outputs {
            if !self.saved_tool_ids.contains(id) {
                append_record(
                    &mut buf,
                    &LogRecord::<&M, &U, &T>::Out {
                        id: id.clone(),
                        d: output,
                    },
                )?;
                new_tool_ids.push(id.clone());
            }
        }

        let new_sub_counts =
            self.append_subagent_records::<M, U, T>(&mut buf, &session.subagent_messages)?;

        for entry in &session.transcript[self.saved_transcript.len()..] {
            append_record(
                &mut buf,
                &TranscriptRecord {
                    t: TRANSCRIPT_RECORD_TYPE,
                    d: entry,
                },
            )?;
        }

        let current_meta = meta_record_bytes(session, self.appended_frames)?;
        let meta_changed = current_meta != self.saved_meta;
        if buf.is_empty() && !meta_changed {
            return Ok(());
        }

        let next_log_appends = self.appended_frames + 1;
        let persisted_meta = meta_record_bytes(session, next_log_appends)?;
        buf.extend_from_slice(&persisted_meta);

        let start = self.file.metadata().map_err(StorageError::from)?.len();
        if let Err(e) = encode_frame(&mut self.file, &buf) {
            // A failed write can leave a partial zstd frame; roll back to the
            // last complete frame boundary so a retry appends cleanly.
            let _ = self.file.set_len(start);
            return Err(e);
        }
        if let Err(e) = self.file.sync_data().map_err(StorageError::from) {
            let _ = self.file.set_len(start);
            return Err(e.into());
        }

        self.saved_messages = current_messages;
        if let Some(current_transcript) = current_transcript {
            self.saved_transcript = current_transcript;
        }
        self.saved_transcript_revision = session.transcript_revision;
        self.appended_frames = next_log_appends;
        self.saved_tool_ids.extend(new_tool_ids);
        for (sub_id, count) in new_sub_counts {
            self.saved_sub_msg_counts.insert(sub_id, count);
        }
        self.saved_meta = persisted_meta;
        self.saved_title.clone_from(&session.title);

        Ok(())
    }

    fn append_subagent_records<M, U, T>(
        &self,
        buf: &mut Vec<u8>,
        subagent_messages: &HashMap<String, Vec<M>>,
    ) -> Result<Vec<(String, usize)>, SessionError>
    where
        M: Serialize,
        U: Serialize,
        T: Serialize,
    {
        let mut new_sub_counts = Vec::new();
        for (sub_id, msgs) in subagent_messages {
            let saved = self
                .saved_sub_msg_counts
                .get(sub_id)
                .copied()
                .unwrap_or_else(|| 0);
            for msg in &msgs[saved..] {
                append_record(
                    buf,
                    &LogRecord::<&M, &U, &T>::SubMsg {
                        sub: sub_id.clone(),
                        d: msg,
                    },
                )?;
            }
            if msgs.len() > saved {
                new_sub_counts.push((sub_id.clone(), msgs.len()));
            }
        }
        Ok(new_sub_counts)
    }

    /// # Errors
    /// Returns `SessionError` if the session ID does not match or the compact operation fails.
    pub fn compact<M, U, T>(
        &mut self,
        dir: &Path,
        session: &Session<M, U, T>,
    ) -> Result<(), SessionError>
    where
        M: Serialize + Clone,
        U: Serialize,
        T: Serialize,
    {
        self.require_same_id(session)?;

        let file = write_session_file(dir, session)?;
        *self = Self::cursor_from(dir, session, file, 0)?;
        update_cwd_index(dir, &session.cwd, session.id)?;

        Ok(())
    }

    fn transcript_replaced(&self, current: Option<&MessageCursor>) -> bool {
        current.is_some_and(|cursor| !self.saved_transcript.is_prefix_of(cursor))
    }

    fn updated_transcript_cursor<M, U, T>(
        &self,
        session: &Session<M, U, T>,
    ) -> Result<Option<MessageCursor>, SessionError>
    where
        M: Serialize,
    {
        let unchanged = session.transcript_revision.is_some()
            && session.transcript_revision == self.saved_transcript_revision
            && session.transcript.len() == self.saved_transcript.len();
        if unchanged {
            Ok(None)
        } else {
            MessageCursor::capture(&session.transcript).map(Some)
        }
    }

    fn cursor_from<M, U, T>(
        dir: &Path,
        session: &Session<M, U, T>,
        file: File,
        appended_frames: u64,
    ) -> Result<Self, SessionError>
    where
        M: Serialize + Clone,
        U: Serialize,
        T: Serialize,
    {
        Ok(Self {
            session_id: session.id,
            dir: dir.to_path_buf(),
            file,
            saved_messages: MessageCursor::capture(&session.messages)?,
            saved_transcript: MessageCursor::capture(&session.transcript)?,
            saved_tool_ids: session.tool_outputs.keys().cloned().collect(),
            saved_sub_msg_counts: sub_msg_snapshot(&session.subagent_messages),
            appended_frames,
            saved_transcript_revision: session.transcript_revision,
            saved_meta: meta_record_bytes(session, appended_frames)?,
            saved_title: session.title.clone(),
        })
    }

    fn require_same_id<M, U, T>(&self, session: &Session<M, U, T>) -> Result<(), SessionError> {
        if session.id != self.session_id {
            return Err(SessionError::IdMismatch {
                log_id: self.session_id,
                given_id: session.id,
            });
        }
        Ok(())
    }

    fn cursor_ahead<M, U, T>(&self, session: &Session<M, U, T>) -> bool {
        self.saved_messages.len() > session.messages.len()
            || self
                .saved_tool_ids
                .iter()
                .any(|id| !session.tool_outputs.contains_key(id))
            || self.saved_sub_msg_counts.iter().any(|(sub, &count)| {
                session
                    .subagent_messages
                    .get(sub)
                    .is_none_or(|msgs| count > msgs.len())
            })
    }
}

fn meta_record_bytes<M, U, T>(
    session: &Session<M, U, T>,
    log_appends: u64,
) -> Result<Vec<u8>, SessionError>
where
    M: Serialize + Clone,
    U: Serialize,
    T: Serialize,
{
    let mut buf = Vec::new();
    append_record(
        &mut buf,
        &LogRecord::<M, &U, &T>::Meta {
            title: session.title.clone(),
            token_usage: &session.token_usage,
            updated_at: session.updated_at,
            log_appends,
            transcript: None,
            meta: session.meta.clone(),
        },
    )?;
    Ok(buf)
}

fn write_session_file<M, U, T>(dir: &Path, session: &Session<M, U, T>) -> Result<File, SessionError>
where
    M: Serialize + Clone,
    U: Serialize,
    T: Serialize,
{
    fs::create_dir_all(dir).map_err(StorageError::from)?;
    let path = jsonl_path(dir, session.id);
    atomic_write_streaming(&path, |file| write_full_session(file, session))?;
    let file = OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(StorageError::from)?;
    Ok(file)
}

fn write_full_session<M, U, T, W: Write>(
    file: &mut W,
    session: &Session<M, U, T>,
) -> Result<(), SessionError>
where
    M: Serialize + Clone,
    U: Serialize,
    T: Serialize,
{
    let mut encoder = Encoder::new(file, COMPRESS_LEVEL).map_err(StorageError::from)?;
    append_record(
        &mut encoder,
        &LogRecord::<&M, &U, &T>::Header {
            v: LOG_FORMAT_VERSION,
            id: session.id,
            model: session.model.clone(),
            cwd: session.cwd.clone(),
            title: Some(session.title.clone()),
            created_at: session.created_at,
            parent_id: session.meta.parent_id,
        },
    )?;
    for msg in &session.messages {
        append_record(&mut encoder, &LogRecord::<&M, &U, &T>::Msg { d: msg })?;
    }
    for (id, output) in &session.tool_outputs {
        append_record(
            &mut encoder,
            &LogRecord::<&M, &U, &T>::Out {
                id: id.clone(),
                d: output,
            },
        )?;
    }
    for (sub_id, msgs) in &session.subagent_messages {
        for msg in msgs {
            append_record(
                &mut encoder,
                &LogRecord::<&M, &U, &T>::SubMsg {
                    sub: sub_id.clone(),
                    d: msg,
                },
            )?;
        }
    }
    for entry in &session.transcript {
        append_record(
            &mut encoder,
            &TranscriptRecord {
                t: TRANSCRIPT_RECORD_TYPE,
                d: entry,
            },
        )?;
    }
    append_record(
        &mut encoder,
        &LogRecord::<M, &U, &T>::Meta {
            title: session.title.clone(),
            token_usage: &session.token_usage,
            updated_at: session.updated_at,
            log_appends: 0,
            transcript: None,
            meta: session.meta.clone(),
        },
    )?;
    encoder.finish().map_err(StorageError::from)?;
    Ok(())
}

struct BoundedRecordBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedRecordBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedRecordBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(IoError::other("session record exceeds size limit"));
        };
        if next_len > self.limit {
            self.exceeded = true;
            return Err(IoError::other("session record exceeds size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn append_record<W: Write, R: Serialize>(writer: &mut W, record: &R) -> Result<(), SessionError> {
    let mut encoded = BoundedRecordBuffer::new(MAX_SESSION_RECORD_BYTES.saturating_sub(1));
    if let Err(error) = serde_json::to_writer(&mut encoded, record) {
        if encoded.exceeded {
            return Err(SessionError::RecordTooLargeWrite {
                maximum: MAX_SESSION_RECORD_BYTES,
            });
        }
        return Err(StorageError::from(error).into());
    }
    writer
        .write_all(&encoded.bytes)
        .map_err(StorageError::from)?;
    writer.write_all(b"\n").map_err(StorageError::from)?;
    Ok(())
}

/// Tag-only probe used to classify a line that failed the strict `LogRecord`
/// parse: distinguishes a header with a bad id from a genuinely unknown record.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum RawTag {
    Header {
        id: String,
    },
    #[serde(other)]
    Other,
}

struct SessionBuilder<M, U, T> {
    id: Option<n00nId>,
    model: String,
    cwd: String,
    created_at: u64,
    messages: Vec<M>,
    tool_outputs: HashMap<String, T>,
    subagent_messages: HashMap<String, Vec<M>>,
    title: String,
    token_usage: U,
    updated_at: u64,
    transcript: Vec<TranscriptEntry<M>>,
    meta: SessionMeta,
    log_appends: u64,
    saw_legacy_transcript: bool,
}

impl<M, U, T> Default for SessionBuilder<M, U, T>
where
    U: Default,
{
    fn default() -> Self {
        Self {
            id: None,
            model: String::new(),
            cwd: String::new(),
            created_at: 0,
            messages: Vec::new(),
            tool_outputs: HashMap::new(),
            subagent_messages: HashMap::new(),
            title: String::new(),
            token_usage: U::default(),
            updated_at: 0,
            transcript: Vec::new(),
            meta: SessionMeta::default(),
            log_appends: 0,
            saw_legacy_transcript: false,
        }
    }
}

fn parse_records<M, U, T>(path: &Path) -> Result<(Session<M, U, T>, bool, bool, u64), SessionError>
where
    M: DeserializeOwned + Default + Clone,
    U: DeserializeOwned + Default,
    T: DeserializeOwned,
{
    let mut line_count = 0usize;
    let mut builder = SessionBuilder {
        title: DEFAULT_TITLE.to_string(),
        ..Default::default()
    };
    let mut got_header = false;

    let recovered_tail = visit_zstd_lines_with_limits(path, DecodeLimits::LOAD, |line| {
        line_count += 1;
        if line.is_empty() {
            return Ok(());
        }
        let record: LogRecord<M, U, T> = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) => {
                if !got_header
                    && let Ok(RawTag::Header { id: raw_id }) = serde_json::from_str::<RawTag>(line)
                    && let Err(source) = raw_id.parse::<n00nId>()
                {
                    return Err(SessionError::CorruptHeaderId {
                        path: path.display().to_string(),
                        raw_id,
                        source,
                    });
                }
                let tag = match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(v) => v.get("t").and_then(|t| t.as_str()).map(String::from),
                    Err(tag_error) => {
                        warn!(
                            path = %path.display(),
                            tag_error = %tag_error,
                            line = line_count,
                            "failed to extract record tag from malformed JSONL line"
                        );
                        None
                    }
                };
                let record_tag = tag.as_deref().map_or("?", |t| t);
                warn!(
                    path = %path.display(),
                    error = %error,
                    line = line_count,
                    record_tag = %record_tag,
                    record_len = line.len(),
                    "skipping unrecognized JSONL record",
                );
                return Ok(());
            }
        };
        apply_record(&mut builder, record, &mut got_header)
    })?;

    let id = builder
        .id
        .ok_or(StorageError::NotFound(path.display().to_string()))?;
    let saw_legacy_transcript = builder.saw_legacy_transcript;
    let log_appends = builder.log_appends;
    let mut session = Session {
        version: SESSION_VERSION,
        id,
        title: normalize_title(&builder.title),
        cwd: builder.cwd,
        model: builder.model,
        messages: builder.messages,
        transcript: builder.transcript,
        transcript_revision: None,
        token_usage: builder.token_usage,
        tool_outputs: builder.tool_outputs,
        subagent_messages: builder.subagent_messages,
        meta: builder.meta,
        created_at: builder.created_at,
        updated_at: builder.updated_at,
    };
    let transcript_only = session.messages.is_empty() && !session.transcript.is_empty();
    let hydrated_messages = if transcript_only {
        session.messages = active_messages_from_transcript(&session.transcript);
        if session.messages.is_empty() {
            warn!(
                session_id = %session.id,
                "session transcript has no recoverable active provider messages"
            );
            false
        } else {
            warn!(
                session_id = %session.id,
                recovered_messages = session.messages.len(),
                "recovered active provider messages from session transcript"
            );
            true
        }
    } else {
        false
    };
    Ok((
        session,
        saw_legacy_transcript || hydrated_messages,
        recovered_tail,
        log_appends,
    ))
}

fn apply_record<M, U, T>(
    builder: &mut SessionBuilder<M, U, T>,
    record: LogRecord<M, U, T>,
    got_header: &mut bool,
) -> Result<(), SessionError>
where
    M: DeserializeOwned + Default,
    U: DeserializeOwned + Default,
    T: DeserializeOwned,
{
    match record {
        LogRecord::Header {
            v,
            id: h_id,
            model: h_model,
            cwd: h_cwd,
            title: h_title,
            created_at: h_created,
            parent_id: h_parent,
        } => {
            if v != LOG_FORMAT_VERSION {
                return Err(SessionError::VersionMismatch {
                    found: v,
                    expected: LOG_FORMAT_VERSION,
                });
            }
            builder.id = Some(h_id);
            builder.model = h_model;
            builder.cwd = h_cwd;
            builder.created_at = h_created;
            builder.meta.parent_id = h_parent;
            if let Some(t) = h_title {
                builder.title = t;
            }
            *got_header = true;
        }
        LogRecord::Msg { d } => builder.messages.push(d),
        LogRecord::Out { id: out_id, d } => {
            builder.tool_outputs.insert(out_id, d);
        }
        LogRecord::SubMsg { sub, d } => {
            builder.subagent_messages.entry(sub).or_default().push(d);
        }
        LogRecord::Transcript { d } => builder.transcript.push(d),
        LogRecord::Meta {
            title: m_title,
            token_usage: m_usage,
            updated_at: m_updated,
            log_appends: m_log_appends,
            transcript: m_transcript,
            meta: m_meta,
        } => {
            builder.title = m_title;
            builder.token_usage = m_usage;
            builder.updated_at = m_updated;
            builder.log_appends = m_log_appends;
            if let Some(m_transcript) = m_transcript {
                builder.transcript = m_transcript;
                builder.saw_legacy_transcript = true;
            }
            builder.meta = m_meta;
        }
        LogRecord::Unknown => return Err(SessionError::UnknownRecord),
    }
    Ok(())
}

fn encode_frame<W: Write>(file: &mut W, bytes: &[u8]) -> Result<(), SessionError> {
    let mut enc = Encoder::new(file, COMPRESS_LEVEL).map_err(StorageError::from)?;
    enc.write_all(bytes).map_err(StorageError::from)?;
    enc.finish().map_err(StorageError::from)?;
    Ok(())
}

fn is_zst_data(data: &[u8]) -> bool {
    data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
}

#[derive(Clone, Copy)]
struct DecodeLimits {
    line_bytes: usize,
    decoded_bytes: usize,
    window_log: u32,
}

impl DecodeLimits {
    const LOAD: Self = Self::new(
        MAX_SESSION_RECORD_BYTES,
        MAX_SESSION_DECODED_BYTES,
        MAX_ZSTD_WINDOW_LOG,
    );
    const SCAN: Self = Self::new(
        MAX_SCAN_RECORD_BYTES,
        MAX_SCAN_DECODED_BYTES,
        MAX_ZSTD_WINDOW_LOG,
    );

    const fn new(max_line_bytes: usize, max_decoded_bytes: usize, max_window_log: u32) -> Self {
        Self {
            line_bytes: max_line_bytes,
            decoded_bytes: max_decoded_bytes,
            window_log: max_window_log,
        }
    }
}

enum DecodedLine {
    Eof,
    Line(String),
    Oversized,
}

enum LineReadError {
    Io(IoError),
    DecoderWindowLimitExceeded,
    RecordTooLarge,
    BudgetExceeded,
}

struct BoundedZstdLines {
    reader: BufReader<Decoder<'static, BufReader<File>>>,
    path: String,
    limits: DecodeLimits,
    decoded_bytes: usize,
}

impl BoundedZstdLines {
    fn open(path: &Path, offset: u64, limits: DecodeLimits) -> Result<Self, SessionError> {
        let mut file = File::open(path).map_err(StorageError::from)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(StorageError::from)?;
        let mut decoder = Decoder::new(file).map_err(StorageError::from)?;
        decoder
            .window_log_max(limits.window_log)
            .map_err(StorageError::from)?;
        Ok(Self {
            reader: BufReader::new(decoder),
            path: path.display().to_string(),
            limits,
            decoded_bytes: 0,
        })
    }

    fn next(&mut self, drain_oversized: bool) -> Result<DecodedLine, LineReadError> {
        let initial_capacity = self.limits.line_bytes.min(8 * 1024);
        let mut line = Vec::with_capacity(initial_capacity);
        let mut oversized = false;
        loop {
            let available = self.reader.fill_buf().map_err(classify_decoder_error)?;
            if available.is_empty() {
                return if line.is_empty() && !oversized {
                    Ok(DecodedLine::Eof)
                } else if oversized {
                    Ok(DecodedLine::Oversized)
                } else {
                    String::from_utf8(line)
                        .map(DecodedLine::Line)
                        .map_err(|error| {
                            LineReadError::Io(IoError::new(ErrorKind::InvalidData, error))
                        })
                };
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |position| position + 1);
            let next_decoded = self
                .decoded_bytes
                .checked_add(consumed)
                .filter(|total| *total <= self.limits.decoded_bytes)
                .ok_or(LineReadError::BudgetExceeded)?;
            let content_len = match newline {
                Some(position) => position,
                None => consumed,
            };
            if !oversized {
                let remaining = self.limits.line_bytes.saturating_sub(line.len());
                if content_len <= remaining {
                    line.extend_from_slice(&available[..content_len]);
                } else {
                    oversized = true;
                    if !drain_oversized {
                        return Err(LineReadError::RecordTooLarge);
                    }
                }
            }
            self.reader.consume(consumed);
            self.decoded_bytes = next_decoded;

            if newline.is_some() {
                if oversized {
                    return Ok(DecodedLine::Oversized);
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return String::from_utf8(line)
                    .map(DecodedLine::Line)
                    .map_err(|error| {
                        LineReadError::Io(IoError::new(ErrorKind::InvalidData, error))
                    });
            }
        }
    }

    fn limit_error(&self, error: LineReadError) -> SessionError {
        match error {
            LineReadError::Io(error) => SessionError::Storage(StorageError::from(error)),
            LineReadError::DecoderWindowLimitExceeded => SessionError::DecoderWindowLimitExceeded {
                path: self.path.clone(),
                window_log: self.limits.window_log,
            },
            LineReadError::RecordTooLarge => SessionError::RecordTooLarge {
                path: self.path.clone(),
                limit: self.limits.line_bytes,
            },
            LineReadError::BudgetExceeded => SessionError::DecodedBudgetExceeded {
                path: self.path.clone(),
                limit: self.limits.decoded_bytes,
            },
        }
    }
}

fn classify_decoder_error(error: IoError) -> LineReadError {
    let window_too_large_code = 0usize.wrapping_sub(ZSTD_WINDOW_TOO_LARGE_ERROR_CODE);
    let window_too_large = zstd::zstd_safe::get_error_name(window_too_large_code);
    if error.kind() == ErrorKind::Other && error.to_string() == window_too_large {
        LineReadError::DecoderWindowLimitExceeded
    } else {
        LineReadError::Io(error)
    }
}

fn visit_zstd_lines_with_limits(
    path: &Path,
    limits: DecodeLimits,
    mut visit: impl FnMut(&str) -> Result<(), SessionError>,
) -> Result<bool, SessionError> {
    let mut reader = BoundedZstdLines::open(path, 0, limits)?;
    loop {
        match reader.next(false) {
            Ok(DecodedLine::Eof) => return Ok(false),
            Ok(DecodedLine::Line(line)) => visit(&line)?,
            Ok(DecodedLine::Oversized) => {
                return Err(reader.limit_error(LineReadError::RecordTooLarge));
            }
            Err(LineReadError::Io(error)) => {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "recovering records before corrupt zstd tail",
                );
                return Ok(true);
            }
            Err(error) => return Err(reader.limit_error(error)),
        }
    }
}

const ZSTD_MAGIC: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd];
const LAST_FRAME_SEARCH_CHUNK: usize = 1024 * 1024;
const MAX_LAST_FRAME_SEARCH_BYTES: u64 = 64 * 1024 * 1024;

struct DecodedWorkBudget {
    remaining: usize,
}

impl DecodedWorkBudget {
    const fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn finish_attempt(&mut self, decoded_bytes: usize, allowance: usize, exhausted: bool) {
        let spent = if exhausted {
            allowance
        } else {
            decoded_bytes.min(allowance)
        };
        self.remaining = self.remaining.saturating_sub(spent);
    }
}

fn try_decode_header_at(path: &Path, offset: u64) -> Option<ZstHeader> {
    let mut reader = BoundedZstdLines::open(path, offset, DecodeLimits::SCAN).ok()?;
    loop {
        match reader.next(false) {
            Ok(DecodedLine::Line(line)) if line.is_empty() => {}
            Ok(DecodedLine::Line(line)) => return serde_json::from_str(&line).ok(),
            Ok(DecodedLine::Eof | DecodedLine::Oversized) | Err(_) => return None,
        }
    }
}

fn try_decode_last_meta_at<M>(path: &Path, offset: u64) -> Option<(String, u64, Option<String>)>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let mut budget = DecodedWorkBudget::new(DecodeLimits::SCAN.decoded_bytes);
    try_decode_last_meta_at_with_budget::<M>(path, offset, DecodeLimits::SCAN, &mut budget)
}

fn try_decode_last_meta_at_with_budget<M>(
    path: &Path,
    offset: u64,
    limits: DecodeLimits,
    budget: &mut DecodedWorkBudget,
) -> Option<(String, u64, Option<String>)>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let mut reader = BoundedZstdLines::open(path, offset, limits).ok()?;
    let mut title = String::new();
    let mut updated_at = 0u64;
    let mut first_message = None;
    loop {
        match reader.next(true) {
            Ok(DecodedLine::Eof | DecodedLine::Oversized) | Err(LineReadError::BudgetExceeded) => {
                break;
            }
            Ok(DecodedLine::Line(line)) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.starts_with(META_RECORD_PREFIX)
                    && let Ok(MetaScan {
                        title: t,
                        updated_at: u,
                    }) = serde_json::from_str(trimmed)
                {
                    title = t;
                    updated_at = u;
                }
                if offset == 0
                    && first_message.is_none()
                    && trimmed.len() <= MAX_FIRST_MESSAGE_LINE_BYTES
                    && trimmed.starts_with(MSG_RECORD_PREFIX)
                    && let Ok(LogRecord::<M, serde_json::Value, serde_json::Value>::Msg { d }) =
                        serde_json::from_str(trimmed)
                    && let Some(text) = d.first_user_text().map(str::trim).filter(|t| !t.is_empty())
                {
                    first_message = Some(cap_text(text, MAX_FIRST_MESSAGE_TEXT_BYTES));
                }
            }
            Err(_) => return None,
        }
    }
    budget.finish_attempt(reader.decoded_bytes, limits.decoded_bytes, false);
    if updated_at == 0 && title.is_empty() {
        return None;
    }
    Some((title, updated_at, first_message))
}

// -- CWD index --

fn load_cwd_index(dir: &Path) -> HashMap<String, String> {
    fs::read(dir.join(CWD_INDEX_FILE))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_else(HashMap::new)
}

fn update_cwd_index(dir: &Path, cwd: &str, session_id: n00nId) -> Result<(), StorageError> {
    let mut index = load_cwd_index(dir);
    let id_str = session_id.to_string();
    if index.get(cwd).is_some_and(|v| *v == id_str) {
        return Ok(());
    }
    index.insert(cwd.to_string(), id_str);
    atomic_write(&dir.join(CWD_INDEX_FILE), &serde_json::to_vec(&index)?)
}

fn jsonl_path(dir: &Path, id: n00nId) -> PathBuf {
    dir.join(format!("{id}.jsonl"))
}

fn openai_response_chain_path(dir: &Path, id: n00nId) -> PathBuf {
    dir.join(format!("{id}.{OPENAI_RESPONSE_CHAIN_SUFFIX}"))
}

fn openai_response_chain_lock_path(dir: &Path, id: n00nId) -> PathBuf {
    dir.join(format!("{id}.{OPENAI_RESPONSE_CHAIN_LOCK_SUFFIX}"))
}

/// Acquire the cross-process lock for one session's `OpenAI` continuation state.
///
/// # Errors
/// Returns an error when the sessions directory or lock file cannot be opened or locked.
pub fn lock_openai_response_chain(
    state_dir: &StateDir,
    session_id: n00nId,
) -> Result<OpenAiResponseChainLock, StorageError> {
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    lock_openai_response_chain_in(&sessions_dir, session_id)
}

fn lock_openai_response_chain_in(
    sessions_dir: &Path,
    session_id: n00nId,
) -> Result<OpenAiResponseChainLock, StorageError> {
    let file = open_openai_response_chain_lock(sessions_dir, session_id)?;
    file.lock()?;
    Ok(OpenAiResponseChainLock { file })
}

/// Try to acquire the cross-process lock for one session's `OpenAI` continuation state.
///
/// A contended lock returns `Ok(None)` immediately so callers can apply a bounded retry policy
/// without blocking an executor thread.
///
/// # Errors
/// Returns an error when the sessions directory or lock file cannot be opened or locked.
pub fn try_lock_openai_response_chain(
    state_dir: &StateDir,
    session_id: n00nId,
) -> Result<Option<OpenAiResponseChainLock>, StorageError> {
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    let file = open_openai_response_chain_lock(&sessions_dir, session_id)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(OpenAiResponseChainLock { file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn open_openai_response_chain_lock(
    sessions_dir: &Path,
    session_id: n00nId,
) -> Result<File, StorageError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options
        .mode(OPENAI_RESPONSE_CHAIN_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW);
    let file = options.open(openai_response_chain_lock_path(sessions_dir, session_id))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "OpenAI response-chain lock is not a regular file",
        )
        .into());
    }
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(OPENAI_RESPONSE_CHAIN_FILE_MODE))?;
    Ok(file)
}

/// Return whether the parent session log still exists.
///
/// # Errors
/// Returns an error when the sessions directory cannot be opened.
pub fn openai_response_chain_parent_exists(
    state_dir: &StateDir,
    session_id: n00nId,
) -> Result<bool, StorageError> {
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    Ok(locate_session_file(&sessions_dir, session_id).is_some())
}

/// Load the durable `OpenAI` Responses continuation state for a session.
///
/// Expired state is removed instead of being returned to the provider.
///
/// # Errors
/// Returns an error when the sessions directory cannot be opened, the state is invalid,
/// or an expired state file cannot be removed.
pub fn load_openai_response_chain(
    state_dir: &StateDir,
    session_id: n00nId,
    lock: &OpenAiResponseChainLock,
) -> Result<Option<StoredOpenAiResponseChain>, StorageError> {
    load_openai_response_chain_at(state_dir, session_id, now_epoch(), lock)
}

fn load_openai_response_chain_at(
    state_dir: &StateDir,
    session_id: n00nId,
    now: u64,
    lock: &OpenAiResponseChainLock,
) -> Result<Option<StoredOpenAiResponseChain>, StorageError> {
    let _ = lock;
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    let path = openai_response_chain_path(&sessions_dir, session_id);
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let chain: StoredOpenAiResponseChain = serde_json::from_slice(&data)?;
    if chain.expires_at <= now {
        try_remove(&path)?;
        return Ok(None);
    }
    Ok(Some(chain))
}

/// Persist the `OpenAI` Responses continuation state for a session atomically.
///
/// # Errors
/// Returns an error when the sessions directory cannot be created or the state cannot be written.
pub fn save_openai_response_chain(
    state_dir: &StateDir,
    session_id: n00nId,
    chain: &StoredOpenAiResponseChain,
    lock: &OpenAiResponseChainLock,
) -> Result<(), StorageError> {
    let _ = lock;
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    let path = openai_response_chain_path(&sessions_dir, session_id);
    atomic_write_permissions(
        &path,
        &serde_json::to_vec(chain)?,
        OPENAI_RESPONSE_CHAIN_FILE_MODE,
    )?;
    if locate_session_file(&sessions_dir, session_id).is_none() {
        try_remove(&path)?;
        return Err(StorageError::NotFound(session_id.to_string()));
    }
    Ok(())
}

/// Remove any `OpenAI` Responses continuation state for a session.
///
/// # Errors
/// Returns an error when the sessions directory cannot be opened or the file cannot be removed.
pub fn delete_openai_response_chain(
    state_dir: &StateDir,
    session_id: n00nId,
    lock: &OpenAiResponseChainLock,
) -> Result<(), StorageError> {
    let _ = lock;
    let sessions_dir = state_dir.ensure_subdir(SESSIONS_DIR)?;
    try_remove(&openai_response_chain_path(&sessions_dir, session_id))?;
    Ok(())
}

fn file_is_zst(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && is_zst_data(&magic)
}

fn try_remove(path: &Path) -> Result<bool, StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn remove_from_cwd_index(dir: &Path, session_id: n00nId) -> Result<(), StorageError> {
    let mut index = load_cwd_index(dir);
    let before = index.len();
    let session_id = session_id.to_string();
    index.retain(|_, value| value != &session_id);
    if index.len() != before {
        atomic_write(&dir.join(CWD_INDEX_FILE), &serde_json::to_vec(&index)?)?;
    }
    Ok(())
}

// -- Header scanning for session list --

#[derive(Deserialize)]
struct ZstHeader {
    v: u32,
    id: n00nId,
    #[serde(default)]
    model: String,
    cwd: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    parent_id: Option<n00nId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct MetaScan {
    title: String,
    updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScannedHeader {
    id: n00nId,
    cwd: String,
    title: String,
    updated_at: u64,
    #[serde(default)]
    model: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_message: Option<String>,
    #[serde(default)]
    parent_id: Option<n00nId>,
}

/// Cached scan result for one session file, keyed by file name and validated
/// by (size, mtime): stale entries are rescanned, deleted files pruned.
/// `header: None` marks files that failed to scan (wrong version, foreign
/// format), so they are not re-read on every list either.
#[derive(Serialize, Deserialize)]
struct ScanCacheEntry {
    size: u64,
    mtime_ms: u64,
    header: Option<ScannedHeader>,
}

type ScanCache = HashMap<String, ScanCacheEntry>;

fn load_scan_cache(dir: &Path) -> ScanCache {
    fs::read(dir.join(SCAN_CACHE_FILE))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_else(HashMap::new)
}

fn file_signature(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_millis()).ok())?;
    Some((meta.len(), mtime_ms))
}

fn scan_headers<M>(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, StorageError>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let mut cache = load_scan_cache(dir);
    let mut fresh = ScanCache::new();
    let mut dirty = false;
    let mut with_created: Vec<(u64, SessionSummary)> = Vec::new();
    for path in session_entries(dir)? {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((size, mtime_ms)) = file_signature(&path) else {
            continue;
        };
        let entry = match cache.remove(name) {
            Some(e) if e.size == size && e.mtime_ms == mtime_ms => e,
            _ => {
                dirty = true;
                let header = scan_zst_header::<M>(&path);
                ScanCacheEntry {
                    size,
                    mtime_ms,
                    header,
                }
            }
        };
        if let Some(h) = &entry.header
            && h.cwd == cwd
        {
            let (display_title, kind) = classify_and_display(&h.title, h.first_message.as_deref());
            let created_at = if h.created_at != 0 {
                h.created_at
            } else {
                h.updated_at
            };
            with_created.push((
                created_at,
                SessionSummary {
                    id: h.id,
                    title: normalize_title(&h.title),
                    display_title,
                    kind,
                    parent_id: h.parent_id,
                    updated_at: h.updated_at,
                    cwd: h.cwd.clone(),
                    model: h.model.clone(),
                },
            ));
        }
        fresh.insert(name.to_owned(), entry);
    }
    // Leftover cache entries belong to deleted files; rewriting prunes them.
    if (dirty || !cache.is_empty())
        && let Ok(data) = serde_json::to_vec(&fresh)
        && let Err(e) = atomic_write(&dir.join(SCAN_CACHE_FILE), &data)
    {
        warn!(error = %e, "failed to write session scan cache");
    }
    if let Err(error) = fs::remove_file(dir.join(SCAN_CACHE_FILE_V2))
        && error.kind() != ErrorKind::NotFound
    {
        warn!(error = %error, "failed to remove stale v2 session scan cache");
    }

    let mut order: Vec<usize> = (0..with_created.len()).collect();
    order.sort_unstable_by_key(|i| (with_created[*i].0, *with_created[*i].1.id.as_bytes()));
    let mut last_main: Option<n00nId> = None;
    for i in order {
        let summary = &mut with_created[i].1;
        if summary.kind == "main" {
            last_main = Some(summary.id);
        } else if summary.parent_id.is_none()
            && let Some(parent) = last_main
        {
            summary.parent_id = Some(parent);
        }
    }

    Ok(with_created.into_iter().map(|(_, s)| s).collect())
}

fn try_decode_first_message_at<M>(path: &Path, offset: u64) -> Option<String>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let file = File::open(path).ok()?;
    let mut file = file;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let decoder = Decoder::new(file).ok()?;
    let limited = decoder.take(MAX_FIRST_MESSAGE_BYTES as u64);
    let mut reader = BufReader::new(limited);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty()
            || !trimmed.starts_with(MSG_RECORD_PREFIX)
            || trimmed.len() > MAX_FIRST_MESSAGE_LINE_BYTES
        {
            continue;
        }
        if let Ok(LogRecord::<M, serde_json::Value, serde_json::Value>::Msg { d }) =
            serde_json::from_str(trimmed)
            && let Some(text) = d.first_user_text().map(str::trim).filter(|t| !t.is_empty())
        {
            return Some(cap_text(text, MAX_FIRST_MESSAGE_TEXT_BYTES));
        }
    }
}

fn find_last_frame_meta<M>(path: &Path) -> Option<(String, u64, Option<String>)>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let file_len = fs::metadata(path).ok()?.len();
    if file_len < ZSTD_MAGIC.len() as u64 {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut end = file_len;
    let mut start = file_len.saturating_sub(LAST_FRAME_SEARCH_CHUNK as u64);
    let mut searched = 0u64;
    let mut buf = Vec::new();
    loop {
        file.seek(SeekFrom::Start(start)).ok()?;
        let chunk_len = usize::try_from(end - start).ok()?;
        searched += end - start;
        buf.resize(chunk_len, 0);
        file.read_exact(&mut buf).ok()?;

        let mut positions: Vec<usize> = buf
            .windows(ZSTD_MAGIC.len())
            .enumerate()
            .filter(|(_, w)| *w == ZSTD_MAGIC)
            .map(|(i, _)| i)
            .collect();
        positions.sort_unstable();

        for &pos in positions.iter().rev() {
            let offset = start + pos as u64;
            if let Some(meta) = try_decode_last_meta_at::<M>(path, offset) {
                return Some(meta);
            }
        }

        if searched >= MAX_LAST_FRAME_SEARCH_BYTES {
            return try_decode_last_meta_at::<M>(path, 0);
        }

        if start == 0 {
            break;
        }
        end = start + (ZSTD_MAGIC.len() as u64 - 1);
        start = end.saturating_sub(LAST_FRAME_SEARCH_CHUNK as u64);
    }
    None
}

fn scan_zst_header<M>(path: &Path) -> Option<ScannedHeader>
where
    M: TitleSource + DeserializeOwned + Default,
{
    let header = try_decode_header_at(path, 0)?;
    if header.v != LOG_FORMAT_VERSION {
        return None;
    }

    let (meta_title, updated_at, first_message) =
        find_last_frame_meta::<M>(path).unwrap_or_else(|| {
            let mut title = String::new();
            let mut updated_at = 0u64;
            let mut first_message = None;
            let _ = visit_zstd_lines_with_limits(path, DecodeLimits::SCAN, |line| {
                if !line.is_empty() {
                    if line.starts_with(META_RECORD_PREFIX)
                        && let Ok(MetaScan {
                            title: t,
                            updated_at: u,
                        }) = serde_json::from_str(line)
                    {
                        title = t;
                        updated_at = u;
                    }
                    if first_message.is_none()
                        && line.starts_with(MSG_RECORD_PREFIX)
                        && line.len() <= MAX_FIRST_MESSAGE_LINE_BYTES
                        && let Ok(LogRecord::<M, serde_json::Value, serde_json::Value>::Msg { d }) =
                            serde_json::from_str(line)
                        && let Some(text) =
                            d.first_user_text().map(str::trim).filter(|t| !t.is_empty())
                    {
                        first_message = Some(cap_text(text, MAX_FIRST_MESSAGE_TEXT_BYTES));
                    }
                }
                Ok(())
            });
            (title, updated_at, first_message)
        });

    let first_message = first_message.or_else(|| try_decode_first_message_at::<M>(path, 0));

    Some(ScannedHeader {
        id: header.id,
        cwd: header.cwd,
        title: if meta_title.is_empty() {
            match header.title {
                Some(t) => t,
                None => DEFAULT_TITLE.to_string(),
            }
        } else {
            meta_title
        },
        updated_at,
        model: header.model,
        created_at: header.created_at,
        first_message,
        parent_id: header.parent_id,
    })
}

fn session_entries(dir: &Path) -> Result<Vec<PathBuf>, StorageError> {
    Ok(fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| is_session_file(p))
        .collect())
}

fn is_session_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<n00nId>().ok())
            .is_some_and(|id| {
                let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
                match parent {
                    Some(p) => jsonl_path(p, id) == path,
                    None => false,
                }
            })
}

fn locate_session_file(dir: &Path, id: n00nId) -> Option<PathBuf> {
    let path = jsonl_path(dir, id);
    (path.exists() && file_is_zst(&path)).then_some(path)
}

fn load_session_at<M, U, T>(path: &Path) -> Result<Session<M, U, T>, SessionError>
where
    M: DeserializeOwned + Default + Clone,
    U: DeserializeOwned + Default,
    T: DeserializeOwned,
{
    parse_records(path).map(|(session, _, _, _)| session)
}

fn tool_ids_in_transcript<M, F>(entries: &[TranscriptEntry<M>], f: &F) -> Vec<String>
where
    F: Fn(&M) -> Vec<String>,
{
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            TranscriptEntry::Message(m) | TranscriptEntry::GeneratedMessage(m) => {
                out.extend(f(m));
            }
            TranscriptEntry::Compaction {
                entries: children, ..
            } => {
                out.extend(tool_ids_in_transcript(children, f));
            }
        }
    }
    out
}

// -- Session impl --

impl<M, U, T> Session<M, U, T>
where
    M: Serialize + DeserializeOwned + TitleSource + Clone + Default,
    U: Serialize + DeserializeOwned + Default,
    T: Serialize + DeserializeOwned,
{
    #[must_use]
    pub fn new(model: &str, cwd: &str) -> Self {
        let now = now_epoch();
        Self {
            version: SESSION_VERSION,
            id: n00nId::generate(),
            title: DEFAULT_TITLE.into(),
            cwd: cwd.into(),
            model: model.into(),
            messages: Vec::new(),
            transcript: Vec::new(),
            transcript_revision: None,
            token_usage: U::default(),
            tool_outputs: HashMap::new(),
            subagent_messages: HashMap::new(),
            meta: SessionMeta {
                mode: Some(StoredMode::Build),
                ..Default::default()
            },
            created_at: now,
            updated_at: now,
        }
    }

    pub fn set_transcript_revision(&mut self, revision: Option<u64>) {
        self.transcript_revision = revision;
    }

    /// After `messages` is truncated (rewind), state keyed by `tool_use_id` can
    /// point at calls that no longer exist. On restore that shows up as ghost
    /// subagent tabs and leaked tool outputs, so this drops everything not
    /// reachable from `messages` or the saved `transcript` (the latter keeps
    /// compacted history and historical subagent tabs intact).
    ///
    /// If you add another field keyed by `tool_use_id`, prune it here too.
    pub fn prune_orphans(&mut self, tool_ids: impl Fn(&M) -> Vec<String>) {
        let mut main_ids: Vec<String> = self.messages.iter().flat_map(&tool_ids).collect();
        main_ids.extend(tool_ids_in_transcript(&self.transcript, &tool_ids));
        let main_ids: HashSet<String> = main_ids.into_iter().collect();
        self.subagent_messages.retain(|id, _| main_ids.contains(id));
        self.meta
            .subagents
            .retain(|sa| main_ids.contains(&sa.tool_use_id));

        let live: HashSet<String> = self
            .subagent_messages
            .values()
            .flatten()
            .flat_map(&tool_ids)
            .chain(main_ids)
            .collect();
        self.tool_outputs.retain(|id, _| live.contains(id));
    }

    /// # Errors
    /// Returns `SessionError` if the sessions directory cannot be created or the save fails.
    pub fn save(&mut self, dir: &StateDir) -> Result<(), SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        self.save_to(&sessions_dir)
    }

    /// # Errors
    /// Returns `SessionError` if the session file cannot be written or the index cannot be updated.
    pub fn save_to(&mut self, dir: &Path) -> Result<(), SessionError> {
        self.updated_at = now_epoch();
        write_session_file(dir, self)?;
        update_cwd_index(dir, &self.cwd, self.id)?;
        Ok(())
    }

    /// # Errors
    /// Returns `SessionError` if the sessions directory cannot be created or the session cannot be loaded.
    pub fn load(id: n00nId, dir: &StateDir) -> Result<Self, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::load_from(id, &sessions_dir)
    }

    /// # Errors
    /// Returns `SessionError` if the session file cannot be found, read, or parsed,
    /// or if the session ID does not match.
    pub fn load_from(id: n00nId, dir: &Path) -> Result<Self, SessionError> {
        let Some(path) = locate_session_file(dir, id) else {
            return Err(StorageError::NotFound(id.to_string()).into());
        };
        let session = load_session_at::<M, U, T>(&path)?;
        if session.id != id {
            return Err(SessionError::IdMismatch {
                log_id: session.id,
                given_id: id,
            });
        }
        Ok(session)
    }

    /// # Errors
    /// Returns `SessionError` if the sessions directory cannot be created or the scan fails.
    pub fn list(cwd: &str, dir: &StateDir) -> Result<Vec<SessionSummary>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::list_in(cwd, &sessions_dir)
    }

    /// # Errors
    /// Returns `SessionError` if the scan fails.
    pub fn list_in(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
        let mut summaries = scan_headers::<M>(cwd, dir)?;
        summaries.sort_unstable_by_key(|s| (Reverse(s.updated_at), *s.id.as_bytes()));
        Ok(summaries)
    }

    /// # Errors
    /// Returns `SessionError` if the sessions directory cannot be created or the latest session cannot be loaded.
    pub fn latest(cwd: &str, dir: &StateDir) -> Result<Option<Self>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::latest_in(cwd, &sessions_dir)
    }

    /// # Errors
    /// Returns `SessionError` if the scan or load fails.
    pub fn latest_in(cwd: &str, dir: &Path) -> Result<Option<Self>, SessionError> {
        let latest = scan_headers::<M>(cwd, dir)?
            .into_iter()
            .max_by_key(|s| s.updated_at);
        match latest {
            Some(summary) => {
                update_cwd_index(dir, cwd, summary.id)?;
                Self::load_from(summary.id, dir).map(Some)
            }
            None => Ok(None),
        }
    }

    pub fn update_title_if_default(&mut self) {
        if self.title == DEFAULT_TITLE {
            self.title = generate_title(&self.messages);
        }
    }

    /// # Errors
    /// Returns `SessionError` if the sessions directory cannot be created or the delete fails.
    pub fn delete(id: n00nId, dir: &StateDir) -> Result<(), SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::delete_from(id, &sessions_dir)
    }

    /// # Errors
    /// Returns `SessionError` if the session file cannot be found or removed.
    pub fn delete_from(id: n00nId, dir: &Path) -> Result<(), SessionError> {
        let _lock = lock_openai_response_chain_in(dir, id)?;
        let Some(path) = locate_session_file(dir, id) else {
            return Err(StorageError::NotFound(id.to_string()).into());
        };
        try_remove(&path)?;
        try_remove(&openai_response_chain_path(dir, id))?;
        remove_from_cwd_index(dir, id)?;
        Ok(())
    }

    /// # Errors
    /// Returns `SessionError` if the session file cannot be created or written.
    pub fn migrate_to_jsonl(dir: &Path, session: &Self) -> Result<SessionLog, SessionError> {
        SessionLog::create(dir, session)
    }
}

#[cfg(test)]
mod tests {
    use super::ThinkingParseError;
    use super::{BodyOverride, EffortDialectId, ThinkingFieldConfig, ToggleEntry};
    use super::{
        CWD_INDEX_FILE, DEFAULT_TITLE, LOG_FORMAT_VERSION, LogRecord, MAX_TITLE_LEN,
        SESSION_VERSION, StoredDelivery, StoredQueuedMessage, StoredSubagent, append_record,
        classify_and_display, encode_frame, generate_title, jsonl_path, load_cwd_index, now_epoch,
        update_cwd_index,
    };
    use super::{Effort, StoredReasoningContext, StoredReasoningMode, StoredThinking};
    use super::{
        OPENAI_RESPONSE_CHAIN_TTL_SECONDS, SESSIONS_DIR, StoredOpenAiResponseChain,
        delete_openai_response_chain, load_openai_response_chain, load_openai_response_chain_at,
        lock_openai_response_chain, openai_response_chain_path, save_openai_response_chain,
        try_lock_openai_response_chain,
    };
    use super::{
        SCAN_CACHE_FILE, Session, SessionError, SessionLog, StorageError, StoredFusionUsage,
        StoredTokenUsage, TitleSource, TranscriptEntry,
    };
    use crate::StateDir;
    use crate::id::n00nId;
    use serde::Serializer;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use test_case::test_case;

    type TestSession = Session<Value, Value, Value>;

    static MESSAGE_SERIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone, Default, serde::Deserialize)]
    struct CountingMessage;

    impl TitleSource for CountingMessage {
        fn first_user_text(&self) -> Option<&str> {
            None
        }
    }

    impl serde::Serialize for CountingMessage {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            MESSAGE_SERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
            serializer.serialize_unit()
        }
    }

    impl TitleSource for Value {
        fn first_user_text(&self) -> Option<&str> {
            if self.get("role")?.as_str()? != "user" {
                return None;
            }
            self.get("content")?.as_array()?.iter().find_map(|b| {
                if b.get("type")?.as_str()? == "text" {
                    let text = b.get("text")?.as_str()?;
                    (!text.is_empty()).then_some(text)
                } else {
                    None
                }
            })
        }
    }

    fn user_message(text: &str) -> Value {
        text_message("user", text)
    }

    fn assistant_message(text: &str) -> Value {
        text_message("assistant", text)
    }

    fn text_message(role: &str, text: &str) -> Value {
        serde_json::json!({
            "role": role,
            "content": [{"type": "text", "text": text}]
        })
    }

    #[test]
    fn prune_orphans_drops_unreachable_tool_state() {
        fn ids(m: &Value) -> Vec<String> {
            vec![m.as_str().unwrap().to_owned()]
        }
        fn subagent(id: &str) -> StoredSubagent {
            StoredSubagent {
                tool_use_id: id.into(),
                name: "sub".into(),
                prompt: None,
                model: None,
            }
        }

        let mut session: TestSession = Session::new("model", "/p");
        session.messages.push("task-live".into());
        session
            .subagent_messages
            .insert("task-live".into(), vec!["sub-tool".into()]);
        session
            .subagent_messages
            .insert("task-stale".into(), vec!["stale-sub-tool".into()]);
        session.meta.subagents = vec![subagent("task-live"), subagent("task-stale")];
        for id in ["task-live", "sub-tool", "stale-sub-tool", "orphan"] {
            session.tool_outputs.insert(id.into(), Value::Null);
        }

        session.prune_orphans(ids);

        assert_eq!(
            session.subagent_messages.keys().collect::<Vec<_>>(),
            ["task-live"]
        );
        let subagent_ids: Vec<_> = session
            .meta
            .subagents
            .iter()
            .map(|sa| sa.tool_use_id.as_str())
            .collect();
        assert_eq!(subagent_ids, ["task-live"]);
        let mut outputs: Vec<_> = session.tool_outputs.keys().cloned().collect();
        outputs.sort();
        assert_eq!(outputs, ["sub-tool", "task-live"]);
    }

    #[test]
    fn prune_orphans_keeps_state_reachable_from_transcript() {
        fn ids(m: &Value) -> Vec<String> {
            vec![m.as_str().unwrap().to_owned()]
        }
        fn subagent(id: &str) -> StoredSubagent {
            StoredSubagent {
                tool_use_id: id.into(),
                name: "sub".into(),
                prompt: None,
                model: None,
            }
        }

        let mut session: TestSession = Session::new("model", "/p");
        session.messages.push("summary".into());
        session.transcript.push(TranscriptEntry::Compaction {
            entries: vec![
                TranscriptEntry::Message("tool-a".into()),
                TranscriptEntry::Message("tool-b".into()),
            ],
            generated_summary: None,
        });
        session
            .subagent_messages
            .insert("tool-a".into(), vec!["tool-c".into()]);
        session.subagent_messages.insert("stale".into(), vec![]);
        session.meta.subagents = vec![subagent("tool-a"), subagent("stale")];
        for id in ["tool-a", "tool-b", "tool-c", "stale", "orphan"] {
            session.tool_outputs.insert(id.into(), Value::Null);
        }

        session.prune_orphans(ids);

        let mut outputs: Vec<_> = session.tool_outputs.keys().cloned().collect();
        outputs.sort();
        assert_eq!(outputs, ["tool-a", "tool-b", "tool-c"]);
        assert!(session.subagent_messages.contains_key("tool-a"));
        assert!(!session.subagent_messages.contains_key("stale"));
        let subagent_ids: Vec<_> = session
            .meta
            .subagents
            .iter()
            .map(|sa| sa.tool_use_id.as_str())
            .collect();
        assert_eq!(subagent_ids, ["tool-a"]);
    }

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession =
            Session::new("anthropic/claude-sonnet-4", "/home/test/project");
        session.messages.push(user_message("hello"));
        session.transcript = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(user_message("before compaction"))],
                generated_summary: Some(assistant_message("generated summary")),
            },
            TranscriptEntry::GeneratedMessage(user_message("summary prompt")),
            TranscriptEntry::GeneratedMessage(assistant_message("generated summary")),
        ];
        session.subagent_messages.insert(
            "tool-1".into(),
            vec![user_message("sub-prompt"), assistant_message("sub-reply")],
        );
        session.save_to(dir).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model, "anthropic/claude-sonnet-4");
        assert_eq!(loaded.cwd, "/home/test/project");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.version, SESSION_VERSION);
        assert_eq!(loaded.subagent_messages["tool-1"].len(), 2);
        assert!(matches!(
            loaded.transcript.as_slice(),
            [
                TranscriptEntry::Compaction {
                    generated_summary: Some(summary),
                    ..
                },
                TranscriptEntry::GeneratedMessage(_),
                TranscriptEntry::GeneratedMessage(_),
            ] if summary == &assistant_message("generated summary")
        ));
    }

    #[test]
    fn legacy_compaction_without_summary_metadata_deserializes() {
        let entry: TranscriptEntry<Value> = serde_json::from_value(serde_json::json!({
            "Compaction": { "entries": [] }
        }))
        .unwrap();

        assert!(matches!(
            entry,
            TranscriptEntry::Compaction {
                generated_summary: None,
                ..
            }
        ));
    }

    #[test]
    fn active_messages_exclude_nested_compaction_entries() {
        let transcript = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(user_message("archived"))],
                generated_summary: Some(assistant_message("archived summary")),
            },
            TranscriptEntry::GeneratedMessage(user_message("summary prompt")),
            TranscriptEntry::GeneratedMessage(assistant_message("active summary")),
            TranscriptEntry::Message(user_message("continued")),
        ];

        let messages = super::active_messages_from_transcript(&transcript);

        assert_eq!(
            messages,
            vec![
                user_message("summary prompt"),
                assistant_message("active summary"),
                user_message("continued"),
            ]
        );
    }

    #[test]
    fn opening_transcript_only_session_hydrates_and_rewrites_messages() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.transcript = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(user_message("archived"))],
                generated_summary: None,
            },
            TranscriptEntry::GeneratedMessage(user_message("summary prompt")),
            TranscriptEntry::GeneratedMessage(assistant_message("summary")),
            TranscriptEntry::Message(user_message("continued")),
        ];
        let log = SessionLog::create(dir, &session).unwrap();
        drop(log);

        let (loaded, log) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert_eq!(
            loaded.messages,
            vec![
                user_message("summary prompt"),
                assistant_message("summary"),
                user_message("continued"),
            ]
        );
        drop(log);

        let (rewritten, needs_rewrite, _, _) =
            super::parse_records::<Value, Value, Value>(&jsonl_path(dir, session.id)).unwrap();
        assert_eq!(rewritten.messages, loaded.messages);
        assert!(!needs_rewrite);
    }

    #[test]
    fn roundtrip_usage_by_model() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("anthropic/claude-sonnet-4", "/project");
        session.meta.usage_by_model.insert(
            "claude-sonnet-4".into(),
            super::StoredTokenUsage {
                input: 100,
                output: 20,
                cache_creation: 5,
                cache_read: 40,
            },
        );
        session.meta.usage_by_model.insert(
            "claude-haiku-4".into(),
            super::StoredTokenUsage {
                input: 30,
                output: 10,
                ..Default::default()
            },
        );
        session.save_to(dir).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        let sonnet = &loaded.meta.usage_by_model["claude-sonnet-4"];
        assert_eq!(sonnet.input, 100);
        assert_eq!(sonnet.output, 20);
        assert_eq!(sonnet.cache_read, 40);
        assert_eq!(sonnet.total_input(), 145);
        assert_eq!(loaded.meta.usage_by_model["claude-haiku-4"].total(), 40);
    }

    #[test]
    fn roundtrip_jsonl_incremental() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("first"));

        let mut log = SessionLog::create(dir, &session).unwrap();

        session.messages.push(assistant_message("reply"));
        session.messages.push(user_message("second"));
        session
            .tool_outputs
            .insert("tool-1".into(), serde_json::json!({"result": "ok"}));
        session
            .subagent_messages
            .insert("sub-1".into(), vec![user_message("sub-prompt")]);
        log.append(&session).unwrap();

        session
            .subagent_messages
            .get_mut("sub-1")
            .unwrap()
            .push(assistant_message("sub-reply"));
        session
            .subagent_messages
            .insert("sub-2".into(), vec![user_message("sub-2-prompt")]);
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.tool_outputs.len(), 1);
        assert!(loaded.tool_outputs.contains_key("tool-1"));
        assert_eq!(loaded.subagent_messages["sub-1"].len(), 2);
        assert_eq!(loaded.subagent_messages["sub-2"].len(), 1);
    }

    #[test]
    fn transcript_appends_incrementally_in_zstd_frames() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session
            .transcript
            .push(TranscriptEntry::Message(user_message("first")));
        let mut log = SessionLog::create(dir, &session).unwrap();
        let path = jsonl_path(dir, session.id);
        assert!(super::file_is_zst(&path));

        session
            .transcript
            .push(TranscriptEntry::Message(assistant_message("second")));
        session.updated_at += 1;
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.transcript.len(), 2);
        assert!(matches!(
            &loaded.transcript[1],
            TranscriptEntry::Message(message)
                if message["content"][0]["text"].as_str() == Some("second")
        ));
    }

    #[test]
    fn transcript_replacement_compacts_even_with_same_length() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.transcript = vec![
            TranscriptEntry::Message(user_message("old first")),
            TranscriptEntry::Message(user_message("same tail")),
        ];
        let mut log = SessionLog::create(dir, &session).unwrap();

        session.transcript = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(user_message("old first"))],
                generated_summary: None,
            },
            TranscriptEntry::Message(user_message("same tail")),
        ];
        session.updated_at += 1;
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert!(matches!(
            loaded.transcript.as_slice(),
            [TranscriptEntry::Compaction { entries, .. }, TranscriptEntry::Message(_)]
                if entries.len() == 1
        ));
    }

    #[test]
    fn meta_only_saves_do_not_repeat_full_transcript() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.transcript.extend((0..2_000).map(|index| {
            TranscriptEntry::Message(user_message(&format!(
                "distinct transcript message {index}"
            )))
        }));
        let mut log = SessionLog::create(dir, &session).unwrap();
        let path = jsonl_path(dir, session.id);
        let initial_size = fs::metadata(&path).unwrap().len();

        for _ in 0..10 {
            session.updated_at += 1;
            log.append(&session).unwrap();
        }

        let growth = fs::metadata(path).unwrap().len() - initial_size;
        assert!(
            growth < initial_size,
            "growth={growth}, initial={initial_size}"
        );
    }

    #[test]
    fn meta_only_append_does_not_serialize_saved_transcript() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: Session<CountingMessage, Value, Value> = Session::new("m", "/project");
        session.transcript = vec![TranscriptEntry::Compaction {
            entries: vec![TranscriptEntry::Message(CountingMessage)],
            generated_summary: None,
        }];
        session.set_transcript_revision(Some(1));
        let mut log = SessionLog::create(dir, &session).unwrap();
        MESSAGE_SERIALIZATIONS.store(0, Ordering::Relaxed);

        session.updated_at += 1;
        log.append(&session).unwrap();

        assert_eq!(MESSAGE_SERIALIZATIONS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn incremental_frame_limit_triggers_zstd_compaction() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session
            .transcript
            .push(TranscriptEntry::Message(user_message("persisted")));
        let mut log = SessionLog::create(dir, &session).unwrap();
        let path = jsonl_path(dir, session.id);

        for _ in 0..10 {
            session.updated_at += 1;
            log.append(&session).unwrap();
        }
        let expanded_size = fs::metadata(&path).unwrap().len();
        log.appended_frames = super::MAX_INCREMENTAL_FRAMES;
        session.updated_at += 1;
        log.append(&session).unwrap();

        let compacted_size = fs::metadata(&path).unwrap().len();
        let (_, _, recovered_tail, log_appends) =
            super::parse_records::<Value, Value, Value>(&path).unwrap();
        assert!(compacted_size < expanded_size);
        assert!(!recovered_tail);
        assert_eq!(log_appends, 0);
        assert_eq!(log.appended_frames, 0);
    }

    #[test]
    fn persisted_append_count_triggers_compaction_after_reopen() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        let mut log = SessionLog::create(dir, &session).unwrap();
        log.appended_frames = super::MAX_INCREMENTAL_FRAMES - 1;
        session.updated_at += 1;
        log.append(&session).unwrap();
        drop(log);

        let (_, mut reopened) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert_eq!(reopened.appended_frames, super::MAX_INCREMENTAL_FRAMES);

        session.updated_at += 1;
        reopened.append(&session).unwrap();
        let path = jsonl_path(dir, session.id);
        let (_, _, recovered_tail, log_appends) =
            super::parse_records::<Value, Value, Value>(&path).unwrap();
        assert!(!recovered_tail);
        assert_eq!(log_appends, 0);
        assert_eq!(reopened.appended_frames, 0);
    }

    #[test]
    fn legacy_meta_without_append_count_defaults_to_zero() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let path = jsonl_path(dir, session.id);
        let records = format!(
            "{}\n{}\n",
            serde_json::json!({
                "t": "header",
                "v": LOG_FORMAT_VERSION,
                "id": session.id,
                "model": session.model,
                "cwd": session.cwd,
                "created_at": session.created_at,
            }),
            serde_json::json!({
                "t": "meta",
                "title": session.title,
                "token_usage": {},
                "updated_at": session.updated_at,
            }),
        );
        let mut file = File::create(&path).unwrap();
        encode_frame(&mut file, records.as_bytes()).unwrap();
        drop(file);

        let (_, _, recovered_tail, log_appends) =
            super::parse_records::<Value, Value, Value>(&path).unwrap();
        assert!(!recovered_tail);
        assert_eq!(log_appends, 0);
    }

    #[test]
    fn opening_corrupt_zstd_tail_atomically_repairs_log() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("persisted"));
        let log = SessionLog::create(dir, &session).unwrap();
        drop(log);
        let path = jsonl_path(dir, session.id);

        let mut encoded = Vec::new();
        encode_frame(&mut encoded, b"{\"t\":\"meta\"}\n").unwrap();
        let partial_len = encoded.len() / 2;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&encoded[..partial_len]).unwrap();
        drop(file);

        let (loaded, repaired_log) =
            SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert_eq!(loaded.messages, session.messages);
        assert_eq!(repaired_log.appended_frames, 0);
        drop(repaired_log);

        let (_, _, recovered_tail, log_appends) =
            super::parse_records::<Value, Value, Value>(&path).unwrap();
        assert!(!recovered_tail);
        assert_eq!(log_appends, 0);
    }

    #[test]
    fn rejects_unknown_session_record() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let log = SessionLog::create(dir, &session).unwrap();
        drop(log);

        let mut encoded = Vec::new();
        encode_frame(&mut encoded, b"{\"t\":\"future_record\"}\n").unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(jsonl_path(dir, session.id))
            .unwrap();
        file.write_all(&encoded).unwrap();
        drop(file);

        let error = TestSession::load_from(session.id, dir).unwrap_err();
        assert!(matches!(error, SessionError::UnknownRecord));
    }
    #[test]
    fn bounded_record_buffer_accepts_exact_limit_and_rejects_next_byte() {
        let mut writer = super::BoundedRecordBuffer::new(3);
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert_eq!(writer.bytes, b"abc");
    }

    #[test]
    fn append_record_rejects_oversized_records() {
        let record = serde_json::json!({
            "t": "msg",
            "d": "x".repeat(super::MAX_SESSION_RECORD_BYTES),
        });
        let error = append_record(&mut Vec::new(), &record).unwrap_err();
        assert!(matches!(error, SessionError::RecordTooLargeWrite { .. }));
    }

    #[test]
    fn session_open_rejects_oversized_decompressed_records_without_repair() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let log = SessionLog::create(dir, &session).unwrap();
        drop(log);

        let prefix = b"{\"t\":\"meta\",\"state_snapshot\":{\"schema_version\":2,\"opaque\":\"";
        let mut record = prefix.to_vec();
        record.extend(std::iter::repeat_n(
            b'x',
            super::MAX_SESSION_RECORD_BYTES - prefix.len(),
        ));
        record.extend_from_slice("é\"}}\n".as_bytes());
        let mut encoded = Vec::new();
        encode_frame(&mut encoded, &record).unwrap();
        let path = jsonl_path(dir, session.id);
        let original_len = fs::metadata(&path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&encoded).unwrap();
        drop(file);

        let Err(error) = SessionLog::open::<Value, Value, Value>(dir, session.id) else {
            panic!("oversized record unexpectedly loaded");
        };
        assert!(matches!(error, SessionError::RecordTooLarge { .. }));
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            original_len + encoded.len() as u64
        );
    }

    #[test]
    fn opens_large_single_frame_legacy_transcript() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let path = jsonl_path(dir, session.id);
        let payload = (0..65_536).fold(String::new(), |mut payload, index| {
            write!(&mut payload, "legacy-{index:08x}").unwrap();
            payload
        });
        let mut records = Vec::new();
        append_record(
            &mut records,
            &LogRecord::<Value, &Value, &Value>::Header {
                v: LOG_FORMAT_VERSION,
                id: session.id,
                model: session.model.clone(),
                cwd: session.cwd.clone(),
                title: Some(session.title.clone()),
                created_at: session.created_at,
                parent_id: None,
            },
        )
        .unwrap();
        append_record(
            &mut records,
            &LogRecord::<Value, &Value, &Value>::Meta {
                title: session.title.clone(),
                token_usage: &session.token_usage,
                updated_at: session.updated_at,
                log_appends: 0,
                transcript: Some(vec![TranscriptEntry::Message(user_message(&payload))]),
                meta: session.meta.clone(),
            },
        )
        .unwrap();
        let mut file = File::create(&path).unwrap();
        encode_frame(&mut file, &records).unwrap();
        drop(file);

        let (loaded, _) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert!(matches!(
            loaded.transcript.as_slice(),
            [TranscriptEntry::Message(message)]
                if message["content"][0]["text"].as_str() == Some(payload.as_str())
        ));
    }
    #[test]
    fn opening_legacy_transcript_log_migrates_to_compact_zstd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let path = jsonl_path(dir, session.id);
        let mut file = File::create(&path).unwrap();
        let mut header = Vec::new();
        append_record(
            &mut header,
            &LogRecord::<Value, &Value, &Value>::Header {
                v: LOG_FORMAT_VERSION,
                id: session.id,
                model: session.model.clone(),
                cwd: session.cwd.clone(),
                title: Some(session.title.clone()),
                created_at: session.created_at,
                parent_id: None,
            },
        )
        .unwrap();
        encode_frame(&mut file, &header).unwrap();

        for index in 0..50 {
            let mut meta = Vec::new();
            append_record(
                &mut meta,
                &LogRecord::<Value, &Value, &Value>::Meta {
                    title: session.title.clone(),
                    token_usage: &session.token_usage,
                    updated_at: session.updated_at + index,
                    log_appends: 0,
                    transcript: Some(vec![TranscriptEntry::Message(user_message(
                        "legacy transcript payload",
                    ))]),
                    meta: session.meta.clone(),
                },
            )
            .unwrap();
            encode_frame(&mut file, &meta).unwrap();
        }
        drop(file);
        let legacy_size = fs::metadata(&path).unwrap().len();

        let (loaded, _) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        let migrated_size = fs::metadata(&path).unwrap().len();

        assert_eq!(loaded.transcript.len(), 1);
        assert!(migrated_size < legacy_size / 2);
        assert!(super::file_is_zst(&path));
    }

    #[test]
    fn append_rewrites_equal_length_replacement() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages = vec![user_message("old prompt"), assistant_message("old reply")];
        let mut log = SessionLog::create(dir, &session).unwrap();

        session.messages = vec![user_message("new prompt"), assistant_message("new reply")];
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages, session.messages);
    }

    #[test]
    fn append_rewrites_longer_replacement() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages = vec![user_message("old prompt"), assistant_message("old reply")];
        let mut log = SessionLog::create(dir, &session).unwrap();

        session.messages = vec![
            user_message("replacement prompt"),
            assistant_message("replacement reply"),
            user_message("new tail"),
        ];
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages, session.messages);
    }

    #[cfg(unix)]
    #[test]
    fn append_only_messages_keep_append_fast_path() {
        use std::os::unix::fs::MetadataExt;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("first"));
        let mut log = SessionLog::create(dir, &session).unwrap();
        let path = jsonl_path(dir, session.id);
        let inode = fs::metadata(&path).unwrap().ino();

        session.messages.push(assistant_message("reply"));
        log.append(&session).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages, session.messages);
    }

    #[test]
    fn reopened_log_rewrites_mutated_saved_prefix() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("old"));
        let log = SessionLog::create(dir, &session).unwrap();
        drop(log);

        let (_loaded, mut log) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        session.messages = vec![user_message("replacement"), assistant_message("new tail")];
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages, session.messages);
    }

    #[test]
    fn append_persists_meta_changes_without_new_messages() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("first"));

        let mut log = SessionLog::create(dir, &session).unwrap();

        session.meta.input_draft = Some("draft line".into());
        session.meta.queued_messages = vec!["queued".into()];
        session.title = "updated title".into();
        session.updated_at = now_epoch() + 1;
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.meta.input_draft.as_deref(), Some("draft line"));
        assert_eq!(loaded.meta.queued_messages, vec!["queued".to_string()]);
        assert_eq!(loaded.title, "updated title");
    }

    #[test]
    fn open_trims_partial_trailing_line_before_append() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("survives"));
        SessionLog::create(dir, &session).unwrap();

        let path = jsonl_path(dir, session.id);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":\"msg\",\"d\":{\"trun").unwrap();
        drop(file);

        let (loaded, mut log) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert_eq!(loaded.messages.len(), 1);

        session.messages.push(user_message("after-crash"));
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(
            loaded.messages[1]["content"][0]["text"].as_str(),
            Some("after-crash")
        );
    }

    #[test]
    fn append_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("first"));

        let mut log = SessionLog::create(dir, &session).unwrap();
        log.append(&session).unwrap();
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages.len(), 1);
    }

    #[test]
    fn append_wrong_session_returns_id_mismatch() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session_a: TestSession = Session::new("m", "/project");
        let session_b: TestSession = Session::new("m", "/project");
        let mut log = SessionLog::create(dir, &session_a).unwrap();

        let err = log.append(&session_b).unwrap_err();
        assert!(matches!(err, SessionError::IdMismatch { .. }));
    }

    #[test]
    fn rewind_compact() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        for i in 0..10 {
            session.messages.push(user_message(&format!("msg-{i}")));
        }
        session.subagent_messages.insert(
            "sub-1".into(),
            vec![user_message("sub-prompt"), assistant_message("sub-reply")],
        );
        let mut log = SessionLog::create(dir, &session).unwrap();

        session.messages.truncate(5);
        session.tool_outputs.clear();
        session.subagent_messages.remove("sub-1");
        log.compact(dir, &session).unwrap();

        session.messages.push(user_message("after-compact-1"));
        session.messages.push(user_message("after-compact-2"));
        session.messages.push(user_message("after-compact-3"));
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.messages.len(), 8);
        assert!(loaded.subagent_messages.is_empty());
    }

    /// A rename with no new messages must survive restart, while a no-op
    /// append must not grow the file.
    #[test]
    fn append_writes_meta_only_when_it_changed() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("hi"));
        let mut log = SessionLog::create(dir, &session).unwrap();

        let path = jsonl_path(dir, session.id);
        let size_before = fs::metadata(&path).unwrap().len();
        log.append(&session).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), size_before);

        session.title = "renamed".into();
        session.updated_at = 42;
        log.append(&session).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.title, "renamed");
        assert_eq!(loaded.updated_at, 42);
    }

    #[test]
    fn load_nonexistent_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let id = n00nId::generate();
        let err = TestSession::load_from(id, tmp.path()).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Storage(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn uncompressed_jsonl_and_json_sessions_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("model", "/project");
        let jsonl = jsonl_path(dir, session.id);
        let header = serde_json::json!({
            "t": "header",
            "v": 3,
            "id": session.id,
            "model": session.model,
            "cwd": session.cwd,
            "created_at": session.created_at,
        });
        fs::write(&jsonl, format!("{header}\n")).unwrap();
        fs::write(
            dir.join(format!("{}.json", session.id)),
            serde_json::to_vec(&session).unwrap(),
        )
        .unwrap();

        let err = TestSession::load_from(session.id, dir).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Storage(StorageError::NotFound(_))
        ));
        assert!(TestSession::list_in("/project", dir).unwrap().is_empty());

        let err = TestSession::delete_from(session.id, dir).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Storage(StorageError::NotFound(_))
        ));
        assert!(jsonl.exists());
    }

    #[test]
    fn response_chain_try_lock_is_contended_across_processes() {
        const CHILD_ENV: &str = "N00N_RESPONSE_CHAIN_LOCK_CHILD";
        const DIR_ENV: &str = "N00N_RESPONSE_CHAIN_LOCK_DIR";
        const READY_ENV: &str = "N00N_RESPONSE_CHAIN_LOCK_READY";
        const SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";

        if std::env::var_os(CHILD_ENV).is_some() {
            let dir = std::env::var_os(DIR_ENV)
                .map(std::path::PathBuf::from)
                .unwrap();
            let ready = std::env::var_os(READY_ENV)
                .map(std::path::PathBuf::from)
                .unwrap();
            let state_dir = StateDir::from_path(dir);
            let session_id = SESSION_ID.parse::<n00nId>().unwrap();
            let _lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
            fs::write(ready, b"ready").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            return;
        }

        let temp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(temp.path().to_path_buf());
        let ready = temp.path().join("ready");
        let executable = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(executable)
            .args([
                "--exact",
                "sessions::tests::response_chain_try_lock_is_contended_across_processes",
            ])
            .env(CHILD_ENV, "1")
            .env(DIR_ENV, state_dir.path())
            .env(READY_ENV, &ready)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ready.exists());
        let session_id = SESSION_ID.parse::<n00nId>().unwrap();
        assert!(
            try_lock_openai_response_chain(&state_dir, session_id)
                .unwrap()
                .is_none()
        );
        assert!(child.wait().unwrap().success());
        assert!(
            try_lock_openai_response_chain(&state_dir, session_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn openai_response_chain_round_trips_and_expires_after_thirty_days() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut session: TestSession = Session::new("model", "/project");
        session.save(&state_dir).unwrap();
        let session_id = session.id;
        let now = 1_000;
        let chain = StoredOpenAiResponseChain {
            response_id: "resp_1".into(),
            message_count: 3,
            tools_hash: "tools".into(),
            messages_hash: "messages".into(),
            auth_scope_hash: "account".into(),
            expires_at: now + OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
        };

        let lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
        save_openai_response_chain(&state_dir, session_id, &chain, &lock).unwrap();
        assert_eq!(
            load_openai_response_chain_at(&state_dir, session_id, now, &lock).unwrap(),
            Some(chain)
        );
        assert!(
            load_openai_response_chain_at(
                &state_dir,
                session_id,
                now + OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
                &lock
            )
            .unwrap()
            .is_none()
        );
        assert!(!openai_response_chain_path(&tmp.path().join(SESSIONS_DIR), session_id).exists());
    }

    #[test]
    fn deleting_openai_response_chain_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let session_id = n00nId::generate();

        let lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
        delete_openai_response_chain(&state_dir, session_id, &lock).unwrap();
        delete_openai_response_chain(&state_dir, session_id, &lock).unwrap();
    }

    #[test]
    fn response_chain_write_cannot_recreate_a_deleted_session_sidecar() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let session_id = n00nId::generate();
        let chain = StoredOpenAiResponseChain {
            response_id: "resp_1".into(),
            message_count: 1,
            tools_hash: "tools".into(),
            messages_hash: "messages".into(),
            auth_scope_hash: "account".into(),
            expires_at: now_epoch() + OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
        };

        let lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
        assert!(save_openai_response_chain(&state_dir, session_id, &chain, &lock).is_err());
        assert!(!openai_response_chain_path(&tmp.path().join(SESSIONS_DIR), session_id).exists());
    }

    #[test]
    fn response_chain_lock_serializes_same_session_across_file_handles() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let session_id = n00nId::generate();
        let first = lock_openai_response_chain(&state_dir, session_id).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let second_dir = state_dir;
        let join = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let second = lock_openai_response_chain(&second_dir, session_id).unwrap();
            acquired_tx.send(()).unwrap();
            second
        });

        ready_rx.recv().unwrap();
        assert!(matches!(
            acquired_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first);
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        drop(join.join().unwrap());
    }
    #[test]
    fn response_chain_clear_cannot_delete_a_later_update() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut session: TestSession = Session::new("model", "/project");
        session.save(&state_dir).unwrap();
        let session_id = session.id;
        let updated = StoredOpenAiResponseChain {
            response_id: "resp_new".into(),
            message_count: 2,
            tools_hash: "tools".into(),
            messages_hash: "messages".into(),
            auth_scope_hash: "account".into(),
            expires_at: now_epoch() + OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
        };
        let clear_lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (updated_tx, updated_rx) = std::sync::mpsc::channel();
        let writer_dir = state_dir.clone();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let lock = lock_openai_response_chain(&writer_dir, session_id).unwrap();
            save_openai_response_chain(&writer_dir, session_id, &updated, &lock).unwrap();
            updated_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        delete_openai_response_chain(&state_dir, session_id, &clear_lock).unwrap();
        assert!(matches!(
            updated_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(clear_lock);
        updated_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        writer.join().unwrap();

        let lock = lock_openai_response_chain(&state_dir, session_id).unwrap();
        assert_eq!(
            load_openai_response_chain(&state_dir, session_id, &lock)
                .unwrap()
                .map(|chain| chain.response_id),
            Some("resp_new".into())
        );
    }

    #[test]
    fn deleting_session_removes_openai_response_chain() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut session: TestSession = Session::new("model", "/project");
        session.save(&state_dir).unwrap();
        let chain = StoredOpenAiResponseChain {
            response_id: "resp_1".into(),
            message_count: 1,
            tools_hash: "tools".into(),
            messages_hash: "messages".into(),
            auth_scope_hash: "account".into(),
            expires_at: now_epoch() + OPENAI_RESPONSE_CHAIN_TTL_SECONDS,
        };
        let lock = lock_openai_response_chain(&state_dir, session.id).unwrap();
        save_openai_response_chain(&state_dir, session.id, &chain, &lock).unwrap();
        drop(lock);

        TestSession::delete(session.id, &state_dir).unwrap();

        let sessions_dir = tmp.path().join(SESSIONS_DIR);
        assert!(!openai_response_chain_path(&sessions_dir, session.id).exists());
    }

    #[test]
    fn list_filters_by_cwd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1: TestSession = Session::new("m", "/project-a");
        let mut s2: TestSession = Session::new("m", "/project-b");
        let mut s3: TestSession = Session::new("m", "/project-a");
        s1.save_to(dir).unwrap();
        s2.save_to(dir).unwrap();
        s3.save_to(dir).unwrap();

        let list = TestSession::list_in("/project-a", dir).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|s| s.id != s2.id));

        let list = TestSession::list_in("/project-b", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, s2.id);
    }

    #[test]
    fn list_rescans_changed_file_and_prunes_deleted() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1: TestSession = Session::new("m", "/project");
        s1.messages.push(user_message("hi"));
        let mut log = SessionLog::create(dir, &s1).unwrap();
        let s2: TestSession = Session::new("m", "/project");
        SessionLog::create(dir, &s2).unwrap();
        TestSession::list_in("/project", dir).unwrap();

        s1.title = "renamed".into();
        log.append(&s1).unwrap();
        TestSession::delete_from(s2.id, dir).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "renamed");
        let cache: Value =
            serde_json::from_slice(&fs::read(dir.join(SCAN_CACHE_FILE)).unwrap()).unwrap();
        assert_eq!(cache.as_object().unwrap().len(), 1, "deleted entry pruned");
    }

    #[test]
    fn dirty_persisted_title_normalized_on_list_and_load() {
        const NORMALIZED: &str = "line one line two";
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s: TestSession = Session::new("m", "/project");
        s.messages.push(user_message("hi"));
        let mut log = SessionLog::create(dir, &s).unwrap();
        s.title = "line one\n\n\tline two".into();
        log.append(&s).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list[0].title, NORMALIZED);
        assert_eq!(TestSession::load_from(s.id, dir).unwrap().title, NORMALIZED);
    }

    #[test_case(Some(b"{ not json".as_slice()) ; "corrupt_cache")]
    #[test_case(None ; "missing_cache")]
    fn list_survives_bad_scan_cache(content: Option<&[u8]>) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s: TestSession = Session::new("m", "/project");
        s.save_to(dir).unwrap();
        if let Some(content) = content {
            fs::write(dir.join(SCAN_CACHE_FILE), content).unwrap();
        }

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, s.id);
    }

    fn save_with_time(session: &mut TestSession, dir: &Path, time: u64) {
        session.updated_at = time;
        SessionLog::create(dir, session).unwrap();
        update_cwd_index(dir, &session.cwd, session.id).unwrap();
    }

    #[test]
    fn latest_returns_most_recent_for_cwd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1: TestSession = Session::new("m", "/project");
        s1.title = "first".into();
        save_with_time(&mut s1, dir, 1000);

        let mut s2: TestSession = Session::new("m", "/other");
        save_with_time(&mut s2, dir, 2000);

        let mut s3: TestSession = Session::new("m", "/project");
        s3.title = "latest".into();
        save_with_time(&mut s3, dir, 3000);

        let latest = TestSession::latest_in("/project", dir).unwrap().unwrap();
        assert_eq!(latest.title, "latest");
    }

    #[test]
    fn latest_falls_back_when_index_stale() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.save_to(dir).unwrap();

        let index_path = dir.join(CWD_INDEX_FILE);
        let stale: HashMap<String, String> = [("/project".into(), "deleted-id".into())].into();
        fs::write(&index_path, serde_json::to_vec(&stale).unwrap()).unwrap();

        let latest = TestSession::latest_in("/project", dir).unwrap().unwrap();
        assert_eq!(latest.id, session.id);
    }

    #[test_case("short title", "short title" ; "short_passthrough")]
    #[test_case("", DEFAULT_TITLE ; "empty_defaults")]
    #[test_case(
        "This is a very long title that exceeds the sixty character limit and should be truncated at a word boundary",
        "This is a very long title that exceeds the sixty character…"
        ; "long_truncates_at_word"
    )]
    #[test_case("one\n\ntwo\t three", "one two three" ; "whitespace_collapses")]
    fn title_extraction(input: &str, expected: &str) {
        let messages: Vec<Value> = if input.is_empty() {
            vec![]
        } else {
            vec![user_message(input)]
        };
        assert_eq!(generate_title(&messages), expected);
    }

    #[test_case(
        "Use the team tool now. Do not only describe this request.\n\n{\"goal\":\"fix grouping\"}",
        "fix grouping",
        "team"
        ; "recognized_directive"
    )]
    #[test_case(
        "Use the bash tool now.\n\n{\"prompt\":\"not a sub-task\"}",
        "Use the bash tool now.",
        "main"
        ; "unrecognized_tool"
    )]
    #[test_case(
        "Use the team tool nowish.\n\n{\"goal\":\"not a directive\"}",
        "Use the team tool nowish.",
        "main"
        ; "malformed_directive"
    )]
    fn display_title_only_classifies_known_tool_directives(
        message: &str,
        expected_title: &str,
        expected_kind: &str,
    ) {
        let (title, kind) = classify_and_display(DEFAULT_TITLE, Some(message));
        assert_eq!(title, expected_title);
        assert_eq!(kind, expected_kind);
    }

    #[test]
    fn equal_creation_times_group_and_sort_deterministically() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let ids = [
            "00000000-0000-7000-8000-000000000001",
            "00000000-0000-7000-8000-000000000002",
            "00000000-0000-7000-8000-000000000003",
            "00000000-0000-7000-8000-000000000004",
        ]
        .map(|id| id.parse::<n00nId>().unwrap());
        let titles = ["main one", "task: child one", "main two", "team: child two"];

        for (id, title) in ids.into_iter().zip(titles) {
            let mut session: TestSession = Session::new("m", "/project");
            session.id = id;
            session.title = title.into();
            session.created_at = 100;
            session.updated_at = 200;
            SessionLog::create(dir, &session).unwrap();
        }

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(
            list.iter().map(|summary| summary.id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(list[0].parent_id, None);
        assert_eq!(list[1].parent_id, Some(ids[0]));
        assert_eq!(list[2].parent_id, None);
        assert_eq!(list[3].parent_id, Some(ids[2]));
    }

    #[test]
    fn delete_removes_file_and_cwd_index() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1: TestSession = Session::new("m", "/project");
        s1.save_to(dir).unwrap();
        let mut s2: TestSession = Session::new("m", "/other");
        s2.save_to(dir).unwrap();

        TestSession::delete_from(s1.id, dir).unwrap();
        assert!(!jsonl_path(dir, s1.id).exists());
        let index = load_cwd_index(dir);
        assert!(!index.values().any(|v| *v == s1.id.to_string()));
        assert_eq!(index.get("/other"), Some(&s2.id.to_string()));
    }

    #[test]
    fn delete_nonexistent_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let id = n00nId::generate();
        let err = TestSession::delete_from(id, tmp.path()).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Storage(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn title_unicode_safe() {
        let input = "あ".repeat(100);
        let title = generate_title(&[user_message(&input)]);
        assert!(title.len() <= MAX_TITLE_LEN * 4);
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn open_roundtrip_resumes_append() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("first"));

        let mut log = SessionLog::create(dir, &session).unwrap();
        session.messages.push(assistant_message("reply"));
        log.append(&session).unwrap();
        drop(log);

        let (loaded, mut log) = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();
        assert_eq!(loaded.messages.len(), 2);

        session.messages.push(user_message("second"));
        log.append(&session).unwrap();
        drop(log);

        let reloaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(reloaded.messages.len(), 3);
    }

    #[test_case(StoredThinking::Off ; "off")]
    #[test_case(StoredThinking::Adaptive ; "adaptive")]
    #[test_case(StoredThinking::Budget { tokens: 4096 } ; "budget")]
    #[test_case(StoredThinking::Effort { level: Effort::High } ; "effort")]
    #[test_case(StoredThinking::WithExtras {
        level: Effort::XHigh,
        reasoning_mode: Some(StoredReasoningMode::Pro),
        reasoning_context: Some(StoredReasoningContext::AllTurns),
    } ; "with_extras")]
    fn stored_thinking_serde_round_trip(variant: StoredThinking) {
        let json = serde_json::to_string(&variant).unwrap();
        let parsed: StoredThinking = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, variant);
    }

    #[test_case("{\"kind\":\"effort\",\"level\":\"high\"}", StoredThinking::Effort { level: Effort::High } ; "legacy_effort")]
    #[test_case("{\"kind\":\"with_extras\",\"level\":\"high\"}", StoredThinking::WithExtras { level: Effort::High, reasoning_mode: None, reasoning_context: None } ; "missing_extras_default")]
    fn stored_thinking_deserializes_compatible_json(json: &str, expected: StoredThinking) {
        assert_eq!(
            serde_json::from_str::<StoredThinking>(json).unwrap(),
            expected
        );
    }

    #[test_case("off", &Ok(StoredThinking::Off) ; "off")]
    #[test_case("adaptive", &Ok(StoredThinking::Adaptive) ; "adaptive")]
    #[test_case(" adaptive ", &Ok(StoredThinking::Adaptive) ; "trims_whitespace")]
    #[test_case("4096", &Ok(StoredThinking::Budget { tokens: 4096 }) ; "valid_budget")]
    #[test_case("1", &Ok(StoredThinking::Budget { tokens: 1 }) ; "minimum_budget")]
    #[test_case("0", &Err(ThinkingParseError::BudgetZero) ; "budget_zero")]
    #[test_case("fast", &Err(ThinkingParseError::Unknown("fast".into())) ; "garbage")]
    fn parse_setting(input: &str, expected: &Result<StoredThinking, ThinkingParseError>) {
        assert_eq!(StoredThinking::parse_setting(input), *expected);
    }

    #[test]
    fn session_state_snapshot_defaults_to_absent_and_is_omitted() {
        let meta = super::SessionMeta::default();
        assert!(meta.state_snapshot.is_none());
        assert_eq!(
            super::StoredSessionStateSnapshot::default().state_revision(),
            Some(0)
        );
        let encoded = serde_json::to_value(meta).unwrap();
        assert!(encoded.get("state_snapshot").is_none());
    }

    #[test]
    fn session_state_snapshot_round_trips_unknown_plugin_entries() {
        let mut snapshot = super::StoredSessionStateSnapshot::new(7);
        snapshot
            .set_plugin_state(
                "future_plugin",
                99,
                super::StoredStateScope::Root,
                serde_json::json!({ "future": [1, 2, 3] }),
            )
            .unwrap();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: super::StoredSessionStateSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert!(matches!(
            decoded.plugin_payload_for_apply("future_plugin", 1, super::StoredStateScope::Root),
            Err(super::SessionStateError::UnsupportedPluginVersion { .. })
        ));
    }

    #[test]
    fn session_state_snapshot_preserves_unsupported_envelopes() {
        let raw = serde_json::json!({
            "schema_version": 2,
            "state_revision": 8,
            "future_field": { "opaque": true }
        });
        let snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();
        assert!(matches!(
            snapshot.validate_for_apply(),
            Err(super::SessionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
        assert_eq!(serde_json::to_value(snapshot).unwrap(), raw);
    }
    #[test]
    fn session_state_snapshot_preserves_unknown_current_envelope_fields() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 8,
            "future_field": { "opaque": true }
        });
        let snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(snapshot).unwrap(), raw);
    }

    #[test]
    fn session_state_snapshot_rejects_oversized_unsupported_envelopes() {
        let raw = serde_json::json!({
            "schema_version": 2,
            "opaque": "x".repeat(super::MAX_SESSION_STATE_BYTES),
        });
        assert!(serde_json::from_value::<super::StoredSessionStateSnapshot>(raw).is_err());
    }

    #[test]
    fn session_state_snapshot_preserves_valid_state_beside_malformed_state() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 3,
            "plugins": {
                "good": {
                    "root": { "schema_version": 1, "payload": { "value": 7 } }
                },
                "bad": {
                    "session": { "schema_version": "invalid", "future": true }
                },
                "bad_container": null
            }
        });
        let snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            snapshot
                .plugin_payload_for_apply("good", 1, super::StoredStateScope::Root)
                .unwrap(),
            Some(&serde_json::json!({ "value": 7 }))
        );
        assert!(matches!(
            snapshot.plugin_payload_for_apply("bad", 1, super::StoredStateScope::Session),
            Err(super::SessionStateError::InvalidPluginState { .. })
        ));
        assert!(matches!(
            snapshot
                .plugin_payload_for_apply("bad_container", 1, super::StoredStateScope::Session,),
            Err(super::SessionStateError::InvalidPluginContainer { .. })
        ));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), raw);

        let temp = TempDir::new().unwrap();
        let mut session: TestSession = Session::new("model", "/project");
        session.meta.state_snapshot = Some(snapshot);
        session.save_to(temp.path()).unwrap();
        let loaded = TestSession::load_from(session.id, temp.path()).unwrap();
        assert_eq!(
            serde_json::to_value(loaded.meta.state_snapshot).unwrap(),
            raw
        );
    }

    #[test]
    fn session_state_snapshot_persists_through_session_log() {
        let temp = TempDir::new().unwrap();
        let mut session: TestSession = Session::new("model", "/project");
        let mut snapshot = super::StoredSessionStateSnapshot::new(4);
        snapshot
            .set_plugin_state(
                "todo_write",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({ "todos": [{ "content": "ship", "status": "in_progress" }] }),
            )
            .unwrap();
        session.meta.state_snapshot = Some(snapshot.clone());
        session.save_to(temp.path()).unwrap();

        let loaded = TestSession::load_from(session.id, temp.path()).unwrap();
        assert_eq!(loaded.meta.state_snapshot, Some(snapshot));
    }

    #[test]
    fn session_state_snapshot_supports_both_scopes_for_one_plugin() {
        let mut snapshot = super::StoredSessionStateSnapshot::new(1);
        snapshot
            .set_plugin_state(
                "plugin",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({ "root": true }),
            )
            .unwrap();
        snapshot
            .set_plugin_state(
                "plugin",
                2,
                super::StoredStateScope::Session,
                serde_json::json!({ "session": true }),
            )
            .unwrap();

        assert_eq!(
            snapshot
                .plugin_payload_for_apply("plugin", 1, super::StoredStateScope::Root)
                .unwrap(),
            Some(&serde_json::json!({ "root": true }))
        );
        assert_eq!(
            snapshot
                .plugin_payload_for_apply("plugin", 2, super::StoredStateScope::Session)
                .unwrap(),
            Some(&serde_json::json!({ "session": true }))
        );
    }

    #[test]
    fn session_state_snapshot_absent_requested_scope_is_none() {
        let mut snapshot = super::StoredSessionStateSnapshot::new(1);
        snapshot
            .set_plugin_state(
                "todo_write",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({ "todos": [] }),
            )
            .unwrap();
        assert_eq!(
            snapshot
                .plugin_payload_for_apply("todo_write", 1, super::StoredStateScope::Session)
                .unwrap(),
            None
        );
    }

    #[test]
    fn session_state_snapshot_enumerates_only_valid_supported_entries() {
        let snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "state_revision": 3,
                "plugins": {
                    "plugin": {
                        "root": { "schema_version": 1, "payload": { "valid": true } },
                        "session": { "schema_version": 2, "payload": { "future": true } },
                        "future_scope": { "schema_version": 1, "payload": { "opaque": true } }
                    },
                    "malformed": {
                        "root": { "schema_version": "invalid", "payload": null }
                    },
                    "malformed_container": null
                }
            }))
            .unwrap();

        let entries = snapshot.plugin_entries_for_apply(1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].plugin, "plugin");
        assert_eq!(entries[0].scope, super::StoredStateScope::Root);
        assert_eq!(entries[0].payload, &serde_json::json!({ "valid": true }));
    }

    #[test]
    fn session_state_snapshot_mutations_preserve_opaque_data() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 3,
            "future_envelope": { "kept": true },
            "plugins": {
                "target": {
                    "root": { "schema_version": 1, "payload": "old", "state_extra": 7 },
                    "future_scope": { "opaque": true },
                    "session": { "malformed": true }
                },
                "malformed_sibling": null,
                "unknown_sibling": { "other": [1, 2, 3] }
            }
        });
        let mut snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();

        snapshot.set_state_revision(4).unwrap();
        snapshot
            .set_plugin_state(
                "target",
                1,
                super::StoredStateScope::Root,
                serde_json::json!("new"),
            )
            .unwrap();
        let after_set = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(after_set["state_revision"], 4);
        assert_eq!(
            after_set["plugins"]["target"]["root"]["state_extra"],
            raw["plugins"]["target"]["root"]["state_extra"]
        );
        assert_eq!(after_set["future_envelope"], raw["future_envelope"]);
        assert_eq!(
            after_set["plugins"]["target"]["future_scope"],
            raw["plugins"]["target"]["future_scope"]
        );
        assert_eq!(
            after_set["plugins"]["target"]["session"],
            raw["plugins"]["target"]["session"]
        );
        assert_eq!(
            after_set["plugins"]["malformed_sibling"],
            raw["plugins"]["malformed_sibling"]
        );
        assert_eq!(
            after_set["plugins"]["unknown_sibling"],
            raw["plugins"]["unknown_sibling"]
        );

        snapshot
            .remove_plugin_state("target", super::StoredStateScope::Root)
            .unwrap();
        let after_remove = serde_json::to_value(snapshot).unwrap();
        assert!(after_remove["plugins"]["target"].get("root").is_none());
        assert_eq!(
            after_remove["plugins"]["target"]["future_scope"],
            raw["plugins"]["target"]["future_scope"]
        );
        assert_eq!(
            after_remove["plugins"]["target"]["session"],
            raw["plugins"]["target"]["session"]
        );
    }

    #[test]
    fn session_state_snapshot_rejects_mutating_malformed_plugin_container() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 3,
            "plugins": {"target": null}
        });
        let mut snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();

        assert!(matches!(
            snapshot.set_plugin_state(
                "target",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({"new": true}),
            ),
            Err(super::SessionStateError::InvalidPluginContainer { .. })
        ));
        assert_eq!(serde_json::to_value(snapshot).unwrap(), raw);
    }

    #[test]
    fn session_state_snapshot_counts_opaque_state_fields_toward_entry_limit() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 3,
            "plugins": {
                "target": {
                    "root": {
                        "schema_version": 1,
                        "payload": null,
                        "opaque": "x".repeat(super::MAX_PLUGIN_STATE_BYTES)
                    }
                }
            }
        });

        assert!(serde_json::from_value::<super::StoredSessionStateSnapshot>(raw).is_err());
    }

    #[test]
    fn session_state_snapshot_counts_malformed_version_as_opaque_state() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "state_revision": 3,
            "plugins": {
                "target": {
                    "root": {
                        "schema_version": "x".repeat(super::MAX_PLUGIN_STATE_BYTES),
                        "payload": null
                    }
                }
            }
        });

        assert!(serde_json::from_value::<super::StoredSessionStateSnapshot>(raw).is_err());
    }

    #[test]
    fn session_state_snapshot_mutations_are_atomic() {
        let mut snapshot = super::StoredSessionStateSnapshot::new(5);
        snapshot
            .set_plugin_state(
                "kept",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({ "value": true }),
            )
            .unwrap();
        let before = serde_json::to_value(&snapshot).unwrap();

        assert!(matches!(
            snapshot.set_state_revision(4),
            Err(super::SessionStateError::StateRevisionRegression { .. })
        ));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);

        assert!(matches!(
            snapshot.set_plugin_state(
                "oversized",
                1,
                super::StoredStateScope::Session,
                Value::String("x".repeat(super::MAX_PLUGIN_STATE_BYTES)),
            ),
            Err(super::SessionStateError::PluginStateTooLarge { .. })
        ));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);

        assert!(matches!(
            snapshot.remove_plugin_state("bad/name", super::StoredStateScope::Root),
            Err(super::SessionStateError::InvalidPluginName { .. })
        ));
        assert_eq!(serde_json::to_value(snapshot).unwrap(), before);
    }

    #[test]
    fn session_state_snapshot_unsupported_envelope_mutations_fail_unchanged() {
        let raw = serde_json::json!({
            "schema_version": 2,
            "state_revision": 9,
            "opaque": { "kept": true }
        });
        let mut snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(raw.clone()).unwrap();

        assert!(matches!(
            snapshot.plugin_entries_for_apply(1),
            Err(super::SessionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
        assert!(matches!(
            snapshot.set_state_revision(10),
            Err(super::SessionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
        assert!(matches!(
            snapshot.set_plugin_state(
                "plugin",
                1,
                super::StoredStateScope::Root,
                serde_json::json!(null),
            ),
            Err(super::SessionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
        assert!(matches!(
            snapshot.remove_plugin_state("plugin", super::StoredStateScope::Root),
            Err(super::SessionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
        assert_eq!(serde_json::to_value(snapshot).unwrap(), raw);
    }

    #[test]
    fn malformed_session_state_does_not_invalidate_session_meta() {
        let raw = serde_json::json!({
            "mode": "plan",
            "plan_written": true,
            "state_snapshot": {
                "schema_version": 1,
                "plugins": {"todo_write": {"root": {"schema_version": 1}}}
            }
        });
        let meta: super::SessionMeta = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(meta.mode, Some(super::StoredMode::Plan));
        assert!(meta.plan_written);
        let snapshot = meta.state_snapshot.as_ref().unwrap();
        assert!(matches!(
            snapshot.validate_for_apply(),
            Err(super::SessionStateError::InvalidEnvelope)
        ));
        assert_eq!(
            serde_json::to_value(meta).unwrap()["state_snapshot"],
            raw["state_snapshot"]
        );
    }

    #[test]
    fn session_state_snapshot_requires_envelope_fields() {
        let json = r#"{"schema_version":1,"plugins":{}}"#;
        assert!(serde_json::from_str::<super::StoredSessionStateSnapshot>(json).is_err());
    }

    #[test_case(r#"{"schema_version":1,"state_revision":1,"plugins":{"p":{"root":{"payload":{}}}}}"# ; "missing_plugin_version")]
    #[test_case(r#"{"schema_version":1,"state_revision":1,"plugins":{"p":{"root":{"schema_version":1}}}}"# ; "missing_plugin_payload")]
    fn session_state_snapshot_quarantines_malformed_plugin_fields(json: &str) {
        let snapshot: super::StoredSessionStateSnapshot = serde_json::from_str(json).unwrap();
        assert!(matches!(
            snapshot.plugin_payload_for_apply("p", 1, super::StoredStateScope::Root),
            Err(super::SessionStateError::InvalidPluginState { .. })
        ));
        assert_eq!(serde_json::to_string(&snapshot).unwrap(), json);
    }

    #[test]
    fn session_state_snapshot_empty_plugin_scope_is_absent() {
        let json = r#"{"schema_version":1,"state_revision":1,"plugins":{"p":{}}}"#;
        let snapshot: super::StoredSessionStateSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(
            snapshot
                .plugin_payload_for_apply("p", 1, super::StoredStateScope::Root)
                .unwrap(),
            None
        );
    }

    #[test]
    fn session_state_snapshot_enforces_name_and_entry_boundaries() {
        let mut snapshot = super::StoredSessionStateSnapshot::new(1);
        let maximum_name = "x".repeat(super::MAX_PLUGIN_STATE_NAME_BYTES);
        snapshot
            .set_plugin_state(
                &maximum_name,
                1,
                super::StoredStateScope::Root,
                serde_json::json!(null),
            )
            .unwrap();
        assert!(matches!(
            snapshot.set_plugin_state(
                &"x".repeat(super::MAX_PLUGIN_STATE_NAME_BYTES + 1),
                1,
                super::StoredStateScope::Root,
                serde_json::json!(null),
            ),
            Err(super::SessionStateError::InvalidPluginName { .. })
        ));

        let mut entries = super::StoredSessionStateSnapshot::new(1);
        for index in 0..super::MAX_PLUGIN_STATE_ENTRIES {
            entries
                .set_plugin_state(
                    &format!("plugin_{index}"),
                    1,
                    super::StoredStateScope::Root,
                    serde_json::json!(null),
                )
                .unwrap();
        }
        assert!(matches!(
            entries.set_plugin_state(
                "one_too_many",
                1,
                super::StoredStateScope::Root,
                serde_json::json!(null),
            ),
            Err(super::SessionStateError::TooManyPlugins { .. })
        ));
    }

    #[test_case("" ; "empty")]
    #[test_case("bad/name" ; "path_separator")]
    #[test_case("bad name" ; "whitespace")]
    #[test_case("bad\0name" ; "null_byte")]
    #[test_case("pluginé" ; "non_ascii")]
    fn session_state_snapshot_rejects_unsafe_plugin_names(plugin: &str) {
        let mut snapshot = super::StoredSessionStateSnapshot::new(1);
        assert!(matches!(
            snapshot.set_plugin_state(
                plugin,
                1,
                super::StoredStateScope::Root,
                serde_json::json!(null),
            ),
            Err(super::SessionStateError::InvalidPluginName { .. })
        ));
    }

    #[test]
    fn empty_plugin_scope_maps_do_not_consume_state_entry_quota() {
        let mut plugins = serde_json::Map::new();
        for index in 0..super::MAX_PLUGIN_STATE_ENTRIES {
            plugins.insert(format!("empty_{index}"), serde_json::json!({}));
        }
        let mut snapshot: super::StoredSessionStateSnapshot =
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "state_revision": 1,
                "plugins": plugins,
            }))
            .unwrap();
        snapshot
            .set_plugin_state(
                "usable",
                1,
                super::StoredStateScope::Root,
                serde_json::json!({ "value": true }),
            )
            .unwrap();
        assert_eq!(
            snapshot
                .plugin_payload_for_apply("usable", 1, super::StoredStateScope::Root)
                .unwrap(),
            Some(&serde_json::json!({ "value": true }))
        );
    }

    #[test]
    fn session_state_snapshot_enforces_payload_and_aggregate_boundaries() {
        let mut payload = super::StoredSessionStateSnapshot::new(1);
        payload
            .set_plugin_state(
                "maximum",
                1,
                super::StoredStateScope::Root,
                Value::String("x".repeat(super::MAX_PLUGIN_STATE_BYTES - 2)),
            )
            .unwrap();
        assert!(matches!(
            payload.set_plugin_state(
                "oversized",
                1,
                super::StoredStateScope::Root,
                Value::String("x".repeat(super::MAX_PLUGIN_STATE_BYTES - 1)),
            ),
            Err(super::SessionStateError::PluginStateTooLarge { .. })
        ));

        let mut aggregate = super::StoredSessionStateSnapshot::new(1);
        for index in 0..4 {
            aggregate
                .set_plugin_state(
                    &format!("large_{index}"),
                    1,
                    super::StoredStateScope::Root,
                    Value::String("x".repeat(240_000)),
                )
                .unwrap();
        }
        assert!(matches!(
            aggregate.set_plugin_state(
                "aggregate_overflow",
                1,
                super::StoredStateScope::Root,
                Value::String("x".repeat(240_000)),
            ),
            Err(super::SessionStateError::SnapshotTooLarge { .. })
        ));
    }

    #[test]
    fn session_meta_backward_compat_defaults() {
        let json = r#"{"mode":"build"}"#;
        let meta: super::SessionMeta = serde_json::from_str(json).unwrap();
        assert!(meta.thinking.is_none());
        assert!(!meta.fast);
        assert!(!meta.workflow);
    }

    #[test]
    fn queued_message_backward_compat_defaults_semantics() {
        let stored: StoredQueuedMessage =
            serde_json::from_str(r#"{"text":"queued","images":[]}"#).unwrap();
        assert_eq!(stored.text, "queued");
        assert!(stored.mode.is_none());
        assert!(stored.plan_path.is_none());
        assert!(stored.thinking.is_none());
        assert!(!stored.fast);
        assert!(!stored.workflow);
        assert!(!stored.control);
        assert_eq!(stored.delivery, StoredDelivery::TurnEnd);
        assert!(stored.prompt.is_none());
    }

    #[test]
    fn session_meta_persists_through_save_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.meta.thinking = Some(StoredThinking::Budget { tokens: 8192 });
        session.meta.fast = true;
        session.meta.workflow = true;
        session.save_to(dir).unwrap();

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(
            loaded.meta.thinking,
            Some(StoredThinking::Budget { tokens: 8192 })
        );
        assert!(loaded.meta.fast);
        assert!(loaded.meta.workflow);
    }

    #[test]
    fn compressed_v3_scan_uses_final_meta_for_order_and_summary() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut older: TestSession = Session::new("older-model", "/project");
        older.title = "older".into();
        older.updated_at = 100;
        let mut older_log = SessionLog::create(dir, &older).unwrap();

        let mut newer: TestSession = Session::new("newer-model", "/project");
        newer.title = "initial".into();
        newer.updated_at = 50;
        let mut newer_log = SessionLog::create(dir, &newer).unwrap();
        newer.title = "final title".into();
        newer.updated_at = 200;
        newer.messages.push(user_message("update"));
        newer_log.append(&newer).unwrap();

        older.title = "still older".into();
        older.updated_at = 150;
        older.messages.push(assistant_message("update"));
        older_log.append(&older).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, newer.id);
        assert_eq!(list[0].updated_at, 200);
        assert_eq!(list[0].title, "final title");
        assert_eq!(list[0].model, "newer-model");
        assert_eq!(list[0].cwd, "/project");
        assert_eq!(list[1].id, older.id);
        assert_eq!(list[1].updated_at, 150);
    }

    #[test]
    fn scan_lists_session_without_meta_record() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let path = jsonl_path(dir, session.id);
        let header = LogRecord::<String, Value, Value>::Header {
            v: LOG_FORMAT_VERSION,
            id: session.id,
            model: session.model.clone(),
            cwd: session.cwd.clone(),
            created_at: session.created_at,
            parent_id: None,
            title: Some("header title".into()),
        };
        let mut bytes = Vec::new();
        append_record(&mut bytes, &header).unwrap();
        let mut file = File::create(path).unwrap();
        encode_frame(&mut file, &bytes).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "header title");
        assert_eq!(list[0].updated_at, 0);
    }

    #[test]
    fn scan_skips_corrupt_zstd_session() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        fs::write(
            jsonl_path(dir, session.id),
            [0x28, 0xb5, 0x2f, 0xfd, 0xff, 0xff, 0xff, 0xff],
        )
        .unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn scan_handles_large_meta_record() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.messages.push(user_message("msg"));
        let mut log = SessionLog::create(dir, &session).unwrap();

        session.title = "big-meta".into();
        session.meta.input_draft = Some("x".repeat(8192));
        session.messages.push(assistant_message("reply"));
        log.append(&session).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "big-meta");
    }

    #[test]
    fn latest_fallback_rewrites_cwd_index() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let mut older: TestSession = Session::new("m", "/project");
        older.title = "older".into();
        save_with_time(&mut older, dir, 1000);

        let mut newer: TestSession = Session::new("m", "/project");
        newer.title = "newer".into();
        save_with_time(&mut newer, dir, 2000);

        fs::remove_file(dir.join(CWD_INDEX_FILE)).unwrap();

        let latest = TestSession::latest_in("/project", dir).unwrap().unwrap();
        assert_eq!(latest.id, newer.id);

        let index = load_cwd_index(dir);
        assert_eq!(index.get("/project"), Some(&newer.id.to_string()));
    }

    #[test]
    fn session_log_open_updates_missing_cwd_index() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        SessionLog::create(dir, &session).unwrap();

        fs::remove_file(dir.join(CWD_INDEX_FILE)).unwrap();

        let _ = SessionLog::open::<Value, Value, Value>(dir, session.id).unwrap();

        let index = load_cwd_index(dir);
        assert_eq!(index.get("/project"), Some(&session.id.to_string()));
    }

    #[test]
    fn session_log_compact_updates_cwd_index() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let session: TestSession = Session::new("m", "/project");
        let mut log = SessionLog::create(dir, &session).unwrap();

        fs::remove_file(dir.join(CWD_INDEX_FILE)).unwrap();

        log.compact(dir, &session).unwrap();

        let index = load_cwd_index(dir);
        assert_eq!(index.get("/project"), Some(&session.id.to_string()));
    }

    #[test]
    fn scan_header_skips_huge_non_meta_records() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.title = "huge-output".into();
        session
            .tool_outputs
            .insert("tool-1".into(), Value::String("x".repeat(1_000_000)));
        SessionLog::create(dir, &session).unwrap();

        let list = TestSession::list_in("/project", dir).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "huge-output");
    }

    #[test]
    fn session_load_tolerates_corrupt_fusion_in_meta() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("m", "/project");
        session.title = "old".into();
        session.meta.fusion = Some(StoredFusionUsage {
            lead_cost: 0.12,
            sidekick_cost: 0.34,
            lead_usage: StoredTokenUsage {
                input: 1,
                output: 2,
                cache_creation: 0,
                cache_read: 0,
            },
            sidekick_usage: StoredTokenUsage::default(),
            delegation_count: 1,
            compact_count: 0,
            final_lane: "lead".into(),
        });
        session.save_to(dir).unwrap();

        // Append a corrupt meta record where lead_cost is a map instead of an f64.
        let bad_meta = br#"{"t":"meta","title":"updated","token_usage":null,"updated_at":1,"log_appends":0,"fusion":{"lead_cost":{"foo":"bar"}}}"#;
        let path = jsonl_path(dir, session.id);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        encode_frame(&mut file, bad_meta).unwrap();
        drop(file);

        let loaded = TestSession::load_from(session.id, dir).unwrap();
        assert_eq!(loaded.title, "updated");
        assert!(loaded.meta.fusion.is_none());
    }

    #[test_case(EffortDialectId::Standard ; "standard")]
    #[test_case(EffortDialectId::OpenaiExtended ; "openai_extended")]
    #[test_case(EffortDialectId::PreferHigh ; "prefer_high")]
    #[test_case(EffortDialectId::HighOnly ; "high_only")]
    #[test_case(EffortDialectId::Glm ; "glm")]
    #[test_case(EffortDialectId::DeepSeek ; "deep_seek")]
    #[test_case(EffortDialectId::AnthropicAdaptive ; "anthropic_adaptive")]
    #[test_case(EffortDialectId::TensorX ; "tensor_x")]
    fn effort_dialect_id_round_trip(id: EffortDialectId) {
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<EffortDialectId>(&json).unwrap(), id);
    }

    #[test]
    fn effort_dialect_id_parses_kebab_case() {
        let parsed: EffortDialectId = serde_json::from_str("\"prefer-high\"").unwrap();
        assert_eq!(parsed, EffortDialectId::PreferHigh);
    }

    #[test]
    fn thinking_field_config_round_trip() {
        let config = ThinkingFieldConfig {
            effort_path: Some("reasoning.effort".into()),
            budget_path: Some("generationConfig.thinkingConfig.thinkingBudget".into()),
            budget_max: Some(32_768),
            toggles: vec![ToggleEntry {
                path: "thinking".into(),
                on: Some(serde_json::json!({"type": "enabled"})),
                off: Some(serde_json::json!({"type": "disabled"})),
                adaptive: Some(serde_json::json!({"type": "adaptive"})),
                budget_key: Some("budget_tokens".into()),
            }],
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<ThinkingFieldConfig>(&json).unwrap(),
            config
        );
    }

    #[test]
    fn empty_thinking_config_and_body_override_serialize_empty() {
        assert_eq!(
            serde_json::to_string(&ThinkingFieldConfig::default()).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::to_string(&BodyOverride::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn body_override_round_trip() {
        let override_config = BodyOverride {
            defaults: Some(serde_json::json!({"chat_template_kwargs": {"enable_thinking": true}})),
            replace: Some(serde_json::json!({"max_tokens": 8192})),
            filter: vec!["context_management".into()],
        };
        let json = serde_json::to_string(&override_config).unwrap();
        assert_eq!(
            serde_json::from_str::<BodyOverride>(&json).unwrap(),
            override_config
        );
    }
}
