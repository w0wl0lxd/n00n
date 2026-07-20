//! Session persistence with append-only, zstd-compressed JSONL logs.
//!
//! Each session is stored as a canonical `{id}.jsonl`, with one or more zstd frames.
//! The format is crash-safe: on load, any trailing partial frame is discarded.
//! `SessionLog` tracks cursor state to enable O(delta) incremental saves.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::UNIX_EPOCH;

use tracing::warn;

use crate::id::{NoonId, NoonIdParseError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use zstd::stream::{Decoder, Encoder};

use crate::{StateDir, StorageError, atomic_write, now_epoch};

const SESSION_VERSION: u32 = 1;
const LOG_FORMAT_VERSION: u32 = 3;
const COMPRESS_LEVEL: i32 = 3;
pub const SESSIONS_DIR: &str = "sessions";
const CWD_INDEX_FILE: &str = "cwd_latest.json";
const SCAN_CACHE_FILE: &str = "scan_cache_v2.json";
const DEFAULT_TITLE: &str = "New session";
const MAX_TITLE_LEN: usize = 60;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("incompatible session version {found} (expected {expected})")]
    VersionMismatch { found: u32, expected: u32 },
    #[error("session ID mismatch: log owns {log_id}, got {given_id}")]
    IdMismatch { log_id: NoonId, given_id: NoonId },
    #[error("session log {path} has header id {raw_id:?} that is not a valid id: {source}")]
    CorruptHeaderId {
        path: String,
        raw_id: String,
        source: NoonIdParseError,
    },
    #[error("cursor ahead of session (log has {saved}, session has {actual}); compact required")]
    CursorAhead { saved: usize, actual: usize },
}

