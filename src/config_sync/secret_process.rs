//! All secret-bearing children use direct process calls, bounded memory output,
//! closed stderr, and a deadline. Neither commands nor child diagnostics are logged.
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Result};
use std::fs::File;
use std::io::Read;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Deadline for one secret-bearing child process.
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);
/// Upper bound on secret output read into memory (32 MiB); vault inputs may be twice this.
pub(super) const MAX_SECRET_BYTES: usize = 32 * 1024 * 1024;

/// Where a secret child's stdout goes.
pub(super) enum Output {
    /// Read into memory, at most this many bytes.
    Memory(usize),
    /// Stream straight into this file (age ciphertext).
    Ciphertext(File),
}
/// Kills and reaps the child when dropped, so no secret process outlives an error.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits for the child, killing it after [`CHILD_TIMEOUT`].
fn wait(child: &mut Child) -> Result<ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < CHILD_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("secret subprocess failed or timed out");
            }
        }
    }
}

/// Runs `command` with stderr closed, feeding stdin through `writer` on one
/// thread while another collects stdout per `output`; fails on timeout, a
/// non-zero exit or output above the limit. Output bytes are returned only after
/// the child succeeded. Used for every rclone/age call that handles secrets.
pub(super) fn execute<W>(command: &mut Command, writer: W, output: Output) -> Result<SensitiveBytes>
where
    W: FnOnce(&mut ChildStdin) -> Result<()> + Send,
{
    let limit = match &output {
        Output::Memory(n) => *n,
        Output::Ciphertext(_) => 0,
    };
    command.stdin(Stdio::piped()).stderr(Stdio::null());
    match output {
        Output::Memory(_) => {
            command.stdout(Stdio::piped());
        }
        Output::Ciphertext(file) => {
            command.stdout(Stdio::from(file));
        }
    }
    let mut child = ChildGuard(
        command
            .spawn()
            .map_err(|_| anyhow!("cannot start secret subprocess"))?,
    );
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| anyhow!("secret subprocess input unavailable"))?;
    let stdout = child.0.stdout.take();
    std::thread::scope(|scope| {
        let writer_thread = scope.spawn(move || writer(&mut input));
        let reader_thread = scope.spawn(move || -> Result<SensitiveBytes> {
            let mut bytes = SensitiveBytes(Vec::with_capacity(limit.saturating_add(1)));
            if let Some(stream) = stdout {
                stream
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes.0)
                    .map_err(|_| anyhow!("cannot read secret subprocess output"))?;
                if bytes.0.len() > limit {
                    bail!("secret subprocess output exceeded limit");
                }
            }
            Ok(bytes)
        });
        let status = wait(&mut child.0);
        // Join both workers even on errors; do not detach a secret-bearing writer.
        let written = writer_thread.join();
        let read = reader_thread.join();
        if !status?.success() {
            bail!("secret subprocess returned failure");
        }
        written.map_err(|_| anyhow!("secret subprocess input worker failed"))??;
        read.map_err(|_| anyhow!("secret subprocess output worker failed"))?
    })
}

/// `rclone --config <config>` command with inherited `RCLONE_*` overrides removed
/// (except `RCLONE_CONFIG_PASS`) and quiet, non-interactive logging.
pub(super) fn rclone_command(executable: &std::path::Path, config: &std::path::Path) -> Command {
    let mut command = Command::new(executable);
    // Do not allow inherited debugging, logging, backend overrides, daemon key
    // files, or RC server flags to alter a secret operation. Retain only the
    // existing config-unlock password; it is not a crypt password or a CLI arg.
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if (upper.starts_with("RCLONE_") && upper != "RCLONE_CONFIG_PASS")
            || upper.starts_with("_RCLONE_")
        {
            command.env_remove(key);
        }
    }
    command.arg("--config").arg(config).args([
        "--ask-password=false",
        "--log-level",
        "ERROR",
        "--log-file",
        "",
    ]);
    command
}
