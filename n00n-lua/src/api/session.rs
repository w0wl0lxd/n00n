//! `n00n.session`: host session primitives. Every call round-trips to the UI
//! event loop, which owns the live session runtimes and the session store;
//! the loop answers `list` from a background task so slow scans never block.

use mlua::{Lua, Result as LuaResult, Table, Value};
use n00n_lua_macro::{lua_fn, lua_table};

use crate::api::util::command::{SessionBootstrap, SessionReply, SessionRequest, UiAction};
use crate::api::util::convert::{json_to_lua, lua_to_json};
use crate::runtime::active_session_identity;

const NO_UI_ERR: &str = "no interactive UI attached";

type Pair = (Value, Option<String>);

fn err_pair(err: impl ToString) -> Pair {
    (Value::Nil, Some(err.to_string()))
}

async fn roundtrip(
    lua: Lua,
    tx: Option<flume::Sender<UiAction>>,
    req: SessionRequest,
) -> LuaResult<Pair> {
    let Some(tx) = tx else {
        return Ok(err_pair(NO_UI_ERR));
    };
    let (reply_tx, reply_rx) = flume::bounded::<SessionReply>(1);
    if tx.try_send(UiAction::Session { req, reply_tx }).is_err() {
        return Ok(err_pair(NO_UI_ERR));
    }
    match reply_rx.recv_async().await {
        Ok(Ok(value)) => Ok((json_to_lua(&lua, &value)?, None)),
        Ok(Err(e)) => Ok(err_pair(e)),
        Err(_) => Ok(err_pair("ui event loop dropped the request")),
    }
}

/// Lists sessions stored for the current project. Answered from a
/// background scan, so a slow disk never blocks the UI.
///
/// @return (table|nil, string|nil) Array of `{id, title, display_title, kind,
/// parent_id, updated_at, cwd, model}`, or nil and an error.
/// @example
/// local stored, err = n00n.session.list()
#[lua_fn]
async fn list(lua: Lua, #[ctx] tx: Option<flume::Sender<UiAction>>) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::List).await
}

/// Lists the sessions currently running in this UI. Status is "working",
/// "needs_input", or "idle".
///
/// @return (table|nil, string|nil) Array of `{id, title, status, updated_at, focused}`, or nil and an error.
/// @example
/// local live, err = n00n.session.live()
#[lua_fn]
async fn live(lua: Lua, #[ctx] tx: Option<flume::Sender<UiAction>>) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::Live).await
}

/// Returns one live session with its status, latest assistant text output,
/// and paused team run metadata when the latest tool result is from `team`.
///
/// @param id string Live session id.
/// @return (table|nil, string|nil) `{id, title, status, updated_at, focused, output?, paused_team?}` where `paused_team` is `{paused, run_id, mode?, ...}` when a paused team run is present, or nil and an error.
#[lua_fn]
async fn status(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    id: String,
) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::Status { id }).await
}

/// Returns the id of the currently focused session.
///
/// @return (string|nil, string|nil) Session id, or nil and an error.
/// @example
/// local id = n00n.session.current()
#[lua_fn]
async fn current(lua: Lua, #[ctx] tx: Option<flume::Sender<UiAction>>) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::Current).await
}

/// Switches the UI to the session with {id}. The session must be live.
///
/// @param id string Session id, as returned by `list()` or `live()`.
/// @return (boolean|nil, string|nil) true on success, or nil and an error.
/// @example
/// local _, err = n00n.session.focus(id)
#[lua_fn]
async fn focus(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    id: String,
) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::Focus { id }).await
}

/// Deletes a session and its stored history, cancelling it first if it
/// is running. The focused session cannot be deleted.
///
/// @param id string Session id to delete.
/// @return (boolean|nil, string|nil) true on success, or nil and an error.
/// @example
/// local _, err = n00n.session.delete(id)
#[lua_fn]
async fn delete(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    id: String,
) -> LuaResult<Pair> {
    roundtrip(lua, tx, SessionRequest::Delete { id }).await
}

