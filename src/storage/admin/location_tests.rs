//! Multiple roots share one connection and outage, never multiply one disk's quota.
use super::*;
use domains::{DomainIdentity, DomainStore};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Admin(AtomicUsize);
impl BackendAdmin for Admin {
    fn discover(&self) -> Result<Vec<String>> {
        unreachable!()
    }
    fn catalog(&self) -> Result<RemoteCatalog> {
        unreachable!()
    }
    fn probe(&self, _: &str) -> Result<()> {
        unreachable!()
    }
    fn ensure_encrypted(&self, _: &str) -> Result<()> {
        unreachable!()
    }
    fn quota(&self, remote: &str) -> QuotaReport {
        self.0.fetch_add(1, Ordering::SeqCst);
        quota::parse(remote, br#"{"total":100,"used":0,"free":100}"#)
    }
}
fn catalog(ids: &[(&str, &str)]) -> RemoteCatalog {
    let mut c = RemoteCatalog::parse(&serde_json::json!({
        "nas":{"type":"sftp"},
        "one":{"type":"crypt","remote":"nas:/disk1"},
        "two":{"type":"crypt","remote":"nas:/disk2"},
        "same":{"type":"crypt","remote":"nas:/disk1/./"},
        "cloud":{"type":"drive"},
        "cloud_folder":{"type":"crypt","remote":"cloud:folder"}
    }))
    .unwrap();
    c.set_domains(DomainStore {
        version: 1,
        remotes: ids
            .iter()
            .map(|(name, cap)| {
                (
                    (*name).into(),
                    DomainIdentity {
                        capacity: (*cap).into(),
                        failure: format!("host-{name}"),
                    },
                )
            })
            .collect(),
    });
    c
}
#[test]
fn separate_declared_disks_add_capacity_but_share_connection_outage() {
    let c = catalog(&[("nas", "first"), ("two", "second")]);
    let first = c.capacity("one:").unwrap();
    let second = c.capacity("two:").unwrap();
    assert_eq!(first.target, "nas:/disk1");
    assert_eq!(second.target, "nas:/disk2");
    assert_eq!(first.failure_domain, second.failure_domain);
    assert_eq!(second.failure_domain.unwrap().as_str(), "host-nas");
    assert_eq!(c.placement_target("two:").unwrap(), "nas");
    assert!(c.identity_declared("two:"));
    let a = Admin(AtomicUsize::new(0));
    let s = budget::BudgetSnapshot::query(&a, &c, &["one:".into(), "two:".into()]);
    assert_eq!(s.targets.len(), 2);
    assert_eq!(s.total_free().unwrap(), 200);
    assert_eq!(a.0.load(Ordering::SeqCst), 2);
}
#[test]
fn same_disk_or_unverified_locations_never_double_free_space() {
    for c in [
        catalog(&[]),
        catalog(&[("nas", "shared"), ("two", "shared")]),
    ] {
        let s = budget::BudgetSnapshot::query(
            &Admin(AtomicUsize::new(0)),
            &c,
            &["one:".into(), "two:".into()],
        );
        assert_eq!(s.targets.len(), 2);
        assert_eq!(s.total_free().unwrap(), 100);
    }
}
#[test]
fn identical_backing_scope_with_different_groups_is_rejected_before_quota() {
    for (ids, remotes) in [
        (
            vec![("nas", "base"), ("same", "duplicate")],
            vec!["one:".into(), "same:".into()],
        ),
        (
            vec![("nas", "base"), ("one", "duplicate")],
            vec!["nas:/disk1".into(), "one:".into()],
        ),
    ] {
        let c = catalog(&ids);
        let a = Admin(AtomicUsize::new(0));
        let s = budget::BudgetSnapshot::query(&a, &c, &remotes);
        assert!(s.targets.is_empty());
        assert_eq!(s.rejected.len(), 2);
        assert_eq!(a.0.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn whole_account_backend_cannot_claim_independent_folder_capacity() {
    let c = catalog(&[("cloud", "account"), ("cloud_folder", "not-a-disk")]);
    assert!(c.capacity("cloud_folder:").is_err());
    assert!(!c.identity_declared("cloud_folder:"));
    assert_eq!(c.capacity("cloud:folder").unwrap().target, "cloud:");
}
#[test]
fn declaring_only_a_location_is_recognized_without_fabricating_outage_identity() {
    let c = catalog(&[("two", "disk2")]);
    assert!(c.identity_declared("two:"));
    assert!(c.capacity("two:").unwrap().failure_domain.is_none());
    let s = budget::BudgetSnapshot::query(&Admin(AtomicUsize::new(0)), &c, &["two:".into()]);
    assert_eq!(s.targets.len(), 1);
}

fn alias_catalog(ids: &[(&str, &str)]) -> RemoteCatalog {
    let mut c = RemoteCatalog::parse(&serde_json::json!({
        "nas":{"type":"sftp"},
        "root":{"type":"alias","remote":"nas:/"},
        "nested":{"type":"alias","remote":"root:disk1"},
        "one":{"type":"crypt","remote":"nested:encrypted"},
        "two":{"type":"crypt","remote":"root:disk2"},
        "inner":{"type":"crypt","remote":"nas:/disk1"},
        "outer":{"type":"crypt","remote":"inner:plaintext"},
        "over_crypt":{"type":"alias","remote":"inner:folder"}
    }))
    .unwrap();
    c.set_domains(DomainStore {
        version: 1,
        remotes: ids
            .iter()
            .map(|(name, cap)| {
                (
                    (*name).into(),
                    DomainIdentity {
                        capacity: (*cap).into(),
                        failure: format!("host-{name}"),
                    },
                )
            })
            .collect(),
    });
    c
}

#[test]
fn alias_suffixes_select_distinct_disks_and_keep_base_outage() {
    let c = alias_catalog(&[("nas", "disk1"), ("two", "disk2")]);
    let one = c.capacity("one:plaintext").unwrap();
    let two = c.capacity("two:").unwrap();
    assert_eq!(one.target, "nas:/disk1/encrypted");
    assert_eq!(two.target, "nas:/disk2");
    assert_eq!(one.failure_domain, two.failure_domain);
    assert_eq!(one.failure_domain.unwrap().as_str(), "host-nas");
    assert_eq!(c.placement_target("one:").unwrap(), "nas");
    let a = Admin(AtomicUsize::new(0));
    let s = budget::BudgetSnapshot::query(&a, &c, &["one:".into(), "two:".into()]);
    assert!(s.rejected.is_empty());
    assert_eq!(s.total_free().unwrap(), 200);
    assert_eq!(a.0.load(Ordering::SeqCst), 2);
}

#[test]
fn direct_and_alias_same_root_conflict_before_quota() {
    let c = alias_catalog(&[("nas", "base"), ("nested", "different")]);
    let a = Admin(AtomicUsize::new(0));
    let s = budget::BudgetSnapshot::query(&a, &c, &["nas:/disk1".into(), "nested:".into()]);
    assert!(s.targets.is_empty());
    assert_eq!(s.rejected.len(), 2);
    assert_eq!(a.0.load(Ordering::SeqCst), 0);
}

#[test]
fn crypt_and_nested_crypt_suffixes_are_not_plaintext_physical_paths() {
    let c = alias_catalog(&[("nas", "base"), ("outer", "different")]);
    for remote in ["inner:plaintext", "outer:more", "over_crypt:child"] {
        assert_eq!(c.capacity(remote).unwrap().target, "nas:/disk1");
        assert_eq!(c.placement_target(remote).unwrap(), "nas");
        assert_eq!(
            c.capacity(remote).unwrap().failure_domain.unwrap().as_str(),
            "host-nas"
        );
    }
    let a = Admin(AtomicUsize::new(0));
    let s = budget::BudgetSnapshot::query(&a, &c, &["inner:".into(), "outer:".into()]);
    assert!(s.targets.is_empty());
    assert_eq!(s.rejected.len(), 2);
    assert_eq!(a.0.load(Ordering::SeqCst), 0);
}

#[test]
fn windows_local_quota_keys_normalize_separators_case_and_root_boundaries() {
    use super::catalog::quota_path_key;
    for (raw, expected) in [
        (r"C:\Pool\.\child\..\", "c:/pool"),
        ("c:/POOL/", "c:/pool"),
        (r"C:\..\Pool", "c:/pool"),
        (r"C:Pool", "c:pool"),
        (r"C:\", "c:/"),
        (r"D:\Pool", "d:/pool"),
        (r"\\Server\Share\Pool\..\", "//server/share"),
        ("//SERVER/share/../Pool", "//server/share/pool"),
        (r"\Server\Share", "/server/share"),
    ] {
        assert_eq!(quota_path_key(raw, "local", true), expected, "{raw}");
    }
    assert_ne!(
        quota_path_key("C:Pool", "local", true),
        quota_path_key("C:/Pool", "local", true)
    );
    assert_ne!(
        quota_path_key("//server/share", "local", true),
        quota_path_key("/server/share", "local", true)
    );
    // Flag off: macOS/POSIX local paths do not gain Windows semantics.
    assert_eq!(quota_path_key(r"C:\Pool", "local", false), r"C:\Pool");
}

#[test]
fn sftp_backslashes_and_case_are_literal_on_every_host() {
    use super::catalog::quota_path_key;
    for windows in [false, true] {
        assert_eq!(
            quota_path_key(r"/Disk\Folder", "sftp", windows),
            r"/Disk\Folder"
        );
        assert_ne!(
            quota_path_key(r"/Disk\Folder", "sftp", windows),
            quota_path_key("/Disk/Folder", "sftp", windows)
        );
    }
    let c = RemoteCatalog::parse(&serde_json::json!({
        "nas":{"type":"sftp"},
        "literal":{"type":"alias","remote":r"nas:/Disk\Folder"},
        "crypt":{"type":"crypt","remote":"literal:child"}
    }))
    .unwrap();
    assert_eq!(
        c.capacity("crypt:").unwrap().target,
        r"nas:/Disk\Folder/child"
    );
}

#[test]
fn smb_leading_slash_and_case_aliases_conflict_before_quota() {
    let mut c = RemoteCatalog::parse(&serde_json::json!({
        "nas":{"type":"smb"},
        "root":{"type":"alias","remote":"nas:/SHARE"},
        "one":{"type":"crypt","remote":"nas:share/disk"},
        "two":{"type":"crypt","remote":"root:DISK"}
    }))
    .unwrap();
    c.set_domains(DomainStore {
        version: 1,
        remotes: [
            (
                "one".into(),
                DomainIdentity {
                    capacity: "one".into(),
                    failure: String::new(),
                },
            ),
            (
                "two".into(),
                DomainIdentity {
                    capacity: "two".into(),
                    failure: String::new(),
                },
            ),
        ]
        .into_iter()
        .collect(),
    });
    let a = Admin(AtomicUsize::new(0));
    let s = budget::BudgetSnapshot::query(&a, &c, &["one:".into(), "two:".into()]);
    assert!(s.targets.is_empty());
    assert_eq!(s.rejected.len(), 2);
    assert_eq!(a.0.load(Ordering::SeqCst), 0);
}

#[cfg(windows)]
#[test]
fn windows_local_actual_budget_rejects_separator_drive_and_unc_aliases() {
    for (first, second) in [
        (r"disk:C:\Pool", "disk:c:/pool"),
        (r"disk:\\Server\Share\Pool", "disk://server/share/pool"),
    ] {
        let mut c = RemoteCatalog::parse(&serde_json::json!({
            "disk":{"type":"local"},
            "one":{"type":"crypt","remote":first},
            "two":{"type":"crypt","remote":second}
        }))
        .unwrap();
        c.set_domains(DomainStore {
            version: 1,
            remotes: [
                (
                    "one".into(),
                    DomainIdentity {
                        capacity: "one".into(),
                        failure: String::new(),
                    },
                ),
                (
                    "two".into(),
                    DomainIdentity {
                        capacity: "two".into(),
                        failure: String::new(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        let a = Admin(AtomicUsize::new(0));
        let s = budget::BudgetSnapshot::query(&a, &c, &["one:".into(), "two:".into()]);
        assert!(s.targets.is_empty());
        assert_eq!(s.rejected.len(), 2);
        assert_eq!(a.0.load(Ordering::SeqCst), 0);
    }
}
