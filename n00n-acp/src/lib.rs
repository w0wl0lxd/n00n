pub mod methods;
pub mod permissions;
pub mod server;
pub mod translate;

use std::path::PathBuf;
use std::sync::Arc;

use n00n_agent::prompt::ResolvedSlots;
use n00n_agent::{AgentConfig, PermissionsConfig};
use n00n_providers::model::Model;
use n00n_providers::{OpenAiOptions, Timeouts};

pub struct AcpParams {
    pub model: Model,
    pub config: AgentConfig,
    pub permissions_config: PermissionsConfig,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub initial_wd: PathBuf,
    pub mcp_handle: Option<n00n_agent::McpHandle>,
    pub prompt_slots: Arc<ResolvedSlots>,
    pub yolo: bool,
}

/// Runs the ACP server with the given parameters.
///
/// # Errors
/// Returns an error if the server fails to start or run.
pub fn run(params: AcpParams) -> color_eyre::Result<()> {
    smol::block_on(server::serve(params))
}
