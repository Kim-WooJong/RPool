//! Background tasks of the GUI: every long operation runs as a child `rpool`
//! process whose output and progress are streamed back. Entry point is
//! `TaskRunner`; screens start tasks and the app polls it each frame.

/// Task status, progress, invocation and log line types.
mod model;
/// Turns progress protocol events into [`TaskProgress`] with rate and ETA.
mod progress;
/// Spawns the child process and collects its events ([`TaskRunner`]).
mod runner;

pub(crate) use model::{JobStatus, LogKind, LogLine, TaskInfo, TaskInvocation, TaskProgress};
pub(crate) use runner::TaskRunner;
