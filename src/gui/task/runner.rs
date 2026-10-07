//! Runs GUI operations as child `rpool` processes (the current executable with
//! `--rclone` and the task's arguments). A worker thread streams stdout/stderr
//! lines and progress events through a channel; the GUI drains it with
//! [`TaskRunner::poll`] each frame. Only one task runs at a time.
//!
//! Sending never blocks the child: while the window is minimized or hidden
//! the GUI may not draw (and drain) for a long time, and a blocked pipe
//! reader would stall the child process itself (a mount, an upload) as soon
//! as it writes a line. Past [`PENDING_MAX`] undrained events, log lines and
//! byte progress are dropped and counted instead; the terminal events always
//! arrive.

use super::progress::ProgressTracker;
use super::{JobStatus, LogKind, LogLine, TaskInfo, TaskInvocation};
use crate::gui::i18n::{tr, trf};
use crate::progress::{parse_line as parse_progress_line, ProgressEvent, PROGRESS_ENV};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Final result of a child process.
#[derive(Debug, Clone)]
pub(crate) struct TaskOutcome {
    /// Exited with status 0 and was not cancelled.
    pub(crate) success: bool,
    /// Process exit code; `None` when killed by a signal or unknown.
    pub(crate) code: Option<i32>,
    /// The user cancelled the task.
    pub(crate) cancelled: bool,
}

/// Undrained events after which droppable ones (logs, byte progress) are
/// skipped rather than queued.
const PENDING_MAX: usize = 10_000;

/// Counters shared by an [`EventSender`] and the runner draining it.
#[derive(Default)]
struct Backlog {
    /// Events sent and not yet drained.
    pending: AtomicUsize,
    /// Droppable events skipped since the last drain.
    dropped: AtomicUsize,
}

/// Non-blocking sender of task events (see the module docs).
#[derive(Clone)]
struct EventSender {
    /// Unbounded channel to the runner.
    sender: Sender<TaskEvent>,
    /// Shared backlog counters.
    backlog: Arc<Backlog>,
}

impl EventSender {
    /// A sender and the receiver/backlog the runner keeps.
    fn channel() -> (Self, Receiver<TaskEvent>, Arc<Backlog>) {
        let (sender, receiver) = mpsc::channel();
        let backlog = Arc::new(Backlog::default());
        (
            Self {
                sender,
                backlog: Arc::clone(&backlog),
            },
            receiver,
            backlog,
        )
    }
    /// Queues `event` without blocking; a droppable event past
    /// [`PENDING_MAX`] is counted as dropped instead. Errors only when the
    /// runner is gone.
    fn send(&self, event: TaskEvent) -> Result<(), ()> {
        let droppable = matches!(
            event,
            TaskEvent::Log(_)
                | TaskEvent::Progress(ProgressEvent::Advance { .. } | ProgressEvent::Items { .. })
        );
        if droppable && self.backlog.pending.load(Ordering::Relaxed) >= PENDING_MAX {
            self.backlog.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.backlog.pending.fetch_add(1, Ordering::Relaxed);
        self.sender.send(event).map_err(|_| ())
    }
}

/// Messages from the worker thread to the GUI.
enum TaskEvent {
    /// One stdout/stderr/system line for the log.
    Log(LogLine),
    /// A parsed progress line from stderr.
    Progress(ProgressEvent),
    /// The child exited; always the last event.
    Finished(TaskOutcome),
    /// The child could not be spawned; carries the error message.
    StartFailed(String),
}

/// The single background-task slot of the GUI, held in the app state.
#[derive(Default)]
pub(crate) struct TaskRunner {
    /// Event channel of the running task; `None` when idle.
    receiver: Option<Receiver<TaskEvent>>,
    /// Backlog counters of `receiver`.
    backlog: Option<Arc<Backlog>>,
    /// Set to request cancellation of the running task.
    cancel_flag: Option<Arc<AtomicBool>>,
    /// A task is in progress (cleared by `poll` on a terminal event).
    running: bool,
    /// The running task.
    current_task: Option<TaskInfo>,
    /// The most recently finished task, kept for display.
    last_task: Option<TaskInfo>,
    /// Log of the running/last task, bounded to about 2000 lines.
    logs: Vec<LogLine>,
    /// Outcome of the last finished task.
    last_outcome: Option<TaskOutcome>,
    /// Rate/ETA tracker for `current_task`.
    progress: ProgressTracker,
}

impl TaskRunner {
    /// Starts `rpool --rclone <rclone> <args>` as a child process in the
    /// background. Fails if a task is already running or the executable path
    /// cannot be determined. Called by the screens for every long operation.
    pub(crate) fn start_rpool<I, S>(
        &mut self,
        task_name: impl Into<String>,
        rclone: &str,
        args: I,
    ) -> Result<(), String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        if self.running {
            return Err(tr("another rpool operation is already running").to_string());
        }