/// Starts a new session in the current project.
///
/// @param opts table? Optional fields: prompt (string) first user message
///   to submit right away; focus (boolean) switch the UI to the new session;
///   parent_id (string?) session that spawned this session; tool (string),
///   input (table), and title (string?) for a direct host-executed bootstrap.
/// @return (string|nil, string|nil) New session id, or nil and an error.
/// @example
/// local id, err = n00n.session.new({ prompt = "fix the tests", focus = true })
#[lua_fn]
async fn new(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    opts: Option<Table>,
) -> LuaResult<Pair> {
    let caller_id = active_session_identity(&lua).map(|identity| identity.session_id().clone());
    let (prompt, focus, parent_id, tool, input, title) = match opts {
        Some(opts) => (
            opts.get("prompt")?,
            opts.get("focus").unwrap_or_else(|_| false),
            opts.get("parent_id")?,
            opts.get::<Option<String>>("tool")?,
            opts.get::<Option<Value>>("input")?,
            opts.get::<Option<String>>("title")?,
        ),
        None => (None, false, None, None, None, None),
    };
    let bootstrap = match tool {
        Some(tool) => {
            if prompt.is_some() {
                return Ok(err_pair("direct session bootstrap cannot include prompt"));
            }
            let input = match input {
                Some(input) => input,
                None => Value::Table(lua.create_table()?),
            };
            Some(SessionBootstrap {
                tool,
                input: lua_to_json(&lua, &input)?,
                title,
            })
        }
        None if input.is_some() || title.is_some() => {
            return Ok(err_pair("session bootstrap input/title requires tool"));
        }
        None => None,
    };
    roundtrip(
        lua,
        tx,
        SessionRequest::New {
            prompt,
            focus,
            parent_id,
            caller_id,
            bootstrap,
        },
    )
    .await
}

/// Sends {text} as a regular user prompt to a live session. The text is
/// never interpreted: slash commands, `exit`, and `!` shell prefixes are
/// all sent to the model verbatim. If the session is currently streaming,
/// the prompt is queued and picked up when the agent reaches it.
///
/// @param text string The prompt to send. Must not be blank.
/// @param opts table? Optional fields: session (string) id of a live
///   session (defaults to the focused one); steer (boolean) request
///   delivery as a steering interrupt when the session is busy; control
///   (boolean) mark the message as an agent-to-agent control message.
/// @return (string|nil, string|nil) "started" or "queued", or nil and an error.
/// @example
/// local state, err = n00n.session.prompt("run the tests", { session = id, steer = true, control = true })
#[lua_fn]
async fn prompt(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    text: String,
    opts: Option<Table>,
) -> LuaResult<Pair> {
    let caller_id = active_session_identity(&lua).map(|identity| identity.session_id().clone());
    let (id, steer, control) = match opts {
        Some(opts) => (
            opts.get("session")?,
            opts.get("steer")?,
            opts.get("control")?,
        ),
        None => (None, false, false),
    };
    roundtrip(
        lua,
        tx,
        SessionRequest::Prompt {
            id,
            text,
            steer,
            control,
            caller_id,
        },
    )
    .await
}

/// Cancels the current turn in a live session without deleting the session.
///
/// @param id string Live session id.
/// @return (boolean|nil, string|nil) true on success, or nil and an error.
#[lua_fn]
async fn cancel(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    id: String,
) -> LuaResult<Pair> {
    let caller_id = active_session_identity(&lua).map(|identity| identity.session_id().clone());
    roundtrip(lua, tx, SessionRequest::Cancel { id, caller_id }).await
}

/// Renames a session, live or stored.
///
/// @param opts table Required fields: id (string) session to rename;
///   title (string) the new title.
/// @return (boolean|nil, string|nil) true on success, or nil and an error.
/// @example
/// local _, err = n00n.session.set_title({ id = id, title = "refactor" })
#[lua_fn]
async fn set_title(
    lua: Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    opts: Table,
) -> LuaResult<Pair> {
    let req = SessionRequest::SetTitle {
        id: opts.get("id")?,
        title: opts.get("title")?,
    };
    roundtrip(lua, tx, req).await
}

