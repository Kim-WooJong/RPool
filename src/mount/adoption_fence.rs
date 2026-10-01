//! Mount-side guard of drive adoption (pool change migration, phase 3).
//!
//! - Opening a NEW pool-sync workspace of a pool whose drive was adopted
//!   initializes it on the adopted epoch, so a PC without the old workspace
//!   opens the migrated drive directly.
//! - Opening a workspace on a superseded generation is refused with the
//!   command that switches it (`pool migrate adopt --workspace`).
//! - While a migration has frozen the workspace's generation, a mount may run
//!   but every publication (`VirtualDrive::sync`) is refused: changes stay in
//!   the local spool. Publication re-checks at most every [`CHECK_INTERVAL`];
//!   adoption waits longer than that after the freeze
//!   (`drive_adopt::SETTLE_SECONDS`), so nothing is published to the source
//!   generation after it was read for adoption.
use super::virtual_drive::VirtualDrive;
use crate::migration::drive_generations::{fence, fresh_workspace, Fence, Known};
use crate::migration::drive_model::GenerationRef;
use crate::prelude::*;
use std::time::{Duration, Instant};

/// Longest time a publishing mount relies on its last fence check.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(120);

/// The drive generation a workspace is bound to (None: no binding yet, or
/// not a pool-sync workspace).
pub(crate) fn workspace_generation(root: &Path) -> Result<Option<GenerationRef>> {
    let path = root.join("virtual.json");
    if !path.exists() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Binding {
        version: u32,
        #[serde(default)]
        epoch: Option<String>,
    }
    let binding: Binding = crate::utils::read_json(&path)?;
    Ok(match binding.version {
        6 | 7 => Some(GenerationRef {
            epoch: binding.epoch,
            v7: binding.version == 7,
        }),
        _ => None,
    })
}

/// Before a pool-sync mount opens `workspace`: `Some(epoch)` when a new
/// workspace must be initialized on an adopted epoch; an error for a
/// workspace on a superseded generation.
pub(crate) fn before_open(
    rclone: &str,
    pool: &str,
    workspace: &Path,
    v7: bool,
) -> Result<Option<String>> {
    let known = crate::migration::drive_journal::known(rclone, pool)
        .context("cannot check this pool's migrations before mounting; local data is retained")?;
    open_decision(workspace_generation(workspace)?, v7, &known, pool)
}

pub(crate) fn open_decision(
    generation: Option<GenerationRef>,
    v7: bool,
    known: &Known,
    pool: &str,
) -> Result<Option<String>> {
    let Some(generation) = generation else {
        return Ok(fresh_workspace(known, v7).map(|adoption| {
            println!(
                "Drive adopted by pool migration {}: this new workspace opens the migrated drive (epoch {})",
                adoption.migration_id,
                &adoption.epoch[..12]
            );
            adoption.epoch.clone()
        }));
    };
    let decision = fence(&generation, known);
    match (&decision, decision.message(pool)) {
        (Fence::Superseded(_), Some(message)) => bail!("{message}"),
        (Fence::Frozen(_), Some(message)) => eprintln!("[warning] {message}"),
        _ => {}
    }
    Ok(None)
}

/// "Apply pool changes" from a workspace whose generation a pool migration
/// froze or replaced would fork the drive into a private epoch: refused.
pub(crate) fn check_transition_source(rclone: &str, pool: &str, workspace: &Path) -> Result<()> {
    let Some(generation) = workspace_generation(workspace)? else {
        return Ok(());
    };
    let known = crate::migration::drive_journal::known(rclone, pool)
        .context("cannot check this pool's migrations before applying pool changes")?;
    match fence(&generation, &known).message(pool) {
        Some(message) => bail!("apply pool changes refused: {message}"),
        None => Ok(()),
    }
}

/// Last fence check per workspace root: when, and the refusal (if any).
type Checks = BTreeMap<PathBuf, (Instant, Option<String>)>;
fn checks() -> &'static Mutex<Checks> {
    static CHECKS: std::sync::OnceLock<Mutex<Checks>> = std::sync::OnceLock::new();
    CHECKS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Refuses publication from a workspace whose generation is frozen or
/// superseded. Drives without a pool-sync binding are not affected.
pub(crate) fn check_publish(drive: &VirtualDrive) -> Result<()> {
    if drive.pool_sync_roots.is_empty() {
        return Ok(());
    }
    let Some(generation) = workspace_generation(&drive.root)? else {
        return Ok(());
    };
    check_cached(&drive.root, Instant::now(), || {
        let known = crate::migration::drive_journal::known(&drive.rclone, &drive.pool)?;
        Ok(fence(&generation, &known).message(&drive.pool))
    })
}