        let executable = std::env::current_exe().map_err(|error| {
            trf(
                "cannot locate the current rpool executable: {error}",
                &[("error", &error)],
            )
        })?;
        let task_name = task_name.into();
        let task_args: Vec<OsString> = args.into_iter().map(Into::into).collect();

        let mut full_args = vec![OsString::from("--rclone"), OsString::from(rclone)];
        full_args.extend(task_args.iter().cloned());

        let command_preview = format_command(&executable, &full_args);
        let invocation = TaskInvocation {
            task_name: task_name.clone(),
            args: task_args,
        };

        self.logs.clear();
        self.last_outcome = None;
        self.current_task = Some(TaskInfo::running(task_name, command_preview, invocation));
        self.progress.reset();

        let (sender, receiver, backlog) = EventSender::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel_flag);

        thread::spawn(move || run_process(executable, full_args, sender, worker_cancel));

        self.receiver = Some(receiver);
        self.backlog = Some(backlog);
        self.cancel_flag = Some(cancel_flag);
        self.running = true;
        Ok(())
    }

    /// Drains pending worker events: appends logs (trimmed to stay bounded),
    /// applies progress and on exit moves the task to `last_task`. Returns the
    /// terminal status once, when the task has just finished. Called every frame.
    pub(crate) fn poll(&mut self) -> Option<JobStatus> {
        let mut terminal_status = None;
        if let Some(dropped) = self
            .backlog
            .as_ref()
            .map(|b| b.dropped.swap(0, Ordering::Relaxed))
            .filter(|n| *n > 0)
        {
            self.logs.push(LogLine {
                kind: LogKind::System,
                text: format!(
                    "[{dropped} output line(s) skipped while the window was not drawing]"
                ),
            });
        }

        while let Some(received) = self.receiver.as_ref().map(Receiver::try_recv) {
            let event = match received {
                Ok(event) => {
                    if let Some(backlog) = &self.backlog {
                        backlog.pending.fetch_sub(1, Ordering::Relaxed);
                    }
                    event
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if terminal_status.is_none() {
                        self.logs.push(LogLine {
                            kind: LogKind::System,
                            text: "task worker stopped unexpectedly".to_string(),
                        });
                        let outcome = TaskOutcome {
                            success: false,
                            code: None,
                            cancelled: false,
                        };
                        self.finish_current(JobStatus::Failed, None);
                        self.last_outcome = Some(outcome);
                        terminal_status = Some(JobStatus::Failed);
                    }
                    break;
                }
            };

            match event {
                TaskEvent::Log(line) => {
                    // Mount services can run for days; retained UI output must be bounded.
                    if self.logs.len() >= 2000 {
                        self.logs.drain(..500);
                    }
                    self.logs.push(line);
                }
                TaskEvent::Progress(event) => {
                    if let Some(task) = self.current_task.as_mut() {
                        self.progress.apply(&mut task.progress, event);
                    }
                }
                TaskEvent::Finished(outcome) => {
                    let status = if outcome.cancelled {
                        JobStatus::Cancelled
                    } else if outcome.success {
                        JobStatus::Completed
                    } else {
                        JobStatus::Failed
                    };
                    self.finish_current(status, outcome.code);
                    self.last_outcome = Some(outcome);
                    terminal_status = Some(status);
                    break;
                }
                TaskEvent::StartFailed(message) => {
                    self.logs.push(LogLine {
                        kind: LogKind::System,
                        text: message,
                    });
                    let outcome = TaskOutcome {
                        success: false,
                        code: None,
                        cancelled: false,
                    };
                    self.finish_current(JobStatus::Failed, None);
                    self.last_outcome = Some(outcome);
                    terminal_status = Some(JobStatus::Failed);
                    break;
                }
            }
        }

        if terminal_status.is_some() {
            self.running = false;
            self.receiver = None;
            self.backlog = None;
            self.cancel_flag = None;
        }

        terminal_status
    }

    /// Moves the current task to `last_task` with its final status and code.
    fn finish_current(&mut self, status: JobStatus, exit_code: Option<i32>) {
        if let Some(mut task) = self.current_task.take() {
            task.status = status;
            task.exit_code = exit_code;
            self.last_task = Some(task);
        }
    }

    /// Requests cancellation; the worker then terminates the process tree.
    pub(crate) fn cancel(&self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Clears the log, last outcome and last task; ignored while running.
    pub(crate) fn clear_log(&mut self) {
        if !self.running {
            self.logs.clear();
            self.last_outcome = None;
            self.last_task = None;
            self.progress.reset();
        }
    }

    /// Whether a task is currently running.
    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    /// Name of the running task, if any.
    pub(crate) fn task_name(&self) -> Option<&str> {
        self.current_task.as_ref().map(|task| task.name.as_str())
    }

    /// Command line of the running task, or of the last one when idle.
    pub(crate) fn command_preview(&self) -> Option<&str> {
        self.current_task
            .as_ref()
            .or(self.last_task.as_ref())
            .map(|task| task.command_preview.as_str())
    }

    /// The running task, if any.
    pub(crate) fn current_task(&self) -> Option<&TaskInfo> {
        self.current_task.as_ref()
    }

    /// The last finished task, if any.
    pub(crate) fn last_task(&self) -> Option<&TaskInfo> {
        self.last_task.as_ref()
    }

    /// Log lines of the running or last task.
    pub(crate) fn logs(&self) -> &[LogLine] {
        &self.logs
    }

    /// Outcome of the last finished task, if any.
    pub(crate) fn last_outcome(&self) -> Option<&TaskOutcome> {
        self.last_outcome.as_ref()
    }
}

