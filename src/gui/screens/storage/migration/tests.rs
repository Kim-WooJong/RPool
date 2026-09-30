use super::lost::{copy_text, groups_text, missing_text};
use super::review_step::quota_verdict;
use super::run_step::state_label;
use super::state::*;
use crate::gui::task::JobStatus;
use crate::migration::model::*;
use crate::models::PoolDefinition;
use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) const ID: &str = "0123456789abcdef0123456789abcdef";

pub(crate) fn sample_lost() -> Vec<LostFile> {
    vec![LostFile {
        archive_id: "fedcba9876543210fedcba".into(),
        original_name:
            "a very long file name that should never widen the page beyond its window width.bin"
                .into(),
        size: 3 * 1024 * 1024,
        groups: vec![GroupLoss {
            group: 3,
            required_k: 2,
            available: 1,
            missing: vec![
                MissingShard {
                    index: 1,
                    remote: "gone-crypt:".into(),
                    reason: MissingReason::RemoteRemoved,
                },
                MissingShard {
                    index: 2,
                    remote: "b-crypt:".into(),
                    reason: MissingReason::Corrupt,
                },
            ],
        }],
        detected: "plan".into(),
    }]
}

pub(crate) fn sample_plan(pool: &str) -> Plan {
    let lost = &sample_lost()[0];
    let entry = |id: &str, action| Entry {
        archive_id: id.into(),
        original_name: format!("{id}.bin"),
        size: 10,
        source: "local".into(),
        fingerprint: "f".into(),
        action,
        download_bytes: 1,
        upload_bytes: 1,
        losses: vec![],
        detail: None,
    };
    let mut lost_entry = entry(&lost.archive_id, Action::Lost);
    lost_entry.original_name = lost.original_name.clone();
    lost_entry.size = lost.size;
    lost_entry.losses = lost.groups.clone();
    Plan {
        version: 1,
        migration_id: ID.into(),
        pool: pool.into(),
        created_unix: 0,
        created_by: "pc".into(),
        target: PoolDefinition::default(),
        entries: vec![entry("x", Action::Relocate), lost_entry],
        counts: Counts {
            unaffected: 4,
            relocate: 1,
            reencode: 0,
            lost: 1,
            unknown: 0,
        },
        download_bytes: 2 << 30,
        upload_bytes: 3 << 30,
        new_storage_bytes: 3 << 30,
        estimated_seconds: Some((600.0, 5400.0)),
        download_mib_s: Some(20.0),
        upload_mib_s: None,
        quota_ok: Some(true),
        notes: vec!["Adding an account moves nothing without --rebalance.".into()],
    }
}

pub(crate) fn sample_status(pool: &str) -> MigrationStatus {
    MigrationStatus {
        migration_id: ID.into(),
        pool: pool.into(),
        created_unix: 0,
        created_by: "studio".into(),
        counts: sample_plan(pool).counts,
        to_move: 4,
        switched: 1,
        verified: 2,
        failed_unknown: 1,
        lost: sample_lost(),
        abandoned: false,
        complete: false,
        pcs: vec!["studio".into(), "laptop".into()],
        last_activity_unix: 0,
    }
}

/// A form showing `step` with fake data and no background work due.
pub(crate) fn sample_form(pool: &str, step: Step) -> MigrationForm {
    let mut form = MigrationForm::default();
    form.select_pool(pool);
    form.statuses = vec![sample_status(pool)];
    form.status_pool = Some(pool.into());
    form.last_status_at = Some(Instant::now());
    form.step = step;
    form.active_id = (step != Step::Plan).then(|| ID.to_string());
    if step == Step::Review {
        form.plan = Some(sample_plan(pool));
    }
    form
}

