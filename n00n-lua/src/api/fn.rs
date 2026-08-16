use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table, Value};
use n00n_lua_macro::{lua_fn, lua_table};

use crate::api::fs::expand_tilde;
use crate::plugin_permissions::PluginPermissions;
use crate::runtime::{active_task_id, with_jobs};

const READER_BUF_SIZE: usize = 8 * 1024;
const OWNER_TASK: &str = "task";
const OWNER_PLUGIN: &str = "plugin";
const DEFER_DELAY_RANGE_ERR: &str = "defer delay is out of range";
const JOBWAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(crate) enum JobSpec {
    Shell(String),
    Program { program: String, args: Vec<String> },
}

#[derive(Clone)]
pub(crate) enum JobEvent {
    Stdout(String),
    Stderr(String),
    Exit(i32),
}

/// Lifetime of a job. Task-owned jobs die with the call that started them;
/// plugin-owned jobs survive until their plugin unloads or reloads.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum JobOwner {
    Task(u64),
    Plugin(Arc<str>),
}

struct JobMeta {
    owner: JobOwner,
    pid: Option<u32>,
    deadline: Option<Instant>,
    on_stdout: Option<RegistryKey>,
    on_stderr: Option<RegistryKey>,
    on_exit: Option<RegistryKey>,
    event_rx: Option<flume::Receiver<JobEvent>>,
}

impl JobMeta {
    fn can_access(&self, task_id: Option<u64>, plugin: &str) -> bool {
        match &self.owner {
            JobOwner::Task(owner_id) => task_id == Some(*owner_id),
            JobOwner::Plugin(owner_plugin) => owner_plugin.as_ref() == plugin,
        }
    }
}

pub(crate) struct JobStore {
    jobs: HashMap<u32, JobMeta>,
    next_id: u32,
}

/// Holds a receiver borrowed out of the store for the duration of a
/// `jobwait`, and hands it back on drop so a timed-out or cancelled wait
/// does not strand the job's remaining events.
enum JobWaitWake {
    Event(Option<JobEvent>),
    Poll,
}

struct CheckedOutReceiver {
    lua: Lua,
    job_id: u32,
    receiver: flume::Receiver<JobEvent>,
}

impl CheckedOutReceiver {
    fn new(lua: &Lua, job_id: u32, receiver: flume::Receiver<JobEvent>) -> Self {
        Self {
            lua: lua.clone(),
            job_id,
            receiver,
        }
    }

    fn get(&self) -> &flume::Receiver<JobEvent> {
        &self.receiver
    }
}

impl Drop for CheckedOutReceiver {
    fn drop(&mut self) {
        let receiver = self.receiver.clone();
        with_jobs(&self.lua, |store| {
            store.restore_receiver(self.job_id, receiver);
        });
    }
}

