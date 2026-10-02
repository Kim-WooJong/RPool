//! Private, redacted and bounded tail of the rclone mount log file.

use super::*;

/// Recreates the private rclone log, keeping the previous session's log for
/// diagnosis. rclone does not log its RC credentials or the WebDAV bearer token;
/// display still redacts them and the file stays owner-only in `.rpool`.
pub(super) fn open_mount_log(path: &Path) -> Result<std::fs::File> {
    reject_link(path)?;
    let previous = path.with_extension("previous.log");
    reject_link(&previous)?;
    if path.exists() {
        std::fs::rename(path, &previous).context("cannot keep previous rclone mount log")?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).context("cannot create rclone mount log")
}

/// Incrementally reads rclone's log file into bounded, redacted display lines.
pub(super) struct MountLog {
    /// rclone log file being tailed.
    pub(super) path: PathBuf,
    /// Bytes of the file already consumed; reset if the file shrinks.
    pub(super) offset: u64,
    /// Bytes of an unfinished line carried to the next read.
    pub(super) partial: Vec<u8>,
    /// Current line exceeded `LOG_LINE_LIMIT` and is being skipped until its newline.
    pub(super) discarding: bool,
    /// Non-empty strings replaced by `[redacted]` in every returned line.
    pub(super) secrets: Vec<String>,
}

impl MountLog {
    /// Creates a reader starting at offset 0; empty secrets are dropped.
    pub(super) fn new(path: PathBuf, secrets: Vec<String>) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            discarding: false,
            secrets: secrets.into_iter().filter(|s| !s.is_empty()).collect(),
        }
    }

    /// Reads up to `LOG_READ_LIMIT` new bytes and returns complete, redacted lines
    /// (at most `LOG_LINES_RETURNED`). Read errors return nothing. Backs `MountProcess::logs`.
    pub(super) fn read_new(&mut self) -> Vec<String> {
        let mut chunk = Vec::new();
        let read = std::fs::File::open(&self.path).and_then(|mut file| {
            if file.metadata()?.len() < self.offset {
                self.offset = 0;
                self.partial.clear();
                self.discarding = false;
            }
            file.seek(SeekFrom::Start(self.offset))?;
            file.take(LOG_READ_LIMIT).read_to_end(&mut chunk)
        });
        if read.is_err() {
            return Vec::new();
        }
        self.offset += chunk.len() as u64;
        let mut lines = Vec::new();
        let mut rest = &chunk[..];
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            self.push(&rest[..end]);
            lines.push(self.finish_line());
            rest = &rest[end + 1..];
        }
        self.push(rest);
        if lines.len() > LOG_LINES_RETURNED {
            let omitted = lines.len() - LOG_LINES_RETURNED;
            lines.drain(..omitted);
            lines.insert(0, format!("[{omitted} earlier mount log lines omitted]"));
        }
        lines
    }

    /// Appends bytes to the current line, switching to discard mode once it is too long.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        if self.discarding {
            return;
        }
        self.partial.extend_from_slice(bytes);
        if self.partial.len() > LOG_LINE_LIMIT {
            // Never publish a truncated fragment that might end inside a secret.
            self.partial.clear();
            self.discarding = true;
        }
    }

    /// Ends the current line: redacts secrets, or returns a placeholder for an oversized line.
    pub(super) fn finish_line(&mut self) -> String {
        if std::mem::take(&mut self.discarding) {
            return "[oversized mount log line omitted]".into();
        }
        let mut text = String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned();
        for secret in &self.secrets {
            text = text.replace(secret, "[redacted]");
        }
        text.trim_end().to_string()
    }
}
