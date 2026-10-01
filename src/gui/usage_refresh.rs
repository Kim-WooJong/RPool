use crate::gui::i18n::{tr, trf};
use crate::models::QuotaReport;
use crate::storage::admin::{collect_quota_reports, BackendAdmin, RcloneAdmin};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

pub(crate) struct UsageSnapshot {
    pub(crate) catalog_signature: String,
    pub(crate) needs_encryption: bool,
    pub(crate) missing_encryption: Vec<String>,
    pub(crate) reports: Vec<QuotaReport>,
    pub(crate) crypt_remotes: Vec<String>,
    pub(crate) backing_remotes: Vec<String>,
    /// Backend type (`drive`, `dropbox`, …) and wrapping crypt remotes of
    /// each backing provider, for the provider cards.
    pub(crate) providers: ProviderDetails,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProviderDetails {
    pub(crate) kinds: std::collections::BTreeMap<String, String>,
    pub(crate) crypts: std::collections::BTreeMap<String, Vec<String>>,
}

impl ProviderDetails {
    fn from_catalog(
        catalog: &crate::storage::admin::RemoteCatalog,
        backing: &[String],
        crypts: &[String],
    ) -> Self {
        let mut details = Self::default();
        for name in backing {
            if let Some(kind) = catalog.backend_kind(name) {
                details.kinds.insert(name.clone(), kind.to_string());
            }
        }
        for crypt in crypts {
            if let Ok(target) = catalog.placement_target(crypt) {
                details
                    .crypts
                    .entry(target)
                    .or_default()
                    .push(crypt.clone());
            }
        }
        details
    }
}

#[derive(Default)]
pub(crate) struct UsageRefresh {
    receiver: Option<Receiver<Result<UsageSnapshot, String>>>,
    running: bool,
}

impl UsageRefresh {
    pub(crate) fn start(&mut self, rclone: String, workers: usize) {
        self.start_with_admin(Arc::new(RcloneAdmin::inherited(&rclone)), workers);
    }
    pub(crate) fn start_with_admin(&mut self, admin: Arc<dyn BackendAdmin>, workers: usize) {
        if self.running {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| {
                let catalog = admin.catalog().map_err(|error| {
                    trf(
                        "remote discovery failed: {error}",
                        &[("error", &format!("{error:#}"))],
                    )
                })?;
                let missing_encryption = catalog.missing_encryption_remotes();
                let needs_encryption = !missing_encryption.is_empty();
                let catalog_signature = format!("{catalog:?}");
                let crypt_remotes = catalog.crypt_remotes();
                let backing_remotes = catalog.backing_remotes();
                let providers =
                    ProviderDetails::from_catalog(&catalog, &backing_remotes, &crypt_remotes);
                // Discovery is useful even when a provider cannot report capacity.
                let capacity = (|| {
                    let physical = catalog.physical_remotes()?;
                    let remotes = catalog.capacity_remotes(&physical)?;
                    collect_quota_reports(admin.as_ref(), &remotes, workers.max(1))
                })();
                let (reports, warning) = match capacity {
                    Ok(reports) => (reports, None),
                    Err(error) => (
                        Vec::new(),
                        Some(trf(
                            "Capacity unavailable: {error}",
                            &[("error", &format!("{error:#}"))],
                        )),
                    ),
                };
                Ok(UsageSnapshot {
                    catalog_signature,
                    needs_encryption,
                    missing_encryption,
                    reports,
                    crypt_remotes,
                    backing_remotes,
                    providers,
                    warning,
                })
            })();
            let _ = sender.send(result);
        });

