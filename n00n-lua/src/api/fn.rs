use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::{HashSet, VecDeque};
use std::env;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table, Value};
use n00n_lua_macro::{lua_fn, lua_table};

use crate::api::fs::expand_tilde;
use crate::plugin_permissions::PluginPermissions;
use crate::runtime::{active_task_id, run_non_yieldable, with_jobs};

const READER_BUF_SIZE: usize = 8 * 1024;
const MAX_JOB_LINE_BYTES: usize = 64 * 1024;
const JOB_EVENT_CAPACITY: usize = 256;
pub(crate) const MAX_JOB_EVENTS_PER_TURN: usize = 64;
const MAX_JOBWAIT_RETAINED_LINES: usize = 10_000;
const MAX_JOBWAIT_RETAINED_BYTES: usize = 1024 * 1024;
const JOBWAIT_TRUNCATION_MARKER: &str = "[... job output truncated ...]";
#[cfg(target_os = "linux")]
const JOB_RSS_POLL_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const JOB_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(target_os = "linux")]
const JOB_RSS_LIMIT_ENV: &str = "N00N_TOOL_MAX_RSS_MB";
#[cfg(target_os = "linux")]
const DEFAULT_JOB_RSS_LIMIT_MIN: u64 = 512 * 1024 * 1024;
#[cfg(target_os = "linux")]
const DEFAULT_JOB_RSS_LIMIT_MAX: u64 = 8 * 1024 * 1024 * 1024;
#[cfg(unix)]
const JOB_NICE_ADJUSTMENT: i32 = 10;
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
#[derive(Clone, PartialEq, Eq, Hash)]
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

fn pump_job_output<R: BufRead>(
    mut reader: R,
    event_tx: &flume::Sender<JobEvent>,
    event: fn(String) -> JobEvent,
) {
    let mut line = Vec::with_capacity(READER_BUF_SIZE);
    let mut split_line = false;
    loop {
        let available = match reader.fill_buf() {
            Ok([]) => break,
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(%error, "job output read failed");
                break;
            }
        };
        let newline = available.iter().position(|byte| *byte == b'\n');
        let available_end = newline.map_or(available.len(), |position| position + 1);
        let remaining = MAX_JOB_LINE_BYTES.saturating_sub(line.len());
        let consumed = available_end.min(remaining);
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);

        let complete = newline.is_some_and(|position| consumed > position);
        if line.len() == MAX_JOB_LINE_BYTES || complete {
            if complete {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            let suppress_split_terminator = split_line && line.is_empty() && complete;
            if !suppress_split_terminator {
                let text = String::from_utf8_lossy(&line).into_owned();
                if event_tx.send(event(text)).is_err() {
                    break;
                }
            }
            split_line = !complete;
            line.clear();
        }
    }
    if !line.is_empty() {
        let text = String::from_utf8_lossy(&line).into_owned();
        let _ = event_tx.send(event(text));
    }
}

fn job_command(spec: JobSpec) -> Result<Command, String> {
    match spec {
        JobSpec::Shell(script) => n00n_config::bash_command(&script),
        JobSpec::Program { program, args } => {
            let mut command = Command::new(program);
            command.args(args);
            Ok(command)
        }
    }
}

#[cfg(unix)]
fn process_pid(pid: u32) -> Result<rustix::process::Pid, String> {
    let raw = i32::try_from(pid).map_err(|error| error.to_string())?;
    rustix::process::Pid::from_raw(raw).ok_or_else(|| "child process id cannot be zero".into())
}

#[cfg(unix)]
fn lower_job_priority(pid: u32) -> Result<(), String> {
    let process = process_pid(pid)?;
    rustix::process::setpriority_process(Some(process), JOB_NICE_ADJUSTMENT)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    use rustix::process::{Signal, kill_process_group};

    let Ok(process_group) = process_pid(pid) else {
        tracing::warn!(pid, "invalid job process group id");
        return;
    };
    let Some(signal) = Signal::from_named_raw(libc::SIGKILL) else {
        tracing::error!("SIGKILL is unavailable on this platform");
        return;
    };
    if let Err(error) = kill_process_group(process_group, signal) {
        tracing::debug!(pid, %error, "job process group kill failed");
    }
}

#[cfg(target_os = "linux")]
fn system_memory_bytes() -> Result<u64, String> {
    let contents = std::fs::read_to_string("/proc/meminfo").map_err(|error| error.to_string())?;
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "MemTotal is missing from /proc/meminfo".to_string())?;
    let kilobytes = value.parse::<u64>().map_err(|error| error.to_string())?;
    kilobytes
        .checked_mul(1024)
        .ok_or_else(|| "system memory size overflowed u64".to_string())
}