/// Worker thread body: spawns the child with the progress protocol enabled
/// (`PROGRESS_ENV`), streams both pipes on their own threads, polls for exit
/// or cancellation every 100 ms and sends [`TaskEvent::Finished`] at the end.
fn run_process(
    executable: std::path::PathBuf,
    args: Vec<OsString>,
    sender: EventSender,
    cancel_flag: Arc<AtomicBool>,
) {
    let mut command = Command::new(&executable);
    command
        .args(&args)
        .env(PROGRESS_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    configure_no_console(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.send(TaskEvent::StartFailed(format!(
                "failed to start rpool child process: {error}"
            )));
            return;
        }
    };

    let stdout_thread = child.stdout.take().map(|stdout| {
        let sender = sender.clone();
        thread::spawn(move || stream_lines(stdout, LogKind::Stdout, false, sender))
    });
    let stderr_thread = child.stderr.take().map(|stderr| {
        let sender = sender.clone();
        thread::spawn(move || stream_lines(stderr, LogKind::Stderr, true, sender))
    });

    let mut cancelled = false;
    let status = loop {
        if cancel_flag.load(Ordering::Relaxed) && !cancelled {
            cancelled = true;
            terminate_process_tree(&mut child);
        }

        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                let _ = sender.send(TaskEvent::Log(LogLine {
                    kind: LogKind::System,
                    text: format!("failed while waiting for child process: {error}"),
                }));
                break None;
            }
        }
    };

    if let Some(handle) = stdout_thread {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }

    let outcome = TaskOutcome {
        success: status.as_ref().is_some_and(|value| value.success()) && !cancelled,
        code: status.as_ref().and_then(|value| value.code()),
        cancelled,
    };
    let _ = sender.send(TaskEvent::Finished(outcome));
}

/// Puts the child in its own process group so cancel can signal the whole tree.
#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// No process groups on this platform.
#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

/// Hides the console window of the child on Windows.
#[cfg(windows)]
fn configure_no_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Nothing to hide outside Windows.
#[cfg(not(windows))]
fn configure_no_console(_command: &mut Command) {}

/// Kills the child and its descendants: `taskkill /T /F` on Windows, SIGTERM to
/// the process group on Unix, falling back to killing only the child.
fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        if Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
    }

    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        if Command::new("kill")
            .args(["-TERM", "--", &process_group])
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
    }

    let _ = child.kill();
}

