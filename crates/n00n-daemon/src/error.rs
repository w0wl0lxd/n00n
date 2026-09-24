use thiserror::Error;

use crate::protocol::BackendKind;

pub type ControlResult<T> = Result<T, ControlError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ControlError {
    #[error("unsupported verb `{verb}` on backend `{backend}`")]
    Unsupported {
        backend: BackendKind,
        verb: &'static str,
    },
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("invalid agent id: {0}")]
    InvalidId(String),
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("connection forbidden: {0}")]
    Forbidden(String),
    #[error("io error: {0}")]
    Io(String),
}

impl ControlError {
    /// Convert any displayable failure into [`ControlError::Io`].
    /// Takes `impl Display` by value so it works with `map_err(ControlError::io)`.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn io(err: impl std::fmt::Display) -> Self {
        Self::Io(err.to_string())
    }

    /// Convert any displayable failure into [`ControlError::Protocol`].
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn protocol(err: impl std::fmt::Display) -> Self {
        Self::Protocol(err.to_string())
    }
}
