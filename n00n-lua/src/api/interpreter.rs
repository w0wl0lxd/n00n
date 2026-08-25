//! Runs Python in a killable Monty worker process. Newline-delimited JSON
//! preserves ordered stdout and tool callbacks while cancellation terminates
//! and reaps the worker process group.

use std::collections::HashMap;
use std::io::{self, Write};
#[cfg(debug_assertions)]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use async_process::{ChildStdin, ChildStdout};
use futures::future::join_all;
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use mlua::{Function, Lua, Result as LuaResult, Table};
use n00n_agent::ChildGuard;
use n00n_agent::cancel::CancelToken;
use n00n_agent::tools::interpreter_bridge::build_tool_input;
use n00n_interpreter::worker::{
    StartRequest, WireCall, WireCallResult, WorkerEvent, WorkerRequest,
};
use n00n_lua_macro::{lua_fn, lua_table};
use serde_json::Value;

use crate::runtime::run_non_yieldable;

use crate::api::util::convert::{json_to_lua, lua_tool_result};
use crate::plugin_permissions::PluginPermissions;
use crate::runtime::{TaskHandle, lock_cell, task_deadline};

const MAX_INTERPRETER_TIMEOUT_SECS: u64 = 300;
const INTERPRETER_TIMEOUT_ERR: &str = "interpreter timed out";
const INTERPRETER_WORKER_NAME: &str = "n00n-interpreter-worker";
const INTERPRETER_WORKER_TEST: &str = "interpreter_worker_entry";

#[cfg(debug_assertions)]
static INTERPRETER_WORKER_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn run_ruff(args: &[&str], code: &str, pid_tx: &flume::Sender<u32>) -> Option<String> {
    let mut command = Command::new("ruff");
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    let _ = pid_tx.send(child.id());
    let write_result = child
        .stdin
        .take()
        .ok_or(())
        .and_then(|mut stdin| stdin.write_all(code.as_bytes()).map_err(|_| ()));
    if write_result.is_err() {
        terminate_process_tree(child.id());
        let _ = child.wait();
        return None;
    }
    let output = child.wait_with_output().ok()?;
    let fixed = String::from_utf8(output.stdout).ok()?;
    (!fixed.is_empty()).then_some(fixed)
}

#[cfg(unix)]
fn terminate_process_tree(pid: u32) {
    use rustix::process::{Pid, Signal, kill_process_group};

    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    let _ = kill_process_group(pid, Signal::KILL);
}

#[cfg(not(unix))]
fn terminate_process_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .status();
}

#[derive(Clone, Copy, Debug)]
enum StopReason {
    Cancelled,
    Deadline,
}

async fn wait_for_stop(cancel: &CancelToken, deadline: Instant) -> StopReason {
    futures_lite::future::race(
        async {
            cancel.cancelled().await;
            StopReason::Cancelled
        },
        async {
            smol::Timer::at(deadline).await;
            StopReason::Deadline
        },
    )
    .await
}

