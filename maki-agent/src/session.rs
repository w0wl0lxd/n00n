use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use maki_providers::{Message, TokenUsage};

use crate::ToolOutput;

const SESSION_VERSION: u32 = 1;
const SESSIONS_DIR: &str = "sessions";
const DEFAULT_TITLE: &str = "New session";
const MAX_TITLE_LEN: usize = 60;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("incompatible session version {found} (expected {SESSION_VERSION})")]
    VersionMismatch { found: u32 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("cannot resolve sessions directory")]
    NoSessionsDir,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub token_usage: TokenUsage,
    #[serde(default)]
    pub tool_outputs: HashMap<String, ToolOutput>,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
}

#[derive(Deserialize)]
struct SessionHeader {
    version: u32,
    id: String,
    title: String,
    cwd: String,
    updated_at: u64,
}

fn default_sessions_dir() -> Result<PathBuf, SessionError> {
    let dir = maki_providers::data_dir()
        .map_err(|_| SessionError::NoSessionsDir)?
        .join(SESSIONS_DIR);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn generate_title(messages: &[Message]) -> String {
    let first_user_text = messages.iter().find_map(|m| {
        if matches!(m.role, maki_providers::Role::User) {
            m.content.iter().find_map(|b| match b {
                maki_providers::ContentBlock::Text { text } if !text.is_empty() => {
                    Some(text.as_str())
                }
                _ => None,
            })
        } else {
            None
        }
    });

    let Some(text) = first_user_text else {
        return DEFAULT_TITLE.into();
    };

    let trimmed = text.trim();
    if trimmed.len() <= MAX_TITLE_LEN {
        return trimmed.to_string();
    }

    let boundary = trimmed.floor_char_boundary(MAX_TITLE_LEN);
    let truncated = &trimmed[..boundary];
    match truncated.rfind(' ') {
        Some(pos) if pos > MAX_TITLE_LEN / 2 => format!("{}…", &truncated[..pos]),
        _ => format!("{truncated}…"),
    }
}

fn scan_headers(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
    let mut out = Vec::new();
    for path in json_entries(dir)? {
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        let Ok(h) = serde_json::from_slice::<SessionHeader>(&data) else {
            continue;
        };
        if h.version != SESSION_VERSION || h.cwd != cwd {
            continue;
        }
        out.push(SessionSummary {
            id: h.id,
            title: h.title,
            updated_at: h.updated_at,
        });
    }
    Ok(out)
}

fn json_entries(dir: &Path) -> Result<Vec<PathBuf>, SessionError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            entries.push(path);
        }
    }
    Ok(entries)
}