#[cfg(target_os = "linux")]
fn default_job_rss_limit() -> u64 {
    let total = match system_memory_bytes() {
        Ok(bytes) => bytes / 4,
        Err(error) => {
            tracing::warn!(%error, "failed to read system memory; using maximum default job RSS limit");
            DEFAULT_JOB_RSS_LIMIT_MAX
        }
    };
    total.clamp(DEFAULT_JOB_RSS_LIMIT_MIN, DEFAULT_JOB_RSS_LIMIT_MAX)
}

#[cfg(target_os = "linux")]
fn job_rss_limit() -> u64 {
    let Ok(value) = env::var(JOB_RSS_LIMIT_ENV) else {
        return default_job_rss_limit();
    };
    match value.parse::<u64>() {
        Ok(megabytes) if megabytes > 0 => {
            let Some(bytes) = megabytes.checked_mul(1024 * 1024) else {
                tracing::warn!("tool RSS limit overflowed; using default");
                return default_job_rss_limit();
            };
            bytes
        }
        Ok(_) | Err(_) => {
            tracing::warn!("invalid tool RSS limit; using default");
            default_job_rss_limit()
        }
    }
}

#[cfg(target_os = "linux")]
fn status_rss_bytes(status: &str) -> Result<u64, String> {
    let Some(kilobytes) = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
    else {
        return Ok(0);
    };
    kilobytes
        .parse::<u64>()
        .map_err(|error| error.to_string())?
        .checked_mul(1024)
        .ok_or_else(|| "process RSS overflowed u64".to_string())
}

#[cfg(target_os = "linux")]
fn process_rss(pid: u32) -> Result<Option<u64>, String> {
    let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    status_rss_bytes(&status).map(Some)
}

#[cfg(target_os = "linux")]
fn process_children(pid: u32) -> Result<Vec<u32>, String> {
    let task_dir = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut children = Vec::new();
    for entry in task_dir {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().join("children");
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        for child in contents.split_whitespace() {
            children.push(child.parse::<u32>().map_err(|error| error.to_string())?);
        }
    }
    Ok(children)
}

#[cfg(target_os = "linux")]
fn process_tree_rss(pid: u32) -> Result<u64, String> {
    let mut pending = VecDeque::from([pid]);
    let mut visited = HashSet::new();
    let mut total = 0_u64;
    while let Some(process_id) = pending.pop_front() {
        if !visited.insert(process_id) {
            continue;
        }
        if let Some(bytes) = process_rss(process_id)? {
            total = total
                .checked_add(bytes)
                .ok_or_else(|| "process tree RSS overflowed u64".to_string())?;
        }
        pending.extend(process_children(process_id)?);
    }
    Ok(total)
}