async fn run_ruff_guarded(
    args: &'static [&'static str],
    code: String,
    cancel: &CancelToken,
    deadline: Instant,
) -> Result<Option<String>, StopReason> {
    if cancel.is_cancelled() {
        return Err(StopReason::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(StopReason::Deadline);
    }
    let (pid_tx, pid_rx) = flume::bounded(1);
    let mut task = smol::unblock(move || run_ruff(args, &code, &pid_tx));
    let pid = match pid_rx.recv_async().await {
        Ok(pid) => pid,
        Err(_) => return Ok(task.await),
    };
    enum RuffOutcome {
        Done(Option<String>),
        Stopped(StopReason),
    }
    match futures_lite::future::race(async { RuffOutcome::Done((&mut task).await) }, async {
        RuffOutcome::Stopped(wait_for_stop(cancel, deadline).await)
    })
    .await
    {
        RuffOutcome::Done(output) => Ok(output),
        RuffOutcome::Stopped(reason) => {
            terminate_process_tree(pid);
            let _ = task.await;
            Err(reason)
        }
    }
}

async fn ruff_fix(
    code: String,
    cancel: &CancelToken,
    deadline: Instant,
) -> Result<String, StopReason> {
    let fixed = run_ruff_guarded(
        &[
            "check",
            "--fix",
            "--unsafe-fixes",
            "--isolated",
            "--stdin-filename",
            "code_execution.py",
            "-",
        ],
        code.clone(),
        cancel,
        deadline,
    )
    .await?
    .unwrap_or_else(|| code.clone());
    Ok(run_ruff_guarded(
        &[
            "format",
            "--isolated",
            "--stdin-filename",
            "code_execution.py",
            "-",
        ],
        fixed.clone(),
        cancel,
        deadline,
    )
    .await?
    .unwrap_or(fixed))
}

fn required<T: mlua::FromLua>(opts: &Table, key: &str) -> LuaResult<T> {
    opts.get::<Option<T>>(key)?
        .ok_or_else(|| mlua::Error::runtime(format!("interpreter.run: '{key}' is required")))
}

#[cfg(debug_assertions)]
pub(crate) fn set_worker_executable_for_tests(path: PathBuf) -> Result<(), PathBuf> {
    INTERPRETER_WORKER_OVERRIDE.set(path)
}

fn worker_command() -> io::Result<Command> {
    #[cfg(debug_assertions)]
    if let Some(worker) = INTERPRETER_WORKER_OVERRIDE.get() {
        return Ok(Command::new(worker));
    }

    let current = std::env::current_exe()?;
    let file_name = current.file_name().and_then(|name| name.to_str());
    let is_test_binary = cfg!(test)
        || file_name.is_some_and(|name| name.starts_with("plugin_host-"))
        || current
            .parent()
            .and_then(|directory| directory.file_name())
            .is_some_and(|name| name == "deps");
    if is_test_binary {
        let mut command = Command::new(current);
        command.args([INTERPRETER_WORKER_TEST, "--ignored", "--nocapture"]);
        return Ok(command);
    }

    let worker_name = format!("{INTERPRETER_WORKER_NAME}{}", std::env::consts::EXE_SUFFIX);
    let worker = current
        .parent()
        .map(|directory| directory.join(&worker_name))
        .filter(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("could not find {INTERPRETER_WORKER_NAME} beside the current executable"),
            )
        })?;
    Ok(Command::new(worker))
}

fn spawn_worker() -> io::Result<(ChildGuard, ChildStdin, BufReader<ChildStdout>)> {
    let mut command = worker_command()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut command: async_process::Command = command.into();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("interpreter worker stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("interpreter worker stdout was not piped"))?;
    Ok((ChildGuard::new(child), stdin, BufReader::new(stdout)))
}

async fn send_worker_request(stdin: &mut ChildStdin, request: &WorkerRequest) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).await?;
    stdin.flush().await
}

async fn read_worker_event(stdout: &mut BufReader<ChildStdout>) -> io::Result<Option<WorkerEvent>> {
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if !line.trim_start().starts_with('{') {
            continue;
        }
        return serde_json::from_str(&line)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
}

async fn call_lua_tool(lua: Lua, f: Option<Function>, pc: &WireCall) -> Result<Value, String> {
    let Some(f) = f else {
        return Err(format!("unknown tool: {}", pc.name));
    };
    let input = build_tool_input(&pc.args, &pc.kwargs)?;
    let arg = json_to_lua(&lua, &input).map_err(|e| e.to_string())?;
    let values = f
        .call_async::<mlua::MultiValue>(arg)
        .await
        .map_err(|e| e.to_string())?;
    lua_tool_result(values)
        .map(Value::String)
        .map_err(|e| format!("{}: {e}", pc.name))
}