impl Session {
    pub fn new(model: &str, cwd: &str) -> Self {
        let now = now_epoch();
        Self {
            version: SESSION_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            title: DEFAULT_TITLE.into(),
            cwd: cwd.into(),
            model: model.into(),
            messages: Vec::new(),
            token_usage: TokenUsage::default(),
            tool_outputs: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn save(&mut self) -> Result<(), SessionError> {
        self.save_to(&default_sessions_dir()?)
    }

    pub fn save_to(&mut self, dir: &Path) -> Result<(), SessionError> {
        fs::create_dir_all(dir)?;
        self.updated_at = now_epoch();
        let path = dir.join(format!("{}.json", self.id));
        let tmp = path.with_extension("tmp");
        let data = serde_json::to_vec(self)?;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn load(id: &str) -> Result<Self, SessionError> {
        Self::load_from(id, &default_sessions_dir()?)
    }

    pub fn load_from(id: &str, dir: &Path) -> Result<Self, SessionError> {
        let path = dir.join(format!("{id}.json"));
        if !path.exists() {
            return Err(SessionError::NotFound(id.into()));
        }
        let data = fs::read(&path)?;
        let session: Self = serde_json::from_slice(&data)?;
        if session.version != SESSION_VERSION {
            return Err(SessionError::VersionMismatch {
                found: session.version,
            });
        }
        Ok(session)
    }

    pub fn list(cwd: &str) -> Result<Vec<SessionSummary>, SessionError> {
        Self::list_in(cwd, &default_sessions_dir()?)
    }

    pub fn list_in(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
        let mut summaries = scan_headers(cwd, dir)?;
        summaries.sort_unstable_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    pub fn latest(cwd: &str) -> Result<Option<Self>, SessionError> {
        Self::latest_in(cwd, &default_sessions_dir()?)
    }

    pub fn latest_in(cwd: &str, dir: &Path) -> Result<Option<Self>, SessionError> {
        let summaries = scan_headers(cwd, dir)?;
        let latest = summaries.into_iter().max_by_key(|s| s.updated_at);
        match latest {
            Some(s) => Self::load_from(&s.id, dir).map(Some),
            None => Ok(None),
        }
    }

    pub fn update_title_if_default(&mut self) {
        if self.title == DEFAULT_TITLE {
            self.title = generate_title(&self.messages);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_providers::{ContentBlock, Role};
    use tempfile::TempDir;
    use test_case::test_case;

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session = Session::new("anthropic/claude-sonnet-4", "/home/test/project");
        session.messages.push(user_message("hello"));
        session.token_usage.input = 100;
        session.save_to(dir).unwrap();

        let loaded = Session::load_from(&session.id, dir).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model, "anthropic/claude-sonnet-4");
        assert_eq!(loaded.cwd, "/home/test/project");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.token_usage.input, 100);
        assert_eq!(loaded.version, SESSION_VERSION);
    }

    #[test]
    fn load_nonexistent_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = Session::load_from("nonexistent-id", tmp.path()).unwrap_err();
        assert!(matches!(err, SessionError::NotFound(_)));
    }

    #[test]
    fn load_wrong_version_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session = Session::new("test/model", "/tmp");
        session.version = 999;
        let path = dir.join(format!("{}.json", session.id));
        fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();

        let err = Session::load_from(&session.id, dir).unwrap_err();
        assert!(matches!(err, SessionError::VersionMismatch { found: 999 }));
    }

    #[test]
    fn list_filters_by_cwd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1 = Session::new("m", "/project-a");
        let mut s2 = Session::new("m", "/project-b");
        let mut s3 = Session::new("m", "/project-a");
        s1.save_to(dir).unwrap();
        s2.save_to(dir).unwrap();
        s3.save_to(dir).unwrap();

        let list = Session::list_in("/project-a", dir).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|s| s.id != s2.id));
    }

    fn save_with_time(session: &mut Session, dir: &Path, time: u64) {
        session.updated_at = time;
        let path = dir.join(format!("{}.json", session.id));
        fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();
    }

    #[test]
    fn latest_returns_most_recent_for_cwd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut s1 = Session::new("m", "/project");
        s1.title = "first".into();
        save_with_time(&mut s1, dir, 1000);

        let mut s2 = Session::new("m", "/other");
        save_with_time(&mut s2, dir, 2000);

        let mut s3 = Session::new("m", "/project");
        s3.title = "latest".into();
        save_with_time(&mut s3, dir, 3000);

        let latest = Session::latest_in("/project", dir).unwrap().unwrap();
        assert_eq!(latest.title, "latest");
    }

    #[test_case("short title", "short title" ; "short_passthrough")]
    #[test_case("", DEFAULT_TITLE ; "empty_defaults")]
    #[test_case(
        "This is a very long title that exceeds the sixty character limit and should be truncated at a word boundary",
        "This is a very long title that exceeds the sixty character…"
        ; "long_truncates_at_word"
    )]
    fn title_extraction(input: &str, expected: &str) {
        let messages = if input.is_empty() {
            vec![]
        } else {
            vec![user_message(input)]
        };
        assert_eq!(generate_title(&messages), expected);
    }

    #[test]
    fn title_unicode_safe() {
        let input = "あ".repeat(100);
        let title = generate_title(&[user_message(&input)]);
        assert!(title.len() <= MAX_TITLE_LEN * 4);
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn update_title_if_default_only_replaces_default() {
        let mut session = Session::new("m", "/tmp");
        session.messages.push(user_message("my topic"));
        session.update_title_if_default();
        assert_eq!(session.title, "my topic");

        session.update_title_if_default();
        assert_eq!(session.title, "my topic", "should not revert to default");
    }
}