impl JobStore {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            next_id: 1,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn start(
        &mut self,
        owner: JobOwner,
        spec: JobSpec,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        on_stdout: Option<RegistryKey>,
        on_stderr: Option<RegistryKey>,
        on_exit: Option<RegistryKey>,
    ) -> Result<u32, String> {
        let mut command = match spec {
            JobSpec::Shell(cmd) => n00n_config::bash_command(&cmd)?,
            JobSpec::Program { program, args } => {
                let mut c = Command::new(&program);
                c.args(&args);
                c
            }
        };
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        if let Some(dir) = cwd.as_deref().map(expand_tilde) {
            if !dir.is_dir() {
                return Err(format!("cwd is not a directory: {}", dir.display()));
            }
            command.current_dir(dir);
        }
        if let Some(ref env_map) = env {
            for (k, v) in env_map {
                command.env(k, v);
            }
        }

        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();
        let id = self.next_id;
        self.next_id += 1;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (event_tx, event_rx) = flume::unbounded();

        macro_rules! spawn_reader {
            ($stream:expr, $name:expr, $variant:ident) => {
                if let Some(stream) = $stream {
                    let tx = event_tx.clone();
                    Some(
                        thread::Builder::new()
                            .name($name.into())
                            .spawn(move || {
                                let mut reader = BufReader::with_capacity(READER_BUF_SIZE, stream);
                                let mut line = String::new();
                                loop {
                                    line.clear();
                                    match reader.read_line(&mut line) {
                                        Ok(0) => break,
                                        Ok(_) => {
                                            if line.ends_with('\n') {
                                                line.pop();
                                                if line.ends_with('\r') {
                                                    line.pop();
                                                }
                                            }
                                            if tx.send(JobEvent::$variant(line.clone())).is_err() {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            })
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                }
            };
        }
        let stdout_handle = spawn_reader!(stdout, "job-stdout", Stdout);
        let stderr_handle = spawn_reader!(stderr, "job-stderr", Stderr);

        thread::Builder::new()
            .name("job-wait".into())
            .spawn(move || {
                let code = match child.wait() {
                    Ok(status) => status.code().unwrap_or_else(|| -1),
                    Err(error) => {
                        tracing::error!(error = %error, "job wait failed");
                        -1
                    }
                };
                if let Some(h) = stdout_handle {
                    let _ = h.join();
                }
                if let Some(h) = stderr_handle {
                    let _ = h.join();
                }
                let _ = event_tx.send(JobEvent::Exit(code));
            })
            .map_err(|e| e.to_string())?;

        self.jobs.insert(
            id,
            JobMeta {
                owner,
                pid: Some(pid),
                deadline: None,
                on_stdout,
                on_stderr,
                on_exit,
                event_rx: Some(event_rx),
            },
        );

        Ok(id)
    }

    pub fn defer(
        &mut self,
        owner: JobOwner,
        delay: Duration,
        on_exit: RegistryKey,
    ) -> Result<u32, &'static str> {
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or(DEFER_DELAY_RANGE_ERR)?;
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.insert(
            id,
            JobMeta {
                owner,
                pid: None,
                deadline: Some(deadline),
                on_stdout: None,
                on_stderr: None,
                on_exit: Some(on_exit),
                event_rx: None,
            },
        );
        Ok(id)
    }

    pub fn is_empty(&self, owner: &JobOwner) -> bool {
        !self.jobs.values().any(|job| job.owner == *owner)
    }

    pub fn callback_key(&self, job_id: u32, event: &JobEvent) -> Option<&RegistryKey> {
        let meta = self.jobs.get(&job_id)?;
        match event {
            JobEvent::Stdout(_) => meta.on_stdout.as_ref(),
            JobEvent::Stderr(_) => meta.on_stderr.as_ref(),
            JobEvent::Exit(_) => meta.on_exit.as_ref(),
        }
    }

    pub fn take_receiver(
        &mut self,
        job_id: u32,
        task_id: Option<u64>,
        plugin: &str,
    ) -> Option<flume::Receiver<JobEvent>> {
        let job = self.jobs.get_mut(&job_id)?;
        job.can_access(task_id, plugin)
            .then(|| job.event_rx.take())?
    }

    pub fn restore_receiver(&mut self, job_id: u32, receiver: flume::Receiver<JobEvent>) {
        if let Some(job) = self.jobs.get_mut(&job_id)
            && job.event_rx.is_none()
        {
            job.event_rx = Some(receiver);
        }
    }

    pub fn drain_events(&mut self, owner: &JobOwner, buf: &mut Vec<(u32, JobEvent)>) {
        self.drain_matching(buf, |job| job.owner == *owner);
    }

    pub fn drain_plugin_events(&mut self, buf: &mut Vec<(u32, JobEvent)>) {
        self.drain_matching(buf, |job| matches!(job.owner, JobOwner::Plugin(_)));
    }

    fn drain_matching(&mut self, buf: &mut Vec<(u32, JobEvent)>, keep: impl Fn(&JobMeta) -> bool) {
        buf.clear();
        for (&id, job) in self.jobs.iter_mut().filter(|(_, job)| keep(job)) {
            if job
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                job.deadline = None;
                buf.push((id, JobEvent::Exit(0)));
            } else if let Some(ref rx) = job.event_rx {
                while let Ok(event) = rx.try_recv() {
                    buf.push((id, event));
                }
            }
        }
    }

    pub fn kill(&mut self, job_id: u32, task_id: Option<u64>, plugin: &str) {
        let can_access = self
            .jobs
            .get(&job_id)
            .is_some_and(|job| job.can_access(task_id, plugin));
        if !can_access {
            return;
        }
        if let Some(job) = self.jobs.get_mut(&job_id) {
            if job.pid.is_none() {
                if job.deadline.take().is_none() {
                    return;
                }
                let (event_tx, event_rx) = flume::bounded(1);
                let _ = event_tx.send(JobEvent::Exit(-1));
                job.event_rx = Some(event_rx);
            } else {
                kill_job(job);
            }
        }
    }

    /// Kill and forget every job belonging to {owner}. Used when a task
    /// scope ends and when a plugin is unloaded or reloaded.
    pub fn kill_owner(&mut self, lua: &Lua, owner: &JobOwner) {
        let ids = self
            .jobs
            .iter()
            .filter_map(|(&id, job)| (job.owner == *owner).then_some(id))
            .collect::<Vec<_>>();
        for id in ids {
            self.remove(lua, id, true);
        }
    }

    /// Forget a job that already exited, releasing its callback keys.
    pub fn finish(&mut self, lua: &Lua, job_id: u32) {
        self.remove(lua, job_id, false);
    }

    fn remove(&mut self, lua: &Lua, job_id: u32, kill: bool) {
        if let Some(mut job) = self.jobs.remove(&job_id) {
            if kill {
                kill_job(&mut job);
            }
            for key in [job.on_stdout, job.on_stderr, job.on_exit]
                .into_iter()
                .flatten()
            {
                if let Err(error) = lua.remove_registry_value(key) {
                    tracing::warn!(job_id, %error, "failed to drop job callback key");
                }
            }
        }
    }
}

impl Drop for JobStore {
    fn drop(&mut self) {
        for job in self.jobs.values_mut() {
            kill_job(job);
        }
    }
}

fn kill_job(meta: &mut JobMeta) {
    let Some(pid) = meta.pid else {
        return;
    };
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};
        let Ok(raw) = i32::try_from(pid) else {
            return;
        };
        if let Some(pid) = Pid::from_raw(raw) {
            let Some(sig) = Signal::from_named_raw(libc::SIGKILL) else {
                return;
            };
            let _ = kill_process_group(pid, sig);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .status();
    }
}

/// Run a shell command in the background. The command runs through
/// `bash -c` on Unix or `cmd /C` on Windows. You get back a job id
/// that you can pass to `jobstop` or `jobwait` to control the process.
///
/// For commands that don't need shell features (pipes, redirection, globs),
/// pass an array to run the program directly with preserved argument quoting:
/// `n00n.fn.jobstart({ "git", "commit", "-m", "feat: msg" })`
///
/// @param cmd string|table Shell command string, or array of program + args.
/// @param opts table? Optional settings:
///   `cwd` (string?) working directory (tilde is expanded).
///   `env` (table?) extra environment variables, `{ VAR = "value" }`.
///   `on_stdout` (function?) called with `(job_id, line)` for each stdout line.
///   `on_stderr` (function?) called with `(job_id, line)` for each stderr line.
///   `on_exit` (function?) called with `(job_id, code)` when the process finishes.
///   `owner` (string?) job lifetime. `"task"` (default) ends the job with
///     the current call. `"plugin"` keeps it alive until the plugin unloads
///     or reloads.
/// @return (integer) Job id.
/// @example
/// -- String mode (shell features available)
/// local id = n00n.fn.jobstart("ls -la", {
///   cwd = "~/projects",
///   on_stdout = function(_, line) print(line) end,
///   on_exit = function(_, code) print("exit: " .. code) end,
/// })
/// -- List mode (preserves argument quoting)
/// local id = n00n.fn.jobstart({ "git", "commit", "-m", "feat: preserve spaces" }, opts)
/// -- Plugin-owned watcher that outlives the call that started it
/// local watcher = n00n.fn.jobstart("tail -F app.log", { owner = "plugin" })
#[lua_fn(guard = Run)]
#[allow(clippy::needless_pass_by_value)]
fn jobstart(lua: &Lua, #[ctx] plugin: Arc<str>, cmd: Value, opts: Option<Table>) -> LuaResult<u32> {
    let spec = match cmd {
        Value::String(s) => {
            let cmd = s.to_str()?.to_string();
            JobSpec::Shell(cmd)
        }
        Value::Table(t) => {
            let mut iter = t.sequence_values::<String>();
            let program = match iter.next() {
                Some(Ok(p)) => p,
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(mlua::Error::runtime(
                        "jobstart: table must have at least a program",
                    ));
                }
            };
            if program.is_empty() {
                return Err(mlua::Error::runtime("jobstart: program cannot be empty"));
            }
            let args: Vec<String> = iter
                .filter_map(Result::ok)
                .filter(|s| !s.is_empty())
                .collect();
            JobSpec::Program { program, args }
        }
        _ => {
            return Err(mlua::Error::runtime(
                "jobstart: cmd must be a string or table",
            ));
        }
    };

    let owner_name: Option<String> = opts
        .as_ref()
        .map(|opts| opts.get("owner"))
        .transpose()?
        .flatten();
    let owner = match owner_name.as_deref() {
        None | Some(OWNER_TASK) => active_task_id(lua).map(JobOwner::Task).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "jobstart: no active task; use owner = {OWNER_PLUGIN:?}"
            ))
        })?,
        Some(OWNER_PLUGIN) => JobOwner::Plugin(Arc::clone(&plugin)),
        Some(other) => {
            return Err(mlua::Error::runtime(format!(
                "jobstart: unknown owner {other:?}; expected {OWNER_TASK:?} or {OWNER_PLUGIN:?}"
            )));
        }
    };

    let (cwd, env, on_stdout, on_stderr, on_exit) = match opts {
        Some(ref opts) => {
            let cwd: Option<String> = opts.get("cwd").ok();
            let env: Option<HashMap<String, String>> = opts
                .get::<Table>("env")
                .ok()
                .map(|t| t.pairs::<String, String>().filter_map(Result::ok).collect());
            let on_stdout = opts
                .get::<Function>("on_stdout")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;
            let on_stderr = opts
                .get::<Function>("on_stderr")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;
            let on_exit = opts
                .get::<Function>("on_exit")
                .ok()
                .map(|f| lua.create_registry_value(f))
                .transpose()?;
            (cwd, env, on_stdout, on_stderr, on_exit)
        }
        None => (None, None, None, None, None),
    };

    with_jobs(lua, |store| {
        store.start(owner, spec, cwd, env, on_stdout, on_stderr, on_exit)
    })
    .map_err(mlua::Error::runtime)
}

