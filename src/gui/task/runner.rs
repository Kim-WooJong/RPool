use super::progress::ProgressTracker;
use super::{JobStatus, LogKind, LogLine, TaskInfo, TaskInvocation};
use crate::progress::{parse_line as parse_progress_line, ProgressEvent, PROGRESS_ENV};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct TaskOutcome {
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) cancelled: bool,
}

enum TaskEvent {
    Log(LogLine),
    Progress(ProgressEvent),
    Finished(TaskOutcome),
    StartFailed(String),
}

pub(crate) struct TaskRunner {
    receiver: Option<Receiver<TaskEvent>>,
    cancel_flag: Option<Arc<AtomicBool>>,
    running: bool,
    current_task: Option<TaskInfo>,
    last_task: Option<TaskInfo>,
    logs: Vec<LogLine>,
    last_outcome: Option<TaskOutcome>,
    progress: ProgressTracker,
}

impl Default for TaskRunner {
    fn default() -> Self {
        Self {
            receiver: None,
            cancel_flag: None,
            running: false,
            current_task: None,
            last_task: None,
            logs: Vec::new(),
            last_outcome: None,
            progress: ProgressTracker::default(),
        }
    }
}

impl TaskRunner {
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
            return Err("another rpool operation is already running".to_string());
        }

        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot locate the current rpool executable: {error}"))?;
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

        let (sender, receiver) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel_flag);

        thread::spawn(move || run_process(executable, full_args, sender, worker_cancel));

        self.receiver = Some(receiver);
        self.cancel_flag = Some(cancel_flag);
        self.running = true;
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> Option<JobStatus> {
        let mut terminal_status = None;

        loop {
            let received = match self.receiver.as_ref() {
                Some(receiver) => receiver.try_recv(),
                None => break,
            };
            let event = match received {
                Ok(event) => event,
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
                TaskEvent::Log(line) => self.logs.push(line),
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
            self.cancel_flag = None;
        }

        terminal_status
    }

    fn finish_current(&mut self, status: JobStatus, exit_code: Option<i32>) {
        if let Some(mut task) = self.current_task.take() {
            task.status = status;
            task.exit_code = exit_code;
            self.last_task = Some(task);
        }
    }

    pub(crate) fn cancel(&self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn clear_log(&mut self) {
        if !self.running {
            self.logs.clear();
            self.last_outcome = None;
            self.last_task = None;
            self.progress.reset();
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    pub(crate) fn task_name(&self) -> Option<&str> {
        self.current_task.as_ref().map(|task| task.name.as_str())
    }

    pub(crate) fn command_preview(&self) -> Option<&str> {
        self.current_task
            .as_ref()
            .or(self.last_task.as_ref())
            .map(|task| task.command_preview.as_str())
    }

    pub(crate) fn current_task(&self) -> Option<&TaskInfo> {
        self.current_task.as_ref()
    }

    pub(crate) fn last_task(&self) -> Option<&TaskInfo> {
        self.last_task.as_ref()
    }

    pub(crate) fn logs(&self) -> &[LogLine] {
        &self.logs
    }

    pub(crate) fn last_outcome(&self) -> Option<&TaskOutcome> {
        self.last_outcome.as_ref()
    }
}

fn run_process(
    executable: std::path::PathBuf,
    args: Vec<OsString>,
    sender: Sender<TaskEvent>,
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

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(windows)]
fn configure_no_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_no_console(_command: &mut Command) {}

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

fn stream_lines<R: std::io::Read>(
    reader: R,
    kind: LogKind,
    parse_progress: bool,
    sender: Sender<TaskEvent>,
) {
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(text) => {
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

fn format_command(executable: &std::path::Path, args: &[OsString]) -> String {
    let mut parts = vec![quote(&executable.to_string_lossy())];
    parts.extend(args.iter().map(|arg| quote(&arg.to_string_lossy())));
    parts.join(" ")
}

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
