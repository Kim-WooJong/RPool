//! Console output that never panics. The crate root redefines `println!`,
//! `eprintln!`, `print!` and `eprint!` to call these. std's macros panic
//! when stdout or stderr is gone: the GUI that started a mount closed its
//! pipes, the terminal was closed, or the output was piped into `head`. In a
//! mount that panic unwound past the graceful stop and dropped the WebDAV
//! backend under a live macOS NFS server, so the volume hung until a reboot.
#![cfg_attr(
    test,
    allow(dead_code, reason = "tests keep std's capturing print macros")
)]
use std::fmt::Arguments;
use std::io::Write;

/// Writes `args` to stdout; a closed or broken stream is ignored.
pub(crate) fn out(args: Arguments<'_>) {
    let _ = std::io::stdout().lock().write_fmt(args);
}

/// Writes `args` and a newline to stdout under one lock; errors are ignored.
pub(crate) fn out_line(args: Arguments<'_>) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_fmt(args).and_then(|()| out.write_all(b"\n"));
}

/// Writes `args` to stderr; a closed or broken stream is ignored.
#[allow(dead_code, reason = "backs eprint!, which no code uses today")]
pub(crate) fn err(args: Arguments<'_>) {
    let _ = std::io::stderr().lock().write_fmt(args);
}

/// Writes `args` and a newline to stderr under one lock; errors are ignored.
pub(crate) fn err_line(args: Arguments<'_>) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_fmt(args).and_then(|()| err.write_all(b"\n"));
}

#[cfg(all(test, unix))]
mod tests {
    /// Writing while stdout and stderr lead into a pipe without a reader
    /// (EPIPE) neither panics nor kills the process.
    #[test]
    fn writing_to_closed_pipes_does_not_panic() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (saved_out, saved_err) = unsafe { (libc::dup(1), libc::dup(2)) };
        unsafe {
            libc::close(fds[0]);
            libc::dup2(fds[1], 1);
            libc::dup2(fds[1], 2);
        }
        for _ in 0..4 {
            super::out_line(format_args!("line {}", 1));
            super::out(format_args!("partial"));
            super::err_line(format_args!("error {}", 2));
            super::err(format_args!("partial"));
        }
        unsafe {
            libc::dup2(saved_out, 1);
            libc::dup2(saved_err, 2);
            libc::close(saved_out);
            libc::close(saved_err);
            libc::close(fds[1]);
        }
    }
}
