use std::ffi::OsString;
use std::path::Path;

#[test]
fn pool_profiles_restore_all_options_and_isolate_new_pools() {
    let mut settings = crate::gui::settings::GuiSettings::default();
    settings.mount_cache.native_gib = 7;
    let mut form = super::MountForm::from_settings(&settings);
    form.select_pool("A".into(), &mut settings);
    form.workspace = "/persistent/A".into();
    form.mountpoint = "/mount/A".into();
    form.pc_name = "desktop".into();
    form.manifests = vec!["archive.json".into()];
    form.interval_seconds = 42;
    form.frontend = crate::cli::Frontend::Dav;
    form.native_read_only = true;
    form.cache_gib = 23;
    let a = form.profile();
    form.recovery_source = "source".into();
    form.recovery_skip_remotes = "old-remote".into();
    form.recovery_reprocess_plan = "plan.json".into();
    form.manifest_input = "unfinished".into();
    form.select_pool("B".into(), &mut settings);
    assert_eq!(
        form.profile(),
        crate::gui::settings::MountProfile {
            cache: settings.mount_cache.clone(),
            ..Default::default()
        }
    );
    assert!(form.recovery_source.is_empty());
    assert!(form.recovery_skip_remotes.is_empty());
    assert!(form.recovery_reprocess_plan.is_empty());
    assert!(form.manifest_input.is_empty());
    form.workspace = "/persistent/B".into();
    form.spool_gib = 9;
    let b = form.profile();
    form.select_pool("A".into(), &mut settings);
    assert_eq!(form.profile(), a);
    form.select_pool("B".into(), &mut settings);
    assert_eq!(form.profile(), b);
    // Simulate restarting with persisted settings, without writing real GUI config.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("gui-settings.json");
    crate::utils::save_json_atomic(&path, &settings).unwrap();
    let mut restored = crate::utils::read_json(&path).unwrap();
    let mut restarted = super::MountForm::from_settings(&restored);
    restarted.select_pool("A".into(), &mut restored);
    assert_eq!(restarted.profile(), a);
    restarted.select_pool("B".into(), &mut restored);
    assert_eq!(restarted.profile(), b);
}

#[test]
fn legacy_settings_seed_only_safe_cache_defaults_for_each_pool() {
    // Settings written before the single drive mode still load: removed mode
    // fields are ignored and the old worker name becomes the PC name.
    let mut settings: crate::gui::settings::GuiSettings =
        serde_json::from_str(r#"{"mount_cache":{"online_drive":false,"native_gib":5}}"#).unwrap();
    assert!(settings.mount_profiles.is_empty());
    let mut form = super::MountForm::from_settings(&settings);
    form.select_pool("legacy".into(), &mut settings);
    assert_eq!(form.vfs_cache_gib, 5);
    assert!(form.workspace.is_empty());
    let partial: crate::gui::settings::GuiSettings = serde_json::from_str(
        r#"{"mount_profiles":{"A":{"workspace":"/A","worker_name":"laptop","pool_sync":false,
            "pool_retention":true,"shared_root":"crypt:team","bounded_shared":true,
            "keep_previous":7,"cache":{"online_drive":false,"shard_gib":4}}}}"#,
    )
    .unwrap();
    let profile = &partial.mount_profiles["A"];
    assert_eq!(profile.workspace, "/A");
    assert_eq!(profile.pc_name, "laptop");
    assert_eq!(profile.cache.shard_gib, 4);
    let json = serde_json::to_string(profile).unwrap();
    for removed in [
        "worker_name",
        "pool_sync",
        "pool_retention",
        "shared_root",
        "bounded_shared",
        "keep_previous",
        "online_drive",
    ] {
        assert!(!json.contains(removed), "{removed}: {json}");
    }
}