/// Run a callback after a delay without spawning a process.
///
/// The timer belongs to the current tool call and is cancelled when that call
/// ends. Use this from tool handlers; a timer scheduled by a plugin-owned
/// callback does not outlive that callback's task scope.
///
/// @param delay_ms integer Delay in milliseconds.
/// @param callback function Called with the timer id and exit code `0` after the delay, or `-1` when cancelled by `jobstop`.
/// @return (integer) Timer job id accepted by `jobstop`.
/// @example
/// n00n.fn.defer(1000, function(timer_id, code) refresh() end)
#[lua_fn(guard = Run)]
fn defer(lua: &Lua, delay_ms: u64, callback: Function) -> LuaResult<u32> {
    let owner = active_task_id(lua)
        .map(JobOwner::Task)
        .ok_or_else(|| mlua::Error::runtime("defer: no active task"))?;
    let callback = lua.create_registry_value(callback)?;
    with_jobs(lua, |store| {
        store.defer(owner, Duration::from_millis(delay_ms), callback)
    })
    .map_err(mlua::Error::runtime)
}

/// Kill a running process immediately (SIGKILL on Unix) or cancel a deferred
/// timer. Safe to call on jobs that already exited or on unknown ids. A
/// cancelled timer's callback runs with exit code `-1`.
///
/// @param job_id integer Job id returned by `jobstart` or `defer`.
/// @return
/// @example
/// n00n.fn.jobstop(id)
#[lua_fn(guard = Run)]
fn jobstop(lua: &Lua, #[ctx] plugin: Arc<str>, job_id: u32) -> LuaResult<()> {
    let task_id = active_task_id(lua);
    with_jobs(lua, |store| store.kill(job_id, task_id, &plugin));
    Ok(())
}