#[test]
fn plan_result_moves_to_review_and_errors_stay_on_plan() {
    let mut form = MigrationForm::default();
    form.select_pool("family");
    form.apply_plan(PlanOutcome {
        plan: Err("boom".into()),
        speed_note: None,
    });
    assert_eq!(form.step, Step::Plan);
    assert!(form.error.as_deref().unwrap().contains("boom"));

    form.apply_plan(PlanOutcome {
        plan: Ok(sample_plan("other")),
        speed_note: None,
    });
    assert_eq!(form.step, Step::Plan, "a plan of another pool is not shown");

    form.apply_plan(PlanOutcome {
        plan: Ok(sample_plan("family")),
        speed_note: Some("measure failed".into()),
    });
    assert_eq!(form.step, Step::Review);
    assert_eq!(form.active_id.as_deref(), Some(ID));
    assert!(form.error.is_none());
    assert_eq!(form.notice.as_deref(), Some("measure failed"));
    assert_eq!(form.active_lost(), sample_lost(), "lost rows from the plan");
    assert!(
        form.status_due(Instant::now()),
        "list reloads with the new id"
    );
}

#[test]
fn status_results_ignore_stale_pools_and_feed_the_run_step() {
    let mut form = MigrationForm::default();
    form.select_pool("family");
    assert!(form.status_due(Instant::now()), "first open loads the list");
    form.apply_status(StatusOutcome {
        pool: "old".into(),
        result: Ok(vec![sample_status("old")]),
    });
    assert!(form.statuses.is_empty());
    form.apply_status(StatusOutcome {
        pool: "family".into(),
        result: Ok(vec![sample_status("family")]),
    });
    assert_eq!(form.statuses.len(), 1);
    form.last_status_at = Some(Instant::now());
    assert!(!form.status_due(Instant::now()), "plan step: no polling");
    form.open(ID);
    assert_eq!(form.step, Step::Run);
    assert!(form.status_due(Instant::now()), "open refreshes at once");
    form.last_status_at = Some(Instant::now());
    assert!(!form.status_due(Instant::now()));
    assert!(form.status_due(Instant::now() + STATUS_REFRESH + Duration::from_millis(1)));
    assert_eq!(form.active_status().unwrap().switched, 1);
    assert_eq!(form.active_lost().len(), 1);

    form.apply_status(StatusOutcome {
        pool: "family".into(),
        result: Err("offline".into()),
    });
    assert_eq!(form.status_error.as_deref(), Some("offline"));
    assert_eq!(form.statuses.len(), 1, "keeps the last good list");

    form.select_pool("other");
    assert_eq!(form.step, Step::Plan);
    assert!(form.statuses.is_empty() && form.active_id.is_none());
}

#[test]
fn finished_tasks_update_the_wizard() {
    let mut form = sample_form("family", Step::Run);
    form.pausing = true;
    form.finish_task(Watched::Run(ID.into()), Some(JobStatus::Completed));
    assert!(form.notice.as_deref().unwrap().starts_with("Paused"));
    assert!(!form.pausing);
    assert_eq!(form.step, Step::Run);
    assert!(form.status_due(Instant::now()), "refresh after a run");

    form.finish_task(Watched::Run(ID.into()), Some(JobStatus::Failed));
    assert!(form.notice.as_deref().unwrap().contains("error"));

    form.finish_task(Watched::Abandon(ID.into()), Some(JobStatus::Failed));
    assert_eq!(form.step, Step::Run);
    assert!(form.error.is_some());
    form.finish_task(Watched::Abandon(ID.into()), Some(JobStatus::Completed));
    assert_eq!(form.step, Step::Plan);
    assert!(form.active_id.is_none());
    assert!(form.request_pause().is_err(), "nothing to pause");
}

#[test]
fn background_planning_reports_failures_without_blocking() {
    let mut form = MigrationForm::default();
    form.select_pool("family");
    form.download_mib_s = -3.0;
    form.upload_mib_s = 12.5;
    let options = form.plan_options(6);
    assert_eq!(
        (
            options.download_mib_s,
            options.upload_mib_s,
            options.workers
        ),
        (None, Some(12.5), 6)
    );
    form.start_plan("/nonexistent/rclone-for-test", vec![], 2);
    assert!(form.planning());
    let deadline = Instant::now() + Duration::from_secs(20);
    while form.poll() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!form.planning());
    assert_eq!(form.step, Step::Plan);
    assert!(form
        .error
        .as_deref()
        .unwrap()
        .starts_with("Planning failed"));
}

