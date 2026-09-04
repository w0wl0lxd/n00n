use crate::{
    api::util::{
        command::{SessionBootstrap, SessionReply, SessionRequest, UiAction},
        convert::{json_to_lua, lua_to_json},
    },
    runtime::active_session_identity,
};
use mlua::{Lua, Result as LuaResult, Table, Value};
use n00n_lua_macro::{lua_fn, lua_table};
use n00n_runs::{
    ExecutionBackend, NewRunSpec, OutcomeStatus, RunCapabilities, RunEventPayload, RunFailure,
    RunKind, RunLifecycle, RunOutcome, RunService, TransitionRequest,
};
use n00n_storage::id::SessionRef;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

const NO_CONTEXT_ERR: &str = "background runs require an active trusted session context";
const NO_SERVICE_ERR: &str = "background run service is unavailable";
const NO_UI_ERR: &str = "no interactive UI attached";
const START_FAILED_SUMMARY: &str = "TUI session bootstrap failed";

type Pair = (Value, Option<String>);

fn err_pair(error: impl ToString) -> Pair {
    (Value::Nil, Some(error.to_string()))
}

async fn fail_start(service: &RunService, run: &n00n_runs::RunRecord) {
    let _ = service
        .transition(TransitionRequest {
            run_id: run.run_id,
            expected_revision: run.revision,
            owner: None,
            target: RunLifecycle::Failed,
            wait_reason: None,
            outcome: Some(RunOutcome {
                status: OutcomeStatus::Failed,
                summary: Some(START_FAILED_SUMMARY.to_owned()),
                output: None,
                error: Some(RunFailure {
                    code: "tui_start_failed".to_owned(),
                    message: START_FAILED_SUMMARY.to_owned(),
                    source: "tui_session".to_owned(),
                    retryable: true,
                }),
                stop_reason: None,
                usage: None,
                cost: None,
                cleanup_error: None,
                verification: None,
            }),
            event_type: "start_failed".to_owned(),
            event: RunEventPayload {
                summary: Some(START_FAILED_SUMMARY.to_owned()),
                details: BTreeMap::new(),
            },
            operation_id: format!("tui-start-failed:{}", run.run_id),
            progress: false,
        })
        .await;
}

/// Starts a trusted background run backed by a child TUI session.
///
/// @param opts table Required task kind, bootstrap tool, input, and title.
/// @return (table|nil, string|nil) Run identity and lifecycle, or nil and an error.
#[lua_fn]
async fn start(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    opts: Table,
) -> LuaResult<Pair> {
    let Some(identity) = active_session_identity(&lua) else {
        return Ok(err_pair(NO_CONTEXT_ERR));
    };
    let Some(service) = lua
        .app_data_ref::<Arc<RunService>>()
        .map(|service| Arc::clone(&service))
    else {
        return Ok(err_pair(NO_SERVICE_ERR));
    };
    let Some(tx) = tx else {
        return Ok(err_pair(NO_UI_ERR));
    };
    let kind: String = opts.get("kind")?;
    if kind != "task" {
        return Ok(err_pair("TUI background start only supports task runs"));
    }
    let tool: String = opts.get("tool")?;
    let title: String = opts.get("title")?;
    let input = match opts.get::<Option<Value>>("input")? {
        Some(input) => lua_to_json(&lua, &input)?,
        None => json!({}),
    };
    let session_id = SessionRef::generate();
    let mut spec = NewRunSpec::new(RunKind::Task, ExecutionBackend::TuiSession, title.clone());
    spec.root_session_id = Some(identity.root_session_id().to_string());
    spec.session_id = Some(session_id.to_string());
    spec.parent_session_id = Some(identity.session_id().to_string());
    spec.capabilities = RunCapabilities {
        send: true,
        answer: true,
        cancel: true,
        events: true,
        logs: true,
        ..RunCapabilities::default()
    };
    let queued = match service.create_run(spec).await {
        Ok(run) => run,
        Err(error) => return Ok(err_pair(error)),
    };
    let starting = match service
        .transition(TransitionRequest {
            run_id: queued.run_id,
            expected_revision: queued.revision,
            owner: None,
            target: RunLifecycle::Starting,
            wait_reason: None,
            outcome: None,
            event_type: "starting".to_owned(),
            event: RunEventPayload {
                summary: Some("Starting TUI background task".to_owned()),
                details: BTreeMap::new(),
            },
            operation_id: format!("tui-start:{}", queued.run_id),
            progress: true,
        })
        .await
    {
        Ok(run) => run,
        Err(error) => return Ok(err_pair(error)),
    };
    let (reply_tx, reply_rx) = flume::bounded::<SessionReply>(1);
    let request = SessionRequest::New {
        prompt: None,
        focus: false,
        requested_id: Some(session_id.clone()),
        managed_run_id: Some(starting.run_id),
        parent_id: None,
        caller_id: Some(identity.session_id().clone()),
        bootstrap: Some(SessionBootstrap {
            tool,
            input,
            title: Some(title),
        }),
    };
    if tx
        .try_send(UiAction::Session {
            req: request,
            reply_tx,
        })
        .is_err()
    {
        fail_start(&service, &starting).await;
        return Ok(err_pair(NO_UI_ERR));
    }
    match reply_rx.recv_async().await {
        Ok(Ok(value)) if value == json!(session_id) => {
            let lifecycle =
                serde_json::to_value(starting.lifecycle).map_err(mlua::Error::external)?;
            let response = json!({
                "run_id": starting.run_id,
                "chain_id": starting.chain_id,
                "session_id": session_id,
                "lifecycle": lifecycle,
            });
            Ok((json_to_lua(&lua, &response)?, None))
        }
        Ok(Ok(_)) => {
            fail_start(&service, &starting).await;
            Ok(err_pair("TUI returned an unexpected session id"))
        }
        Ok(Err(error)) => {
            fail_start(&service, &starting).await;
            Ok(err_pair(error))
        }
        Err(_) => {
            fail_start(&service, &starting).await;
            Ok(err_pair("ui event loop dropped the request"))
        }
    }
}

