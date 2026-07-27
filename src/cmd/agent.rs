//! Thin CLI client for the on-device agent control plane (`n00n-daemon`).

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};
use color_eyre::eyre::{Result, WrapErr, eyre};
use n00n_daemon::backend::WorkerBackend;
use n00n_daemon::client;
use n00n_daemon::paths::{daemon_socket_in, daemon_socket_path};
use n00n_daemon::protocol::{BackendKind, ControlRequest, ControlResponse, MessageOpts};
use n00n_daemon::registry::ControlPlane;
use n00n_daemon::server;
use n00n_storage::StateDir;

#[derive(Subcommand, Debug)]
pub enum AgentAction {
    /// Start a foreground control-plane listener (worker backend only)
    Daemon {
        /// Override state directory
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// List agents (daemon sock if present, else worker fixtures on disk)
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one agent
    Status {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Send a steering message
    Message { id: String, text: String },
    /// Pause a worker agent (unsupported on TUI)
    Pause { id: String },
    /// Resume a worker agent
    Resume { id: String },
    /// Stop / cancel an agent
    Stop { id: String },
}

#[derive(Args, Debug)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub action: AgentAction,
}

fn print_response(resp: &ControlResponse, json: bool) -> Result<()> {
    if json {
        let line = resp.to_line().map_err(|e| eyre!(e))?;
        println!("{line}");
        return Ok(());
    }
    match resp {
        ControlResponse::Ok {
            agents: Some(agents),
            ..
        } => {
            if agents.is_empty() {
                println!("(no agents)");
            }
            for a in agents {
                println!(
                    "{}\t{}\t{}\t{}",
                    a.id,
                    a.backend,
                    a.status,
                    a.title.as_deref().map_or("", |t| t)
                );
            }
        }
        ControlResponse::Ok { agent: Some(a), .. } => {
            println!("id:\t{}", a.id);
            println!("backend:\t{}", a.backend);
            println!("status:\t{}", a.status);
            if let Some(t) = &a.title {
                println!("title:\t{t}");
            }
            if let Some(m) = &a.model {
                println!("model:\t{m}");
            }
        }
        ControlResponse::Ok { state: Some(_), .. } => {
            let line = resp.to_line().map_err(|e| eyre!(e))?;
            println!("{line}");
        }
        ControlResponse::Ok {
            version: Some(v), ..
        } => {
            println!("ok\tprotocol={v}");
        }
        ControlResponse::Ok { .. } => println!("ok"),
        ControlResponse::Err { error, code } => {
            if let Some(c) = code {
                return Err(eyre!("[{c}] {error}"));
            }
            return Err(eyre!("{error}"));
        }
    }
    Ok(())
}

fn local_plane(state_dir: &StateDir) -> Arc<ControlPlane> {
    let worker = Arc::new(WorkerBackend::new(state_dir.path()));
    Arc::new(ControlPlane::new(None, Some(worker)))
}

fn call_or_local(req: ControlRequest) -> Result<ControlResponse> {
    if let Ok(sock) = daemon_socket_path()
        && sock.exists()
    {
        match client::call_blocking(&sock, &req) {
            Ok(r) => return Ok(r),
            Err(e) => {
                tracing::warn!(error = %e, "daemon call failed; falling back to local worker plane");
            }
        }
    }
    let storage = StateDir::resolve().wrap_err("state dir")?;
    local_plane(&storage).handle(req).map_err(|e| eyre!(e))
}

/// # Errors
/// Returns CLI-level failures.
pub fn run(args: AgentArgs) -> Result<()> {
    match args.action {
        AgentAction::Daemon { state_dir } => {
            #[cfg(unix)]
            {
                let storage = match state_dir {
                    Some(p) => StateDir::from_path(p),
                    None => StateDir::resolve().wrap_err("state dir")?,
                };
                let sock = daemon_socket_in(storage.path());
                let plane = local_plane(&storage);
                let (_tx, rx) = flume::bounded::<()>(1);
                println!("listening on {}", sock.display());
                smol::block_on(server::serve(&sock, plane, rx)).map_err(|e| eyre!(e))?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = state_dir;
                Err(eyre!("agent daemon requires unix"))
            }
        }
        AgentAction::List { json } => {
            let resp = call_or_local(ControlRequest::List)?;
            print_response(&resp, json)
        }
        AgentAction::Status { id, json } => {
            let resp = call_or_local(ControlRequest::Status { id, backend: None })?;
            print_response(&resp, json)
        }
        AgentAction::Message { id, text } => {
            let resp = call_or_local(ControlRequest::Message {
                id,
                text,
                backend: None,
                opts: MessageOpts {
                    steer: true,
                    control: true,
                },
            })?;
            print_response(&resp, false)
        }
        AgentAction::Pause { id } => {
            let resp = call_or_local(ControlRequest::Pause {
                id,
                backend: Some(BackendKind::Worker),
            })?;
            print_response(&resp, false)
        }
        AgentAction::Resume { id } => {
            let resp = call_or_local(ControlRequest::Resume {
                id,
                backend: Some(BackendKind::Worker),
            })?;
            print_response(&resp, false)
        }
        AgentAction::Stop { id } => {
            let resp = call_or_local(ControlRequest::Stop { id, backend: None })?;
            print_response(&resp, false)
        }
    }
}
