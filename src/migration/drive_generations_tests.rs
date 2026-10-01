use super::*;
use crate::migration::drive_model::{DriveAdoption, DriveFreeze};

fn generation(epoch: Option<&str>, v7: bool, newest: i128) -> Generation {
    Generation {
        epoch: epoch.map(str::to_owned),
        v7,
        newest,
        records: 1,
    }
}

fn adoption(id: &str, source: GenerationRef, ts: u64) -> DriveAdoption {
    DriveAdoption {
        version: 1,
        migration_id: id.into(),
        source,
        epoch: epoch_for(id),
        v7: false,
        files: 3,
        dropped: vec![],
        pc_id: "pc-a".into(),
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
fn adopted_generation_wins_even_when_the_old_one_has_newer_records() {
    let new = epoch_for("m1");
    let listed = vec![
        // A PC still on the original generation wrote after the adoption.
        generation(None, false, 300),
        generation(Some(&new), false, 200),
        generation(Some("other"), true, 100),
    ];
    let mut known = Known::default();
    known.migration_ids.insert("m1".into());
    // Before the marker the migration epoch is a partial publication.
    let before = effective(listed.clone(), &known);
    assert_eq!(
        before.iter().map(|g| g.epoch.clone()).collect::<Vec<_>>(),
        vec![None, Some("other".to_string())]
    );
    known.adoptions.push(adoption("m1", original(), 50));
    let after = effective(listed, &known);
    assert_eq!(after.len(), 2, "the superseded original is dropped");
    assert_eq!(after[0].epoch.as_deref(), Some(new.as_str()));
    assert_eq!(after[1].epoch.as_deref(), Some("other"));
}

#[test]
fn unrelated_transition_epochs_are_untouched() {
    let listed = vec![
        generation(Some("apply-pool-changes"), false, 20),
        generation(None, false, 10),
    ];
    let mut known = Known::default();
    known.migration_ids.insert("m9".into());
    assert_eq!(effective(listed.clone(), &known), listed);
}

#[test]
fn fresh_workspace_follows_the_last_adoption_of_its_protocol() {
    let mut known = Known::default();
    assert!(fresh_workspace(&known, false).is_none());
    known.adoptions.push(adoption("m1", original(), 10));
    let first = known.adoptions[0].generation();
    known.adoptions.push(adoption("m2", first, 20));
    assert_eq!(fresh_workspace(&known, false).unwrap().migration_id, "m2");
    assert!(fresh_workspace(&known, true).is_none());
}

#[test]
fn fence_refuses_superseded_and_freezes_the_source() {
    let mut known = Known::default();
    assert_eq!(fence(&original(), &known), Fence::Proceed);
    assert!(Fence::Proceed.message("p").is_none());
    known.freezes.push(DriveFreeze {
        version: 1,
        migration_id: "m1".into(),
        source: original(),
        epoch: epoch_for("m1"),
        pc_id: "pc-a".into(),
        ts_unix: 5,
    });
    let frozen = fence(&original(), &known);
    assert!(matches!(frozen, Fence::Frozen(_)));
    assert!(frozen.message("p").unwrap().contains("not published"));
    known.freezes.clear();
    known.adoptions.push(adoption("m1", original(), 10));
    let refused = fence(&original(), &known);
    let message = refused.message("p").unwrap();
    assert!(matches!(refused, Fence::Superseded(_)));
    assert!(
        message.contains("pool migrate adopt p --id m1 --workspace"),
        "{message}"
    );
    // Another protocol's generation is not affected.
    let v7 = GenerationRef {
        epoch: None,
        v7: true,
    };
    assert_eq!(fence(&v7, &known), Fence::Proceed);
}