lua_table! {
    "n00n.run" => pub(crate) fn create_run_table(tx: Option<flume::Sender<UiAction>>),
    DOCS [start(tx)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::util::command::{SessionRequest, UiAction},
        runtime::{TaskCell, TaskScope},
    };
    use mlua::{Lua, Table, Value};
    use n00n_agent::{cancel::CancelToken, tools::SessionIdentity};
    use n00n_runs::{ExecutionBackend, ProjectKey, RunLifecycle, RunService, RunStore};
    use n00n_storage::id::SessionRef;
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    use tempfile::TempDir;

    fn service(temp: &TempDir) -> Arc<RunService> {
        let store = RunStore::open_path(
            temp.path().join("runs.sqlite3"),
            ProjectKey::new("/trusted/project").unwrap(),
            Duration::from_millis(50),
        )
        .unwrap();
        Arc::new(RunService::new(store))
    }

    #[test]
    fn start_requires_active_trusted_session_context() {
        let temp = TempDir::new().unwrap();
        let lua = Lua::new();
        lua.set_app_data(service(&temp));
        let table = create_run_table(&lua, None).unwrap();
        lua.globals().set("run", table).unwrap();

        let (value, error): (Value, Option<String>) = smol::block_on(
            lua.load("return run.start({ kind = 'task', tool = 'run_task' })")
                .eval_async(),
        )
        .unwrap();

        assert!(value.is_nil());
        assert_eq!(
            error.as_deref(),
            Some("background runs require an active trusted session context")
        );
    }

    #[test]
    fn start_derives_lineage_backend_and_return_contract_from_host_context() {
        let temp = TempDir::new().unwrap();
        let service = service(&temp);
        let (tx, rx) = flume::unbounded::<UiAction>();
        let caller = SessionRef::generate();
        let root = SessionRef::generate();
        let lua = Lua::new();
        lua.set_app_data(Arc::clone(&service));
        let table = create_run_table(&lua, Some(tx)).unwrap();
        lua.globals().set("run", table).unwrap();
        let _scope = TaskScope::new(
            &lua,
            TaskCell::new(
                CancelToken::none(),
                None,
                None,
                Some(SessionIdentity::child(caller.clone(), root.clone())),
            ),
        );
        let expected_caller = caller.clone();
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req:
                    SessionRequest::New {
                        caller_id,
                        requested_id: Some(requested_id),
                        managed_run_id: Some(managed_run_id),
                        bootstrap: Some(bootstrap),
                        ..
                    },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected managed run bootstrap request");
            };
            assert_eq!(caller_id.as_ref(), Some(&expected_caller));
            assert!(!managed_run_id.to_string().is_empty());
            assert_eq!(bootstrap.tool, "run_task");
            assert_eq!(
                bootstrap.input,
                json!({ "prompt": "inspect", "background": false })
            );
            reply_tx.send(Ok(json!(requested_id))).unwrap();
        });

        let (result, error): (Table, Option<String>) = smol::block_on(
            lua.load(
                r#"
                return run.start({
                    kind = "task",
                    tool = "run_task",
                    input = { prompt = "inspect", background = false },
                    title = "task: inspect",
                    backend = "worker_process",
                    parent_session_id = "spoof",
                })
                "#,
            )
            .eval_async(),
        )
        .unwrap();
        checker.join().unwrap();

        assert_eq!(error, None);
        let run_id = result.get::<String>("run_id").unwrap();
        let chain_id = result.get::<String>("chain_id").unwrap();
        let session_id = result.get::<String>("session_id").unwrap();
        assert!(!run_id.is_empty());
        assert!(!chain_id.is_empty());
        assert_eq!(result.get::<String>("lifecycle").unwrap(), "starting");
        let record = smol::block_on(service.get_run(run_id.parse().unwrap())).unwrap();
        assert_eq!(record.backend, ExecutionBackend::TuiSession);
        assert_eq!(record.lifecycle, RunLifecycle::Starting);
        assert_eq!(
            record.parent_session_id.as_deref(),
            Some(caller.to_string().as_str())
        );
        assert_eq!(record.session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(record.chain_id.to_string(), chain_id);
    }
}
