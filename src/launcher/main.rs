//! `rpool-gui`: starts the RPool GUI without a console window.
//!
//! `rpool` is a console program because it is also the CLI, so Windows
//! opens a console for it when it is double-clicked. This tiny launcher is a
//! Windows (GUI subsystem) program: it starts `rpool gui` from its own
//! folder without a window and exits. Arguments are passed on before `gui`
//! (e.g. `--rclone PATH`). Elsewhere it simply starts `rpool gui`.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn rpool_next_to_me() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let dir = me
        .parent()
        .ok_or_else(|| "this program has no folder".to_string())?;
    let rpool = dir.join(if cfg!(windows) { "rpool.exe" } else { "rpool" });
    if rpool.is_file() {
        Ok(rpool)
    } else {
        Err(format!(
            "{} was not found next to rpool-gui",
            rpool.display()
        ))
    }
}

fn start() -> Result<(), String> {
    let mut command = Command::new(rpool_next_to_me()?);
    command
        .args(std::env::args_os().skip(1))
        .arg(OsString::from("gui"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start RPool: {e}"))
}

#[cfg(windows)]
fn show_error(text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("RPool"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(0, text.as_ptr(), caption.as_ptr(), 0x10); // MB_ICONERROR
    }
}

#[cfg(not(windows))]
fn show_error(text: &str) {
    eprintln!("{text}");
}

fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            show_error(&error);
            ExitCode::FAILURE
        }
    }
}