#[test]
fn recovery_form_forwards_source_and_skip_aliases_without_mount_or_retention() {
    let mut form = super::MountForm {
        pool: "recovered-pool".into(),
        ..Default::default()
    };
    let base = std::env::temp_dir();
    form.workspace = base.join("new destination 한 글").display().to_string();
    form.recovery_source = base.join("original source 한 글").display().to_string();
    form.recovery_skip_remotes = "badcrypt\n another crypt \n".into();
    form.recovery_reprocess_plan = base
        .join("reprocess operation/plan.json")
        .display()
        .to_string();
    let args = form.recovery_args(&base.join("stop")).unwrap();
    let parsed = crate::cli::Cli::try_parse_from(
        std::iter::once(std::ffi::OsString::from("rpool")).chain(args),
    )
    .unwrap();
    let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
        panic!("mount")
    };
    assert_eq!(
        args.account_recovery_from,
        Some(form.recovery_source.clone().into())
    );
    assert_eq!(args.recovery_skip_remote, ["badcrypt", "another crypt"]);
    assert_eq!(
        args.recovery_reprocess_plan,
        Some(form.recovery_reprocess_plan.clone().into())
    );
    assert!(args.mountpoint.is_none() && !args.sync_only);
    let invalid = super::MountForm {
        recovery_source: "relative".into(),
        ..form
    };
    assert!(invalid.recovery_args(&base.join("stop")).is_err());
}
#[test]
fn native_frontend_choice_reaches_mount_cli() {
    use clap::Parser;
    let parse = |form: &super::MountForm, sync_only: bool| {
        let mut args: Vec<std::ffi::OsString> = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/w",
            "--mountpoint=/m",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        form.append_cache_args(&mut args);
        args.extend(form.frontend_args(sync_only));
        let parsed = crate::cli::Cli::try_parse_from(args).unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount")
        };
        (args.frontend, args.native_read_only)
    };
    use crate::cli::Frontend;
    // Read-only reaches the CLI only where a native frontend exists.
    let native_here = Frontend::native_here().is_some();
    let mut form = super::MountForm::default();
    assert_eq!(
        form.frontend,
        Frontend::Auto,
        "native where available is the default"
    );
    form.frontend = Frontend::Fuse;
    form.native_read_only = true;
    assert_eq!(parse(&form, false), (Frontend::Fuse, native_here));
    assert_eq!(
        parse(&form, true),
        (Frontend::Auto, false),
        "sync-only passes nothing"
    );
    form.frontend = Frontend::Dav;
    assert_eq!(
        form.frontend_args(false),
        [std::ffi::OsString::from("--frontend=dav")],
        "read-only applies to native frontends only"
    );
    // The choice is saved per pool and restored.
    form.frontend = Frontend::Fuse;
    let profile = form.profile();
    let json = serde_json::to_string(&profile).unwrap();
    assert!(json.contains("\"frontend\":\"fuse\""), "{json}");
    let restored: crate::gui::settings::MountProfile = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, profile);
    let legacy: crate::gui::settings::MountProfile = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.frontend, Frontend::Auto);
}
#[test]
fn saved_cache_preferences_reach_mount_cli() {
    use clap::Parser;
    {
        let mut settings = crate::gui::settings::GuiSettings::default();
        settings.mount_cache = crate::gui::settings::MountCacheSettings {
            shard_gib: 4,
            native_gib: 5,
            min_free_gib: 3,
            spool_gib: 8,
        };
        let restored = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        let form = super::MountForm::from_settings(&restored);
        assert_eq!(form.cache_settings(), settings.mount_cache);
        let mut args: Vec<std::ffi::OsString> = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/new",
            "--sync-only",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        form.append_cache_args(&mut args);
        let parsed = crate::cli::Cli::try_parse_from(args).unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount")
        };
        assert_eq!((args.vfs_cache_gib, args.cache_min_free_gib), (5, 3));
        assert_eq!((args.cache_gib, args.spool_gib), (4, 8));
    }
    let form = super::MountForm::from_settings(&crate::gui::settings::GuiSettings::default());
    assert!(form.workspace.is_empty());
}
use super::*;
use clap::Parser;
use std::path::PathBuf;
#[test]
fn mount_arguments_roundtrip_and_sync_omits_mountpoint() {
    for sync_only in [false, true] {
        let mut args = vec![OsString::from("rpool")];
        args.extend(build_args(
            "-pool 한 글",
            &PathBuf::from("/persistent workspace"),
            "R:",
            &[
                "-manifest 한 글.json".into(),
                "crypt:path with spaces/manifest.json".into(),
            ],
            42,
            Path::new("/control dir/stop"),
            sync_only,
        ));
        let cli = crate::cli::Cli::try_parse_from(args).unwrap();
        let Some(crate::cli::Commands::Mount(parsed)) = cli.command else {
            panic!("expected mount");
        };
        assert_eq!(parsed.pool, "-pool 한 글");
        assert_eq!(parsed.workspace, PathBuf::from("/persistent workspace"));
        assert_eq!(parsed.sync_only, sync_only);
        assert_eq!(parsed.mountpoint, (!sync_only).then(|| PathBuf::from("R:")));
        assert_eq!(
            parsed.manifests,
            [
                "-manifest 한 글.json",
                "crypt:path with spaces/manifest.json"
            ]
        );
        assert_eq!(parsed.interval_seconds, 42);
        assert_eq!(parsed.stop_file, Some(PathBuf::from("/control dir/stop")));
    }
}

/// Draws the whole Mount screen off-screen for two frames.
fn render(state: &mut crate::gui::state::GuiState) {
    let ctx = eframe::egui::Context::default();
    for _ in 0..2 {
        let mut output = ctx.run_ui(eframe::egui::RawInput::default(), |ui| {
            super::show(ui, state)
        });
        output.textures_delta.clear();
    }
}

