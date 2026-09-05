mod acp;
#[cfg(unix)]
pub mod agent;
#[cfg(not(unix))]
#[path = "agent_stub.rs"]
pub mod agent;
mod native;
pub(crate) mod session_daemon;
mod subcmd;
mod tui;
mod tui_bridge;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_storage::StateDir;

use crate::cli::{AgentCommand, AuthAction, Cli, Command, McpAction, SafetyFlags};
use crate::update;

pub(super) const fn resolve_fusion_opt_in(
    cli_flag: bool,
    always_fusion: bool,
    agent_enabled: bool,
) -> bool {
    cli_flag || always_fusion || agent_enabled
}

fn run_if_allowed(
    safety: &SafetyFlags,
    action: &str,
    operation: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if crate::safety::allow(safety, action)? {
        operation()?;
    }
    Ok(())
}

pub fn dispatch(cli: Cli) -> Result<()> {
    let uses_project_config = matches!(
        &cli.command,
        None | Some(
            Command::Acp { .. }
                | Command::Index { .. }
                | Command::Prompt { .. }
                | Command::Mcp {
                    action: McpAction::Auth { .. },
                }
                | Command::Agent {
                    action: AgentCommand::Run { .. },
                },
        )
    );
    let project_trusted = if uses_project_config {
        let cwd = std::env::current_dir().context("resolve working directory")?;
        crate::project_trust::require(&cwd, cli.trust_project)?
    } else {
        false
    };

    match cli.command {
        Some(Command::Auth { action }) => match action {
            AuthAction::Login { provider } => {
                let storage = StateDir::resolve().context("resolve data directory")?;
                subcmd::auth_login(provider.as_deref(), &storage)?;
            }
            AuthAction::Logout { provider, safety } => {
                run_if_allowed(
                    &safety,
                    &format!("remove stored credentials for '{provider}'"),
                    || {
                        let storage = StateDir::resolve().context("resolve data directory")?;
                        subcmd::auth_logout(&provider, &storage)
                    },
                )?;
            }
            AuthAction::Status => {
                let storage = StateDir::resolve().context("resolve data directory")?;
                subcmd::auth_status(&storage);
            }
        },
        Some(Command::Index { path }) => {
            subcmd::index(
                &path,
                cli.plugin_flags.no_plugins,
                cli.plugin_flags.no_jit,
                project_trusted,
            )?;
        }
        Some(Command::Models) => {
            subcmd::models();
        }
        Some(Command::Mcp { action }) => match action {
            McpAction::Auth { server } => {
                let storage = StateDir::resolve().context("resolve data directory")?;
                subcmd::mcp_auth(&server, &storage, project_trusted)?;
            }
            McpAction::Logout { server, safety } => {
                run_if_allowed(
                    &safety,
                    &format!("remove stored OAuth credentials for MCP server '{server}'"),
                    || {
                        let storage = StateDir::resolve().context("resolve data directory")?;
                        subcmd::mcp_logout(&server, &storage)
                    },
                )?;
            }
        },
        Some(Command::Update { yes, no_color }) => {
            update::update(yes, no_color).map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Rollback { safety }) => {
            update::rollback(&safety).map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Acp { model, yolo }) => {
            acp::run(
                model.as_deref(),
                yolo,
                cli.plugin_flags.no_jit,
                project_trusted,
            )?;
        }
        Some(Command::Git { action }) => native::git_command(action)?,
        Some(Command::Smell { action }) => native::smell_command(action)?,
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
                    project_trusted,
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
                    fusion: cli.fusion,
                    project_trusted,
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
                state_dir,
                safety,
            } => {
                agent::stop_client(&id, state_dir, &safety)?;
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
    use std::cell::Cell;

    use test_case::test_case;

    use super::{resolve_fusion_opt_in, run_if_allowed};
    use crate::cli::SafetyFlags;

    #[test]
    fn dry_run_skips_late_destructive_operation() {
        let called = Cell::new(false);
        let safety = SafetyFlags {
            dry_run: true,
            no_confirm: false,
        };

        run_if_allowed(&safety, "delete test state", || {
            called.set(true);
            Ok(())
        })
        .expect("dry-run should succeed");

        assert!(!called.get());
    }

    #[test]
    fn no_confirm_runs_late_destructive_operation() {
        let called = Cell::new(false);
        let safety = SafetyFlags {
            dry_run: false,
            no_confirm: true,
        };

        run_if_allowed(&safety, "delete test state", || {
            called.set(true);
            Ok(())
        })
        .expect("explicit bypass should succeed");

        assert!(called.get());
    }

    #[test_case(false, false, false, false ; "default off")]
    #[test_case(true,  false, false, true  ; "cli flag")]
    #[test_case(false, true,  false, true  ; "always fusion")]
    #[test_case(false, false, true,  true  ; "agent fusion")]
    #[test_case(true,  true,  true,  true  ; "all opt ins")]
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
