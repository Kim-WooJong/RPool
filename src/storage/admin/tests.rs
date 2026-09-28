use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fake {
    probes: AtomicUsize,
    quotas: AtomicUsize,
}
impl BackendAdmin for Fake {
    fn discover(&self) -> Result<Vec<String>> {
        Ok(vec!["z:".into(), "a:".into()])
    }
    fn catalog(&self) -> Result<RemoteCatalog> {
        RemoteCatalog::parse(
            &serde_json::json!({"a":{"type":"drive"},"z":{"type":"drive"},"alias":{"type":"crypt","remote":"a:folder"}}),
        )
    }
    fn probe(&self, remote: &str) -> Result<()> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        if remote == "z:" {
            bail!("unavailable")
        }
        Ok(())
    }
    fn quota(&self, remote: &str) -> QuotaReport {
        self.quotas.fetch_add(1, Ordering::SeqCst);
        if remote == "a:" {
            unavailable_quota(remote, "quota unavailable".into())
        } else {
            quota::parse(remote, br#"{"total":100,"used":25,"free":75}"#)
        }
    }
    fn ensure_encrypted(&self, _: &str) -> Result<()> {
        bail!("not crypt")
    }
}
#[test]
fn provider_access_and_quota_are_independent_and_reports_sorted() {
    let fake = Fake {
        probes: AtomicUsize::new(0),
        quotas: AtomicUsize::new(0),
    };
    let reports =
        crate::provider::check_providers_with_admin(&fake, &["z:".into(), "a:".into()], 2).unwrap();
    assert_eq!(reports[0].remote, "a:");
    assert!(reports[0].accessible);
    assert!(reports[0].quota.error.is_some());
    assert!(!reports[1].accessible);
    assert!(reports[1].quota.error.is_none());
    assert_eq!(fake.probes.load(Ordering::SeqCst), 2);
    assert_eq!(fake.quotas.load(Ordering::SeqCst), 2);
}
#[test]
fn capacity_aliases_share_account_quota_but_not_proven_failure_domains() {
    let catalog = RemoteCatalog::parse(&serde_json::json!({
        "drive":{"type":"drive","token":"not-retained"},
        "crypt":{"type":"crypt","remote":"drive:folder-one"},
        "alias":{"type":"alias","remote":"drive:folder-two"},
        "chunk":{"type":"chunker","remote":"crypt:chunks"}
    }))
    .unwrap();
    let targets = catalog
        .capacity_remotes(&["crypt:data".into(), "alias:other".into(), "chunk:".into()])
        .unwrap();
    assert_eq!(targets, vec!["drive:"]);
    let binding = catalog.capacity("alias:").unwrap();
    assert!(binding.domain.is_some());
    assert!(binding.failure_domain.is_none());
    assert!(!format!("{catalog:?}").contains("not-retained"));
}
#[test]
fn unresolved_capacity_is_explicit_and_windows_paths_are_not_remote_aliases() {
    let c=RemoteCatalog::parse(&serde_json::json!({"a":{"type":"alias","remote":"b:"},"b":{"type":"crypt","remote":"a:"},"u":{"type":"union"},"missing":{"type":"crypt"},"C":{"type":"drive"}})).unwrap();
    for raw in [
        "a:",
        "u:",
        "missing:",
        "unknown:",
        r"C:\Users\file",
        "C:/file",
        r"\\server\share",
    ] {
        assert!(c.capacity(raw).is_err(), "{raw}");
    }
    assert!(c.capacity_remotes(&["u:".into(), "C:".into()]).is_err());
}
#[test]
fn malformed_encryption_setting_is_not_discovered_as_usable_crypt() {
    let c=RemoteCatalog::parse(&serde_json::json!({"good":{"type":"crypt"},"bad":{"type":"crypt","no_data_encryption":"maybe"},"off":{"type":"crypt","no_data_encryption":"true"}})).unwrap();
    assert_eq!(c.crypt_remotes(), vec!["good:"]);
}
#[test]
fn native_diagnostics_require_no_tool_or_legacy_admin() {
    let reports = crate::doctor::run_checks_with(None, None);
    assert!(reports.iter().any(|r| r.check == "rclone-version"
        && r.status == "info"
        && r.message.contains("not required")));
    assert!(reports
        .iter()
        .any(|r| r.check == "rclone-remotes" && r.status == "info"));
    assert!(!reports
        .iter()
        .any(|r| r.check.starts_with("rclone") && r.status == "fail"));
}
#[test]
fn quota_parse_does_not_invent_numbers_on_error() {
    let report = quota::parse("x:", b"not json");
    assert!(report.error.is_some());
    assert!(report.total.is_none());
    assert!(report.free.is_none());
}

#[test]
fn health_queries_shared_capacity_once_for_aliases() {
    let fake = Fake {
        probes: AtomicUsize::new(0),
        quotas: AtomicUsize::new(0),
    };
    let reports =
        crate::provider::check_providers_with_admin(&fake, &["a:".into(), "alias:".into()], 2)
            .unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(fake.probes.load(Ordering::SeqCst), 2);
    assert_eq!(fake.quotas.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_encryption_follows_backing_paths_and_rejects_disabled_and_cycles() {
    let catalog = RemoteCatalog::parse(&serde_json::json!({
        "covered": {"type":"drive"}, "missing": {"type":"s3"},
        "disabled": {"type":"dropbox"},
        "nested": {"type":"alias", "remote":"covered:folder/deeper"},
        "crypt": {"type":"crypt", "remote":"nested:path"},
        "off": {"type":"crypt", "remote":"disabled:path", "no_data_encryption":"true"},
        "cycle": {"type":"alias", "remote":"loop:folder"},
        "loop": {"type":"crypt", "remote":"cycle:folder"},
        "union": {"type":"union"}
    }))
    .unwrap();
    assert_eq!(
        catalog.missing_encryption_remotes(),
        ["disabled", "missing"]
    );
}
