//! Several sessions at once: switching, conflicts, unmounting and closing.
use super::form::MountForm;
use super::session::{MountSession, SessionSpec};
use super::sessions::{free_drive_letter, Conflict};
use crate::gui::settings::GuiSettings;
use crate::gui::state::GuiState;

fn spec(pool: &str, workspace: &str, mountpoint: &str) -> SessionSpec {
    SessionSpec {
        pool: pool.into(),
        workspace: workspace.into(),
        mountpoint: mountpoint.into(),
        frontend: Some(crate::cli::Frontend::Dav),
        reads: Vec::new(),
    }
}

/// Selects `pool` with these settings and marks its session running.
fn mount(
    form: &mut MountForm,
    settings: &mut GuiSettings,
    pool: &str,
    workspace: &str,
    mountpoint: &str,
    control: Option<tempfile::TempDir>,
) {
    form.select_pool(pool.into(), settings);
    form.workspace = workspace.into();
    form.mountpoint = mountpoint.into();
    form.session = MountSession::fake_running(spec(pool, workspace, mountpoint), control);
}

fn running_pools(form: &MountForm) -> Vec<String> {
    form.mounted_sessions()
        .into_iter()
        .map(|s| s.pool)
        .collect()
}

/// The Drive page with "family" mounted and shown, and "work" mounted in
/// the background (layout fixture).
pub(crate) fn with_two_sessions(state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    mount(form, settings, "work", "/rpool/work", "/mnt/work", None);
    mount(
        form,
        settings,
        "family",
        "/rpool/family",
        "/mnt/family",
        None,
    );
}

#[test]
fn two_pools_run_side_by_side_and_selection_never_stops_them() {
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    mount(&mut form, &mut settings, "A", "/ws/A", "/mnt/A", None);
    mount(&mut form, &mut settings, "B", "/ws/B", "/mnt/B", None);
    assert_eq!(running_pools(&form), ["A", "B"]);
    for pool in ["A", "C", "B", "A"] {
        form.select_pool(pool.into(), &mut settings);
        assert_eq!(running_pools(&form), ["A", "B"], "after selecting {pool}");
        assert!(form
            .mounted_sessions()
            .iter()
            .all(|s| !s.stopping && s.mounted));
    }
    // The shown pool's form and session belong together again.
    assert_eq!(form.pool, "A");
    assert_eq!(form.workspace, "/ws/A");
    assert!(form.is_running());
    let shown: Vec<_> = form
        .mounted_sessions()
        .into_iter()
        .filter(|s| s.selected)
        .collect();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].pool, "A");
    assert_eq!(shown[0].mountpoint, "/mnt/A");
    assert_eq!(shown[0].frontend, "WebDAV");
    assert!(form.any_running());
}

#[test]
fn same_pool_workspace_or_mountpoint_is_refused_before_starting() {
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    let root = tempfile::tempdir().unwrap();
    let ws = |name: &str| root.path().join(name).display().to_string();
    mount(&mut form, &mut settings, "A", &ws("A"), "/mnt/A", None);
    // The running selected pool cannot start a second time.
    assert_eq!(form.conflict(0), Some(Conflict::Pool("A".into())));

    form.select_pool("B".into(), &mut settings);
    form.workspace = ws("A");
    form.mountpoint = "/mnt/B".into();
    assert_eq!(form.conflict(0), Some(Conflict::Workspace("A".into())));
    assert_eq!(form.conflict(1), Some(Conflict::Workspace("A".into())));
    // A workspace inside the running one would share its files too.
    form.workspace = ws("A/inner");
    assert_eq!(form.conflict(1), Some(Conflict::Workspace("A".into())));
    let error = form.start_action("rclone", 0).unwrap_err();
    assert!(error.contains('A'), "{error}");
    assert!(!form.is_running());

    form.workspace = ws("B");
    form.mountpoint = "/mnt/A/".into();
    assert!(matches!(form.conflict(0), Some(Conflict::Mountpoint(pool, None)) if pool == "A"));
    // Sync and maintenance mount nothing, so only the workspace matters.
    assert_eq!(form.conflict(1), None);
    form.mountpoint = "/mnt/B".into();
    assert_eq!(form.conflict(0), None);

    // The recovery source is read, so it must not be a running workspace.
    form.recovery_source = ws("A");
    assert_eq!(
        form.conflict(super::form::RECOVERY),
        Some(Conflict::Workspace("A".into()))
    );
}