/// [`check_publish`] over an injectable check, cached for [`CHECK_INTERVAL`].
pub(crate) fn check_cached(
    root: &Path,
    now: Instant,
    check: impl FnOnce() -> Result<Option<String>>,
) -> Result<()> {
    let cached = checks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .filter(|(at, _)| now.saturating_duration_since(*at) < CHECK_INTERVAL)
        .map(|(_, refusal)| refusal.clone());
    let refusal = match cached {
        Some(refusal) => refusal,
        None => {
            let refusal = check().context(
                "cannot check this pool's migrations before publishing; changes stay local and are retried",
            )?;
            checks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(root.to_owned(), (now, refusal.clone()));
            refusal
        }
    };
    match refusal {
        Some(message) => bail!("not published: {message}"),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::drive_model::{DriveAdoption, DriveFreeze};

    fn adoption(source: GenerationRef, epoch: &str, ts: u64) -> DriveAdoption {
        DriveAdoption {
            version: 1,
            migration_id: format!("m-{epoch}"),
            source,
            epoch: epoch.repeat(64),
            v7: false,
            files: 1,
            dropped: vec![],
            pc_id: "pc".into(),
            ts_unix: ts,
        }
    }
    fn original() -> GenerationRef {
        GenerationRef {
            epoch: None,
            v7: false,
        }
    }

    #[test]
    fn fresh_workspace_opens_the_adopted_epoch_and_old_one_is_refused() {
        let mut known = Known::default();
        assert_eq!(open_decision(None, false, &known, "p").unwrap(), None);
        known.adoptions.push(adoption(original(), "a", 10));
        assert_eq!(
            open_decision(None, false, &known, "p").unwrap(),
            Some("a".repeat(64))
        );
        // v7 workspaces are not redirected by a v6 adoption.
        assert_eq!(open_decision(None, true, &known, "p").unwrap(), None);
        let error = open_decision(Some(original()), false, &known, "p")
            .unwrap_err()
            .to_string();
        assert!(error.contains("pool migrate adopt p --id m-a"), "{error}");
        // The adopted generation itself mounts normally.
        let adopted = GenerationRef {
            epoch: Some("a".repeat(64)),
            v7: false,
        };
        assert_eq!(
            open_decision(Some(adopted), false, &known, "p").unwrap(),
            None
        );
    }

    #[test]
    fn chained_adoptions_open_the_newest_and_frozen_workspaces_still_mount() {
        let mut known = Known::default();
        known.adoptions.push(adoption(original(), "a", 10));
        let a = GenerationRef {
            epoch: Some("a".repeat(64)),
            v7: false,
        };
        known.adoptions.push(adoption(a.clone(), "b", 20));
        assert_eq!(
            open_decision(None, false, &known, "p").unwrap(),
            Some("b".repeat(64))
        );
        assert!(open_decision(Some(a.clone()), false, &known, "p").is_err());
        let b = GenerationRef {
            epoch: Some("b".repeat(64)),
            v7: false,
        };
        known.freezes.push(DriveFreeze {
            version: 1,
            migration_id: "m3".into(),
            source: b.clone(),
            epoch: "c".repeat(64),
            pc_id: "pc".into(),
            ts_unix: 30,
        });
        assert_eq!(open_decision(Some(b), false, &known, "p").unwrap(), None);
    }

    #[test]
    fn publication_check_is_cached_and_refusals_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        let start = Instant::now();
        let calls = std::cell::Cell::new(0);
        let ok = || {
            calls.set(calls.get() + 1);
            Ok(None)
        };
        check_cached(&root, start, ok).unwrap();
        check_cached(&root, start + Duration::from_secs(5), || {
            calls.set(calls.get() + 1);
            Ok(Some("frozen".into()))
        })
        .unwrap();
        assert_eq!(
            calls.get(),
            1,
            "within the interval the last answer is reused"
        );
        let later = start + CHECK_INTERVAL + Duration::from_secs(1);
        let error = check_cached(&root, later, || Ok(Some("frozen by m1".into()))).unwrap_err();
        assert!(error.to_string().contains("frozen by m1"));
        // Still refused from the cache, and a failing check also refuses.
        assert!(check_cached(&root, later, || Ok(None)).is_err());
        let other = dir.path().join("other");
        assert!(check_cached(&other, later, || bail!("offline")).is_err());
    }

    #[test]
    fn binding_generation_is_read_from_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(workspace_generation(dir.path()).unwrap(), None);
        fs::write(
            dir.path().join("virtual.json"),
            format!(r#"{{"version":7,"pool":"p","epoch":"{}"}}"#, "e".repeat(64)),
        )
        .unwrap();
        assert_eq!(
            workspace_generation(dir.path()).unwrap(),
            Some(GenerationRef {
                epoch: Some("e".repeat(64)),
                v7: true
            })
        );
        fs::write(dir.path().join("virtual.json"), r#"{"version":1}"#).unwrap();
        assert_eq!(workspace_generation(dir.path()).unwrap(), None);
    }
}