        self.receiver = Some(receiver);
        self.running = true;
    }

    pub(crate) fn poll(&mut self) -> Option<Result<UsageSnapshot, String>> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(tr("usage refresh worker stopped unexpectedly").to_string())
            }
        };
        self.receiver = None;
        self.running = false;
        Some(result)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::admin::RemoteCatalog;
    use std::sync::{mpsc, Barrier};
    struct CryptOnly;
    impl BackendAdmin for CryptOnly {
        fn catalog(&self) -> anyhow::Result<RemoteCatalog> {
            RemoteCatalog::parse(
                &serde_json::json!({"encrypted": {"type": "crypt", "remote": "missing:folder"}}),
            )
        }
        fn discover(&self) -> anyhow::Result<Vec<String>> {
            unreachable!()
        }
        fn probe(&self, _: &str) -> anyhow::Result<()> {
            unreachable!()
        }
        fn quota(&self, _: &str) -> QuotaReport {
            unreachable!()
        }
        fn ensure_encrypted(&self, _: &str) -> anyhow::Result<()> {
            unreachable!()
        }
    }
    #[test]
    fn encrypted_provider_discovery_does_not_require_capacity_reports() {
        let mut refresh = UsageRefresh::default();
        refresh.start_with_admin(Arc::new(CryptOnly), 1);
        let snapshot = refresh
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.crypt_remotes, vec!["encrypted:"]);
        assert!(snapshot.reports.is_empty());
    }
    struct MixedProviders;
    impl BackendAdmin for MixedProviders {
        fn catalog(&self) -> anyhow::Result<RemoteCatalog> {
            RemoteCatalog::parse(&serde_json::json!({
                "base": {"type": "drive"},
                "base_crypt": {"type": "crypt", "remote": "base:encrypted"},
                "new": {"type": "dropbox"}
            }))
        }
        fn discover(&self) -> anyhow::Result<Vec<String>> {
            unreachable!()
        }
        fn probe(&self, _: &str) -> anyhow::Result<()> {
            unreachable!()
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            QuotaReport {
                remote: remote.into(),
                total: Some(100),
                used: Some(10),
                free: Some(90),
                trashed: None,
                other: None,
                used_percent: Some(10.0),
                error: None,
            }
        }
        fn ensure_encrypted(&self, _: &str) -> anyhow::Result<()> {
            unreachable!()
        }
    }

    #[test]
    fn discovery_preserves_crypt_picker_but_reports_only_base_capacity() {
        let mut refresh = UsageRefresh::default();
        refresh.start_with_admin(Arc::new(MixedProviders), 1);
        let snapshot = refresh
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(snapshot.needs_encryption);
        assert_eq!(snapshot.backing_remotes, vec!["base", "new"]);
        assert_eq!(snapshot.crypt_remotes, vec!["base_crypt:"]);
        assert_eq!(snapshot.providers.kinds["base"], "drive");
        assert_eq!(snapshot.providers.kinds["new"], "dropbox");
        assert_eq!(snapshot.providers.crypts["base"], vec!["base_crypt:"]);
        assert!(!snapshot.providers.crypts.contains_key("new"));
        assert_eq!(
            snapshot
                .reports
                .iter()
                .map(|r| r.remote.as_str())
                .collect::<Vec<_>>(),
            vec!["base:", "new:"]
        );
    }

    struct Blocked {
        entered: mpsc::Sender<()>,
        release: Arc<Barrier>,
        finished: mpsc::Sender<()>,
    }
    impl BackendAdmin for Blocked {
        fn catalog(&self) -> anyhow::Result<RemoteCatalog> {
            self.entered.send(()).unwrap();
            self.release.wait();
            self.finished.send(()).unwrap();
            anyhow::bail!("synthetic discovery failure")
        }
        fn discover(&self) -> anyhow::Result<Vec<String>> {
            panic!("unexpected")
        }
        fn probe(&self, _: &str) -> anyhow::Result<()> {
            panic!("unexpected")
        }
        fn quota(&self, _: &str) -> QuotaReport {
            panic!("unexpected")
        }
        fn ensure_encrypted(&self, _: &str) -> anyhow::Result<()> {
            panic!("unexpected")
        }
    }
    #[test]
    fn usage_discovery_stays_off_ui_thread_and_failure_clears_running() {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let gate = Arc::new(Barrier::new(2));
        let mut refresh = UsageRefresh::default();
        refresh.start_with_admin(
            Arc::new(Blocked {
                entered: tx,
                release: gate.clone(),
                finished: done_tx,
            }),
            1,
        );
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(refresh.is_running());
        assert!(refresh.poll().is_none());
        gate.wait();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // Receiver blocks only in test; production poll remains nonblocking.
        let result = refresh
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(result.is_err());
        // Replay the received result to exercise poll deterministically. The worker
        // may still own its sender after send; disconnection is not a join signal.
        let (tx, rx) = mpsc::channel();
        tx.send(result).unwrap();
        refresh.receiver = Some(rx);
        assert!(refresh.poll().unwrap().is_err());
        assert!(!refresh.is_running());
    }
}
