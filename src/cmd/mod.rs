mod acp;
#[cfg(unix)]
pub mod agent;
#[cfg(not(unix))]
#[path = "agent_stub.rs"]
pub mod agent;
pub(crate) mod session_daemon;
mod subcmd;
mod tui;
mod tui_bridge;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_storage::StateDir;

use crate::cli::{AgentCommand, AuthAction, Cli, Command, McpAction};
use crate::update;

pub(super) const fn resolve_fusion_opt_in(
    cli_flag: bool,
    always_fusion: bool,
    agent_enabled: bool,
) -> bool {
    cli_flag || always_fusion || agent_enabled
}

pub fn dispatch(cli: Cli) -> Result<()> {
    cli.warn_ignored_flags();
    match cli.command {
        Some(Command::Auth { action }) => {
            let storage = StateDir::resolve().context("resolve data directory")?;
            match action {
                AuthAction::Login { provider } => {
                    subcmd::auth_login(provider.as_deref(), &storage)?;
                }
                AuthAction::Logout { provider, safety } => subcmd::auth_logout(
                    &provider,
                    &storage,
                    safety.no_confirm() || cli.permission_flags.no_confirm(),
                    safety.dry_run,
                    cli.output_format,
                )?,
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
                McpAction::Logout { server, safety } => subcmd::mcp_logout(
                    &server,
                    &storage,
                    safety.no_confirm() || cli.permission_flags.no_confirm(),
                    safety.dry_run,
                    cli.output_format,
                )?,
            }
        }
        Some(Command::Update { safety, no_color }) => {
            update::update(
                safety.no_confirm() || cli.permission_flags.no_confirm(),
                no_color,
                safety.dry_run,
                cli.output_format,
            )
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Rollback { safety }) => {
            update::rollback(
                safety.no_confirm() || cli.permission_flags.no_confirm(),
                safety.dry_run,
                cli.output_format,
            )
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Acp {
            model,
            no_confirm,
            legacy_yolo,
        }) => {
            acp::run(
                model.as_deref(),
                no_confirm || legacy_yolo,
                cli.plugin_flags.no_jit,
            )?;
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
                    yolo: cli.permission_flags.no_confirm(),
                    no_jit: cli.plugin_flags.no_jit,
                    fusion: cli.fusion,
                };
                if background {
                    agent::server(&run_opts, id)?;
                } else {
                    agent::run(&run_opts, json)?;
                }
            }
            AgentCommand::List {
                state_dir,
                json,
                all,
                cwd,
            } => {
                agent::list_client(json, all, cwd, state_dir)?;
            }
            AgentCommand::Status {
                id,
                state_dir,
                json,
            } => {
                agent::status_client(&id, json, state_dir)?;
            }
            AgentCommand::Message {
                id,
                text,
                state_dir,
            } => {
                agent::message_client(
                    &id,
                    &text,
                    matches!(cli.output_format, crate::print::OutputFormat::Json),
                    state_dir,
                )?;
            }
            AgentCommand::Pause { id, state_dir } => {
                agent::pause_client(&id, state_dir)?;
            }
            AgentCommand::Resume { id, state_dir } => {
                agent::resume_client(&id, state_dir)?;
            }
            AgentCommand::Stop {
                id,
                safety,
                state_dir,
            } => {
                agent::stop_client(
                    &id,
                    state_dir,
                    safety.no_confirm() || cli.permission_flags.no_confirm(),
                    safety.dry_run,
                    cli.output_format,
                )?;
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

#[cfg(test)]
mod tests {
    use super::resolve_fusion_opt_in;
    use test_case::test_case;

    #[test_case(false, false, false, false ; "default off")]
    #[test_case(true, false, false, true ; "cli flag")]
    #[test_case(false, true, false, true ; "always fusion")]
    #[test_case(false, false, true, true ; "agent fusion")]
    #[test_case(true, true, true, true ; "all opt ins")]
    #[allow(clippy::fn_params_excessive_bools)]
    fn fusion_opt_in_is_additive(
        cli_flag: bool,
        always_fusion: bool,
        agent_enabled: bool,
        expected: bool,
    ) {
        assert_eq!(
            resolve_fusion_opt_in(cli_flag, always_fusion, agent_enabled),
            expected
        );
    }
}
