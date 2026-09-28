use std::ffi::OsString;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TaskProgress {
    pub(crate) completed_bytes: Option<u64>,
    pub(crate) transferred_bytes: Option<u64>,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) bytes_per_second: Option<f64>,
    pub(crate) eta_seconds: Option<u64>,
    pub(crate) current_item: Option<usize>,
    pub(crate) total_items: Option<usize>,
}

impl TaskProgress {
    pub(crate) fn fraction(&self) -> Option<f32> {
        if let Some(total) = self.total_bytes {
            if total == 0 {
                return Some(1.0);
            }
            let completed = self.completed_bytes.unwrap_or(0).min(total);
            return Some((completed as f64 / total as f64) as f32);
        }
        let total = self.total_items?;
        if total == 0 {
            return Some(1.0);
        }
        let completed = self.current_item.unwrap_or(0).min(total);
        Some(completed as f32 / total as f32)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskInvocation {
    pub(crate) task_name: String,
    pub(crate) args: Vec<OsString>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskInfo {
    pub(crate) name: String,
    pub(crate) command_preview: String,
    pub(crate) status: JobStatus,
    pub(crate) progress: TaskProgress,
    pub(crate) exit_code: Option<i32>,
    pub(crate) invocation: Option<TaskInvocation>,
}

impl TaskInfo {
    pub(crate) fn running(
        name: String,
        command_preview: String,
        invocation: TaskInvocation,
    ) -> Self {
        Self {
            name,
            command_preview,
            status: JobStatus::Running,
            progress: TaskProgress::default(),
            exit_code: None,
            invocation: Some(invocation),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogKind {
    Stdout,
    Stderr,
    System,
}

#[derive(Debug, Clone)]
pub(crate) struct LogLine {
    pub(crate) kind: LogKind,
    pub(crate) text: String,
}