fn next_jobwait_poll(timeout_at: Instant) -> LuaResult<Instant> {
    Instant::now()
        .checked_add(JOBWAIT_POLL_INTERVAL)
        .map(|poll_at| poll_at.min(timeout_at))
        .ok_or_else(|| mlua::Error::runtime("jobwait poll deadline is out of range"))
}

pub(crate) fn deliver_task_job_events(
    lua: &Lua,
    task_events: &mut Vec<(u32, JobEvent)>,
) -> LuaResult<()> {
    let mut first_error = None;
    for (event_job_id, event) in task_events.drain(..) {
        if let Err(error) = deliver_job_event(lua, event_job_id, &event)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn drain_task_job_events(
    lua: &Lua,
    owner: Option<&JobOwner>,
    task_events: &mut Vec<(u32, JobEvent)>,
) -> LuaResult<()> {
    if let Some(owner) = owner {
        with_jobs(lua, |store| store.drain_events(owner, task_events));
        if let Err(error) = deliver_task_job_events(lua, task_events) {
            tracing::warn!(%error, "jobwait sibling job callback failed");
        }
    }
    Ok(())
}

/// Wait for a job to finish and collect its output. Returns a result
/// table with `stdout`, `stderr`, and `exit_code`. Returns `nil` if the
/// job does not finish before the timeout.
///
/// While waiting, the job's `on_stdout`, `on_stderr`, and `on_exit`
/// callbacks fire as events arrive (like Neovim), so you can stream
/// output into a buffer while parked here.
///
/// @param job_id integer Job id returned by `jobstart`.
/// @param timeout_ms integer? Maximum wait in milliseconds (default 30000).
/// @return (table?) `{ stdout, stderr, exit_code }`, or nil on timeout.
/// @example
/// local id = n00n.fn.jobstart("echo hello")
/// local result = n00n.fn.jobwait(id, 5000)
/// if result then
///   print(result.stdout)
/// end
#[lua_fn(guard = Run)]
async fn jobwait(
    lua: Lua,
    #[ctx] plugin: Arc<str>,
    job_id: u32,
    timeout_ms: Option<u64>,
) -> LuaResult<Value> {
    let task_id = active_task_id(&lua);
    let receiver = with_jobs(&lua, |store| store.take_receiver(job_id, task_id, &plugin))
        .ok_or_else(|| mlua::Error::runtime("unknown job id or already waited"))?;
    let rx = CheckedOutReceiver::new(&lua, job_id, receiver);

    let timeout = Duration::from_millis(timeout_ms.unwrap_or_else(|| 30_000));
    let timeout_at = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| mlua::Error::runtime("jobwait timeout is out of range"))?;
    let owner = task_id.map(JobOwner::Task);
    let mut task_events = Vec::new();
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut poll_at = next_jobwait_poll(timeout_at)?;

    let exit_code = loop {
        let now = Instant::now();
        if now >= timeout_at {
            return Ok(mlua::Value::Nil);
        }
        if now >= poll_at {
            drain_task_job_events(&lua, owner.as_ref(), &mut task_events)?;
            poll_at = next_jobwait_poll(timeout_at)?;
            continue;
        }
        let wake = futures_lite::future::or(
            async { JobWaitWake::Event(rx.get().recv_async().await.ok()) },
            async {
                smol::Timer::at(poll_at).await;
                JobWaitWake::Poll
            },
        )
        .await;

        match wake {
            JobWaitWake::Event(Some(event)) => {
                deliver_job_event(&lua, job_id, &event)?;
                match event {
                    JobEvent::Stdout(line) => stdout_lines.push(line),
                    JobEvent::Stderr(line) => stderr_lines.push(line),
                    JobEvent::Exit(code) => {
                        drain_task_job_events(&lua, owner.as_ref(), &mut task_events)?;
                        break code;
                    }
                }
            }
            JobWaitWake::Event(None) => return Ok(mlua::Value::Nil),
            JobWaitWake::Poll => {}
        }
    };

    let result = lua.create_table()?;
    result.set("stdout", stdout_lines.join("\n"))?;
    result.set("stderr", stderr_lines.join("\n"))?;
    result.set("exit_code", exit_code)?;
    Ok(mlua::Value::Table(result))
}

/// Fire the job's Lua callback for {event} (if any) and forget the job
/// on exit. Shared by `jobwait` and the async dispatch loop so both
/// deliver events identically.
pub(crate) fn deliver_job_event(lua: &Lua, job_id: u32, event: &JobEvent) -> LuaResult<()> {
    let callback = with_jobs(lua, |store| {
        store
            .callback_key(job_id, event)
            .and_then(|key| lua.registry_value::<Function>(key).ok())
    });
    if let JobEvent::Exit(_) = event {
        with_jobs(lua, |store| store.finish(lua, job_id));
    }
    if let Some(callback) = callback {
        let arg = match event {
            JobEvent::Stdout(line) | JobEvent::Stderr(line) => {
                Value::String(lua.create_string(line)?)
            }
            JobEvent::Exit(code) => Value::Integer(i64::from(*code)),
        };
        callback.call::<()>((job_id, arg))?;
    }
    Ok(())
}

/// Check whether {name} can be found on `$PATH` or is an absolute path
/// to a file. Returns 1 when found, 0 otherwise (matches Neovim's
/// `vim.fn.executable`).
///
/// @param name string Program name (e.g. `"git"`) or absolute path.
/// @return (integer) `1` if found, `0` otherwise.
/// @example
/// if n00n.fn.executable("rg") == 1 then
///   -- use ripgrep
/// end
#[lua_fn(guard = Env)]
fn executable(_lua: &Lua, name: String) -> LuaResult<i32> {
    let found = env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| dir.join(&name).is_file()))
        || Path::new(&name).is_file();
    Ok(i32::from(found))
}