/// Reads lines from a child pipe and sends them as events. On stderr
/// (`parse_progress`) progress-protocol lines become progress events. Lines
/// over 16 KiB are skipped and replaced by a placeholder so memory stays bounded.
fn stream_lines<R: std::io::Read>(
    reader: R,
    kind: LogKind,
    parse_progress: bool,
    sender: EventSender,
) {
    let mut reader = BufReader::new(reader);
    loop {
        let mut bytes = Vec::new();
        let read = std::io::Read::by_ref(&mut reader)
            .take(16384)
            .read_until(b'\n', &mut bytes);
        match read {
            Ok(0) => break,
            Ok(count) => {
                let oversized = count == 16384 && bytes.last() != Some(&b'\n');
                if oversized {
                    loop {
                        let available = match reader.fill_buf() {
                            Ok(bytes) => bytes,
                            Err(_) => return,
                        };
                        if available.is_empty() {
                            break;
                        }
                        let end = available.iter().position(|b| *b == b'\n');
                        let count = end.map_or(available.len(), |i| i + 1);
                        reader.consume(count);
                        if end.is_some() {
                            break;
                        }
                    }
                }
                let text = if oversized {
                    "[oversized output line omitted]".into()
                } else {
                    String::from_utf8_lossy(&bytes)
                        .trim_end_matches(['\r', '\n'])
                        .to_string()
                };
                if parse_progress {
                    if let Some(event) = parse_progress_line(&text) {
                        if sender.send(TaskEvent::Progress(event)).is_err() {
                            break;
                        }
                        continue;
                    }
                }
                if sender.send(TaskEvent::Log(LogLine { kind, text })).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(TaskEvent::Log(LogLine {
                    kind: LogKind::System,
                    text: format!("failed to read child output: {error}"),
                }));
                break;
            }
        }
    }
}

/// Formats the executable and arguments as a quoted command line for display.
fn format_command(executable: &std::path::Path, args: &[OsString]) -> String {
    let mut parts = vec![quote(&executable.to_string_lossy())];
    parts.extend(args.iter().map(|arg| quote(&arg.to_string_lossy())));
    parts.join(" ")
}

/// Quotes a value for display unless it only contains safe path characters.
fn quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_=./:\\".contains(ch))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

#[cfg(test)]
impl TaskRunner {
    /// Reports running without a child process (session tests and layout fixtures).
    pub(crate) fn fake_running(&mut self, task_name: &str) {
        self.current_task = Some(TaskInfo::running(
            task_name.into(),
            String::new(),
            TaskInvocation {
                task_name: task_name.into(),
                args: Vec::new(),
            },
        ));
        self.running = true;
    }

    /// Delivers a finished event to a [`Self::fake_running`] runner; the next
    /// `poll` reports it.
    pub(crate) fn fake_finish(&mut self, success: bool) {
        let (sender, receiver, backlog) = EventSender::channel();
        sender
            .send(TaskEvent::Finished(TaskOutcome {
                success,
                code: Some(if success { 0 } else { 1 }),
                cancelled: false,
            }))
            .unwrap();
        self.receiver = Some(receiver);
        self.backlog = Some(backlog);
    }
}

#[cfg(test)]
mod bounded_output_tests {
    use super::*;

    #[test]
    fn oversized_line_does_not_swallow_following_status() {
        let mut input = vec![b'x'; 40_000];
        input.extend_from_slice(b"\nwriteback completed\r\n");
        let (sender, receiver, _) = EventSender::channel();
        stream_lines(input.as_slice(), LogKind::System, false, sender);
        let lines: Vec<_> = receiver
            .into_iter()
            .filter_map(|event| match event {
                TaskEvent::Log(line) => Some(line.text),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["[oversized output line omitted]", "writeback completed"]
        );
    }

    /// Nobody drains (the window does not draw): the reader keeps consuming
    /// the child's output instead of blocking it, drops the excess and still
    /// delivers the final event.
    #[test]
    fn an_undrained_runner_never_blocks_the_child_output() {
        let mut input = Vec::new();
        for i in 0..(PENDING_MAX + 5_000) {
            input.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let (sender, receiver, backlog) = EventSender::channel();
        let finisher = sender.clone();
        let reader =
            thread::spawn(move || stream_lines(input.as_slice(), LogKind::Stdout, false, sender));
        reader.join().unwrap();
        finisher
            .send(TaskEvent::Finished(TaskOutcome {
                success: true,
                code: Some(0),
                cancelled: false,
            }))
            .unwrap();
        assert_eq!(backlog.dropped.load(Ordering::Relaxed), 5_000);
        let events: Vec<_> = receiver.try_iter().collect();
        assert_eq!(events.len(), PENDING_MAX + 1);
        assert!(matches!(events.last(), Some(TaskEvent::Finished(_))));
    }
}
