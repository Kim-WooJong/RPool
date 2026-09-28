use super::age_vault::checked_identity_path;
use super::secret_process::{execute, Output};
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_TOOL_OUTPUT: usize = 64 * 1024;

fn regular_config(path: &Path) -> Result<PathBuf> {
    let meta =
        fs::symlink_metadata(path).map_err(|_| anyhow!("rclone configuration file is missing"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("rclone configuration must be a regular non-symlink file");
    }
    path.canonicalize()
        .map_err(|_| anyhow!("cannot resolve rclone configuration path"))
}

pub(crate) fn resolve_rclone_config(executable: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return regular_config(path);
    }

    let output = Command::new(executable)
        .args(["config", "file"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| anyhow!("cannot query rclone configuration path"))?;
    if !output.status.success() || output.stdout.len() > MAX_TOOL_OUTPUT {
        bail!("rclone configuration path discovery failed");
    }
    let candidate = parse_rclone_config_file_output(&output.stdout)?;
    regular_config(&candidate)
}

fn parse_rclone_config_file_output(raw: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| anyhow!("rclone configuration path response is not UTF-8"))?;
    let candidate = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| anyhow!("rclone did not report a configuration path"))?;
    Ok(PathBuf::from(candidate))
}

pub(crate) fn derive_age_recipient(
    age_keygen: &Path,
    identity: &Path,
    artifact_root: &Path,
) -> Result<String> {
    let identity = checked_identity_path(identity, artifact_root)?;
    let mut command = Command::new(age_keygen);
    command.arg("-y").arg(identity);
    let raw = execute(&mut command, |_| Ok(()), Output::Memory(8192))?;
    let recipient = std::str::from_utf8(&raw.0)
        .map_err(|_| anyhow!("age recipient output is not UTF-8"))?
        .trim();
    if recipient.is_empty() || recipient.chars().any(char::is_whitespace) {
        bail!("age-keygen returned an invalid recipient");
    }
    Ok(recipient.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unix_rclone_config_file_output() {
        let path = parse_rclone_config_file_output(
            b"Configuration file is stored at:\n/home/user/.config/rclone/rclone.conf\n",
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/home/user/.config/rclone/rclone.conf"));
    }

    #[test]
    fn parses_windows_rclone_config_file_output_with_spaces() {
        let path = parse_rclone_config_file_output(
            b"Configuration file is stored at:\r\nC:\\Users\\Test User\\AppData\\Roaming\\rclone\\rclone.conf\r\n",
        ).unwrap();
        assert_eq!(
            path,
            PathBuf::from(r"C:\Users\Test User\AppData\Roaming\rclone\rclone.conf")
        );
    }
}