#[cfg(target_os = "linux")]
fn spawn_resource_monitor(
    pid: u32,
    done: Arc<AtomicBool>,
    exceeded: Arc<AtomicBool>,
    limit: u64,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("job-resource".into())
        .spawn(move || {
            while !done.load(Ordering::Acquire) {
                match process_tree_rss(pid) {
                    Ok(rss) if rss > limit => {
                        tracing::warn!(
                            pid,
                            rss,
                            limit,
                            "job exceeded RSS limit; killing process group"
                        );
                        exceeded.store(true, Ordering::Release);
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(pid, %error, "job RSS monitor stopped");
                        return;
                    }
                }
                thread::park_timeout(JOB_RSS_POLL_INTERVAL);
            }
        })
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn wait_for_monitored_child(child: &mut Child, pid: u32, exceeded: &AtomicBool) -> i32 {
    loop {
        if exceeded.swap(false, Ordering::AcqRel) {
            kill_process_group(pid);
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or_else(|| -1),
            Ok(None) => thread::park_timeout(JOB_PROCESS_POLL_INTERVAL),
            Err(error) => {
                tracing::error!(%error, "job wait failed");
                return -1;
            }
        }
    }
}

fn terminate_failed_job_start(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    kill_process_group(pid);
    #[cfg(not(unix))]
    if let Err(error) = child.kill() {
        tracing::debug!(pid, %error, "failed to kill job after startup failure");
    }
    if let Err(error) = child.wait() {
        tracing::debug!(pid, %error, "failed to reap job after startup failure");
    }
}

struct JobWaitState {
    child: Child,
    pid: u32,
    stdout_handle: Option<thread::JoinHandle<()>>,
    stderr_handle: Option<thread::JoinHandle<()>>,
    event_tx: flume::Sender<JobEvent>,
    #[cfg(target_os = "linux")]
    resource_done: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    resource_exceeded: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    resource_monitor: Option<thread::JoinHandle<()>>,
}

impl JobWaitState {
    fn run(mut self) {
        #[cfg(target_os = "linux")]
        let code =
            wait_for_monitored_child(&mut self.child, self.pid, self.resource_exceeded.as_ref());
        #[cfg(not(target_os = "linux"))]
        let code = self.child.wait().map_or_else(
            |error| {
                tracing::error!(%error, "job wait failed");
                -1
            },
            |status| status.code().unwrap_or_else(|| -1),
        );
        self.stop_monitor();
        self.join_readers();
        let _ = self.event_tx.send(JobEvent::Exit(code));
    }

    fn cleanup_start_failure(mut self) {
        #[cfg(unix)]
        kill_process_group(self.pid);
        #[cfg(not(unix))]
        if let Err(error) = self.child.kill() {
            tracing::debug!(pid = self.pid, %error, "failed to kill job after startup failure");
        }
        if let Err(error) = self.child.wait() {
            tracing::debug!(pid = self.pid, %error, "failed to reap job after startup failure");
        }
        self.stop_monitor();
        self.join_readers();
    }

    fn stop_monitor(&mut self) {
        #[cfg(not(target_os = "linux"))]
        let _ = self;
        #[cfg(target_os = "linux")]
        {
            self.resource_done.store(true, Ordering::Release);
            if let Some(handle) = self.resource_monitor.take() {
                handle.thread().unpark();
                if let Err(error) = handle.join() {
                    tracing::debug!(?error, "job resource monitor panicked");
                }
            }
        }
    }

    fn join_readers(&mut self) {
        if let Some(handle) = self.stdout_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum DrainCursor {
    Events(JobOwner),
    Timers(JobOwner),
    PluginEvents,
}

impl DrainCursor {
    fn belongs_to(&self, owner: &JobOwner) -> bool {
        matches!(self, Self::Events(candidate) | Self::Timers(candidate) if candidate == owner)
    }
}

pub(crate) struct JobStore {
    jobs: HashMap<u32, JobMeta>,
    next_id: u32,
    drain_cursors: HashMap<DrainCursor, u32>,
}

struct RetainedJobOutput {
    output: String,
    retained_lines: usize,
    max_lines: usize,
    max_bytes: usize,
    truncated: bool,
}

impl RetainedJobOutput {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            output: String::new(),
            retained_lines: 0,
            max_lines,
            max_bytes,
            truncated: false,
        }
    }

    fn push(&mut self, line: String) {
        if self.truncated {
            return;
        }
        let separator_bytes = usize::from(!self.output.is_empty());
        let next_bytes = self
            .output
            .len()
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(line.len()));
        if self.retained_lines >= self.max_lines
            || next_bytes.is_none_or(|bytes| bytes > self.max_bytes)
        {
            self.truncated = true;
            return;
        }
        if separator_bytes != 0 {
            self.output.push('\n');
        }
        self.output.push_str(&line);
        self.retained_lines += 1;
    }

    fn finish(mut self) -> String {
        if self.truncated {
            if !self.output.is_empty() {
                self.output.push('\n');
            }
            self.output.push_str(JOBWAIT_TRUNCATION_MARKER);
        }
        self.output
    }
}

enum JobWaitWake {
    Event(Option<JobEvent>),
    Poll,
}

