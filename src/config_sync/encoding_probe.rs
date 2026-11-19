//! Isolated, bounded, read-only filename views. No live config updates or daemon.
use super::secret_process::{execute, rclone_command, Output, MAX_SECRET_BYTES};
use crate::crypt::options::CryptConfig;
use crate::models::secrets::validate_remote_name;
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Private snapshot containing only one crypt and its direct backing remote.
/// tempfile creates the file exclusively with private permissions and removes it on drop.
pub(crate) struct Probe {
    config: tempfile::NamedTempFile,
    original: std::path::PathBuf,
    original_hash: blake3::Hash,
    executable: std::path::PathBuf,
    remote: String,
    backing: String,
    pub(crate) current_encoding: String,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    #[serde(rename = "Path")]
    pub(crate) path: String,
    #[serde(rename = "IsDir")]
    pub(crate) is_dir: bool,
    #[serde(rename = "Size", default)]
    pub(crate) size: i64,
}

impl Probe {
    pub(crate) fn new(executable: &Path, name: &str) -> Result<Self> {
        validate_remote_name(name)?;
        // Match native's override boundary, rather than silently inspecting a different config.
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy().to_ascii_uppercase();
            if key.starts_with("RCLONE_CRYPT_") || key.starts_with("RCLONE_CONFIG_") && key != "RCLONE_CONFIG_PASS" {
                bail!("encoding inventory refuses crypt/config environment overrides");
            }
        }
        // Honor RCLONE_CONFIG selection; remove all other backend overrides in children.
        let explicit = std::env::var_os("RCLONE_CONFIG");
        let config = if let Some(explicit) = explicit {
            super::tooling::resolve_rclone_config(executable, Some(Path::new(&explicit)))?
        } else {
            // Existing bounded/deadline helper; do not use tooling's unbounded discovery child.
            let discovered = super::provision::config_path(executable)?;
            super::tooling::resolve_rclone_config(executable, Some(&discovered))?
        };
        let mut command = rclone_command(executable, &config);
        command.args(["config", "dump"]);
        let raw = execute(&mut command, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))?;
        // unique_map rejects duplicate sections; values remain secret and never enter errors.
        #[derive(Deserialize)]
        #[serde(transparent)]
        struct Dump(#[serde(deserialize_with = "crate::models::secrets::unique_map")] BTreeMap<String, BTreeMap<String, String>>);
        let dump: Dump = serde_json::from_slice(&raw.0).map_err(|_| anyhow!("invalid encoding probe config snapshot"))?;
        let crypt = dump.0.get(name).ok_or_else(|| anyhow!("crypt remote not configured"))?;
        let parsed = CryptConfig::from_section(crypt).map_err(|_| anyhow!("unsupported crypt settings for encoding inventory"))?;
        if parsed.name_mode != crate::crypt::options::NameMode::Standard {
            bail!("encoding inventory requires standard filename encryption");
        }
        let (base_name, _) = parsed.remote.split_once(':').ok_or_else(|| anyhow!("encoding inventory needs a named direct backing remote"))?;
        validate_remote_name(base_name)?;
        let base = dump.0.get(base_name).ok_or_else(|| anyhow!("backing remote not configured"))?;
        // Deliberately narrower than native: no wrapper/alias/cloud credential refresh.
        if !matches!(base.get("type").map(String::as_str), Some("sftp" | "smb" | "local")) {
            bail!("encoding inventory currently supports only direct SFTP/SMB/local crypt locations; wrappers and cloud backends are rejected");
        }
        if base_name == name { bail!("invalid crypt backing cycle"); }
        let mut frozen = SensitiveBytes(Vec::new());
        for (section, values) in [(base_name, base), (name, crypt)] {
            writeln!(&mut frozen.0, "[{section}]")?;
            for (key, value) in values {
                if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    || value.chars().any(char::is_control) || value.trim() != value {
                    bail!("encoding snapshot contains unsupported option syntax");
                }
                writeln!(&mut frozen.0, "{key} = {value}")?;
            }
            writeln!(&mut frozen.0)?;
        }
        let mut file = tempfile::NamedTempFile::new().map_err(|_| anyhow!("cannot create private encoding snapshot"))?;
        file.write_all(&frozen.0).map_err(|_| anyhow!("cannot write private encoding snapshot"))?;
        file.flush().map_err(|_| anyhow!("cannot flush private encoding snapshot"))?;
        // Prove INI serialization did not reinterpret any key, root or base option.
        // This is local config parsing, not a cloud call. No values enter diagnostics.
        let mut check = rclone_command(executable, file.path());
        check.args(["config", "dump"]);
        let check_raw = execute(&mut check, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))?;
        let check_dump: Dump = serde_json::from_slice(&check_raw.0)
            .map_err(|_| anyhow!("invalid frozen config response"))?;
        if check_dump.0.len() != 2 || check_dump.0.get(name) != Some(crypt)
            || check_dump.0.get(base_name) != Some(base) {
            bail!("frozen crypt/base snapshot differs; inventory rejected before remote access");
        }
        Ok(Self {
            config: file,
            executable: executable.to_owned(),
            remote: name.to_owned(),
            backing: parsed.remote,
            original: config,
            original_hash: blake3::hash(&raw.0),
            current_encoding: crypt.get("filename_encoding").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "base32".into()),
        })
    }

    /// Original config is only used by the explicitly mutating name-encoding command.
    pub(crate) fn original_config(&self) -> &Path { &self.original }

    /// Optimistic fence before a setting change; snapshot keys/options are never logged.
    pub(crate) fn ensure_unchanged(&self) -> Result<()> {
        let mut command = rclone_command(&self.executable, &self.original);
        command.args(["config", "dump"]);
        let raw = execute(&mut command, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))?;
        if blake3::hash(&raw.0) != self.original_hash {
            bail!("rclone configuration changed during encoding preflight; reopen and inventory again");
        }
        Ok(())
    }

    /// Nonrecursive directory listing only. Errors never expose paths, names or stderr.
    pub(crate) fn list(&self, encoding: &str, relative: &str, max_bytes: usize) -> Result<Vec<Entry>> {
        super::provision::validate_filename_encoding(encoding)?;
        if !relative.is_empty() && !relative.split('/').all(|s| !s.is_empty() && s != "." && s != ".." && !s.contains('\\') && !s.chars().any(char::is_control)) {
            bail!("invalid inventory relative path");
        }
        let address = format!("{}:{relative}", self.remote);
        self.list_address(&address, Some(encoding), max_bytes)
    }

    pub(crate) fn backing_top(&self, max_bytes: usize) -> Result<Vec<Entry>> {
        self.list_address(&self.backing, None, max_bytes)
    }

    /// Opaque binding of the immutable config snapshot (never config values).
    pub(crate) fn binding(&self) -> String { self.original_hash.to_hex().to_string() }

    /// Cleanup-only recursive view. Private pinned config; never a daemon.
    pub(crate) fn cleanup_files(&self, encoding: &str) -> Result<Vec<Entry>> {
        super::provision::validate_filename_encoding(encoding)?;
        let mut command = rclone_command(&self.executable, self.config.path());
        command.args(["--crypt-filename-encoding", encoding, "lsjson", "--recursive",
            "--files-only", "--no-mimetype", "--no-modtime", "--", &format!("{}:", self.remote)]);
        let raw = execute(&mut command, |_| Ok(()), Output::Memory(32 * 1024 * 1024))
            .map_err(|_| anyhow!("cleanup_listing_failed_or_bound_or_timeout"))?;
        let entries: Vec<Entry> = serde_json::from_slice(&raw.0)
            .map_err(|_| anyhow!("invalid cleanup listing"))?;
        if entries.len() > 20_000 { bail!("cleanup listing exceeds 20000 objects per namespace"); }
        Ok(entries)
    }

    /// Exact bounded plaintext read. Errors never mean absence.
    pub(crate) fn cleanup_read(&self, relative: &str, limit: usize) -> Result<Vec<u8>> {
        self.cleanup_path(relative)?;
        let mut command = rclone_command(&self.executable, self.config.path());
        command.args(["--crypt-filename-encoding", &self.current_encoding,
            "cat", "--", &format!("{}:{relative}", self.remote)]);
        Ok(execute(&mut command, |_| Ok(()), Output::Memory(limit))?.0.clone())
    }

    /// Exact stat through the pinned view. Only rclone's documented file/dir
    /// not-found exit codes (4/3) prove absence; every other failure is unknown.
    pub(crate) fn cleanup_size(&self, relative: &str) -> Result<Option<u64>> {
        self.cleanup_path(relative)?;
        let mut command = rclone_command(&self.executable, self.config.path());
        command.args(["--crypt-filename-encoding", &self.current_encoding,
            "lsjson", "--stat", "--no-mimetype", "--no-modtime", "--", &format!("{}:{relative}", self.remote)]);
        match execute(&mut command, |_| Ok(()), Output::Memory(64 * 1024)) {
            Ok(raw) => {
                let item: Entry = serde_json::from_slice(&raw.0)?;
                if item.is_dir || item.size < 0 { bail!("expected exact regular object"); }
                Ok(Some(item.size as u64))
            }
            Err(e) if e.downcast_ref::<super::secret_process::SecretExit>()
                .is_some_and(|x| matches!(x.0, Some(3 | 4))) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Exact-file deletion only. Persist intent BEFORE calling; every failure
    /// has unknown outcome and needs reconciliation, never a blind retry.
    pub(crate) fn cleanup_delete(&self, relative: &str) -> Result<()> {
        self.cleanup_path(relative)?;
        self.ensure_unchanged()?;
        let mut command = rclone_command(&self.executable, self.config.path());
        command.args(["--crypt-filename-encoding", &self.current_encoding,
            "deletefile", "--", &format!("{}:{relative}", self.remote)]);
        execute(&mut command, |_| Ok(()), Output::Memory(1024))?;
        Ok(())
    }

    /// No traversal, directory addresses, alternate remotes or control syntax.
    fn cleanup_path(&self, relative: &str) -> Result<()> {
        if relative.is_empty() || relative.contains([':', '\\']) || relative.chars().any(char::is_control)
            || relative.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            bail!("invalid exact cleanup object path");
        }
        Ok(())
    }

    fn list_address(&self, address: &str, encoding: Option<&str>, max_bytes: usize) -> Result<Vec<Entry>> {
        let mut command = rclone_command(&self.executable, self.config.path());
        if let Some(encoding) = encoding {
            command.args(["--crypt-filename-encoding", encoding]);
        }
        command.args(["lsjson", "--max-depth", "1", "--no-mimetype", "--no-modtime", "--", address]);
        let raw = execute(&mut command, |_| Ok(()), Output::Memory(max_bytes))
            .map_err(|_| anyhow!("listing_failed_or_output_limit_or_timeout"))?;
        serde_json::from_slice(&raw.0).map_err(|_| anyhow!("invalid_listing_response"))
    }
}
