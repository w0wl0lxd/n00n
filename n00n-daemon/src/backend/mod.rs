//! Backend adapters for the control plane.

mod worker;

use crate::error::ControlResult;
use crate::protocol::{AgentRecord, MessageOpts};

pub use worker::WorkerBackend;

/// Backend that can list/inspect/control agents of one kind.
pub trait ControlBackend: Send + Sync {
    /// # Errors
    /// Returns when the backend cannot enumerate agents.
    fn list(&self) -> ControlResult<Vec<AgentRecord>>;

    /// # Errors
    /// Returns [`crate::ControlError::NotFound`] or other backend failures.
    fn status(&self, id: &str) -> ControlResult<AgentRecord>;

    /// # Errors
    /// Returns when messaging fails or the agent is missing.
    fn message(&self, id: &str, text: &str, opts: &MessageOpts) -> ControlResult<sonic_rs::Value>;

    /// # Errors
    /// Returns [`crate::ControlError::Unsupported`] on TUI, or worker failures.
    fn pause(&self, id: &str) -> ControlResult<()>;

    /// # Errors
    /// Returns [`crate::ControlError::Unsupported`] on TUI, or worker failures.
    fn resume(&self, id: &str) -> ControlResult<()>;

    /// # Errors
    /// Returns when stop/cancel fails or the agent is missing.
    fn stop(&self, id: &str) -> ControlResult<()>;
}