/// Holds a receiver borrowed out of the store for the duration of a
/// `jobwait`, and hands it back on drop so a timed-out or cancelled wait
/// does not strand the job's remaining events.
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
            drain_cursors: HashMap::new(),
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
        let mut command = job_command(spec)?;
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
        #[cfg(unix)]
        if let Err(error) = lower_job_priority(pid) {
            tracing::warn!(pid, %error, "failed to lower job process priority");
        }
        let id = self.next_id;
        self.next_id += 1;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (event_tx, event_rx) = flume::bounded(JOB_EVENT_CAPACITY);

        macro_rules! spawn_reader {
            ($stream:expr, $name:expr, $variant:ident) => {
                if let Some(stream) = $stream {
                    let tx = event_tx.clone();
                    thread::Builder::new()
                        .name($name.into())
                        .spawn(move || {
                            let reader = BufReader::with_capacity(READER_BUF_SIZE, stream);
                            pump_job_output(reader, &tx, JobEvent::$variant);
                        })
                        .map(Some)
                        .map_err(|error| error.to_string())
                } else {
                    Ok(None)
                }
            };
        }
        let stdout_handle = match spawn_reader!(stdout, "job-stdout", Stdout) {
            Ok(handle) => handle,
            Err(error) => {
                terminate_failed_job_start(&mut child, pid);
                return Err(error);
            }
        };
        let stderr_handle = match spawn_reader!(stderr, "job-stderr", Stderr) {
            Ok(handle) => handle,
            Err(error) => {
                terminate_failed_job_start(&mut child, pid);
                if let Some(handle) = stdout_handle {
                    let _ = handle.join();
                }
                return Err(error);
            }
        };

        #[cfg(target_os = "linux")]
        let resource_done = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "linux")]
        let resource_exceeded = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "linux")]
        let resource_monitor = match spawn_resource_monitor(
            pid,
            Arc::clone(&resource_done),
            Arc::clone(&resource_exceeded),
            job_rss_limit(),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                terminate_failed_job_start(&mut child, pid);
                if let Some(handle) = stdout_handle {
                    let _ = handle.join();
                }
                if let Some(handle) = stderr_handle {
                    let _ = handle.join();
                }
                return Err(error);
            }
        };

        let wait_state = Arc::new(Mutex::new(Some(JobWaitState {
            child,
            pid,
            stdout_handle,
            stderr_handle,
            event_tx,
            #[cfg(target_os = "linux")]
            resource_done,
            #[cfg(target_os = "linux")]
            resource_exceeded,
            #[cfg(target_os = "linux")]
            resource_monitor: Some(resource_monitor),
        })));
        let wait_state_for_thread = Arc::clone(&wait_state);
        if let Err(error) = thread::Builder::new()
            .name("job-wait".into())
            .spawn(move || {
                let state = wait_state_for_thread
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(state) = state {
                    state.run();
                }
            })
        {
            if let Some(state) = wait_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                state.cleanup_start_failure();
            }
            return Err(error.to_string());
        }

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
        self.drain_matching(DrainCursor::Events(owner.clone()), buf, |job| {
            job.owner == *owner
        });
    }

    pub fn drain_timers(&mut self, owner: &JobOwner, buf: &mut Vec<(u32, JobEvent)>) {
        self.drain_matching(DrainCursor::Timers(owner.clone()), buf, |job| {
            job.owner == *owner && job.pid.is_none()
        });
    }

    pub fn drain_plugin_events(&mut self, buf: &mut Vec<(u32, JobEvent)>) {
        self.drain_matching(DrainCursor::PluginEvents, buf, |job| {
            matches!(job.owner, JobOwner::Plugin(_))
        });
    }

    fn drain_matching(
        &mut self,
        cursor_key: DrainCursor,
        buf: &mut Vec<(u32, JobEvent)>,
        keep: impl Fn(&JobMeta) -> bool,
    ) {
        buf.clear();
        let mut job_ids = self
            .jobs
            .iter()
            .filter_map(|(&job_id, job)| keep(job).then_some(job_id))
            .collect::<Vec<_>>();
        job_ids.sort_unstable();
        if job_ids.is_empty() {
            return;
        }

        let start = self
            .drain_cursors
            .get(&cursor_key)
            .copied()
            .map_or(0, |cursor| {
                let after_cursor = job_ids.partition_point(|job_id| *job_id <= cursor);
                if after_cursor == job_ids.len() {
                    0
                } else {
                    after_cursor
                }
            });
        let now = Instant::now();
        let mut last_drained = None;
        while buf.len() < MAX_JOB_EVENTS_PER_TURN {
            let mut made_progress = false;
            for offset in 0..job_ids.len() {
                let job_id = job_ids[(start + offset) % job_ids.len()];
                let Some(job) = self.jobs.get_mut(&job_id) else {
                    continue;
                };
                let event = if job.deadline.is_some_and(|deadline| now >= deadline) {
                    job.deadline = None;
                    Some(JobEvent::Exit(0))
                } else {
                    job.event_rx.as_ref().and_then(|receiver| {
                        receiver.try_recv().map_or_else(
                            |error| match error {
                                flume::TryRecvError::Empty | flume::TryRecvError::Disconnected => {
                                    None
                                }
                            },
                            Some,
                        )
                    })
                };
                if let Some(event) = event {
                    buf.push((job_id, event));
                    last_drained = Some(job_id);
                    made_progress = true;
                    if buf.len() == MAX_JOB_EVENTS_PER_TURN {
                        break;
                    }
                }
            }
            if !made_progress {
                break;
            }
        }
        if let Some(job_id) = last_drained {
            self.drain_cursors.insert(cursor_key, job_id);
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
            let owner = job.owner.clone();
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
            if !self.jobs.values().any(|job| job.owner == owner) {
                self.drain_cursors
                    .retain(|cursor, _| !cursor.belongs_to(&owner));
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
    kill_process_group(pid);
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
/// Unix jobs run in a separate process group at nice level 10. On Linux, the
/// process tree's summed per-process RSS is limited to one quarter of system
/// memory, clamped between 512 MiB and 8 GiB. Shared pages may be counted more
/// than once. Set `N00N_TOOL_MAX_RSS_MB` to a positive whole number of MiB to
/// override the memory limit.
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

fn defer_timer(lua: &Lua, callback: Function, delay_ms: u64) -> LuaResult<u32> {
    let owner = active_task_id(lua)
        .map(JobOwner::Task)
        .ok_or_else(|| mlua::Error::runtime("defer_fn: no active task"))?;
    let callback = lua.create_registry_value(callback)?;
    with_jobs(lua, |store| {
        store.defer(owner, Duration::from_millis(delay_ms), callback)
    })
    .map_err(mlua::Error::runtime)
}

fn defer_delay_ms(value: Value) -> LuaResult<u64> {
    match value {
        Value::Integer(delay_ms) => u64::try_from(delay_ms)
            .map_err(|_| mlua::Error::runtime("defer delay must be non-negative")),
        Value::Number(delay_ms) if delay_ms >= 0.0 && delay_ms.is_finite() =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(delay_ms as u64)
        }
        _ => Err(mlua::Error::runtime(
            "defer delay must be a non-negative number",
        )),
    }
}

fn defer_args(first: Value, second: Value) -> LuaResult<(Function, u64)> {
    match (first, second) {
        (Value::Function(callback), delay) | (delay, Value::Function(callback)) => {
            Ok((callback, defer_delay_ms(delay)?))
        }
        _ => Err(mlua::Error::runtime(
            "defer expects (callback, delay_ms) or (delay_ms, callback)",
        )),
    }
}

/// Run {callback} after {delay_ms} without spawning a process.
/// Mirrors Neovim's `vim.defer_fn(fn, timeout)`.
///
/// The timer belongs to the current tool call and is cancelled when that call
/// ends. Use this from tool handlers; a timer scheduled by a plugin-owned
/// callback does not outlive that callback's task scope.
///
/// @param callback function Called with the timer id and exit code `0` after the delay, or `-1` when cancelled by `jobstop`.
/// @param delay_ms integer Delay in milliseconds.
/// @return (integer) Timer job id accepted by `n00n.fn.jobstop`.
/// @example
/// n00n.defer_fn(function(timer_id, code) refresh() end, 1000)
#[lua_fn(guard = Run)]
fn defer_fn(lua: &Lua, callback: Function, delay_ms: u64) -> LuaResult<u32> {
    defer_timer(lua, callback, delay_ms)
}

/// Run {callback} after {delay_ms} without spawning a process.
/// Prefer `n00n.defer_fn(callback, delay_ms)`, which mirrors Neovim. This
/// compatibility helper also accepts the previous `(delay_ms, callback)` order.
///
/// @param callback function Called with the timer id and exit code `0` after the delay, or `-1` when cancelled by `jobstop`.
/// @param delay_ms integer Delay in milliseconds.
/// @return (integer) Timer job id accepted by `jobstop`.
/// @example
/// n00n.fn.defer(function(timer_id, code) refresh() end, 1000)
#[lua_fn(guard = Run)]
fn defer(lua: &Lua, callback: Value, delay_ms: Value) -> LuaResult<u32> {
    let (callback, delay_ms) = defer_args(callback, delay_ms)?;
    defer_timer(lua, callback, delay_ms)
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
) {
    if let Some(owner) = owner {
        with_jobs(lua, |store| store.drain_timers(owner, task_events));
        if let Err(error) = deliver_task_job_events(lua, task_events) {
            tracing::warn!(%error, "jobwait sibling timer callback failed");
        }
    }
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
    let mut stdout = RetainedJobOutput::new(MAX_JOBWAIT_RETAINED_LINES, MAX_JOBWAIT_RETAINED_BYTES);
    let mut stderr = RetainedJobOutput::new(MAX_JOBWAIT_RETAINED_LINES, MAX_JOBWAIT_RETAINED_BYTES);
    let mut poll_at = next_jobwait_poll(timeout_at)?;

    let exit_code = loop {
        let now = Instant::now();
        if now >= timeout_at {
            return Ok(mlua::Value::Nil);
        }
        if now >= poll_at {
            drain_task_job_events(&lua, owner.as_ref(), &mut task_events);
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
                drain_task_job_events(&lua, owner.as_ref(), &mut task_events);
                deliver_job_event(&lua, job_id, &event)?;
                match event {
                    JobEvent::Stdout(line) => stdout.push(line),
                    JobEvent::Stderr(line) => stderr.push(line),
                    JobEvent::Exit(code) => break code,
                }
            }
            JobWaitWake::Event(None) => return Ok(mlua::Value::Nil),
            JobWaitWake::Poll => {}
        }
    };

    let result = lua.create_table()?;
    result.set("stdout", stdout.finish())?;
    result.set("stderr", stderr.finish())?;
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
        run_non_yieldable(lua, || callback.call::<()>((job_id, arg)))?;
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

    fn enqueue_job_events(
        store: &mut JobStore,
        owner: JobOwner,
        events: impl IntoIterator<Item = JobEvent>,
    ) -> u32 {
        let id = store.next_id;
        store.next_id += 1;
        let (tx, rx) = flume::unbounded();
        for event in events {
            tx.send(event).unwrap();
        }
        drop(tx);
        store.jobs.insert(
            id,
            JobMeta {
                owner,
                pid: None,
                deadline: None,
                on_stdout: None,
                on_stderr: None,
                on_exit: None,
                event_rx: Some(rx),
            },
        );
        id
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

    #[test]
    fn output_pump_splits_oversized_lines_without_unbounded_allocation() {
        let mut input = vec![b'x'; MAX_JOB_LINE_BYTES + 7];
        input.push(b'\n');
        let (tx, rx) = flume::bounded(4);
        pump_job_output(std::io::Cursor::new(input), &tx, JobEvent::Stdout);
        drop(tx);
        let chunks = rx
            .into_iter()
            .map(|event| match event {
                JobEvent::Stdout(line) => line,
                _ => panic!("unexpected event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_JOB_LINE_BYTES);
        assert_eq!(chunks[1].len(), 7);
    }

    #[test]
    fn output_pump_preserves_carriage_return_at_split_boundary() {
        let mut input = vec![b'x'; MAX_JOB_LINE_BYTES - 1];
        input.extend_from_slice(b"\ry\n");
        let (tx, rx) = flume::bounded(2);
        pump_job_output(std::io::Cursor::new(input), &tx, JobEvent::Stdout);
        drop(tx);
        let chunks = rx
            .into_iter()
            .map(|event| match event {
                JobEvent::Stdout(line) => line,
                _ => panic!("unexpected event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_JOB_LINE_BYTES);
        assert!(chunks[0].ends_with('\r'));
        assert_eq!(chunks[1], "y");
    }

    #[test]
    fn output_pump_preserves_repeated_carriage_returns_before_newline() {
        let (tx, rx) = flume::bounded(1);
        pump_job_output(
            std::io::Cursor::new(b"progress\r\r\n"),
            &tx,
            JobEvent::Stdout,
        );
        drop(tx);
        let event = rx.recv().expect("output event");
        assert!(matches!(event, JobEvent::Stdout(line) if line == "progress\r"));
    }

    #[test]
    fn output_pump_does_not_emit_empty_event_after_exact_size_line() {
        let mut input = vec![b'x'; MAX_JOB_LINE_BYTES];
        input.push(b'\n');
        let (tx, rx) = flume::bounded(2);
        pump_job_output(std::io::Cursor::new(input), &tx, JobEvent::Stdout);
        drop(tx);
        let chunks = rx
            .into_iter()
            .map(|event| match event {
                JobEvent::Stdout(line) => line,
                _ => panic!("unexpected event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), MAX_JOB_LINE_BYTES);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn zombie_status_without_rss_counts_as_zero() {
        assert_eq!(
            status_rss_bytes("Name:\tzombie\nState:\tZ (zombie)\n").unwrap(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn jobs_run_at_reduced_priority() {
        use std::os::unix::process::CommandExt;

        let mut child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("priority probe process");
        let pid = child.id();
        lower_job_priority(pid).expect("lower job priority");
        let process = process_pid(pid).expect("priority probe process");
        let priority =
            rustix::process::getpriority_process(Some(process)).expect("read job process priority");
        kill_process_group(pid);
        child.wait().expect("priority probe status");
        assert_eq!(priority, JOB_NICE_ADJUSTMENT);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resource_monitor_kills_oversized_process_group() {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                "v=$(head -c 33554432 /dev/zero | tr '\\0' x); sleep 2; printf %s \"$v\" >/dev/null",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().expect("memory probe process");
        let pid = child.id();
        let done = Arc::new(AtomicBool::new(false));
        let exceeded = Arc::new(AtomicBool::new(false));
        let monitor = spawn_resource_monitor(
            pid,
            Arc::clone(&done),
            Arc::clone(&exceeded),
            8 * 1024 * 1024,
        )
        .expect("resource monitor");
        let code = wait_for_monitored_child(&mut child, pid, &exceeded);
        done.store(true, Ordering::Release);
        monitor.thread().unpark();
        monitor.join().expect("resource monitor join");
        assert_ne!(code, 0, "oversized process group was not killed");
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

    #[test_case::test_case(())]
    fn deferred_callback_becomes_due_without_a_process(_: ()) {
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

    #[test_case::test_case(())]
    fn deferred_callback_rejects_an_unrepresentable_deadline(_: ()) {
        let lua = Lua::new();
        let callback = lua
            .create_registry_value(lua.create_function(|_, ()| Ok(())).unwrap())
            .unwrap();
        let mut store = make_store();
        let result = store.defer(task_owner(OWNER_TASK_ID), Duration::MAX, callback);
        assert_eq!(result.unwrap_err(), DEFER_DELAY_RANGE_ERR);
    }

    #[test_case::test_case(())]
    fn cancelling_a_timer_still_delivers_its_exit_event(_: ()) {
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
                        "i=0; while [ $i -lt 100 ]; do echo x; sleep 0.005; i=$((i+1)); done"
                            .into(),
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

    #[cfg(unix)]
    #[test]
    fn jobwait_truncates_retained_output_without_dropping_stream_callbacks() {
        let lua = Lua::new();
        let scope = TaskScope::new(&lua, TaskCell::new(CancelToken::none(), None, None, None));
        let owner = task_owner(lock_cell(scope.handle()).id);
        lua.globals().set("streamed_chunks", 0_u32).unwrap();
        let stdout_callback = lua
            .create_registry_value(
                lua.create_function(|lua, (_job_id, _line): (u32, String)| {
                    let streamed = lua.globals().get::<u32>("streamed_chunks")?;
                    lua.globals().set("streamed_chunks", streamed + 1)
                })
                .unwrap(),
            )
            .unwrap();
        let exit_callback = lua
            .create_registry_value(
                lua.create_function(|lua, (_job_id, _code): (u32, i32)| {
                    let streamed = lua.globals().get::<u32>("streamed_chunks")?;
                    lua.globals().set("streamed_chunks_before_exit", streamed)
                })
                .unwrap(),
            )
            .unwrap();
        let output_bytes = MAX_JOBWAIT_RETAINED_BYTES + MAX_JOB_LINE_BYTES;
        let process_id = with_jobs(&lua, |store| {
            store
                .start(
                    owner,
                    JobSpec::Shell(format!("head -c {output_bytes} /dev/zero | tr '\\0' x")),
                    None,
                    None,
                    Some(stdout_callback),
                    None,
                    Some(exit_callback),
                )
                .unwrap()
        });

        let result = smol::block_on(scope.scope_future(jobwait(
            lua.clone(),
            Arc::from(TEST_PLUGIN),
            process_id,
            Some(5_000),
        )))
        .unwrap();
        let result = match result {
            Value::Table(table) => table,
            other => panic!("unexpected result: {other:?}"),
        };
        let stdout = result.get::<String>("stdout").unwrap();
        let streamed_chunks = lua.globals().get::<u32>("streamed_chunks").unwrap();

        assert!(stdout.ends_with(JOBWAIT_TRUNCATION_MARKER));
        assert!(streamed_chunks > 1);
        assert_eq!(
            lua.globals()
                .get::<u32>("streamed_chunks_before_exit")
                .unwrap(),
            streamed_chunks
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

    #[cfg(unix)]
    #[test]
    fn jobwait_preserves_unwaited_sibling_output() {
        let lua = Lua::new();
        let scope = TaskScope::new(&lua, TaskCell::new(CancelToken::none(), None, None, None));
        let owner = task_owner(lock_cell(scope.handle()).id);
        let (first_id, second_id) = with_jobs(&lua, |store| {
            let first_id = start_shell(store, owner.clone(), "printf first");
            let second_id = start_shell(store, owner, "printf second");
            (first_id, second_id)
        });

        let first = smol::block_on(scope.scope_future(jobwait(
            lua.clone(),
            Arc::from(TEST_PLUGIN),
            first_id,
            Some(1_000),
        )))
        .unwrap();
        let second = smol::block_on(scope.scope_future(jobwait(
            lua,
            Arc::from(TEST_PLUGIN),
            second_id,
            Some(1_000),
        )))
        .unwrap();

        let first = match first {
            Value::Table(table) => table,
            other => panic!("unexpected first result: {other:?}"),
        };
        let second = match second {
            Value::Table(table) => table,
            other => panic!("unexpected second result: {other:?}"),
        };
        assert_eq!(first.get::<String>("stdout").unwrap(), "first");
        assert_eq!(second.get::<String>("stdout").unwrap(), "second");
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
    #[cfg(unix)]
    #[test_case::test_case(())]
    fn jobwait_pumps_cancelled_sibling_timer(_: ()) {
        let lua = Lua::new();
        let scope = TaskScope::new(&lua, TaskCell::new(CancelToken::none(), None, None, None));
        let owner = task_owner(lock_cell(scope.handle()).id);
        lua.globals().set("timer_cancelled", false).unwrap();
        let callback = lua
            .create_registry_value(
                lua.create_function(|lua, (_job_id, code): (u32, i32)| {
                    lua.globals().set("timer_cancelled", code == -1)
                })
                .unwrap(),
            )
            .unwrap();
        let process_id = with_jobs(&lua, |store| {
            let process_id = start_shell(store, owner.clone(), "sleep 0.15");
            let timer_id = store
                .defer(owner, Duration::from_mins(1), callback)
                .unwrap();
            store.kill(timer_id, Some(lock_cell(scope.handle()).id), TEST_PLUGIN);
            process_id
        });

        smol::block_on(scope.scope_future(jobwait(
            lua.clone(),
            Arc::from(TEST_PLUGIN),
            process_id,
            Some(1_000),
        )))
        .unwrap();

        assert!(lua.globals().get::<bool>("timer_cancelled").unwrap());
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
    fn drain_events_are_bounded_and_fair_per_turn() {
        let mut store = make_store();
        let noisy_events =
            (0..=MAX_JOB_EVENTS_PER_TURN).map(|index| JobEvent::Stdout(index.to_string()));
        let noisy_id = enqueue_job_events(&mut store, task_owner(OWNER_TASK_ID), noisy_events);
        let quiet_id = enqueue_job_events(
            &mut store,
            task_owner(OWNER_TASK_ID),
            [JobEvent::Stdout("quiet".into())],
        );

        let mut events = Vec::new();
        store.drain_events(&task_owner(OWNER_TASK_ID), &mut events);

        assert_eq!(events.len(), MAX_JOB_EVENTS_PER_TURN);
        assert!(events.iter().any(|(job_id, _)| *job_id == noisy_id));
        assert!(events.iter().any(|(job_id, _)| *job_id == quiet_id));
    }

    #[test]
    fn drain_events_rotates_between_turns() {
        let mut store = make_store();
        let job_ids = (0..=MAX_JOB_EVENTS_PER_TURN)
            .map(|index| {
                enqueue_job_events(
                    &mut store,
                    task_owner(OWNER_TASK_ID),
                    [JobEvent::Stdout(index.to_string())],
                )
            })
            .collect::<Vec<_>>();

        let mut first_turn = Vec::new();
        store.drain_events(&task_owner(OWNER_TASK_ID), &mut first_turn);
        let mut second_turn = Vec::new();
        store.drain_events(&task_owner(OWNER_TASK_ID), &mut second_turn);

        assert_eq!(first_turn.len(), MAX_JOB_EVENTS_PER_TURN);
        assert_eq!(second_turn.len(), 1);
        assert!(
            !first_turn
                .iter()
                .any(|(job_id, _)| *job_id == second_turn[0].0)
        );
        assert!(job_ids.iter().all(|job_id| {
            first_turn.iter().any(|(seen_id, _)| seen_id == job_id)
                || second_turn.iter().any(|(seen_id, _)| seen_id == job_id)
        }));
    }

    #[test]
    fn retained_job_output_marks_line_limit_truncation() {
        let mut output = RetainedJobOutput::new(2, usize::MAX);
        output.push("first".into());
        output.push("second".into());
        output.push("third".into());

        assert_eq!(
            output.finish(),
            format!("first\nsecond\n{JOBWAIT_TRUNCATION_MARKER}")
        );
    }

    #[test]
    fn retained_job_output_marks_byte_limit_truncation() {
        let mut output = RetainedJobOutput::new(usize::MAX, 5);
        output.push("abc".into());
        output.push("de".into());

        assert_eq!(output.finish(), format!("abc\n{JOBWAIT_TRUNCATION_MARKER}"));
    }

    #[test]
    fn drain_events_filters_by_owner() {
        let mut store = make_store();
        let task_job = enqueue_job_events(
            &mut store,
            task_owner(OWNER_TASK_ID),
            [JobEvent::Stdout("task".into()), JobEvent::Exit(0)],
        );
        let plugin_job = enqueue_job_events(
            &mut store,
            plugin_owner(),
            [JobEvent::Stdout("plugin".into()), JobEvent::Exit(0)],
        );

        let mut buf = Vec::new();
        store.drain_events(&task_owner(OWNER_TASK_ID), &mut buf);
        assert!(
            buf.iter()
                .any(|(jid, e)| *jid == task_job && matches!(e, JobEvent::Exit(0))),
            "should receive exit event for completed job"
        );
        assert!(buf.iter().all(|(job_id, _)| *job_id != plugin_job));

        store.drain_plugin_events(&mut buf);
        assert!(
            buf.iter()
                .any(|(jid, e)| *jid == plugin_job && matches!(e, JobEvent::Exit(0))),
            "should receive exit event for the plugin-owned job"
        );
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