#[test]
fn a_taken_drive_letter_offers_a_free_one() {
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    mount(&mut form, &mut settings, "A", "/ws/A", "R:", None);
    form.select_pool("B".into(), &mut settings);
    form.workspace = "/ws/B".into();
    form.mountpoint = "r:\\".into();
    let conflict = form.conflict(0);
    assert!(matches!(&conflict, Some(Conflict::Mountpoint(pool, _)) if pool == "A"));
    if cfg!(not(windows)) {
        // Real Windows drives may occupy letters; elsewhere none do.
        assert_eq!(
            conflict,
            Some(Conflict::Mountpoint("A".into(), Some("S:".into())))
        );
        form.avoid_taken_drive_letter();
        assert_eq!(form.mountpoint, "S:");
        assert_eq!(form.conflict(0), None);
        assert_eq!(
            free_drive_letter(&["R:".into(), "s:/".into()]),
            Some("T:".into())
        );
        let mut all: Vec<String> = ('D'..='Z').map(|l| format!("{l}:")).collect();
        all.pop();
        assert_eq!(free_drive_letter(&all), Some("Z:".into()));
        all.push("Z:".into());
        assert_eq!(free_drive_letter(&all), None);
    }
}

#[test]
fn unmounting_one_pool_leaves_the_other_running() {
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (stop_a, stop_b) = (a.path().join("stop"), b.path().join("stop"));
    mount(&mut form, &mut settings, "A", "/ws/A", "/mnt/A", Some(a));
    mount(&mut form, &mut settings, "B", "/ws/B", "/mnt/B", Some(b));
    form.stop_pool("A").unwrap();
    assert!(stop_a.exists() && !stop_b.exists());
    let stopping: Vec<_> = form
        .mounted_sessions()
        .into_iter()
        .map(|s| (s.pool, s.stopping))
        .collect();
    assert_eq!(stopping, [("A".into(), true), ("B".into(), false)]);

    // A's process exits in the background; B keeps running and stays shown.
    form.background
        .get_mut("A")
        .unwrap()
        .runner
        .fake_finish(true);
    form.poll();
    assert_eq!(running_pools(&form), ["B"]);
    assert_eq!(form.finished_background(), ["A"]);
    assert!(form.is_running());
    // Showing A again presents its result; its stopped session is then dropped.
    form.select_pool("A".into(), &mut settings);
    assert!(!form.is_running() && form.session.notice.is_some());
    assert!(form.finished_background().is_empty());
    form.select_pool("B".into(), &mut settings);
    assert!(form.finished_background().is_empty());
    assert_eq!(running_pools(&form), ["B"]);
}

#[test]
fn closing_requests_a_graceful_stop_of_every_session() {
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (dir_a, dir_b) = (a.path().to_path_buf(), b.path().to_path_buf());
    mount(&mut form, &mut settings, "A", "/ws/A", "/mnt/A", Some(a));
    mount(&mut form, &mut settings, "B", "/ws/B", "/mnt/B", Some(b));
    form.stop_all();
    assert!(form.mounted_sessions().iter().all(|s| s.stopping));
    assert!(dir_a.join("stop").exists() && dir_b.join("stop").exists());
    // Dropping (window gone) keeps both control directories for the children.
    std::fs::remove_file(dir_a.join("stop")).unwrap();
    std::fs::remove_file(dir_b.join("stop")).unwrap();
    drop(form);
    assert!(dir_a.join("stop").exists() && dir_b.join("stop").exists());
    std::fs::remove_dir_all(dir_a).unwrap();
    std::fs::remove_dir_all(dir_b).unwrap();
}

#[test]
fn a_deferred_layout_change_is_read_from_the_mount_status() {
    use crate::mount::layout_refresh::{Deferral, Layout, STATUS_FILE};
    let mut settings = GuiSettings::default();
    let mut form = MountForm::from_settings(&settings);
    let control = tempfile::tempdir().unwrap();
    let layout = |mib: u64, parity| Layout {
        shard_size: mib << 20,
        placement: crate::models::Placement::RoundRobin,
        data_shards: 2,
        parity_shards: parity,
    };
    let deferral = Deferral {
        pending: 2,
        active: layout(1, 1),
        requested: layout(2, 0),
    };
    std::fs::write(
        control.path().join(STATUS_FILE),
        serde_json::to_vec(&deferral).unwrap(),
    )
    .unwrap();
    mount(
        &mut form,
        &mut settings,
        "A",
        "/ws/A",
        "/mnt/A",
        Some(control),
    );
    form.session.capacity_read -= std::time::Duration::from_secs(2);
    form.poll();
    assert_eq!(form.session.layout_status, Some(deferral));
}