lua_table! {
    /// Process and environment helpers, modeled after Neovim's `vim.fn` job
    /// control. Use these to run shell commands, wait for output, and check
    /// whether programs are installed.
    ///
    /// Job functions need the `run` permission. `executable` needs the `env`
    /// permission.
    ///
    /// A job belongs to the call that started it unless you pass
    /// `owner = "plugin"`, which keeps it running until the plugin unloads
    /// or reloads. Only the owning task or plugin can stop or wait on a job.
    ///
    /// ```lua
    /// local id = n00n.fn.jobstart("git status", {
    ///   on_exit = function(code) print("done: " .. code) end,
    /// })
    /// ```
    "n00n.fn" => pub(crate) fn create_fn_table(
        plugin: Arc<str>,
        perms: &PluginPermissions,
    ), DOCS [
        jobstart(perms, plugin), jobstop(perms, plugin), jobwait(perms, plugin),
        defer(perms), executable(perms),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::runtime::{TaskCell, TaskScope, lock_cell};
    #[cfg(unix)]
    use n00n_agent::cancel::CancelToken;

    const TEST_PLUGIN: &str = "test-plugin";
    const OTHER_PLUGIN: &str = "other-plugin";
    const OWNER_TASK_ID: u64 = 1;
    const FOREIGN_TASK_ID: u64 = 2;
    const UNKNOWN_JOB_ID: u32 = 999;
    #[cfg(unix)]
    const SLEEP_CMD: &str = "sleep 30";
    const EVENT_DEADLINE: Duration = Duration::from_secs(5);

    fn make_store() -> JobStore {
        JobStore::new()
    }

    fn task_owner(id: u64) -> JobOwner {
        JobOwner::Task(id)
    }

    fn plugin_owner() -> JobOwner {
        JobOwner::Plugin(Arc::from(TEST_PLUGIN))
    }

    fn start_shell(store: &mut JobStore, owner: JobOwner, cmd: &str) -> u32 {
        store
            .start(
                owner,
                JobSpec::Shell(cmd.to_string()),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
    }

    fn start_echo(store: &mut JobStore) -> u32 {
        start_shell(store, task_owner(OWNER_TASK_ID), "echo hello")
    }

    #[cfg(unix)]
    fn group_alive(pid: u32) -> bool {
        use rustix::process::{Pid, test_kill_process_group};
        i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .is_some_and(|pid| test_kill_process_group(pid).is_ok())
    }

    #[cfg(unix)]
    fn wait_for_group_exit(pid: u32) -> bool {
        (0..500).any(|_| {
            thread::sleep(Duration::from_millis(10));
            !group_alive(pid)
        })
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_store_kills_its_jobs() {
        let mut store = make_store();
        let id = start_shell(&mut store, task_owner(OWNER_TASK_ID), SLEEP_CMD);
        let pid = store.jobs[&id].pid.expect("process job pid");
        assert!(group_alive(pid), "job should be running before the drop");

        drop(store);

        assert!(
            wait_for_group_exit(pid),
            "dropping the store must not orphan the process group"
        );
    }

    #[test]
    fn start_invalid_cwd_returns_error() {
        let mut store = make_store();
        let result = store.start(
            task_owner(OWNER_TASK_ID),
            JobSpec::Shell("echo hello".to_string()),
            Some("/nonexistent_dir_abc_xyz_123".into()),
            None,
            None,
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn finishing_a_job_removes_it() {
        let lua = Lua::new();
        let mut store = make_store();
        let owner = task_owner(OWNER_TASK_ID);
        assert!(store.is_empty(&owner));

        let id = start_echo(&mut store);
        assert!(!store.is_empty(&owner));
        let rx = store
            .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
            .unwrap();
        while !matches!(rx.recv_timeout(EVENT_DEADLINE).unwrap(), JobEvent::Exit(_)) {}

        store.finish(&lua, id);
        assert!(store.is_empty(&owner));
    }

    #[test]
    fn deferred_callback_becomes_due_without_a_process() {
        let lua = Lua::new();
        let callback = lua
            .create_registry_value(lua.create_function(|_, ()| Ok(())).unwrap())
            .unwrap();
        let mut store = make_store();
        let owner = task_owner(OWNER_TASK_ID);
        let id = store
            .defer(owner.clone(), Duration::ZERO, callback)
            .unwrap();
        assert!(store.jobs[&id].pid.is_none());

        let mut events = Vec::new();
        store.drain_events(&owner, &mut events);
        assert!(matches!(events.as_slice(), [(event_id, JobEvent::Exit(0))] if *event_id == id));
        store.drain_events(&owner, &mut events);
        assert!(events.is_empty(), "a due timer must only emit once");

        store.finish(&lua, id);
        assert!(store.is_empty(&owner));
    }

    #[test]
    fn deferred_callback_rejects_an_unrepresentable_deadline() {
        let lua = Lua::new();
        let callback = lua
            .create_registry_value(lua.create_function(|_, ()| Ok(())).unwrap())
            .unwrap();
        let mut store = make_store();
        let result = store.defer(task_owner(OWNER_TASK_ID), Duration::MAX, callback);
        assert_eq!(result.unwrap_err(), DEFER_DELAY_RANGE_ERR);
    }

    #[test]
    fn cancelling_a_timer_still_delivers_its_exit_event() {
        let lua = Lua::new();
        let callback = lua
            .create_registry_value(lua.create_function(|_, ()| Ok(())).unwrap())
            .unwrap();
        let mut store = make_store();
        let owner = task_owner(OWNER_TASK_ID);
        let id = store
            .defer(owner.clone(), Duration::from_mins(1), callback)
            .unwrap();

        let mut events = Vec::new();
        store.kill(id, Some(FOREIGN_TASK_ID), TEST_PLUGIN);
        store.drain_events(&owner, &mut events);
        assert!(events.is_empty());

        store.kill(id, Some(OWNER_TASK_ID), TEST_PLUGIN);
        store.kill(id, Some(OWNER_TASK_ID), TEST_PLUGIN);
        store.drain_events(&owner, &mut events);
        assert!(matches!(events.as_slice(), [(event_id, JobEvent::Exit(-1))] if *event_id == id));
        store.finish(&lua, id);
        assert!(store.is_empty(&owner));
    }

    #[cfg(unix)]
    #[test]
    fn jobwait_pumps_deferred_callbacks() {
        let lua = Lua::new();
        let scope = TaskScope::new(&lua, TaskCell::new(CancelToken::none(), None, None, None));
        let owner = task_owner(lock_cell(scope.handle()).id);
        let callback = lua
            .create_registry_value(
                lua.create_function(|lua, ()| lua.globals().set("timer_fired", true))
                    .unwrap(),
            )
            .unwrap();
        let (process_id, _) = with_jobs(&lua, |store| {
            let process_id = start_shell(store, owner.clone(), "sleep 0.15");
            let timer_id = store.defer(owner, Duration::ZERO, callback).unwrap();
            (process_id, timer_id)
        });

        smol::block_on(scope.scope_future(jobwait(
            lua.clone(),
            Arc::from(TEST_PLUGIN),
            process_id,
            Some(1_000),
        )))
        .unwrap();

        assert!(lua.globals().get::<bool>("timer_fired").unwrap());
    }

    #[test]
    fn failing_task_callback_does_not_drop_later_events() {
        let lua = Lua::new();
        lua.globals().set("later_callback_fired", false).unwrap();
        let owner = task_owner(OWNER_TASK_ID);
        let (failing_id, later_id) = with_jobs(&lua, |store| {
            let failing = lua
                .create_registry_value(
                    lua.create_function(|_, ()| Err::<(), _>(mlua::Error::runtime("boom")))
                        .unwrap(),
                )
                .unwrap();
            let later = lua
                .create_registry_value(
                    lua.create_function(|lua, ()| lua.globals().set("later_callback_fired", true))
                        .unwrap(),
                )
                .unwrap();
            let delay = Duration::from_mins(1);
            (
                store.defer(owner.clone(), delay, failing).unwrap(),
                store.defer(owner.clone(), delay, later).unwrap(),
            )
        });
        let mut events = vec![
            (failing_id, JobEvent::Exit(0)),
            (later_id, JobEvent::Exit(0)),
        ];

        assert!(deliver_task_job_events(&lua, &mut events).is_err());
        assert!(events.is_empty());
        assert!(lua.globals().get::<bool>("later_callback_fired").unwrap());
        assert!(with_jobs(&lua, |store| store.is_empty(&owner)));
    }

    #[cfg(unix)]
    #[test]
    fn jobwait_pumps_deferred_callbacks_during_continuous_output() {
        let lua = Lua::new();
        let scope = TaskScope::new(&lua, TaskCell::new(CancelToken::none(), None, None, None));
        let owner = task_owner(lock_cell(scope.handle()).id);
        lua.globals().set("timer_fired", false).unwrap();
        let timer_callback = lua
            .create_registry_value(
                lua.create_function(|lua, ()| lua.globals().set("timer_fired", true))
                    .unwrap(),
            )
            .unwrap();
        let exit_callback = lua
            .create_registry_value(
                lua.create_function(|lua, (_job_id, _code): (u32, i32)| {
                    let fired = lua.globals().get::<bool>("timer_fired")?;
                    lua.globals().set("timer_fired_before_exit", fired)
                })
                .unwrap(),
            )
            .unwrap();
        let process_id = with_jobs(&lua, |store| {
            let process_id = store
                .start(
                    owner.clone(),
                    JobSpec::Shell(
                        "i=0; while [ $i -lt 100000 ]; do echo x; i=$((i+1)); done".into(),
                    ),
                    None,
                    None,
                    None,
                    None,
                    Some(exit_callback),
                )
                .unwrap();
            store.defer(owner, Duration::ZERO, timer_callback).unwrap();
            process_id
        });

        smol::block_on(scope.scope_future(jobwait(
            lua.clone(),
            Arc::from(TEST_PLUGIN),
            process_id,
            Some(5_000),
        )))
        .unwrap();

        assert!(
            lua.globals()
                .get::<bool>("timer_fired_before_exit")
                .unwrap()
        );
    }

    #[test]
    fn unknown_job_operations_are_noops() {
        let mut store = make_store();
        store.kill(UNKNOWN_JOB_ID, Some(OWNER_TASK_ID), TEST_PLUGIN);
        assert!(
            store
                .take_receiver(UNKNOWN_JOB_ID, Some(OWNER_TASK_ID), TEST_PLUGIN)
                .is_none()
        );
        assert!(
            store
                .callback_key(UNKNOWN_JOB_ID, &JobEvent::Exit(0))
                .is_none()
        );
    }

    #[test]
    fn take_receiver_lifecycle() {
        let mut store = make_store();
        let id = start_echo(&mut store);
        assert!(
            store
                .take_receiver(id, Some(FOREIGN_TASK_ID), TEST_PLUGIN)
                .is_none(),
            "another task must not access the job"
        );
        assert!(
            store
                .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
                .is_some()
        );
        assert!(
            store
                .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
                .is_none(),
            "second take should fail (receiver already moved)"
        );
    }

    #[test]
    fn restore_receiver_hands_the_events_back() {
        let mut store = make_store();
        let id = start_echo(&mut store);
        let rx = store
            .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
            .unwrap();

        store.restore_receiver(id, rx);

        assert!(
            store
                .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
                .is_some(),
            "a restored receiver can be taken again"
        );
    }

    #[test]
    fn plugin_owned_jobs_are_reachable_only_by_their_plugin() {
        let mut store = make_store();
        let id = start_shell(&mut store, plugin_owner(), "echo hello");

        assert!(
            store
                .take_receiver(id, Some(OWNER_TASK_ID), OTHER_PLUGIN)
                .is_none()
        );
        assert!(store.take_receiver(id, None, TEST_PLUGIN).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn kill_requires_owner_access() {
        let lua = Lua::new();
        let mut store = make_store();
        let id = start_shell(&mut store, task_owner(OWNER_TASK_ID), SLEEP_CMD);
        let pid = store.jobs[&id].pid.expect("process job pid");

        store.kill(id, Some(FOREIGN_TASK_ID), TEST_PLUGIN);
        assert!(group_alive(pid), "a foreign task must not kill the job");

        store.kill(id, Some(OWNER_TASK_ID), TEST_PLUGIN);
        assert!(wait_for_group_exit(pid));
        store.finish(&lua, id);
    }

    #[cfg(unix)]
    #[test]
    fn owner_cleanup_is_isolated() {
        let lua = Lua::new();
        let mut store = make_store();
        let task = task_owner(OWNER_TASK_ID);
        let plugin = plugin_owner();
        let task_job = start_shell(&mut store, task.clone(), SLEEP_CMD);
        let plugin_job = start_shell(&mut store, plugin.clone(), SLEEP_CMD);
        let task_pid = store.jobs[&task_job].pid.expect("task process pid");
        let plugin_pid = store.jobs[&plugin_job].pid.expect("plugin process pid");

        store.kill_owner(&lua, &task);

        assert!(store.is_empty(&task));
        assert!(!store.is_empty(&plugin));
        assert!(wait_for_group_exit(task_pid));
        assert!(group_alive(plugin_pid), "plugin jobs outlive the task");

        store.kill_owner(&lua, &plugin);
        assert!(wait_for_group_exit(plugin_pid));
    }

    #[test]
    fn callback_key_returns_none_without_callbacks() {
        let mut store = make_store();
        let id = start_echo(&mut store);
        assert!(
            store
                .callback_key(id, &JobEvent::Stdout("x".into()))
                .is_none()
        );
        assert!(
            store
                .callback_key(id, &JobEvent::Stderr("x".into()))
                .is_none()
        );
        assert!(store.callback_key(id, &JobEvent::Exit(0)).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn kill_job_terminates_long_running_child() {
        let mut store = make_store();
        let id = start_shell(&mut store, task_owner(OWNER_TASK_ID), "sleep 60");

        std::thread::sleep(Duration::from_millis(100));
        store.kill(id, Some(OWNER_TASK_ID), TEST_PLUGIN);

        let rx = store
            .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
            .unwrap();
        let mut got_exit = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(JobEvent::Exit(_)) => {
                    got_exit = true;
                    break;
                }
                Ok(_) | Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(got_exit, "kill should force the child to exit");
    }

    #[test]
    fn take_receiver_delivers_events() {
        let mut store = make_store();
        let id = start_echo(&mut store);
        let rx = store
            .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
            .unwrap();

        let mut got_exit = false;
        let deadline = std::time::Instant::now() + EVENT_DEADLINE;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(JobEvent::Exit(_)) => {
                    got_exit = true;
                    break;
                }
                Ok(_) | Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(got_exit, "should receive exit event for completed job");
    }

    #[test]
    fn drain_events_filters_by_owner() {
        let mut store = make_store();
        let task_job = start_echo(&mut store);
        let plugin_job = start_shell(&mut store, plugin_owner(), "echo plugin");

        let mut buf = Vec::new();
        let deadline = std::time::Instant::now() + EVENT_DEADLINE;
        loop {
            store.drain_events(&task_owner(OWNER_TASK_ID), &mut buf);
            if buf
                .iter()
                .any(|(jid, e)| *jid == task_job && matches!(e, JobEvent::Exit(_)))
            {
                break;
            }
            assert!(
                std::time::Instant::now() <= deadline,
                "should receive exit event for completed job"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(buf.iter().all(|(job_id, _)| *job_id != plugin_job));

        let deadline = std::time::Instant::now() + EVENT_DEADLINE;
        loop {
            store.drain_plugin_events(&mut buf);
            if buf
                .iter()
                .any(|(jid, e)| *jid == plugin_job && matches!(e, JobEvent::Exit(_)))
            {
                break;
            }
            assert!(
                std::time::Instant::now() <= deadline,
                "should receive exit event for the plugin-owned job"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(buf.iter().all(|(job_id, _)| *job_id != task_job));
    }

    #[test]
    fn drain_events_empty_after_take() {
        let mut store = make_store();
        let id = start_echo(&mut store);
        let _rx = store
            .take_receiver(id, Some(OWNER_TASK_ID), TEST_PLUGIN)
            .unwrap();

        let mut buf = Vec::new();
        store.drain_events(&task_owner(OWNER_TASK_ID), &mut buf);
        assert!(
            buf.is_empty(),
            "drained receiver yields no events via drain_events"
        );
    }
}
