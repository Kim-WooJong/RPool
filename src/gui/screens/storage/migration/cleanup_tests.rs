use super::cleanup_state::*;
use super::cleanup_step::{countdown, keep_reason};
use super::state::{MigrationForm, Step};
use super::tests::{sample_form, ID};
use crate::migration::retire::model::*;
use std::ffi::OsString;
use std::path::PathBuf;

fn object(address: &str, size: u64) -> RetireObject {
    RetireObject {
        address: address.into(),
        size,
        root: address
            .split_once(':')
            .map(|(r, _)| format!("{r}:"))
            .unwrap_or_default(),
    }
}

/// A report with something in every section (long names included).
pub(crate) fn sample_report(pool: &str) -> RetireReport {
    let item = |id: &str, kind| Item {
        archive_id: id.into(),
        kind,
        original_name: format!(
            "{id} with a very long name that must never widen the page beyond its window.bin"
        ),
        replacement: (kind == ItemKind::Original)
            .then(|| "migrate-aaaaaaaaaaaaaaaaaaaaaaaa".into()),
        objects: vec![
            object(&format!("a-crypt:{id}/manifest.json"), 100),
            object(&format!("b-crypt:{id}/data/0.bin"), 4 << 20),
        ],
    };
    let view = |id: &str, state, blocked: Option<&str>| FossilView {
        item: item(id, ItemKind::Original),
        fossil_id: "f".into(),
        state,
        since_unix: 0,
        due_unix: 7 * 86_400,
        by_pc: "studio".into(),
        remaining: 2,
        restore_refused: false,
        blocked: blocked.map(str::to_owned),
    };
    let candidates = vec![
        item("old-a", ItemKind::Original),
        item("migrate-cccccccccccccccccccccccc", ItemKind::Orphan),
    ];
    let objects: Vec<RetireObject> = candidates.iter().flat_map(|i| i.objects.clone()).collect();
    RetireReport {
        migration_id: ID.into(),
        pool: pool.into(),
        now_unix: 0,
        dry_run: true,
        candidate_accounts: per_account(&objects),
        quarantine_accounts: per_account(&objects),
        left_on_removed: vec![AccountBytes {
            root: "gone-crypt:".into(),
            objects: 3,
            bytes: 12,
        }],
        candidates,
        quarantine: vec![
            view("old-b", FossilState::Waiting, None),
            view(
                "old-c",
                FossilState::Due,
                Some("still referenced: drive v6 metadata"),
            ),
            view("old-d", FossilState::Deleting, None),
        ],
        purged: 4,
        kept: vec![Kept {
            archive_id: "lost-1".into(),
            kind: ItemKind::Original,
            original_name: "lost.bin".into(),
            reason: KeepReason::Lost,
            detail: "not replaced; the original is kept".into(),
        }],
        guard: GuardView {
            pool_objects: 100,
            pool_bytes: 1 << 30,
            max_percent: 50,
            max_objects: 10_000,
            quarantine_refusal: Some("60 of the pool's 100 objects is more than 50%".into()),
            delete_refusal: None,
        },
        uncertain: vec!["drive workspace /tmp/ws: unreadable".into()],
        actions: vec![],
    }
}

/// The wizard on the cleanup step of a completed migration with a report.
pub(crate) fn sample_cleanup_form(pool: &str) -> MigrationForm {
    let mut form = sample_form(pool, Step::Run);
    form.step = Step::Cleanup;
    form.statuses[0].complete = true;
    form.cleanup.report = Some(sample_report(pool));
    form.cleanup.report_id = Some(ID.into());
    form
}

fn parses(args: Vec<OsString>) {
    use clap::Parser;
    let mut full = vec![OsString::from("rpool")];
    full.extend(args);
    if let Err(error) = crate::cli::Cli::try_parse_from(full) {
        panic!("{error}");
    }
}

