use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{DataDir, StorageError, atomic_write, now_epoch};

const SESSION_VERSION: u32 = 1;
const SESSIONS_DIR: &str = "sessions";
const CWD_INDEX_FILE: &str = "cwd_latest.json";
const DEFAULT_TITLE: &str = "New session";
const MAX_TITLE_LEN: usize = 60;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("incompatible session version {found} (expected {SESSION_VERSION})")]
    VersionMismatch { found: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session<M, U, T> {
    pub version: u32,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: String,
    pub messages: Vec<M>,
    pub token_usage: U,
    #[serde(default = "HashMap::new")]
    pub tool_outputs: HashMap<String, T>,
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

pub trait TitleSource {
    fn first_user_text(&self) -> Option<&str>;
}

pub fn generate_title<M: TitleSource>(messages: &[M]) -> String {
    let first_user_text = messages.iter().find_map(|m| m.first_user_text());

    let Some(text) = first_user_text.map(str::trim).filter(|t| !t.is_empty()) else {
        return DEFAULT_TITLE.into();
    };

    if text.len() <= MAX_TITLE_LEN {
        return text.to_string();
    }

    let boundary = text.floor_char_boundary(MAX_TITLE_LEN);
    let truncated = &text[..boundary];
    match truncated.rfind(' ') {
        Some(pos) if pos > MAX_TITLE_LEN / 2 => format!("{}…", &truncated[..pos]),
        _ => format!("{truncated}…"),
    }
}

fn load_cwd_index(dir: &Path) -> HashMap<String, String> {
    fs::read(dir.join(CWD_INDEX_FILE))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn update_cwd_index(dir: &Path, cwd: &str, session_id: &str) -> Result<(), StorageError> {
    let mut index = load_cwd_index(dir);
    index.insert(cwd.to_string(), session_id.to_string());
    atomic_write(&dir.join(CWD_INDEX_FILE), &serde_json::to_vec(&index)?)
}

fn scan_headers(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, StorageError> {
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

fn json_entries(dir: &Path) -> Result<Vec<PathBuf>, StorageError> {
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
            id: uuid::Uuid::new_v4().to_string(),
            title: DEFAULT_TITLE.into(),
            cwd: cwd.into(),
            model: model.into(),
            messages: Vec::new(),
            token_usage: U::default(),
            tool_outputs: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn save(&mut self, dir: &DataDir) -> Result<(), SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        self.save_to(&sessions_dir)
    }

    pub fn save_to(&mut self, dir: &Path) -> Result<(), SessionError> {
        fs::create_dir_all(dir).map_err(StorageError::from)?;
        self.updated_at = now_epoch();
        let path = dir.join(format!("{}.json", self.id));
        let data = serde_json::to_vec(self).map_err(StorageError::from)?;
        atomic_write(&path, &data)?;
        update_cwd_index(dir, &self.cwd, &self.id)?;
        Ok(())
    }

    pub fn load(id: &str, dir: &DataDir) -> Result<Self, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::load_from(id, &sessions_dir)
    }

    pub fn load_from(id: &str, dir: &Path) -> Result<Self, SessionError> {
        let path = dir.join(format!("{id}.json"));
        if !path.exists() {
            return Err(StorageError::NotFound(id.into()).into());
        }
        let data = fs::read(&path).map_err(StorageError::from)?;
        let session: Self = serde_json::from_slice(&data).map_err(StorageError::from)?;
        if session.version != SESSION_VERSION {
            return Err(SessionError::VersionMismatch {
                found: session.version,
            });
        }
        Ok(session)
    }

    pub fn list(cwd: &str, dir: &DataDir) -> Result<Vec<SessionSummary>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::list_in(cwd, &sessions_dir)
    }

    pub fn list_in(cwd: &str, dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
        let mut summaries = scan_headers(cwd, dir)?;
        summaries.sort_unstable_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    pub fn latest(cwd: &str, dir: &DataDir) -> Result<Option<Self>, SessionError> {
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
        Self::latest_in(cwd, &sessions_dir)
    }

    pub fn latest_in(cwd: &str, dir: &Path) -> Result<Option<Self>, SessionError> {
        let index = load_cwd_index(dir);
        if let Some(id) = index.get(cwd)
            && let Ok(s) = Self::load_from(id, dir)
        {
            return Ok(Some(s));
        }
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
    use serde_json::Value;
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
        serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": text}]
        })
    }

    #[test]
    fn roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession =
            Session::new("anthropic/claude-sonnet-4", "/home/test/project");
        session.messages.push(user_message("hello"));
        session.save_to(dir).unwrap();

        let loaded = TestSession::load_from(&session.id, dir).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.model, "anthropic/claude-sonnet-4");
        assert_eq!(loaded.cwd, "/home/test/project");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.version, SESSION_VERSION);
    }

    #[test]
    fn load_nonexistent_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = TestSession::load_from("nonexistent-id", tmp.path()).unwrap_err();
        assert!(matches!(
            err,
            SessionError::Storage(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn load_wrong_version_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let mut session: TestSession = Session::new("test/model", "/tmp");
        session.version = 999;
        let path = dir.join(format!("{}.json", session.id));
        fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();

        let err = TestSession::load_from(&session.id, dir).unwrap_err();
        assert!(matches!(err, SessionError::VersionMismatch { found: 999 }));
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
    }

    fn save_with_time(session: &mut TestSession, dir: &Path, time: u64) {
        session.updated_at = time;
        let path = dir.join(format!("{}.json", session.id));
        fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();
        update_cwd_index(dir, &session.cwd, &session.id).unwrap();
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
    fn title_extraction(input: &str, expected: &str) {
        let messages: Vec<Value> = if input.is_empty() {
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
}
