//! Launch setup in a real terminal. Credentials remain exclusively in rclone.
use anyhow::{anyhow, Result};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;

/// An owned, nonblocking observer. Dropping it stops observation, not rclone.
#[derive(Debug)]
pub(crate) struct ConnectionSetup {
    /// How completion of the setup wizard is observed (platform specific).
    completion: Completion,
    /// Set once completion has been reported, so [`ConnectionSetup::poll`] reports once.
    finished: bool,
}

#[derive(Debug)]
/// Platform-specific completion signal of the launched `rclone config`.
enum Completion {
    #[cfg(target_os = "windows")]
    /// Windows: receives the exit status of the `rclone config` console process.
    Process(std::sync::mpsc::Receiver<std::io::Result<std::process::ExitStatus>>),
    #[cfg(unix)]
    /// Unix: the wrapper script touches `started`/`complete` marker files in a
    /// private temp directory.
    Marker {
        /// Private temp directory holding the script and markers; removed on drop.
        directory: tempfile::TempDir,
        /// Launch time; used to give up if the script never starts (60 s).
        launched_at: std::time::Instant,
    },
}

impl ConnectionSetup {
    /// Reports completion exactly once. Even a cancelled wizard can have saved
    /// configuration, so its exit status does not suppress provider refresh.
    pub(crate) fn poll(&mut self) -> Option<Result<()>> {
        if self.finished {
            return None;
        }
        let result = match &self.completion {
            #[cfg(target_os = "windows")]
            Completion::Process(receiver) => match receiver.try_recv() {
                Ok(Ok(_)) => Some(Ok(())),
                Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(
                    anyhow!("Cannot observe setup completion. Refresh providers manually."),
                )),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            },
            #[cfg(unix)]
            Completion::Marker {
                directory,
                launched_at,
            } => {
                if directory.path().join("complete").exists() {
                    Some(Ok(()))
                } else if !directory.path().join("started").exists()
                    && launched_at.elapsed() >= std::time::Duration::from_secs(60)
                {
                    Some(Err(anyhow!("Setup did not start in the terminal. Retry Connect, or run rclone config and Refresh providers.")))
                } else {
                    None
                }
            }
        };
        if result.is_some() {
            self.finished = true;
        }
        result
    }
}

/// Opens `rclone config` in a new terminal window and returns an observer for
/// its completion. Rejects an empty executable or one with control characters.
/// Called from the GUI providers screen's Connect action.
pub(crate) fn open_setup(executable: &str) -> Result<ConnectionSetup> {
    if executable.trim().is_empty() || executable.contains(['\n', '\r', '\0']) {
        return Err(anyhow!("Set a valid rclone executable in Settings first"));
    }
    launch(executable)
}

#[cfg(target_os = "windows")]
/// Windows: runs `rclone config` in a new console and waits on it in a thread.
fn launch(executable: &str) -> Result<ConnectionSetup> {
    use std::os::windows::process::CommandExt;
    let mut child = Command::new(executable)
        .arg("config")
        .creation_flags(0x00000010) // CREATE_NEW_CONSOLE; no command shell.
        .spawn()
        .map_err(|_| anyhow!("Cannot open rclone setup. Run rclone config in Windows Terminal."))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    Ok(ConnectionSetup {
        completion: Completion::Process(receiver),
        finished: false,
    })
}

#[cfg(unix)]
/// Unix: writes a private `setup.command` shell script that runs
/// `rclone config` and touches marker files on start and exit.
fn prepare_script(executable: &str) -> Result<ConnectionSetup> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // TempDir is private (0700), and owned by the observer for cleanup on every
    // error/drop path. No credentials or command output are written here.
    let directory = tempfile::Builder::new()
        .prefix("rpool-connect-")
        .tempdir()?;
    let path = directory.path().join("setup.command");
    let mut script = std::fs::File::create(&path)?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    writeln!(script, "#!/bin/sh\nstate={}\ntrap ': > \"$state/complete\"' EXIT\ntrap 'exit 130' HUP INT TERM\n: > \"$state/started\"\n{} config",
        quote(&directory.path().to_string_lossy()), quote(executable))?;
    script.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    Ok(ConnectionSetup {
        completion: Completion::Marker {
            directory,
            launched_at: std::time::Instant::now(),
        },
        finished: false,
    })
}

