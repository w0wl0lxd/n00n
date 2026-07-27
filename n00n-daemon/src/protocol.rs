//! Control-plane wire types. Encoded with `sonic-rs` (not `serde_json`).

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Tui,
    Worker,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tui => write!(f, "tui"),
            Self::Worker => write!(f, "worker"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub backend: BackendKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOpts {
    #[serde(default)]
    pub steer: bool,
    #[serde(default)]
    pub control: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    Health,
    List,
    Status {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<BackendKind>,
    },
    Message {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<BackendKind>,
        #[serde(default)]
        opts: MessageOpts,
    },
    Pause {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<BackendKind>,
    },
    Resume {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<BackendKind>,
    },
    Stop {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<BackendKind>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agents: Option<Vec<AgentRecord>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentRecord>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<sonic_rs::Value>,
    },
    Err {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

impl ControlRequest {
    /// # Errors
    /// Returns a protocol error if the line is not valid JSON for this schema.
    pub fn from_line(line: &str) -> Result<Self, String> {
        sonic_rs::from_str(line.trim()).map_err(|e| e.to_string())
    }

    /// # Errors
    /// Returns a protocol error if encoding fails.
    pub fn to_line(&self) -> Result<String, String> {
        sonic_rs::to_string(self).map_err(|e| e.to_string())
    }
}

impl ControlResponse {
    /// # Errors
    /// Returns a protocol error if the line is not valid JSON for this schema.
    pub fn from_line(line: &str) -> Result<Self, String> {
        sonic_rs::from_str(line.trim()).map_err(|e| e.to_string())
    }

    /// # Errors
    /// Returns a protocol error if encoding fails.
    pub fn to_line(&self) -> Result<String, String> {
        sonic_rs::to_string(self).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn health_ok() -> Self {
        Self::Ok {
            agents: None,
            agent: None,
            version: Some(PROTOCOL_VERSION),
            state: None,
        }
    }

    #[must_use]
    pub fn from_error(err: &crate::ControlError) -> Self {
        let code = match err {
            crate::ControlError::Unsupported { .. } => Some("unsupported".into()),
            crate::ControlError::NotFound(_) => Some("not_found".into()),
            crate::ControlError::InvalidId(_) => Some("invalid_id".into()),
            crate::ControlError::Unavailable(_) => Some("unavailable".into()),
            crate::ControlError::Protocol(_) => Some("protocol".into()),
            crate::ControlError::Forbidden(_) => Some("forbidden".into()),
            crate::ControlError::Io(_) => Some("io".into()),
        };
        Self::Err {
            error: err.to_string(),
            code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_roundtrips_via_sonic() -> Result<(), String> {
        let req = ControlRequest::Health;
        let line = req.to_line()?;
        assert!(line.contains("health"));
        let decoded = ControlRequest::from_line(&line)?;
        assert_eq!(decoded, req);
        Ok(())
    }

    #[test]
    fn list_response_roundtrips() -> Result<(), String> {
        let resp = ControlResponse::Ok {
            agents: Some(vec![AgentRecord {
                id: "abc".into(),
                backend: BackendKind::Tui,
                session_id: Some("abc".into()),
                status: "idle".into(),
                title: Some("t".into()),
                model: None,
                output: None,
                cwd: None,
            }]),
            agent: None,
            version: None,
            state: None,
        };
        let line = resp.to_line()?;
        let decoded = ControlResponse::from_line(&line)?;
        assert_eq!(decoded, resp);
        Ok(())
    }

    #[test]
    fn pause_unsupported_error_has_code() -> Result<(), String> {
        let err = crate::ControlError::Unsupported {
            backend: BackendKind::Tui,
            verb: "pause",
        };
        let resp = ControlResponse::from_error(&err);
        match resp {
            ControlResponse::Err { code, error } => {
                assert_eq!(code.as_deref(), Some("unsupported"));
                assert!(error.contains("pause"));
                assert!(error.contains("tui"));
                Ok(())
            }
            ControlResponse::Ok { .. } => Err("expected Err response".into()),
        }
    }
}