#[test]
fn cleanup_argv_matches_the_cli() {
    let mut form = CleanupForm::default();
    form.grace_days = 3;
    form.include_removed = true;
    form.full_verify = true;
    let ws = vec![PathBuf::from("/ws one")];
    let args = form.args(&CleanupAction::Quarantine, "-my pool", ID, &ws, false);
    assert_eq!(
        args,
        [
            "pool",
            "migrate",
            "retire",
            "--id",
            ID,
            "--confirm",
            "--step",
            "quarantine",
            "--grace-days",
            "3",
            "--include-removed-accounts",
            "--full-verify",
            "--workspace",
            "/ws one",
            "--",
            "-my pool"
        ]
        .map(OsString::from)
    );
    parses(args);
    let args = form.args(&CleanupAction::Delete, "p", ID, &[], true);
    assert!(args.contains(&OsString::from("--force")) && args.contains(&OsString::from("delete")));
    parses(args);
    form.grace_days = 10_000;
    parses(form.args(&CleanupAction::Quarantine, "p", ID, &[], false));
    let restore = form.args(
        &CleanupAction::Restore(vec!["a".into()]),
        "p",
        ID,
        &[],
        false,
    );
    assert_eq!(
        restore,
        ["pool", "migrate", "restore", "--id", ID, "--item", "a", "--", "p"].map(OsString::from)
    );
    parses(restore);
    parses(form.args(&CleanupAction::Restore(vec![]), "p", ID, &[], false));
}

#[test]
fn guard_needs_a_second_confirmation() {
    let mut form = CleanupForm::default();
    form.report = Some(sample_report("family"));
    form.report_id = Some(ID.into());
    // The sample report refuses quarantine: no argv, the confirmation opens.
    assert!(form
        .request(CleanupAction::Quarantine, "family", ID, &[])
        .is_none());
    assert_eq!(form.confirm_force, Some(CleanupAction::Quarantine));
    // Deletion is not refused: it starts directly, without --force.
    let args = form
        .request(CleanupAction::Delete, "family", ID, &[])
        .unwrap();
    assert!(!args.contains(&OsString::from("--force")));
    assert_eq!(form.confirm_force, None);
}

#[test]
fn reports_options_and_messages() {
    let mut form = CleanupForm::default();
    assert_eq!(form.options(vec![]).grace_seconds, 7 * 86_400);
    form.workspace = " /typed ".into();
    let running = vec![PathBuf::from("/run")];
    assert_eq!(
        form.workspaces(&running),
        vec![PathBuf::from("/run"), PathBuf::from("/typed")]
    );
    form.apply_report(ReportOutcome {
        id: "x".into(),
        result: Err("offline".into()),
    });
    assert!(form.report_for("x").is_none() && form.error.as_deref().unwrap().contains("offline"));
    form.apply_report(ReportOutcome {
        id: "x".into(),
        result: Ok(sample_report("p")),
    });
    assert!(form.report_for("x").is_some() && form.report_for("y").is_none());
    assert!(form
        .finish(CleanupAction::Quarantine, true)
        .contains("Nothing was deleted"));
    assert!(
        form.report_id.is_none(),
        "a finished step reloads the report"
    );
    assert!(form.finish(CleanupAction::Delete, false).contains("error"));
    assert_eq!(countdown(100, 100), "ready to delete");
    assert_eq!(countdown(0, 3 * 86_400 + 2 * 3_600), "3d 2h left");
    assert_eq!(countdown(0, 5 * 3_600), "5h left");
    assert_eq!(countdown(0, 60), "under 1h left");
    assert_eq!(keep_reason(KeepReason::Referenced), "still referenced");
}

#[test]
fn finished_cleanup_tasks_update_the_notice() {
    let mut form = sample_cleanup_form("family");
    form.cleanup.watched = Some(CleanupAction::Restore(vec![]));
    let runner = crate::gui::task::TaskRunner::default();
    form.watch_task(&runner);
    assert!(form.cleanup.watched.is_none());
    assert!(
        form.notice.as_deref().unwrap().contains("error"),
        "no completed task: reported as failed"
    );
}

#[test]
fn cleanup_strings_are_translated() {
    use crate::gui::i18n::{tr_in, Language};
    assert_eq!(
        tr_in(Language::Korean, "Move to cleanup quarantine"),
        "정리 격리로 이동"
    );
    assert_eq!(
        tr_in(Language::Japanese, "Delete permanently"),
        "完全に削除"
    );
    assert_eq!(tr_in(Language::Chinese, "Restore all"), "全部恢复");
}