lua_table! {
    /// Host session primitives. The interactive UI can run several sessions
    /// at once; these functions let plugins list, create, focus, rename, and
    /// delete them. Every call round-trips to the UI event loop and returns
    /// the pair `(value, err)`. Without an interactive UI attached, every
    /// call returns `nil, "no interactive UI attached"`.
    "n00n.session" => pub(crate) fn create_session_table(tx: Option<flume::Sender<UiAction>>),
    DOCS [list(tx), live(tx), status(tx), current(tx), focus(tx), delete(tx), new(tx), prompt(tx), cancel(tx), set_title(tx)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_agent::{cancel::CancelToken, tools::SessionIdentity};
    use n00n_storage::id::SessionRef;
    use serde_json::json;
    use test_case::test_case;

    use crate::runtime::{TaskCell, TaskScope};

    fn lua_with_session(tx: Option<flume::Sender<UiAction>>) -> Lua {
        let lua = Lua::new();
        let t = create_session_table(&lua, tx).unwrap();
        lua.globals().set("session", t).unwrap();
        lua
    }

    #[test]
    fn session_requests_attach_runtime_caller_id_not_lua_option() {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let caller_id = SessionRef::generate();
        let lua = lua_with_session(Some(tx));
        let _scope = TaskScope::new(
            &lua,
            TaskCell::new(
                CancelToken::none(),
                None,
                None,
                Some(SessionIdentity::root(caller_id.clone())),
            ),
        );
        let expected_caller_id = caller_id;
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req:
                    SessionRequest::New {
                        caller_id: actual_caller_id,
                        ..
                    },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected new request");
            };
            assert_eq!(actual_caller_id.as_ref(), Some(&expected_caller_id));
            reply_tx.send(Ok(json!("child"))).unwrap();
            let Ok(UiAction::Session {
                req:
                    SessionRequest::Prompt {
                        caller_id: actual_caller_id,
                        ..
                    },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected prompt request");
            };
            assert_eq!(actual_caller_id.as_ref(), Some(&expected_caller_id));
            reply_tx.send(Ok(json!("queued"))).unwrap();
        });

        let (child_id, prompt_status): (String, String) = smol::block_on(
            lua.load(
                r#"
                local child, new_err = session.new({ caller_id = "spoof" })
                if new_err then error(new_err) end
                local status, prompt_err = session.prompt("hello", { caller_id = "spoof" })
                if prompt_err then error(prompt_err) end
                return child, status
                "#,
            )
            .eval_async(),
        )
        .unwrap();
        checker.join().unwrap();
        assert_eq!(child_id, "child");
        assert_eq!(prompt_status, "queued");
    }

    #[test]
    fn direct_bootstrap_forwards_tool_input_title_and_runtime_identity() {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let caller_id = SessionRef::generate();
        let lua = lua_with_session(Some(tx));
        let _scope = TaskScope::new(
            &lua,
            TaskCell::new(
                CancelToken::none(),
                None,
                None,
                Some(SessionIdentity::root(caller_id.clone())),
            ),
        );
        let expected_caller_id = caller_id;
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req:
                    SessionRequest::New {
                        prompt,
                        focus,
                        parent_id,
                        caller_id,
                        bootstrap: Some(bootstrap),
                    },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected direct bootstrap request");
            };
            assert_eq!(prompt, None);
            assert!(!focus);
            assert_eq!(parent_id, None);
            assert_eq!(caller_id.as_ref(), Some(&expected_caller_id));
            assert_eq!(bootstrap.tool, "task");
            assert_eq!(
                bootstrap.input,
                json!({ "prompt": "inspect", "background": false })
            );
            assert_eq!(bootstrap.title.as_deref(), Some("task: inspect"));
            reply_tx.send(Ok(json!("child"))).unwrap();
        });

        let (child_id, error): (String, Option<String>) = smol::block_on(
            lua.load(
                r#"
                return session.new({
                    tool = "task",
                    input = { prompt = "inspect", background = false },
                    title = "task: inspect",
                })
                "#,
            )
            .eval_async(),
        )
        .unwrap();
        checker.join().unwrap();
        assert_eq!(child_id, "child");
        assert_eq!(error, None);
    }

    #[test]
    fn live_without_ui_returns_error_pair() {
        let lua = lua_with_session(None);
        let (val, err): (Value, Option<String>) =
            smol::block_on(lua.load("return session.live()").eval_async()).unwrap();
        assert!(val.is_nil());
        assert_eq!(err.as_deref(), Some(NO_UI_ERR));
    }

    #[test]
    fn focus_roundtrips_through_ui_channel() {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let lua = lua_with_session(Some(tx));
        std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req: SessionRequest::Focus { id },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected focus request");
            };
            reply_tx.send(Ok(json!({ "focused": id }))).unwrap();
        });
        let (val, err): (Table, Option<String>) =
            smol::block_on(lua.load("return session.focus('abc')").eval_async()).unwrap();
        assert_eq!(err, None);
        assert_eq!(val.get::<String>("focused").unwrap(), "abc");
    }

    #[test]
    fn status_forwards_session_id() {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let lua = lua_with_session(Some(tx));
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req: SessionRequest::Status { id },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected status request");
            };
            assert_eq!(id, "abc");
            reply_tx
                .send(Ok(json!({ "id": id, "status": "idle", "output": "done" })))
                .unwrap();
        });
        let (val, err): (Table, Option<String>) =
            smol::block_on(lua.load("return session.status('abc')").eval_async()).unwrap();
        checker.join().unwrap();
        assert_eq!(err, None);
        assert_eq!(val.get::<String>("output").unwrap(), "done");
    }

    #[test]
    fn cancel_forwards_session_id() {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let lua = lua_with_session(Some(tx));
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req: SessionRequest::Cancel { id, caller_id },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected cancel request");
            };
            assert_eq!(id, "abc");
            assert_eq!(caller_id, None);
            reply_tx.send(Ok(json!(true))).unwrap();
        });
        let (val, err): (bool, Option<String>) =
            smol::block_on(lua.load("return session.cancel('abc')").eval_async()).unwrap();
        checker.join().unwrap();
        assert_eq!(err, None);
        assert!(val);
    }

    #[test_case("return session.prompt('hi', { session = 'abc' })", Some("abc"), false, false ; "explicit_session_id")]
    #[test_case("return session.prompt('hi')", None, false, false ; "defaults_to_focused")]
    #[test_case("return session.prompt('hi', { session = 'abc', steer = true })", Some("abc"), true, false ; "explicit_session_id_steer")]
    #[test_case("return session.prompt('hi', { session = 'abc', steer = true, control = true })", Some("abc"), true, true ; "explicit_session_id_control")]
    fn prompt_forwards_text_and_session_id(
        code: &str,
        expected_id: Option<&str>,
        expected_steer: bool,
        expected_control: bool,
    ) {
        let (tx, rx) = flume::unbounded::<UiAction>();
        let lua = lua_with_session(Some(tx));
        let expected_id = expected_id.map(str::to_owned);
        let checker = std::thread::spawn(move || {
            let Ok(UiAction::Session {
                req:
                    SessionRequest::Prompt {
                        id,
                        text,
                        steer,
                        control,
                        caller_id,
                    },
                reply_tx,
            }) = rx.recv()
            else {
                panic!("expected prompt request");
            };
            assert_eq!(id, expected_id);
            assert_eq!(text, "hi");
            assert_eq!(steer, expected_steer);
            assert_eq!(control, expected_control);
            assert_eq!(caller_id, None);
            reply_tx.send(Ok(json!("queued"))).unwrap();
        });
        let (val, err): (String, Option<String>) =
            smol::block_on(lua.load(code).eval_async()).unwrap();
        checker.join().unwrap();
        assert_eq!(err, None);
        assert_eq!(val, "queued");
    }

    #[test]
    fn set_title_with_wrong_type_throws() {
        let lua = lua_with_session(None);
        let result: LuaResult<Value> =
            smol::block_on(lua.load("return session.set_title('oops')").eval_async());
        assert!(result.unwrap_err().to_string().contains("table"));
    }
}