/// Run Python code in a sandboxed interpreter with memory and time limits.
/// Stdout lines are streamed to your {on_output} callback as they are produced.
/// If the Python code calls tools, those calls are dispatched to the Lua
/// functions you provide in {opts}.tools.
///
/// The result table has optional fields: `stdout` (string, trimmed combined
/// output) and `output` (string, the final expression value). On error, the
/// table is empty and the second return value is the error message.
///
/// @param code string Python source code to execute.
/// @param opts table Required fields:
///   `timeout` (integer) - execution time limit in seconds.
///   `max_memory_mb` (integer) - memory limit in megabytes.
///   `on_output` (function) - called with each stdout line (string) as it is
///     produced. Must not yield.
/// Optional fields:
///   `ruff_fix` (boolean?) - run Ruff fix/unsafe-fixes and formatting before execution.
///   `tools` (table?) - map of `name -> function` for tools the sandbox may call.
///     Each function receives the tool input table and must return `(string)` or
///     `(nil, err)`. Tool calls are batched and dispatched concurrently.
/// @return (table, string?) Result table, plus an error string on failure.
/// @example
/// local result, err = n00n.interpreter.run("print(2 + 2)", {
///   timeout = 30,
///   max_memory_mb = 256,
///   on_output = function(line) print("py: " .. line) end,
/// })
/// if err then error(err) end
/// if result.stdout then print(result.stdout) end
#[lua_fn(guard = Run, name = "run")]
#[allow(clippy::too_many_lines)]
async fn interpreter_run(
    lua: Lua,
    code: String,
    opts: Table,
) -> LuaResult<(Table, Option<String>)> {
    let timeout_secs: u64 = required(&opts, "timeout")?;
    if !(1..=MAX_INTERPRETER_TIMEOUT_SECS).contains(&timeout_secs) {
        return Err(mlua::Error::runtime(format!(
            "interpreter.run: 'timeout' must be between 1 and {MAX_INTERPRETER_TIMEOUT_SECS} seconds"
        )));
    }
    let max_memory_mb: usize = required(&opts, "max_memory_mb")?;
    let on_output: Function = required(&opts, "on_output")?;
    let tools_tbl: Option<Table> = opts.get("tools")?;
    let fix_with_ruff = opts
        .get::<Option<bool>>("ruff_fix")?
        .unwrap_or_else(|| false);
    let (cancel, parent_deadline) = {
        let task_handle = lua.app_data_ref::<TaskHandle>();
        task_handle.as_ref().map_or_else(
            || (CancelToken::none(), None),
            |handle| (lock_cell(handle).cancel.clone(), task_deadline(handle)),
        )
    };
    let requested_deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let deadline =
        parent_deadline.map_or(requested_deadline, |parent| parent.min(requested_deadline));
    let code = if fix_with_ruff {
        match ruff_fix(code, &cancel, deadline).await {
            Ok(code) => code,
            Err(StopReason::Cancelled) => return Err(mlua::Error::runtime("cancelled")),
            Err(StopReason::Deadline) => {
                return Err(mlua::Error::runtime(INTERPRETER_TIMEOUT_ERR));
            }
        }
    } else {
        code
    };

    let mut fns: HashMap<String, Function> = HashMap::new();
    if let Some(t) = tools_tbl {
        for pair in t.pairs::<String, Function>() {
            let (name, f) = pair?;
            fns.insert(name, f);
        }
    }
    let names: Vec<String> = fns.keys().cloned().collect();

    if cancel.is_cancelled() {
        return Err(mlua::Error::runtime("cancelled"));
    }
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(mlua::Error::runtime(INTERPRETER_TIMEOUT_ERR));
    }
    let timeout_millis = u64::try_from(timeout.as_millis())
        .map_err(|_| mlua::Error::runtime("interpreter timeout is too large"))?;
    let max_memory_bytes = max_memory_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| mlua::Error::runtime("interpreter memory limit is too large"))?;
    let (mut worker, mut worker_stdin, mut worker_stdout) =
        spawn_worker().map_err(mlua::Error::external)?;
    let start = WorkerRequest::Start(StartRequest {
        code,
        tool_names: names,
        timeout_millis,
        max_memory_bytes,
    });
    if let Err(error) = send_worker_request(&mut worker_stdin, &start).await {
        worker.kill_and_reap().await;
        return Err(mlua::Error::external(error));
    }

    let result = loop {
        enum WorkerOutcome {
            Read(io::Result<Option<WorkerEvent>>),
            Stopped(StopReason),
        }

        let outcome = futures_lite::future::race(
            async { WorkerOutcome::Read(read_worker_event(&mut worker_stdout).await) },
            async { WorkerOutcome::Stopped(wait_for_stop(&cancel, deadline).await) },
        )
        .await;
        let event = match outcome {
            WorkerOutcome::Read(Ok(Some(event))) => event,
            WorkerOutcome::Read(Ok(None)) => {
                worker.kill_and_reap().await;
                return Err(mlua::Error::runtime(
                    "interpreter worker exited without a result",
                ));
            }
            WorkerOutcome::Read(Err(error)) => {
                worker.kill_and_reap().await;
                return Err(mlua::Error::external(error));
            }
            WorkerOutcome::Stopped(reason) => {
                worker.kill_and_reap().await;
                return Err(mlua::Error::runtime(match reason {
                    StopReason::Cancelled => "cancelled",
                    StopReason::Deadline => INTERPRETER_TIMEOUT_ERR,
                }));
            }
        };
        match event {
            WorkerEvent::Started => {}
            WorkerEvent::Output { line } => {
                if let Err(error) = run_non_yieldable(&lua, || on_output.call::<()>(line)) {
                    worker.kill_and_reap().await;
                    return Err(error);
                }
            }
            WorkerEvent::ToolCalls { request_id, calls } => {
                let futures = calls.into_iter().map(|call| {
                    let function = fns.get(&call.name).cloned();
                    let lua = lua.clone();
                    async move {
                        WireCallResult {
                            call_id: call.call_id,
                            value: call_lua_tool(lua, function, &call).await,
                        }
                    }
                });
                let response = WorkerRequest::CallResults {
                    request_id,
                    results: join_all(futures).await,
                };
                if let Err(error) = send_worker_request(&mut worker_stdin, &response).await {
                    worker.kill_and_reap().await;
                    return Err(mlua::Error::external(error));
                }
            }
            WorkerEvent::Complete { output, stdout } => {
                worker.kill_and_reap().await;
                break Ok((output, stdout));
            }
            WorkerEvent::Failed { error } => {
                worker.kill_and_reap().await;
                break Err(error);
            }
        }
    };

    let tbl = lua.create_table()?;
    match result {
        Ok((output, stdout)) => {
            if !stdout.is_empty() {
                tbl.set("stdout", stdout.trim_end())?;
            }
            if let Some(value) = output {
                tbl.set("output", value.to_string())?;
            }
            Ok((tbl, None))
        }
        Err(error) => Ok((tbl, Some(error))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_INTERPRETER_TIMEOUT_SECS, interpreter_run, read_worker_event, ruff_fix,
        send_worker_request, spawn_worker,
    };
    use mlua::Lua;
    use n00n_agent::cancel::CancelToken;
    use n00n_interpreter::worker::{StartRequest, WorkerEvent, WorkerRequest};
    use std::time::{Duration, Instant};

    #[test]
    #[ignore]
    fn interpreter_worker_entry() {
        n00n_interpreter::worker::run_stdio().unwrap();
    }

    #[test]
    fn ruff_fix_removes_unused_import_and_formats() {
        if std::process::Command::new("ruff")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let cancel = CancelToken::none();
        assert_eq!(
            smol::block_on(ruff_fix(
                "import os\nx= 1\nprint(x)\n".into(),
                &cancel,
                Instant::now() + Duration::from_secs(5),
            ))
            .unwrap(),
            "x = 1\nprint(x)\n"
        );
    }

    #[test]
    fn ruff_fix_preserves_top_level_await_despite_lint_errors() {
        if std::process::Command::new("ruff")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let code = "result = await read(path='x')\nprint(result)\n";
        let cancel = CancelToken::none();
        let fixed = smol::block_on(ruff_fix(
            code.into(),
            &cancel,
            Instant::now() + Duration::from_secs(5),
        ))
        .unwrap();
        assert!(fixed.contains("await read"));
        assert!(fixed.contains("print(result)"));
    }

    #[test]
    fn interpreter_subprocess_streams_and_completes() {
        smol::block_on(async {
            let lua = Lua::new();
            let opts = lua.create_table().unwrap();
            opts.set("timeout", 10).unwrap();
            opts.set("max_memory_mb", 16).unwrap();
            opts.set(
                "on_output",
                lua.create_function(|_, _: String| Ok(())).unwrap(),
            )
            .unwrap();
            let _keepalive = lua.clone();
            let (result, error) = interpreter_run(lua, "print('ok')".to_owned(), opts)
                .await
                .unwrap();
            assert_eq!(error, None);
            assert_eq!(result.get::<String>("stdout").unwrap(), "ok");
        });
    }

    #[test]
    fn interpreter_rejects_timeout_above_finite_upper_bound() {
        smol::block_on(async {
            let lua = Lua::new();
            let opts = lua.create_table().unwrap();
            opts.set("timeout", MAX_INTERPRETER_TIMEOUT_SECS + 1)
                .unwrap();
            opts.set("max_memory_mb", 16).unwrap();
            opts.set(
                "on_output",
                lua.create_function(|_, _: String| Ok(())).unwrap(),
            )
            .unwrap();
            let error = interpreter_run(lua, "pass".to_owned(), opts)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("must be between 1 and 300"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_waits_until_cpu_worker_has_terminated() {
        use rustix::process::{Pid, test_kill_process};

        smol::block_on(async {
            let (mut worker, mut stdin, mut stdout) = spawn_worker().unwrap();
            let pid = worker.id();
            send_worker_request(
                &mut stdin,
                &WorkerRequest::Start(StartRequest {
                    code: "while True:\n    pass".to_owned(),
                    tool_names: Vec::new(),
                    timeout_millis: 10_000,
                    max_memory_bytes: 16 * 1024 * 1024,
                }),
            )
            .await
            .unwrap();
            assert!(matches!(
                read_worker_event(&mut stdout).await.unwrap(),
                Some(WorkerEvent::Started)
            ));
            worker.kill_and_reap().await;
            let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
            assert_eq!(test_kill_process(pid), Err(rustix::io::Errno::SRCH));
        });
    }

    #[cfg(unix)]
    #[test]
    fn process_tree_termination_reaps_guarded_process() {
        use std::os::unix::process::CommandExt;

        let mut child = std::process::Command::new("sh")
            .args(["-c", "while :; do :; done"])
            .process_group(0)
            .spawn()
            .unwrap();
        super::terminate_process_tree(child.id());
        let status = child.wait().unwrap();
        assert!(!status.success());
    }
}

lua_table! {
    /// Run Python code in a memory-safe, time-limited sandbox.
    ///
    /// The sandbox uses the monty interpreter. Python code can call back into
    /// Lua-defined tools, and stdout is streamed line by line. Requires the
    /// `run` permission.
    ///
    /// ```lua
    /// local r, err = n00n.interpreter.run("print('hello')", {
    ///   timeout = 10,
    ///   max_memory_mb = 128,
    ///   on_output = function(line) print(line) end,
    /// })
    /// ```
    "n00n.interpreter" => pub(crate) fn create_interpreter_table(perms: &PluginPermissions), DOCS [
        interpreter_run(perms),
    ]
}