#[cfg(unix)]
impl ConnectionSetup {
    /// Path of the generated setup script inside the private temp directory.
    fn script_path(&self) -> std::path::PathBuf {
        match &self.completion {
            Completion::Marker { directory, .. } => directory.path().join("setup.command"),
        }
    }
}

#[cfg(target_os = "macos")]
/// macOS: opens the setup script in Terminal.app.
fn launch(executable: &str) -> Result<ConnectionSetup> {
    let setup = prepare_script(executable)?;
    let result = Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(setup.script_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !result.is_ok_and(|s| s.success()) {
        return Err(anyhow!(
            "Cannot open Terminal. Run rclone config in your terminal."
        ));
    }
    Ok(setup)
}

#[cfg(all(unix, not(target_os = "macos")))]
/// Other Unix: tries common terminal emulators in order until one launches.
fn launch(executable: &str) -> Result<ConnectionSetup> {
    let setup = prepare_script(executable)?;
    for (terminal, flag) in [
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("xterm", "-e"),
    ] {
        if let Ok(mut child) = Command::new(terminal)
            .arg(flag)
            .arg(setup.script_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            // Some terminals immediately hand off to a server. Their process
            // exit is not wizard completion; only the script marker is.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(setup);
        }
    }
    Err(anyhow!(
        "No supported terminal found. Run rclone config in your terminal, then Refresh providers."
    ))
}

#[cfg(not(any(unix, target_os = "windows")))]
/// Unsupported platforms: always asks the user to run `rclone config` manually.
fn launch(_: &str) -> Result<ConnectionSetup> {
    Err(anyhow!(
        "Run rclone config in your terminal, then Refresh providers."
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn completion_is_nonblocking_once_and_cleans_up() {
        let mut setup = prepare_script("/usr/bin/true").unwrap();
        let path = setup.script_path();
        let directory = path.parent().unwrap().to_owned();
        assert!(setup.poll().is_none());
        let status = Command::new("/bin/sh").arg(&path).status().unwrap();
        assert!(status.success());
        assert!(setup.poll().unwrap().is_ok());
        assert!(setup.poll().is_none());
        drop(setup);
        assert!(!directory.exists());
    }

    #[test]
    fn failed_wizard_still_completes_for_refresh() {
        let mut setup = prepare_script("/nonexistent/rpool-test-rclone").unwrap();
        assert!(!Command::new("/bin/sh")
            .arg(setup.script_path())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
        assert!(setup.poll().unwrap().is_ok());
    }

    #[test]
    fn unstarted_setup_times_out_once() {
        let mut setup = prepare_script("rclone").unwrap();
        let Completion::Marker { launched_at, .. } = &mut setup.completion;
        *launched_at = std::time::Instant::now() - std::time::Duration::from_secs(61);
        assert!(setup.poll().unwrap().is_err());
        assert!(setup.poll().is_none());
    }

    #[test]
    fn invalid_executable_does_not_launch() {
        for executable in ["", "  ", "rclone\n", "rclone\0"] {
            assert!(open_setup(executable).is_err());
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn receiver_poll_is_nonblocking_and_reports_wait_failure_once() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut setup = ConnectionSetup {
            completion: Completion::Process(receiver),
            finished: false,
        };
        assert!(setup.poll().is_none());
        sender
            .send(Err(std::io::Error::other("fixture wait failure")))
            .unwrap();
        assert!(setup.poll().unwrap().is_err());
        assert!(setup.poll().is_none());
    }

    #[test]
    fn disconnected_observer_is_recoverable() {
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        let mut setup = ConnectionSetup {
            completion: Completion::Process(receiver),
            finished: false,
        };
        assert!(setup.poll().unwrap().is_err());
    }
}
