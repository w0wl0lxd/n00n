mod acp;
pub mod agent;
mod subcmd;
mod tui;
mod tui_bridge;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_storage::StateDir;

use crate::cli::{AgentCommand, AuthAction, Cli, Command, McpAction};
use crate::update;

pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth { action }) => {
            let storage = StateDir::resolve().context("resolve data directory")?;
            match action {
                AuthAction::Login { provider } => {
                    subcmd::auth_login(provider.as_deref(), &storage)?;
                }
                AuthAction::Logout { provider } => subcmd::auth_logout(&provider, &storage)?,
                AuthAction::Status => subcmd::auth_status(&storage),
            }
        }
        Some(Command::Index { path }) => {
            subcmd::index(&path, cli.plugin_flags.no_plugins, cli.plugin_flags.no_jit)?;
        }
        Some(Command::Models) => {
            subcmd::models();
        }
        Some(Command::Mcp { action }) => {
            let storage = StateDir::resolve().context("resolve data directory")?;
            match action {
                McpAction::Auth { server } => subcmd::mcp_auth(&server, &storage)?,
                McpAction::Logout { server } => subcmd::mcp_logout(&server, &storage)?,
            }
        }
        Some(Command::Update { yes, no_color }) => {
            update::update(yes, no_color).map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Rollback) => {
            update::rollback().map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Acp { model, yolo }) => {
            acp::run(model.as_deref(), yolo, cli.plugin_flags.no_jit)?;
        }
        Some(Command::Prompt {
            variant,
            plan,
            tools,
            names,
        }) => {
            subcmd::prompt(
                &variant,
                subcmd::PromptFlags {
                    plan,
                    tools,
                    names,
                    no_jit: cli.plugin_flags.no_jit,
                },
            )?;
        }
        Some(Command::Agent { action }) => match action {
            AgentCommand::Run {
                prompt,
                model,
                mode,
                goal,
                team_mode,
                max_agents,
                waves,
                workflow_inputs,
                task_description,
                json,
                background,
                id,
            } => {
                let run_opts = agent::AgentRunOptions {
                    prompt: &prompt,
                    model: model.as_deref(),
                    mode,
                    goal: goal.as_deref(),
                    team_mode: team_mode.as_deref(),
                    max_agents,
                    waves,
                    workflow_inputs: workflow_inputs.as_deref(),
                    task_description: task_description.as_deref(),
                    yolo: cli.permission_flags.yolo,
                    no_jit: cli.plugin_flags.no_jit,
                };
                if background {
                    agent::server(&run_opts, id)?;
                } else {
                    agent::run(&run_opts, json)?;
                }
            }
            AgentCommand::List => {
                agent::list_client(matches!(
                    cli.output_format,
                    crate::print::OutputFormat::Json
                ))?;
            }
            AgentCommand::Status { id } => {
                agent::status_client(
                    &id,
                    matches!(cli.output_format, crate::print::OutputFormat::Json),
                )?;
            }
            AgentCommand::Message { id, text } => {
                agent::message_client(
                    &id,
                    &text,
                    matches!(cli.output_format, crate::print::OutputFormat::Json),
                )?;
            }
            AgentCommand::Pause { id } => {
                agent::pause_client(&id)?;
            }
            AgentCommand::Resume { id } => {
                agent::resume_client(&id)?;
            }
            AgentCommand::Stop { id } => {
                agent::stop_client(&id)?;
            }
            AgentCommand::Daemon { state_dir } => {
                agent::daemon_serve(state_dir)?;
            }
        },
        None => {
            tui::run(cli)?;
        }
    }
    Ok(())
}
