//! Read-only history queries (`trash list`, `versions list`, `rollback`
//! preview, `retention show`) run as a child `rpool … --json` off the UI
//! thread with the whole stdout captured, so long listings are not cut by
//! the task console's line limits. Changes go through the task runner.

use crate::gui::i18n::tr;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::sync::mpsc::{Receiver, TryRecvError};

/// The last result and a query still running, kept apart so the old result
/// stays on screen while it refreshes.
#[derive(Debug)]
pub(crate) struct Fetch<T> {
    pub(crate) value: Option<Result<T, String>>,
    pending: Option<Receiver<Result<T, String>>>,
    /// Asked to reload on the next frame (after a change).
    pub(crate) stale: bool,
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Self {
            value: None,
            pending: None,
            stale: false,
        }
    }
}

impl<T: DeserializeOwned + Send + 'static> Fetch<T> {
    pub(crate) fn ready(value: T) -> Self {
        Self {
            value: Some(Ok(value)),
            ..Default::default()
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Never loaded, or marked stale, and nothing running.
    pub(crate) fn needs_load(&self) -> bool {
        self.pending.is_none() && (self.value.is_none() || self.stale)
    }

    #[cfg(test)]
    pub(crate) fn ok(&self) -> Option<&T> {
        self.value.as_ref().and_then(|value| value.as_ref().ok())
    }

    pub(crate) fn start(&mut self, rclone: &str, args: Vec<OsString>) {
        self.stale = false;
        self.pending = Some(spawn(rclone, args));
    }

    /// Collects a finished query. Returns true while one is running.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => Err(tr("The query stopped unexpectedly.").into()),
        };
        self.pending = None;
        self.value = Some(result);
        false
    }
}

fn spawn<T: DeserializeOwned + Send + 'static>(
    rclone: &str,
    args: Vec<OsString>,
) -> Receiver<Result<T, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let rclone = rclone.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&rclone, args));
    });
    rx
}

#[cfg(test)]
fn run<T: DeserializeOwned>(_rclone: &str, _args: Vec<OsString>) -> Result<T, String> {
    // The test binary is not `rpool`: never start it as a child.
    Err("queries do not run in tests".into())
}

#[cfg(not(test))]
fn run<T: DeserializeOwned>(rclone: &str, args: Vec<OsString>) -> Result<T, String> {
    use super::parse;
    use crate::gui::i18n::trf;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|error| {
        trf(
            "cannot locate the current rpool executable: {error}",
            &[("error", &error)],
        )
    })?;
    let mut command = Command::new(exe);
    command
        .arg("--rclone")
        .arg(rclone)
        .args(args)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        if let Some(value) = parse::from_stdout(&stdout) {
            return Ok(value);
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(parse::last_error(&stderr)
        .unwrap_or_else(|| tr("The command printed no readable result.").into()))
}
