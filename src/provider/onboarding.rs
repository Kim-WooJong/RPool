//! Launch the supported rclone setup flow in a real terminal; credentials never
//! pass through RPool forms, task logs or shell arguments.
use anyhow::{anyhow, Result};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;

pub(crate) fn open_setup(executable: &str) -> Result<()> {
    if executable.trim().is_empty() || executable.contains(['\n', '\r', '\0']) {
        return Err(anyhow!("Set a valid rclone executable in Settings first"));
    }
    launch(executable)
}

#[cfg(target_os = "windows")]
fn launch(executable: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut child = Command::new(executable)
        .arg("config")
        .creation_flags(0x00000010) // CREATE_NEW_CONSOLE; no command shell.
        .spawn()
        .map_err(|_| anyhow!("Cannot open rclone setup. Run rclone config in Windows Terminal."))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch(executable: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // Only the executable name is interpolated, using POSIX single-quote
    // escaping. OAuth tokens/passwords remain exclusively inside rclone.
    let mut script = tempfile::Builder::new()
        .prefix("rpool-connect-")
        .suffix(".command")
        .tempfile()?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    writeln!(
        script,
        "#!/bin/sh\ntrap 'rm -f -- \"$0\"' EXIT\n{} config",
        quote(executable)
    )?;
    script
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))?;
    let (_file, path) = script
        .keep()
        .map_err(|_| anyhow!("cannot prepare setup launcher"))?;
    let result = Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !result.is_ok_and(|s| s.success()) {
        let _ = std::fs::remove_file(path);
        return Err(anyhow!(
            "Cannot open Terminal. Run rclone config in your terminal."
        ));
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch(executable: &str) -> Result<()> {
    for (terminal, flag) in [
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("xterm", "-e"),
    ] {
        if let Ok(mut child) = Command::new(terminal)
            .args([flag, executable, "config"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
    }
    Err(anyhow!(
        "No supported terminal found. Run rclone config in your terminal, then Refresh providers."
    ))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn launch(_: &str) -> Result<()> {
    Err(anyhow!(
        "Run rclone config in your terminal, then Refresh providers."
    ))
}
