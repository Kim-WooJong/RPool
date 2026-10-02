//! Executes planned [`Action`]s on a drive. Every action is a normal drive
//! write (new revision) or delete (to the trash); the drive's own sync
//! publishes them. Paths with local writes not yet uploaded are refused,
//! so history never races an unsynced edit.
use super::graph::{History, Payload};
use super::restore::Action;
use crate::mount::history_bridge::{self as bridge, VirtualDrive};
use crate::prelude::*;

/// What actions need from a drive (a fake in tests).
pub(crate) trait Target {
    /// Paths with local writes not yet uploaded.
    fn pending(&self) -> BTreeSet<String>;
    /// Write `payload` as a new revision of `path`.
    fn put(&self, path: &str, payload: &Payload) -> Result<()>;
    /// Delete `path` (it moves to the drive trash).
    fn delete(&self, path: &str) -> Result<()>;
    /// Uploads/publishes queued changes.
    fn publish(&self) -> Result<()>;
}

impl Target for VirtualDrive {
    fn pending(&self) -> BTreeSet<String> {
        bridge::pending_paths(self)
    }
    fn put(&self, path: &str, payload: &Payload) -> Result<()> {
        self.history_base_on_visible(path)?;
        self.history_put_content(path, payload.clone()).map(|_| ())
    }
    fn delete(&self, path: &str) -> Result<()> {
        self.history_base_on_visible(path)?;
        VirtualDrive::delete(self, path)
    }
    fn publish(&self) -> Result<()> {
        self.sync()
    }
}

/// Outcome of applying actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Applied {
    /// Actions that were carried out, in order.
    pub changes: Vec<Action>,
    /// All changes reached the cloud (else they are queued in the workspace).
    pub published: bool,
    /// Degraded-information notes, e.g. publish deferred to the next sync.
    pub notes: Vec<String>,
}

/// Carry out `actions` on `target`: refuse if any path has unsynced local
/// writes or a payload is missing, apply in order, then publish. A failed
/// publish is reported as a note (changes stay queued). Called by
/// `ops::write` for restores and rollbacks.
pub(crate) fn apply(target: &dyn Target, history: &History, actions: &[Action]) -> Result<Applied> {
    if actions.is_empty() {
        return Ok(Applied {
            changes: vec![],
            published: true,
            notes: vec!["nothing to change".into()],
        });
    }
    let pending = target.pending();
    if let Some(busy) = actions
        .iter()
        .find(|a| pending.iter().any(|p| super::graph::collides(p, a.path())))
    {
        bail!(
            "{} has local changes not yet uploaded; let the drive sync first",
            super::graph::display_path(busy.path())
        );
    }
    // Resolve every payload before changing anything.
    for action in actions {
        if let Action::Put { rev, .. } = action {
            if !history.payloads.contains_key(rev) {
                bail!("the data of revision {rev} is no longer available");
            }
        }
    }
    let mut done = Vec::new();
    for action in actions {
        let result = match action {
            Action::Put { path, rev, .. } => target.put(path, &history.payloads[rev]),
            Action::Delete { path, .. } => target.delete(path),
        };
        if let Err(error) = result {
            // Earlier changes stay queued (they are ordinary new revisions).
            let mut message = format!("{error:#}");
            if !done.is_empty() {
                message.push_str(&format!(
                    "; {} earlier change(s) were made and will be published by the next sync",
                    done.len()
                ));
            }
            bail!(
                "could not apply {}: {message}",
                super::graph::display_path(action.path())
            );
        }
        done.push(action.clone());
    }
    let mut notes = Vec::new();
    let published = match target.publish() {
        Ok(()) => true,
        Err(error) => {
            notes.push(format!(
                "changes are saved in the workspace and publish with the next sync ({error:#})"
            ));
            false
        }
    };
    Ok(Applied {
        changes: done,
        published,
        notes,
    })
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    pub(crate) struct Drive {
        pub log: RefCell<Vec<String>>,
        pub pending: BTreeSet<String>,
        pub offline: bool,
    }
    impl Target for Drive {
        fn pending(&self) -> BTreeSet<String> {
            self.pending.clone()
        }
        fn put(&self, path: &str, _: &Payload) -> Result<()> {
            self.log.borrow_mut().push(format!("put {path}"));
            Ok(())
        }
        fn delete(&self, path: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("delete {path}"));
            Ok(())
        }
        fn publish(&self) -> Result<()> {
            if self.offline {
                bail!("offline");
            }
            self.log.borrow_mut().push("publish".into());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev};
    use super::*;

    #[test]
    fn applies_in_order_publishes_and_refuses_pending_or_missing_data() {
        let mut h = history(vec![("a", rev("a.txt", &[], Some("x"), "pc", Some(1)))]);
        let put = Action::Put {
            path: "a.txt".into(),
            rev: h.id("a"),
            from: None,
        };
        let drive = fake::Drive::default();
        let id = h.id("a");
        let payload = h.payloads.remove(&id).unwrap();
        assert!(apply(&drive, &h, std::slice::from_ref(&put)).is_err()); // no payload
        h.payloads.insert(id, payload);
        let delete = Action::Delete {
            path: "b".into(),
            rev: "b".into(),
        };
        let applied = apply(&drive, &h, &[delete.clone(), put.clone()]).unwrap();
        assert!(applied.published);
        assert_eq!(*drive.log.borrow(), ["delete b", "put a.txt", "publish"]);
        let offline = fake::Drive {
            offline: true,
            ..Default::default()
        };
        let applied = apply(&offline, &h, std::slice::from_ref(&put)).unwrap();
        assert!(!applied.published && !applied.notes.is_empty());
        let busy = fake::Drive {
            pending: ["A.TXT".to_string()].into(),
            ..Default::default()
        };
        assert!(apply(&busy, &h, &[put]).is_err());
        assert!(busy.log.borrow().is_empty());
    }
}
