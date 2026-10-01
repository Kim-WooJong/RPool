//! Starting, polling and stopping the owned rclone mount process.

use super::*;

impl MountProcess {
    pub(crate) fn start(config: MountConfig) -> Result<Self> {
        validate_mountpoint(&config)?;
        #[cfg(target_os = "macos")]
        check_nfsmount_version(&config.rclone)?;
        #[cfg(target_os = "macos")]
        let target = config.target.canonicalize()?;
        #[cfg(not(target_os = "macos"))]
        let target = config.target.clone();
        std::fs::create_dir_all(&config.cache_dir).context("cannot create durable VFS cache")?;
        let anchor = config.anchor_dir.canonicalize()?;
        let cache = config.cache_dir.canonicalize()?;
        let lease = MountLease::prepare(&anchor, &cache, &target, &config.webdav)?;
        let log_path = anchor
            .parent()
            .context("workspace root missing")?
            .join(".rpool")
            .join("rclone-mount.log");
        let log = open_mount_log(&log_path)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|e| anyhow::anyhow!("cannot generate mount credentials: {e}"))?;
        let password = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let credential = base64(format!("rpool:{password}").as_bytes());
        let (cache_mode, write_back) = vfs_cache_policy();
        let (url, token) = &config.webdav;
        let mut command = Command::new(&config.rclone);
        command
            .arg(native_mount_command())
            .arg(":webdav:")
            .arg(&target)
            .args([
                "--vfs-cache-mode",
                cache_mode,
                "--vfs-write-back",
                write_back,
                "--cache-dir",
            ])
            .arg(cache)
            .args(["--rc", "--rc-addr", &address.to_string()])
            .env("RCLONE_RC_USER", "rpool")
            .env("RCLONE_RC_PASS", &password)
            .env("RCLONE_RC_NO_AUTH", "false")
            // rclone writes its own log file, never a pipe: if RPool exits first,
            // a pipe would kill rclone (SIGPIPE) while the kernel mount still needs it.
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .env("RCLONE_WEBDAV_URL", url)
            .env("RCLONE_WEBDAV_BEARER_TOKEN", token)
            .env("RCLONE_WEBDAV_VENDOR", "other")
            .env("RCLONE_WEBDAV_USER", "")
            .env("RCLONE_WEBDAV_PASS", "")
            .env("RCLONE_WEBDAV_BEARER_TOKEN_COMMAND", "")
            .args(["--dir-cache-time", "2s", "--vfs-read-chunk-size", "0"]);
        if let Some(name) = config.volume_name.as_deref().map(volume_label) {
            command.arg("--volname").arg(name);
        }
        configure_cache(
            &mut command,
            config.vfs_cache_gib,
            config.cache_min_free_gib,
        );
        // Only rclone may evict its clean, unused cache entries. Short
        // retention allows an unmounted reconciliation without deleting
        // potentially dirty cache files ourselves.
        command.args(["--vfs-cache-max-age", "1s"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        // rclone must bind this port itself. Authentication protects the brief bind race;
        // a stolen port produces an error, not an unauthenticated control connection.
        drop(listener);
        validate_mountpoint(&config)?;
        // Persist uncertainty BEFORE spawning: a crash in the spawn/record gap must fail closed.
        lease.record(&LeaseState::Launching)?;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                // spawn returned an error, so no owned child was launched.
                lease.clear()?;
                return Err(error).context(
                    "cannot start rclone native mount; check rclone and the platform mount support",
                );
            }
        };
        if let Err(error) = lease.record(&LeaseState::Running { pid: child.id() }) {
            // Do not discard a launching lease unless termination has actually been observed.
            if child.kill().is_ok() && child.wait().is_ok() {
                let _ = lease.clear_if_unmounted(&target);
            }
            return Err(error).context("cannot record mount child; uncertain lease retained unless child termination was confirmed");
        }
        let mut secrets = vec![password, credential.clone()];
        secrets.push(config.webdav.1.clone());
        let logs = Mutex::new(MountLog::new(log_path, secrets));
        Ok(Self {
            child,
            target,
            address,
            credential,
            logs,
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: false,
            lease,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if let Some(_exit) = &status {
            self.stopped = true;
            #[cfg(target_os = "macos")]
            if !self.graceful_quit_requested || !_exit.success() {
                self.lease.record(&LeaseState::ShutdownUncertain {
                    pid: self.child.id(),
                })?;
                self.shutdown_uncertain = true;
                bail!("macOS NFS process exited without a confirmed graceful unmount ({_exit}); mount lease retained for OS I/O inspection");
            }
            self.lease.clear_if_unmounted(&self.target)?;
        }
        Ok(status)
    }

    /// Accessibility alone is insufficient on Unix because the mount directory preexists.
    pub(crate) fn ready(&self) -> bool {
        if self.stopped || self.rc("vfs/stats").is_err() {
            return false;
        }
        #[cfg(windows)]
        {
            self.target.join("\\").is_dir()
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Some(parent) = self.target.parent() else {
                return false;
            };
            match (std::fs::metadata(&self.target), std::fs::metadata(parent)) {
                (Ok(target), Ok(parent)) => target.is_dir() && target.dev() != parent.dev(),
                _ => false,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    pub(crate) fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|mut logs| logs.read_new())
            .unwrap_or_default()
    }

    pub(crate) fn is_running(&mut self) -> bool {
        !self.stopped && matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits up to `limit` for the owned rclone child to exit by itself and never
    /// kills it. A live macOS NFS server may still answer kernel I/O through the
    /// WebDAV backend, so the caller keeps that backend serving until this returns true.
    pub(crate) fn wait_for_exit(&mut self, limit: Duration) -> Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            match self.child.try_wait()? {
                Some(status) => {
                    self.stopped = true;
                    // Same criteria as a timely graceful stop; otherwise the lease stays.
                    if self.graceful_quit_requested && status.success() {
                        self.lease.clear_if_unmounted(&self.target)?;
                        self.shutdown_uncertain = false;
                    }
                    return Ok(true);
                }
                None if Instant::now() >= deadline => return Ok(false),
                None => thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    pub(crate) fn stop(&mut self) -> Result<StopReport> {
        self.stop_with_grace(Duration::from_secs(if cfg!(target_os = "macos") {
            12
        } else {
            3
        }))
    }

    pub(super) fn stop_with_grace(&mut self, grace: Duration) -> Result<StopReport> {
        if self.shutdown_uncertain {
            bail!("macOS NFS shutdown remains uncertain; rclone was not killed and mount lease is retained");
        }
        if self.stopped {
            self.lease.clear_if_unmounted(&self.target)?;
            return Ok(StopReport { forced: false });
        }
        if self.poll()?.is_some() {
            return Ok(StopReport { forced: false });
        }
        // Deliver delayed saves to the still-serving WebDAV backend before rclone quits.
        match drain_writeback(self.address, &self.credential, WRITEBACK_DRAIN_LIMIT) {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "Native saves were still queued after {}s; they remain in the durable VFS cache for the next start",
                WRITEBACK_DRAIN_LIMIT.as_secs()
            ),
            Err(error) => eprintln!(
                "Native write-back queue not drained ({error:#}); queued saves remain in the VFS cache"
            ),
        }
        self.graceful_quit_requested = self.rc("core/quit").is_ok();
        // macOS nfsmount must also tear down its loopback NFS server and native
        // mount. Allow its exit callback to complete before a forced kill.
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.poll()?.is_some() {
                return Ok(StopReport { forced: false });
            }
            thread::sleep(Duration::from_millis(50));
        }
        #[cfg(target_os = "macos")]
        {
            self.lease.record(&LeaseState::ShutdownUncertain {
                pid: self.child.id(),
            })?;
            self.shutdown_uncertain = true;
            bail!("macOS NFS unmount did not finish within the grace period; rclone was left alive to finish kernel I/O, mount lease retained. Do not reuse this workspace until the OS mount and I/O state are verified");
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.child
                .kill()
                .context("cannot stop mount process; mount may still be active")?;
            self.child.wait().context("cannot reap mount process")?;
            self.stopped = true;
            self.lease.clear_if_unmounted(&self.target)?;
            Ok(StopReport { forced: true })
        }
    }

    pub(super) fn rc(&self, endpoint: &str) -> Result<Vec<u8>> {
        rc_call(
            self.address,
            &self.credential,
            endpoint,
            "{}",
            Duration::from_millis(400),
            1024 * 1024,
        )
    }
}

impl Drop for MountProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