#[test]
fn mount_screen_renders_every_tab_and_capacity_state() {
    let pools = std::collections::BTreeMap::from([(
        "archive".to_string(),
        crate::models::PoolDefinition::default(),
    )]);
    let mut state = crate::gui::state::GuiState::new(
        crate::gui::settings::GuiSettings::default(),
        Default::default(),
        pools,
    );
    render(&mut state);
    state.mount.pool = "archive".into();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut capacity = crate::mount::capacity::CapacityStatus::default();
    capacity.eligible = vec!["a_crypt:".into()];
    capacity.additional_estimate = 3 << 30;
    capacity.observed_unix = now;
    state.mount.session.capacity = Some(capacity.clone());
    render(&mut state);
    assert!(
        state.mount.session.capacity.is_some(),
        "rendering keeps the measurement"
    );
    capacity.eligible.clear();
    capacity.excluded.push(crate::mount::capacity::Excluded {
        remote: "drime_1_crypt:".into(),
        reason: "Quota query: cancelled".into(),
        temporary: true,
    });
    state.mount.session.capacity = Some(capacity);
    render(&mut state);
    for tab in [
        crate::gui::state::DriveTab::Options,
        crate::gui::state::DriveTab::Import,
        crate::gui::state::DriveTab::Maintenance,
    ] {
        state.mount.tab = tab;
        render(&mut state);
    }
}

#[test]
fn rclone_import_action_reaches_the_cli_without_mounting() {
    use clap::Parser;
    let control = tempfile::tempdir().unwrap();
    let mut form = super::MountForm {
        pool: "p".into(),
        ..Default::default()
    };
    form.workspace = control.path().join("ws").display().to_string();
    assert!(
        form.action_args(9, control.path()).is_err(),
        "source required"
    );
    form.import_source = "old-crypt:photos".into();
    form.import_destination = "/Imported/old/".into();
    form.import_batch_gib = 2;
    form.import_rename = true;
    let args = form.action_args(9, control.path()).unwrap();
    let parsed =
        crate::cli::Cli::try_parse_from(std::iter::once(OsString::from("rpool")).chain(args))
            .unwrap();
    let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
        panic!("mount")
    };
    assert_eq!(args.import_from.as_deref(), Some("old-crypt:photos"));
    assert_eq!(args.import_to.as_deref(), Some("Imported/old"));
    assert_eq!(args.import_batch_gib, 2);
    assert_eq!(
        args.import_conflict,
        crate::mount::rclone_import::OnConflict::Rename
    );
    assert!(args.mountpoint.is_none() && !args.sync_only);
}
#[test]
fn rclone_import_ignores_listed_archive_imports() {
    let control = tempfile::tempdir().unwrap();
    let mut form = super::MountForm {
        pool: "p".into(),
        ..Default::default()
    };
    form.workspace = control.path().join("ws").display().to_string();
    form.import_source = "old:x".into();
    form.manifests = vec!["archive.json".into()];
    let args = form.action_args(9, control.path()).unwrap();
    assert!(!args
        .iter()
        .any(|a| a.to_string_lossy().contains("archive.json")));
    use clap::Parser;
    assert!(
        crate::cli::Cli::try_parse_from(std::iter::once(OsString::from("rpool")).chain(args))
            .is_ok()
    );
}