#[test]
fn run_and_abandon_argv() {
    let stop = Path::new("/ctl dir/stop");
    assert_eq!(
        run_args("-my pool", ID, stop, false),
        [
            "pool",
            "migrate",
            "run",
            "--id",
            ID,
            "--stop-file",
            "/ctl dir/stop",
            "--",
            "-my pool"
        ]
        .map(OsString::from)
    );
    assert_eq!(
        abandon_args("family", ID),
        ["pool", "migrate", "abandon", "--id", ID, "--", "family"].map(OsString::from)
    );
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
fn run_argv_parses_with_the_cli() {
    parses(run_args("-my pool", ID, Path::new("/ctl dir/stop"), false));
    parses(run_args("-my pool", ID, Path::new("/ctl dir/stop"), true));
}

#[test]
fn abandon_argv_parses_with_the_cli() {
    parses(abandon_args("-my pool", ID));
}

#[test]
fn lost_table_text() {
    let lost = sample_lost();
    assert_eq!(groups_text(&lost[0].groups), "g3: 1/3 available (K=2)");
    assert_eq!(
        missing_text(&lost[0].groups),
        "gone-crypt: #1 (account removed), b-crypt: #2 (corrupt)"
    );
    let mut tabbed = lost.clone();
    tabbed[0].original_name = "a\tb\nc".into();
    let text = copy_text(&tabbed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("name\tsize"));
    assert_eq!(lines[1].split('\t').count(), 5);
    assert!(lines[1].starts_with("a b c\t3145728\tfedcba9876543210fedcba\t"));
    assert_eq!(short_id(ID), "0123456789ab");
    assert_eq!(short_id("abc"), "abc");
}

#[test]
fn durations_verdicts_and_labels() {
    assert!(format_eta(None).starts_with("unknown"));
    assert!(format_eta(Some((600.0, 5_400.0))).starts_with("about"));
    assert_eq!(quota_verdict(Some(false)).0, "Does not fit the free space");
    let mut status = sample_status("family");
    assert_eq!(state_label(&status).0, "In progress");
    status.complete = true;
    assert_eq!(state_label(&status).0, "Complete");
    status.abandoned = true;
    assert_eq!(state_label(&status).0, "Discarded");
}

#[test]
fn only_data_changes_offer_a_migration() {
    let old = PoolDefinition {
        remotes: vec!["a:".into(), "b:".into()],
        ..PoolDefinition::default()
    };
    let mut new = old.clone();
    new.remotes.reverse();
    new.workers += 3;
    new.retries += 1;
    assert!(
        !policy_change_affects_data(&old, &new),
        "order/workers only"
    );
    for change in [
        |p: &mut PoolDefinition| p.remotes.push("c:".into()),
        |p: &mut PoolDefinition| p.data_shards += 1,
        |p: &mut PoolDefinition| p.parity_shards += 1,
        |p: &mut PoolDefinition| p.native_crypt = !p.native_crypt,
        |p: &mut PoolDefinition| {
            p.shard_size =
                crate::models::shard_size::ShardSize::from_mib(p.shard_size.mib_ceil() + 1).unwrap()
        },
    ] {
        let mut changed = old.clone();
        change(&mut changed);
        assert!(policy_change_affects_data(&old, &changed));
    }
}

#[test]
fn pools_hint_preselects_the_pool_on_account_changes() {
    let mut form = sample_form("family", Step::Run);
    form.select_pool("family");
    assert_eq!(form.step, Step::Run, "same pool keeps the wizard");
    form.select_pool("work");
    assert_eq!((form.pool.as_str(), form.step), ("work", Step::Plan));
}