/// Per-model token breakdown entry. Mirrors the four usage counters tracked by
/// the active provider; kept storage-local to avoid a circular dependency on
/// `noon-providers`.
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
    pub fn total_input(&self) -> u32 {
        self.input + self.cache_read + self.cache_creation
    }

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session<M, U, T> {
    pub version: u32,
    pub id: NoonId,
    pub title: String,
    pub cwd: String,
    pub model: String,
    pub messages: Vec<M>,
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
    pub id: NoonId,
    pub title: String,
    pub updated_at: u64,
    pub cwd: String,
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
    pub fn budget(self, max: u32) -> u32 {
        let max = max.max(MIN_THINKING_BUDGET);
        let tokens = (u64::from(max) * u64::from(self.percent()) / 100) as u32;
        tokens.clamp(MIN_THINKING_BUDGET, max)
    }

    /// Inverse of [`Self::budget`]: the lowest level whose percentage covers
    /// `n` tokens out of `max`. Budgets at or above `max` map to `Max`.
    pub fn from_budget(n: u32, max: u32) -> Self {
        let pct = u64::from(n).saturating_mul(100) / u64::from(max.max(1));
        Self::ALL
            .into_iter()
            .find(|e| u64::from(e.percent()) >= pct)
            .unwrap_or(Self::Max)
    }

    /// Nearest level a provider accepts: exact match keeps `self`, otherwise
    /// the closest lower supported level, otherwise the lowest supported.
    /// An empty `supported` list returns `self` unchanged (dynamic model
    /// listings may not declare supported efforts).
    pub fn snap(self, supported: &[Self]) -> Self {
        if supported.is_empty() || supported.contains(&self) {
            return self;
        }
        supported
            .iter()
            .rev()
            .find(|&&e| e < self)
            .copied()
            .unwrap_or(supported[0])
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum StoredThinking {
    Off,
    Adaptive,
    Effort { level: Effort },
    Budget { tokens: u32 },
}

impl StoredThinking {
    /// The one string-to-thinking parser: `/thinking`, `always_thinking`
    /// config, and the Lua agent API all delegate here.
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
pub fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn generate_title<M: TitleSource>(messages: &[M]) -> String {
    let first_user_text = messages.iter().find_map(|m| m.first_user_text());

    let Some(text) = first_user_text.map(str::trim).filter(|t| !t.is_empty()) else {
        return DEFAULT_TITLE.into();
    };
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

// -- JSONL record types --

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum LogRecord<M, U, T> {
    #[serde(rename = "header")]
    Header {
        v: u32,
        id: NoonId,
        #[serde(default)]
        model: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        created_at: u64,
    },
    #[serde(rename = "msg")]
    Msg { d: M },
    #[serde(rename = "out")]
    Out { id: String, d: T },
    #[serde(rename = "sub_msg")]
    SubMsg { sub: String, d: M },
    #[serde(rename = "meta")]
    Meta {
        title: String,
        token_usage: U,
        updated_at: u64,
        #[serde(flatten)]
        meta: SessionMeta,
    },
}

// -- SessionLog: append-only persistence --

pub struct SessionLog {
    session_id: NoonId,
    dir: PathBuf,
    file: File,
    saved_msg_count: usize,
    saved_tool_ids: HashSet<String>,
    saved_sub_msg_counts: HashMap<String, usize>,
    /// Serialized trailing meta record; lets `append` persist meta-only
    /// changes (title, draft, updated_at) instead of dropping them.
    saved_meta: Vec<u8>,
    saved_title: String,
}

fn sub_msg_snapshot<M>(map: &HashMap<String, Vec<M>>) -> HashMap<String, usize> {
    map.iter().map(|(k, v)| (k.clone(), v.len())).collect()
}

impl SessionLog {
    pub fn create<M, U, T>(dir: &Path, session: &Session<M, U, T>) -> Result<Self, SessionError>
    where
        M: Serialize,
        U: Serialize,
        T: Serialize,
    {
        let file = write_session_file(dir, session)?;
        update_cwd_index(dir, &session.cwd, session.id)?;
        Ok(Self::cursor_from(dir, session, file))
    }

    pub fn open<M, U, T>(
        dir: &Path,
        session_id: NoonId,
    ) -> Result<(Session<M, U, T>, Self), SessionError>
    where
        M: Serialize + DeserializeOwned,
        U: Serialize + DeserializeOwned + Default,
        T: Serialize + DeserializeOwned,
    {
        let path = locate_session_file(dir, session_id)
            .ok_or_else(|| SessionError::from(StorageError::NotFound(session_id.to_string())))?;
        let session = load_session_at::<M, U, T>(&path)?;

        if session.id != session_id {
            return Err(SessionError::IdMismatch {
                log_id: session.id,
                given_id: session_id,
            });
        }

        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(StorageError::from)?;
        let log = Self::cursor_from(dir, &session, file);
        Ok((session, log))
    }

    pub fn session_id(&self) -> NoonId {
        self.session_id
    }

    pub fn append<M, U, T>(&mut self, session: &Session<M, U, T>) -> Result<(), SessionError>
    where
        M: Serialize,
        U: Serialize,
        T: Serialize,
    {
        self.require_same_id(session)?;

        if session.title != self.saved_title {
            let dir = self.dir.clone();
            return self.compact(&dir, session);
        }

        if self.cursor_ahead(session) {
            return Err(SessionError::CursorAhead {
                saved: self.saved_msg_count,
                actual: session.messages.len(),
            });
        }

        let mut buf = Vec::new();
        let mut new_msg_count = self.saved_msg_count;
        let mut new_tool_ids = Vec::new();

        for msg in &session.messages[self.saved_msg_count..] {
            append_record(&mut buf, &LogRecord::<&M, &U, &T>::Msg { d: msg })?;
            new_msg_count += 1;
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

        let mut new_sub_counts: Vec<(String, usize)> = Vec::new();
        for (sub_id, msgs) in &session.subagent_messages {
            let saved = self.saved_sub_msg_counts.get(sub_id).copied().unwrap_or(0);
            for msg in &msgs[saved..] {
                append_record(
                    &mut buf,
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

        let meta_bytes = meta_record_bytes(session)?;
        let meta_changed = meta_bytes != self.saved_meta;
        if buf.is_empty() && !meta_changed {
            return Ok(());
        }

        if !buf.is_empty() || meta_changed {
            buf.extend_from_slice(&meta_bytes);
        }

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

        self.saved_msg_count = new_msg_count;
        self.saved_tool_ids.extend(new_tool_ids);
        for (sub_id, count) in new_sub_counts {
            self.saved_sub_msg_counts.insert(sub_id, count);
        }
        if meta_changed {
            self.saved_meta = meta_bytes;
        }
        self.saved_title = session.title.clone();

        Ok(())
    }

    pub fn compact<M, U, T>(
        &mut self,
        dir: &Path,
        session: &Session<M, U, T>,
    ) -> Result<(), SessionError>
    where
        M: Serialize,
        U: Serialize,
        T: Serialize,
    {
        self.require_same_id(session)?;

        let path = jsonl_path(dir, session.id);
        atomic_write(&path, &full_session_bytes(session)?)?;

        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(StorageError::from)?;
        *self = Self::cursor_from(dir, session, file);

        Ok(())
    }

    fn cursor_from<M, U, T>(dir: &Path, session: &Session<M, U, T>, file: File) -> Self
    where
        M: Serialize,
        U: Serialize,
        T: Serialize,
    {
        Self {
            session_id: session.id,
            dir: dir.to_path_buf(),
            file,
            saved_msg_count: session.messages.len(),
            saved_tool_ids: session.tool_outputs.keys().cloned().collect(),
            saved_sub_msg_counts: sub_msg_snapshot(&session.subagent_messages),
            saved_meta: meta_record_bytes(session).unwrap_or_default(),
            saved_title: session.title.clone(),
        }
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
        self.saved_msg_count > session.messages.len()
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

fn meta_record_bytes<M, U, T>(session: &Session<M, U, T>) -> Result<Vec<u8>, SessionError>
where
    M: Serialize,
    U: Serialize,
    T: Serialize,
{
    let mut buf = Vec::new();
    append_record(
        &mut buf,
        &LogRecord::<&M, &U, &T>::Meta {
            title: session.title.clone(),
            token_usage: &session.token_usage,
            updated_at: session.updated_at,
            meta: session.meta.clone(),
        },
    )?;
    Ok(buf)
}

fn write_session_file<M, U, T>(dir: &Path, session: &Session<M, U, T>) -> Result<File, SessionError>
where
    M: Serialize,
    U: Serialize,
    T: Serialize,
{
    fs::create_dir_all(dir).map_err(StorageError::from)?;
    let path = jsonl_path(dir, session.id);
    let tmp = path.with_extension("jsonl.tmp");
    let mut tmp_file = File::create(&tmp).map_err(StorageError::from)?;
    write_full_session(&mut tmp_file, session)?;
    tmp_file.sync_data().map_err(StorageError::from)?;
    fs::rename(&tmp, &path).map_err(StorageError::from)?;
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
    M: Serialize,
    U: Serialize,
    T: Serialize,
{
    let mut buf = Vec::new();
    append_record(
        &mut buf,
        &LogRecord::<&M, &U, &T>::Header {
            v: LOG_FORMAT_VERSION,
            id: session.id,
            model: session.model.clone(),
            cwd: session.cwd.clone(),
            title: Some(session.title.clone()),
            created_at: session.created_at,
        },
    )?;
    for msg in &session.messages {
        append_record(&mut buf, &LogRecord::<&M, &U, &T>::Msg { d: msg })?;
    }
    for (id, output) in &session.tool_outputs {
        append_record(
            &mut buf,
            &LogRecord::<&M, &U, &T>::Out {
                id: id.clone(),
                d: output,
            },
        )?;
    }
    for (sub_id, msgs) in &session.subagent_messages {
        for msg in msgs {
            append_record(
                &mut buf,
                &LogRecord::<&M, &U, &T>::SubMsg {
                    sub: sub_id.clone(),
                    d: msg,
                },
            )?;
        }
    }
    buf.extend_from_slice(&meta_record_bytes(session)?);
    encode_frame(file, &buf)
}

fn full_session_bytes<M, U, T>(session: &Session<M, U, T>) -> Result<Vec<u8>, SessionError>
where
    M: Serialize,
    U: Serialize,
    T: Serialize,
{
    let mut bytes = Vec::new();
    write_full_session(&mut bytes, session)?;
    Ok(bytes)
}

fn append_record<R: Serialize>(buf: &mut Vec<u8>, record: &R) -> Result<(), SessionError> {
    serde_json::to_writer(&mut *buf, record).map_err(StorageError::from)?;
    buf.push(b'\n');
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

fn parse_records<M, U, T>(data: &[u8], path: &Path) -> Result<Session<M, U, T>, SessionError>
where
    M: DeserializeOwned,
    U: DeserializeOwned + Default,
    T: DeserializeOwned,
{
    let reader = BufReader::new(data);
    let mut line_count = 0usize;

    let mut id: Option<NoonId> = None;
    let mut model = String::new();
    let mut cwd = String::new();
    let mut created_at = 0u64;
    let mut messages = Vec::new();
    let mut tool_outputs = HashMap::new();
    let mut subagent_messages: HashMap<String, Vec<M>> = HashMap::new();
    let mut title = DEFAULT_TITLE.to_string();
    let mut token_usage = U::default();
    let mut updated_at = 0u64;
    let mut meta = SessionMeta::default();
    let mut got_header = false;

    for line_result in reader.lines() {
        let line = line_result.map_err(StorageError::from)?;
        line_count += 1;
        if line.is_empty() {
            continue;
        }
        let record: LogRecord<M, U, T> = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                if !got_header
                    && let Ok(RawTag::Header { id: raw_id }) = serde_json::from_str::<RawTag>(&line)
                    && let Err(source) = raw_id.parse::<NoonId>()
                {
                    return Err(SessionError::CorruptHeaderId {
                        path: path.display().to_string(),
                        raw_id,
                        source,
                    });
                }
                warn!(
                    path = %path.display(),
                    error = %e,
                    line = line_count,
                    "skipping unrecognized JSONL record",
                );
                continue;
            }
        };
        match record {
            LogRecord::Header {
                v,
                id: h_id,
                model: h_model,
                cwd: h_cwd,
                title: h_title,
                created_at: h_created,
            } => {
                if v != LOG_FORMAT_VERSION {
                    return Err(SessionError::VersionMismatch {
                        found: v,
                        expected: LOG_FORMAT_VERSION,
                    });
                }
                id = Some(h_id);
                model = h_model;
                cwd = h_cwd;
                created_at = h_created;
                if let Some(t) = h_title {
                    title = t;
                }
                got_header = true;
            }
            LogRecord::Msg { d } => messages.push(d),
            LogRecord::Out { id: out_id, d } => {
                tool_outputs.insert(out_id, d);
            }
            LogRecord::SubMsg { sub, d } => {
                subagent_messages.entry(sub).or_default().push(d);
            }
            LogRecord::Meta {
                title: m_title,
                token_usage: m_usage,
                updated_at: m_updated,
                meta: m_meta,
            } => {
                title = m_title;
                token_usage = m_usage;
                updated_at = m_updated;
                meta = m_meta;
            }
        }
    }

    let id = id.ok_or(StorageError::NotFound(path.display().to_string()))?;

    Ok(Session {
        version: SESSION_VERSION,
        id,
        title: normalize_title(&title),
        cwd,
        model,
        messages,
        token_usage,
        tool_outputs,
        subagent_messages,
        meta,
        created_at,
        updated_at,
    })
}

fn encode_frame<W: Write>(file: &mut W, bytes: &[u8]) -> Result<(), SessionError> {
    let mut enc = Encoder::new(file, COMPRESS_LEVEL).map_err(StorageError::from)?;
    enc.write_all(bytes).map_err(StorageError::from)?;
    enc.finish().map_err(StorageError::from)?;
    Ok(())
}

fn decode_all(data: &[u8]) -> Result<Vec<u8>, SessionError> {
    let mut dec = Decoder::new(data).map_err(StorageError::from)?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 65536];
    loop {
        match dec.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                warn!(error = %e, "truncated zstd frame, recovering complete frames");
                break;
            }
        }
    }
    Ok(out)
}

fn is_zst_data(data: &[u8]) -> bool {
    data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
}

fn read_session_bytes(path: &Path) -> Result<Vec<u8>, SessionError> {
    let data = fs::read(path).map_err(StorageError::from)?;
    let valid = zst_valid_len(&data);
    if valid < data.len() {
        warn!(
            path = %path.display(),
            bytes = data.len() - valid,
            "truncating torn zstd frame tail",
        );
        let _ = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_len(valid as u64));
    }
    decode_all(&data[..valid])
}

fn zst_valid_len(data: &[u8]) -> usize {
    let mut offset = 0usize;
    while offset < data.len() {
        match zstd::zstd_safe::find_frame_compressed_size(&data[offset..]) {
            Ok(size) if size > 0 && offset + size <= data.len() => offset += size,
            _ => break,
        }
    }
    offset
}

// -- CWD index --

fn load_cwd_index(dir: &Path) -> HashMap<String, String> {
    fs::read(dir.join(CWD_INDEX_FILE))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn update_cwd_index(dir: &Path, cwd: &str, session_id: NoonId) -> Result<(), StorageError> {
    let mut index = load_cwd_index(dir);
    let id_str = session_id.to_string();
    if index.get(cwd).is_some_and(|v| *v == id_str) {
        return Ok(());
    }
    index.insert(cwd.to_string(), id_str);
    atomic_write(&dir.join(CWD_INDEX_FILE), &serde_json::to_vec(&index)?)
}

fn jsonl_path(dir: &Path, id: NoonId) -> PathBuf {
    dir.join(format!("{id}.jsonl"))
}

fn file_is_zst(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
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

fn remove_from_cwd_index(dir: &Path, session_id: NoonId) -> Result<(), StorageError> {
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
    id: NoonId,
    #[serde(default)]
    model: String,
    cwd: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ScanRecord {
    Meta {
        title: String,
        updated_at: u64,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScannedHeader {
    id: NoonId,
    cwd: String,
    title: String,
    updated_at: u64,
    model: String,
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
        .unwrap_or_default()
}

fn file_signature(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)?;
    Some((meta.len(), mtime_ms))
}

fn scan_headers(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, StorageError> {
    let mut cache = load_scan_cache(dir);
    let mut fresh = ScanCache::new();
    let mut dirty = false;
    let mut summaries = Vec::new();
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
                let header = scan_zst_header(&path);
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
            summaries.push(SessionSummary {
                id: h.id,
                title: normalize_title(&h.title),
                updated_at: h.updated_at,
                cwd: h.cwd.clone(),
                model: h.model.clone(),
            });
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
    Ok(summaries)
}

fn scan_zst_header(path: &Path) -> Option<ScannedHeader> {
    let data = decode_all(&fs::read(path).ok()?).ok()?;
    let mut lines = data
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let header: ZstHeader = serde_json::from_slice(lines.next()?).ok()?;
    if header.v != LOG_FORMAT_VERSION {
        return None;
    }
    let (meta_title, updated_at) = lines.rev().find_map(|line| {
        if let Ok(ScanRecord::Meta { title, updated_at }) = serde_json::from_slice(line) {
            Some((title, updated_at))
        } else {
            None
        }
    })?;

    Some(ScannedHeader {
        id: header.id,
        cwd: header.cwd,
        title: if meta_title.is_empty() {
            header.title.unwrap_or_else(|| DEFAULT_TITLE.to_string())
        } else {
            meta_title
        },
        updated_at,
        model: header.model,
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
            .and_then(|stem| stem.parse::<NoonId>().ok())
            .is_some_and(|id| jsonl_path(path.parent().unwrap_or(Path::new("")), id) == path)
        && file_is_zst(path)
}

fn locate_session_file(dir: &Path, id: NoonId) -> Option<PathBuf> {
    let path = jsonl_path(dir, id);
    (path.exists() && file_is_zst(&path)).then_some(path)
}

fn load_session_at<M, U, T>(path: &Path) -> Result<Session<M, U, T>, SessionError>
where
    M: DeserializeOwned,
    U: DeserializeOwned + Default,
    T: DeserializeOwned,
{
    parse_records(&read_session_bytes(path)?, path)
}

// -- Session impl --

impl<M, U, T> Session<M, U, T>
where
    M: Serialize + DeserializeOwned + TitleSource,
    U: Serialize + DeserializeOwned + Default,
    T: Serialize + DeserializeOwned,
{
    pub fn new(model: &str, cwd: &str) -> Self {
        let now = now_epoch();
        Self {
            version: SESSION_VERSION,
            id: NoonId::generate(),
            title: DEFAULT_TITLE.into(),
            cwd: cwd.into(),
            model: model.into(),
            messages: Vec::new(),
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

    /// After `messages` is truncated (rewind), state keyed by tool_use_id can
    /// point at calls that no longer exist. On restore that shows up as ghost
    /// subagent tabs and leaked tool outputs, so this drops everything not
    /// reachable from `messages`.
    ///
    /// If you add another field keyed by tool_use_id, prune it here too.
    pub fn prune_orphans(&mut self, tool_ids: impl Fn(&M) -> Vec<String>) {
        let main_ids: HashSet<String> = self.messages.iter().flat_map(&tool_ids).collect();
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

    pub fn save(&mut self, dir: &StateDir) -> Result<(), SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        self.save_to(&sessions_dir)
    }

    pub fn save_to(&mut self, dir: &Path) -> Result<(), SessionError> {
        self.updated_at = now_epoch();
        write_session_file(dir, self)?;
        update_cwd_index(dir, &self.cwd, self.id)?;
        Ok(())
    }

    pub fn load(id: NoonId, dir: &StateDir) -> Result<Self, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::load_from(id, &sessions_dir)
    }

    pub fn load_from(id: NoonId, dir: &Path) -> Result<Self, SessionError> {
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

    pub fn list(cwd: &str, dir: &StateDir) -> Result<Vec<SessionSummary>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::list_in(cwd, &sessions_dir)
    }

    pub fn list_in(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
        let mut summaries = scan_headers(cwd, dir)?;
        summaries.sort_unstable_by_key(|s| Reverse(s.updated_at));
        Ok(summaries)
    }

    pub fn latest(cwd: &str, dir: &StateDir) -> Result<Option<Self>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::latest_in(cwd, &sessions_dir)
    }

    pub fn latest_in(cwd: &str, dir: &Path) -> Result<Option<Self>, SessionError> {
        let cached = load_cwd_index(dir)
            .remove(cwd)
            .and_then(|s| match s.parse::<NoonId>() {
                Ok(id) => Some(id),
                Err(e) => {
                    warn!(error = %e, cwd, "indexed session id unparseable; rescanning");
                    None
                }
            });
        if let Some(id) = cached {
            match Self::load_from(id, dir) {
                Ok(s) => return Ok(Some(s)),
                Err(e) => warn!(error = %e, cwd, "indexed session missing on disk; rescanning"),
            }
        }

        scan_headers(cwd, dir)?
            .into_iter()
            .max_by_key(|s| s.updated_at)
            .map(|s| Self::load_from(s.id, dir).map(Some))
            .unwrap_or(Ok(None))
    }

    pub fn update_title_if_default(&mut self) {
        if self.title == DEFAULT_TITLE {
            self.title = generate_title(&self.messages);
        }
    }

    pub fn delete(id: NoonId, dir: &StateDir) -> Result<(), SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::delete_from(id, &sessions_dir)
    }

    pub fn delete_from(id: NoonId, dir: &Path) -> Result<(), SessionError> {
        let Some(path) = locate_session_file(dir, id) else {
            return Err(StorageError::NotFound(id.to_string()).into());
        };
        try_remove(&path)?;
        remove_from_cwd_index(dir, id)?;
        Ok(())
    }

    pub fn migrate_to_jsonl(dir: &Path, session: &Self) -> Result<SessionLog, SessionError> {
        SessionLog::create(dir, session)
    }
}

#[cfg(test)]
mod tests {
    use super::StoredThinking;
    use super::ThinkingParseError;
    use super::{
        CWD_INDEX_FILE, DEFAULT_TITLE, MAX_TITLE_LEN, SESSION_VERSION, StoredSubagent,
        generate_title, jsonl_path, load_cwd_index, now_epoch, update_cwd_index,
    };
    use super::{SCAN_CACHE_FILE, Session, SessionError, SessionLog, StorageError, TitleSource};
    use crate::id::NoonId;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;
    use test_case::test_case;

    type TestSession = Session<Value, Value, Value>;

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
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession =
            Session::new("anthropic/claude-sonnet-4", "/home/test/project");
        session.messages.push(user_message("hello"));
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
        let id = NoonId::generate();
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
        let id = NoonId::generate();
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
    fn stored_thinking_serde_round_trip(variant: StoredThinking) {
        let json = serde_json::to_string(&variant).unwrap();
        let parsed: StoredThinking = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, variant);
    }

    #[test_case("off", Ok(StoredThinking::Off) ; "off")]
    #[test_case("adaptive", Ok(StoredThinking::Adaptive) ; "adaptive")]
    #[test_case(" adaptive ", Ok(StoredThinking::Adaptive) ; "trims_whitespace")]
    #[test_case("4096", Ok(StoredThinking::Budget { tokens: 4096 }) ; "valid_budget")]
    #[test_case("1", Ok(StoredThinking::Budget { tokens: 1 }) ; "minimum_budget")]
    #[test_case("0", Err(ThinkingParseError::BudgetZero) ; "budget_zero")]
    #[test_case("fast", Err(ThinkingParseError::Unknown("fast".into())) ; "garbage")]
    fn parse_setting(input: &str, expected: Result<StoredThinking, ThinkingParseError>) {
        assert_eq!(StoredThinking::parse_setting(input), expected);
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
}