/// Every argv the Drive screen builds (each action, with and without the
/// optional inputs) parses with the real CLI and never carries a removed
/// mode flag.
#[test]
fn every_drive_action_argv_parses_with_the_real_cli() {
    use clap::Parser;
    let control = tempfile::tempdir().unwrap();
    let parse = |args: Vec<OsString>| {
        let line = format!("{args:?}");
        let cli =
            crate::cli::Cli::try_parse_from(std::iter::once(OsString::from("rpool")).chain(args))
                .unwrap_or_else(|e| panic!("{line}: {e}"));
        let Some(crate::cli::Commands::Mount(args)) = cli.command else {
            panic!("mount expected: {line}")
        };
        args
    };
    let plain = super::MountForm {
        pool: "p".into(),
        workspace: control.path().join("ws").display().to_string(),
        mountpoint: "/mnt/rpool".into(),
        import_source: "old-crypt:photos".into(),
        ..Default::default()
    };
    let full = super::MountForm {
        pc_name: "desktop 한 글".into(),
        manifests: vec!["archive.json".into()],
        import_destination: "/Imported/".into(),
        import_rename: true,
        native_read_only: true,
        frontend: crate::cli::Frontend::Dav,
        recovery_reprocess_plan: control.path().join("plan.json").display().to_string(),
        ..plain_clone(&plain)
    };
    for form in [&plain, &full] {
        for action in [0, 1, 2, 4, 5, 6, 9] {
            if action == 6 && !form.manifests.is_empty() {
                assert!(form.action_args(action, control.path()).is_err());
                continue;
            }
            let args = form.action_args(action, control.path()).unwrap();
            for arg in &args {
                let arg = arg.to_string_lossy();
                for removed in [
                    "--virtual-drive",
                    "--pool-sync",
                    "--pool-retention",
                    "--pool-history-limit",
                    "--diagnostic-read-only",
                    "--bounded-shared",
                    "--shared-",
                    "--worker-name",
                    "--retention-report",
                    "--apply-retention",
                    "--exclusive-archive-ownership",
                    "--keep-previous",
                    "--migrate-excluded",
                ] {
                    assert!(!arg.starts_with(removed), "action {action}: {arg}");
                }
            }
            let parsed = parse(args);
            assert_eq!(parsed.pool, "p");
            assert_eq!(parsed.mountpoint.is_some(), action == 0, "action {action}");
            assert_eq!(parsed.sync_only, action == 1, "action {action}");
            assert_eq!(parsed.capacity_only, action == 2);
            assert_eq!(parsed.cleanup_cache, action == 4);
            assert_eq!(parsed.recover_spool, action == 5);
            assert_eq!(parsed.apply_pool_changes, action == 6);
            assert_eq!(parsed.import_from.is_some(), action == 9);
            assert_eq!(
                parsed.pool_worker.as_deref(),
                (!form.pc_name.is_empty()).then_some(form.pc_name.as_str())
            );
            let manifests = if action == 9 { 0 } else { form.manifests.len() };
            assert_eq!(parsed.manifests.len(), manifests, "action {action}");
            assert_eq!((parsed.cache_gib, parsed.spool_gib), (10, 64));
        }
        let recovery = super::MountForm {
            recovery_source: control.path().join("old").display().to_string(),
            ..plain_clone(form)
        };
        let parsed = parse(
            recovery
                .recovery_args(&control.path().join("stop"))
                .unwrap(),
        );
        assert!(parsed.account_recovery_from.is_some() && parsed.mountpoint.is_none());
        assert_eq!(
            parsed.pool_worker.as_deref(),
            (!form.pc_name.is_empty()).then_some(form.pc_name.as_str())
        );
    }
    assert!(super::MountForm {
        frontend: crate::cli::Frontend::Dav,
        ..plain_clone(&plain)
    }
    .action_args(7, control.path())
    .is_err());
}

/// The persisted inputs of `form` (sessions are not cloneable).
fn plain_clone(form: &super::MountForm) -> super::MountForm {
    super::MountForm {
        pool: form.pool.clone(),
        workspace: form.workspace.clone(),
        mountpoint: form.mountpoint.clone(),
        pc_name: form.pc_name.clone(),
        manifests: form.manifests.clone(),
        import_source: form.import_source.clone(),
        import_destination: form.import_destination.clone(),
        import_rename: form.import_rename,
        frontend: form.frontend,
        native_read_only: form.native_read_only,
        recovery_reprocess_plan: form.recovery_reprocess_plan.clone(),
        ..Default::default()
    }
}

/// Every `tr(`/`trf(` literal on the Drive screens has a Korean, Japanese and
/// Chinese translation, including templates that rustfmt moves to the next
/// line (the crate-wide scan only sees a literal right after the parenthesis).
#[test]
fn drive_texts_translate() {
    use crate::gui::i18n::{tr_in, Language};
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui/screens/storage/mount");
    let mut keys = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == "tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        // Spelled with concat! so this file is not itself matched by the scans.
        let (tr, trf) = (concat!("tr", "("), concat!("trf", "("));
        let calls = text.match_indices(tr).chain(text.match_indices(trf));
        for (start, marker) in calls {
            if text[..start]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let rest = text[start + marker.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('"') else {
                continue;
            };
            let mut literal = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => literal.extend(chars.next()),
                    c => literal.push(c),
                }
            }
            let literal: &'static str = literal.leak();
            keys.push(literal);
        }
    }
    assert!(keys.len() > 200, "{}", keys.len());
    let untranslated: Vec<_> = keys
        .iter()
        .filter(|key| {
            [Language::Korean, Language::Japanese, Language::Chinese]
                .iter()
                .any(|language| tr_in(*language, key) == **key)
        })
        .collect();
    assert!(untranslated.is_empty(), "{untranslated:#?}");
    assert_eq!(tr_in(Language::Korean, "Mount"), "마운트");
    assert_eq!(tr_in(Language::Japanese, "PC name"), "PC 名");
    assert_eq!(tr_in(Language::Chinese, "Capacity details"), "容量详情");
}
