pub(crate) mod error;
pub mod model;
pub mod provider;
pub(crate) mod providers;
pub(crate) mod types;

use std::path::PathBuf;
use std::{env, fs};

pub use error::AgentError;
pub use model::{Model, ModelError, ModelFamily, ModelPricing, TokenUsage};
pub use providers::auth;
pub use types::{
    AgentEvent, BatchToolEntry, BatchToolStatus, ContentBlock, DiffHunk, DiffLine, DiffSpan,
    Envelope, GrepFileEntry, GrepMatch, Message, QuestionInfo, QuestionOption, Role,
    StreamResponse, TodoItem, TodoPriority, TodoStatus, ToolDoneEvent, ToolInput, ToolOutput,
    ToolStartEvent,
};

const DATA_DIR_NAME: &str = ".maki";

pub fn data_dir() -> Result<PathBuf, AgentError> {
    let home = env::var("HOME").map_err(|_| AgentError::Api {
        status: 0,
        message: "HOME not set".into(),
    })?;
    let dir = PathBuf::from(home).join(DATA_DIR_NAME);
    fs::create_dir_all(&dir).map_err(AgentError::Io)?;
    Ok(dir)
}
