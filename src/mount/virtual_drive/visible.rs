//! Cached visible namespace. Browsing (lookup, readdir, stat, statfs) used to
//! re-project every namespace event on each call, under the drive lock that
//! the uploader and seals also need. The committed projection is now built
//! once per event set and the pending overlay once per namespace revision
//! (`state_lock`); lookups are then `O(log N)` and a listing reads only the
//! listed directory. Results are identical to the full `view`.
use super::state_lock::StateGuard;
use super::*;
use std::ops::Bound;

/// The committed files of one event set.
pub(crate) struct Projection {
    version: u32,
    /// Every event id with its content size (0 for a deletion). Also the
    /// cache key: event ids are content hashes, so the same ids and
    /// version project to the same files.
    sizes: BTreeMap<String, u64>,
    files: BTreeMap<String, Revision>,
    /// Sum of `files` sizes.
    committed: u128,
    /// Sum of every event's content size (old revisions stay stored).
    events_total: u128,
}

impl Projection {
    pub(super) fn build(version: u32, events: &BTreeMap<String, Event>) -> Result<Self> {
        let resolved = super::super::namespace::resolve_events(version, events)?;
        Ok(Self::from_resolved(version, events, &resolved))
    }
    /// The projection of `state`, reusing the namespace's cached projection
    /// of the same events when one exists.
    pub(super) fn of_state(state: &Namespace) -> Result<Self> {
        Ok(Self::from_resolved(
            state.version,
            &state.events,
            &*state.projected()?,
        ))
    }
    fn from_resolved(
        version: u32,
        events: &BTreeMap<String, Event>,
        resolved: &BTreeMap<String, crate::mount::shared_model::Resolved>,
    ) -> Self {
        let mut files = BTreeMap::new();
        let mut committed = 0u128;
        for (path, resolved) in resolved {
            if let Some(content) = &resolved.event.content {
                committed += u128::from(content.size);
                files.insert(
                    path.clone(),
                    Revision::Cloud {
                        id: resolved.event_id.clone(),
                        content: content.clone(),
                    },
                );
            }
        }
        let sizes: BTreeMap<String, u64> = events
            .iter()
            .map(|(id, e)| (id.clone(), e.content.as_ref().map_or(0, |c| c.size)))
            .collect();
        let events_total = sizes.values().map(|s| u128::from(*s)).sum();
        Self {
            version,
            sizes,
            files,
            committed,
            events_total,
        }
    }
    /// Whether this projection is the one of `state`'s events.
    pub(super) fn matches(&self, state: &Namespace) -> bool {
        self.version == state.version
            && self.sizes.len() == state.events.len()
            && self.sizes.keys().eq(state.events.keys())
    }
    /// The committed (projected) revision of `path`.
    pub(super) fn committed(&self, path: &str) -> Option<&Revision> {
        self.files.get(path)
    }
    /// Bytes of the committed revisions not listed in `known`.
    pub(super) fn uncounted(&self, known: &[String]) -> u64 {
        let size = |id: &String| self.sizes.get(id).map_or(0, |s| u128::from(*s));
        let counted: u128 = if known.windows(2).all(|w| w[0] < w[1]) {
            known.iter().map(size).sum()
        } else {
            known
                .iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(size)
                .sum()
        };
        u64::try_from(self.events_total - counted).unwrap_or(u64::MAX)
    }
}

struct Spooled {
    id: String,
    path: PathBuf,
    size: u64,
}

/// The pending overlay of one namespace revision.
pub(crate) struct Pending {
    /// The last pending intent of each path: a spooled save or a deletion.
    last: BTreeMap<String, Option<Spooled>>,
    /// Every spooled intent `(id, size)`, for capacity accounting.
    spooled: Vec<(String, u64)>,
    directories: crate::mount::namespace::Shared<BTreeSet<String>>,
    generation: u64,
    /// `Namespace::visible_logical_used`, or `None` when it overflows.
    pub(super) used: Option<u64>,
}

impl Pending {
    fn build(drive: &VirtualDrive, state: &Namespace, projection: &Projection) -> Self {
        let mut last = BTreeMap::new();
        let mut spooled = vec![];
        for intent in &state.pending {
            let entry = intent.spool.as_ref().map(|_| {
                spooled.push((intent.id.clone(), intent.size));
                Spooled {
                    id: intent.id.clone(),
                    path: drive.spool_path(intent),
                    size: intent.size,
                }
            });
            last.insert(intent.path.clone(), entry);
        }
        let mut used = projection.committed;
        for (path, entry) in &last {
            if let Some(revision) = projection.files.get(path) {
                used -= u128::from(revision.size());
            }
            if let Some(entry) = entry {
                used += u128::from(entry.size);
            }
        }
        Self {
            last,
            spooled,
            directories: state.directories.clone(),
            generation: state.generation,
            used: u64::try_from(used).ok(),
        }
    }
    /// Bytes of spooled intents not listed in `known`.
    pub(super) fn uncounted(&self, known: &[String]) -> u64 {
        if self.spooled.is_empty() {
            return 0;
        }
        let known: BTreeSet<&str> = known.iter().map(String::as_str).collect();
        self.spooled
            .iter()
            .filter(|(id, _)| !known.contains(id.as_str()))
            .fold(0u64, |sum, (_, size)| sum.saturating_add(*size))
    }
}

/// A consistent snapshot of the visible namespace for metadata queries:
/// files with their revision id and size, explicit directories, generation.
pub(crate) struct VisibleView {
    projection: Arc<Projection>,
    pending: Arc<Pending>,
    /// The full `view` when session pins are in effect (local-only fixture);
    /// it then replaces `projection` and `pending.last`.
    pinned: Option<BTreeMap<String, Revision>>,
}

impl VisibleView {
    /// The visible file at `path`: `(revision or intent id, size)`.
    pub(crate) fn file(&self, path: &str) -> Option<(&str, u64)> {
        if let Some(pinned) = &self.pinned {
            return pinned.get(path).map(|r| (r.id(), r.size()));
        }
        match self.pending.last.get(path) {
            Some(Some(entry)) => Some((entry.id.as_str(), entry.size)),
            Some(None) => None,
            None => self.projection.files.get(path).map(|r| (r.id(), r.size())),
        }
    }
    /// Whether a visible file lies below `prefix` (a directory path plus `/`).
    pub(crate) fn has_descendant(&self, prefix: &str) -> bool {
        if let Some(pinned) = &self.pinned {
            return below(pinned, prefix).next().is_some();
        }
        below(&self.pending.last, prefix).any(|(_, entry)| entry.is_some())
            || below(&self.projection.files, prefix)
                .any(|(path, _)| !self.pending.last.contains_key(path))
    }
    /// The visible children of the directory `prefix` (empty for the root,
    /// else a path plus `/`): a file's `(id, size)`, or `None` for a
    /// subdirectory implied by a file below it. As in a full listing, a
    /// subdirectory wins over a same-named file. Explicit (empty)
    /// directories are separate, see [`Self::directories`].
    pub(crate) fn children(&self, prefix: &str) -> BTreeMap<String, Option<(&str, u64)>> {
        fn add<'a>(
            out: &mut BTreeMap<String, Option<(&'a str, u64)>>,
            relative: &str,
            file: (&'a str, u64),
        ) {
            match relative.split_once('/') {
                Some((child, _)) => {
                    out.insert(child.to_owned(), None);
                }
                None => {
                    out.entry(relative.to_owned()).or_insert(Some(file));
                }
            }
        }
        let mut out = BTreeMap::new();
        if let Some(pinned) = &self.pinned {
            for (path, r) in below(pinned, prefix) {
                add(&mut out, &path[prefix.len()..], (r.id(), r.size()));
            }
            return out;
        }
        // Committed files, skipping each found subdirectory's other files:
        // all paths below `<prefix><child>/` sort before `<prefix><child>0`.
        let mut from = prefix.to_owned();
        'walk: loop {
            for (path, r) in below_from(&self.projection.files, prefix, &from) {
                if self.pending.last.contains_key(path.as_str()) {
                    continue;
                }
                let relative = &path[prefix.len()..];
                if let Some((child, _)) = relative.split_once('/') {
                    out.insert(child.to_owned(), None);
                    from = format!("{prefix}{child}0");
                    continue 'walk;
                }
                add(&mut out, relative, (r.id(), r.size()));
            }
            break;
        }
        for (path, entry) in below(&self.pending.last, prefix) {
            if let Some(entry) = entry {
                add(&mut out, &path[prefix.len()..], (&entry.id, entry.size));
            }
        }
        out
    }
    pub(crate) fn directories(&self) -> &BTreeSet<String> {
        &self.pending.directories
    }
    /// The namespace generation the snapshot was taken at.
    pub(crate) fn generation(&self) -> u64 {
        self.pending.generation
    }
}

fn below<'m, 'p, V>(
    map: &'m BTreeMap<String, V>,
    prefix: &'p str,
) -> impl Iterator<Item = (&'m String, &'m V)> + use<'m, 'p, V> {
    below_from(map, prefix, prefix)
}
fn below_from<'m, 'p, V>(
    map: &'m BTreeMap<String, V>,
    prefix: &'p str,
    from: &str,
) -> impl Iterator<Item = (&'m String, &'m V)> + use<'m, 'p, V> {
    map.range::<str, _>((Bound::Included(from), Bound::Unbounded))
        .take_while(move |(path, _)| path.starts_with(prefix))
}

impl VirtualDrive {
    /// Locks the namespace with an up-to-date projection and overlay. A
    /// missing projection is built from a copy of the events with the lock
    /// released, so the uploader and seals are not held up by it.
    pub(super) fn locked_visible(&self) -> Result<(StateGuard<'_>, Arc<Projection>, Arc<Pending>)> {
        let lock = || {
            self.state
                .lock()
                .map_err(|_| anyhow!("namespace lock poisoned"))
        };
        let mut s = lock()?;
        if let Some((projection, pending)) = s.current() {
            return Ok((s, projection, pending));
        }
        if !s.projection().is_some_and(|p| p.matches(&s)) && !s.has_projected() {
            // The event map is shared, so this copy is O(1).
            let (version, events) = (s.version, s.events.clone());
            drop(s);
            let built = Projection::build(version, &events);
            s = lock()?;
            if let Ok(built) = built {
                if built.matches(&s) {
                    s.set_projection(Arc::new(built));
                }
            }
        }
        let (projection, pending) = self.cached_visible(&mut s)?;
        Ok((s, projection, pending))
    }
    /// [`Self::locked_visible`] for a caller already holding the lock: a
    /// missing projection is built under it.
    pub(super) fn cached_visible(
        &self,
        s: &mut StateGuard<'_>,
    ) -> Result<(Arc<Projection>, Arc<Pending>)> {
        if let Some(current) = s.current() {
            return Ok(current);
        }
        let projection = match s.projection() {
            Some(p) if p.matches(s) => p.clone(),
            _ => {
                let built = Arc::new(Projection::of_state(s)?);
                s.set_projection(built.clone());
                built
            }
        };
        let pending = Arc::new(Pending::build(self, s, &projection));
        s.set_current(projection.clone(), pending.clone());
        Ok((projection, pending))
    }
    /// Whether session pins change what is visible (the local-only fixture:
    /// pool-sync workspaces never overlay pins).
    fn pins_apply(&self) -> bool {
        self.pool_sync_roots.is_empty() && !self.pins.lock().unwrap().is_empty()
    }
    /// The full visible map: committed files, session pins (local-only),
    /// then pending intents in order. Called with the namespace locked, so
    /// local revision leases are taken before a commit can release a spool.
    pub(super) fn full_view(
        &self,
        s: &Namespace,
        projection: &Projection,
    ) -> Result<BTreeMap<String, Revision>> {
        let mut result = projection.files.clone();
        // A DAV transport cannot observe native rclone handle closure. Keep
        // served paths pinned for the mount session and expose incoming revisions
        // as named copies instead of mixing bytes across unconditioned ranges.
        for (name, pinned) in self
            .pins
            .lock()
            .unwrap()
            .iter()
            .filter(|_| self.pool_sync_roots.is_empty())
        {
            if let Some(current) = result.get(name).cloned() {
                if current.id() != pinned.id() {
                    let alternate = crate::mount::shared_model::conflict_path(
                        name,
                        "incoming",
                        current.id(),
                        0,
                    )?;
                    result.insert(alternate, current);
                    result.insert(name.clone(), pinned.clone());
                }
            } else {
                result.insert(name.clone(), pinned.clone());
            }
        }
        for intent in &s.pending {
            if intent.spool.is_some() {
                result.insert(
                    intent.path.clone(),
                    Revision::Local {
                        id: intent.id.clone(),
                        path: self.spool_path(intent),
                        size: intent.size,
                        _lease: self.local_lease(&intent.id),
                    },
                );
            } else {
                result.remove(&intent.path);
            }
        }
        Ok(result)
    }
    /// A metadata snapshot of the visible namespace (see [`VisibleView`]).
    pub(crate) fn visible(&self) -> Result<VisibleView> {
        let (s, projection, pending) = self.locked_visible()?;
        let pinned = if self.pins_apply() {
            Some(self.full_view(&s, &projection)?)
        } else {
            None
        };
        Ok(VisibleView {
            projection,
            pending,
            pinned,
        })
    }
    /// The visible revision of `path`, as `view()[path]` without building
    /// the whole map. A local revision's lease is taken under the lock.
    pub(crate) fn visible_revision(&self, path: &str) -> Result<Option<Revision>> {
        let (s, projection, pending) = self.locked_visible()?;
        if self.pins_apply() {
            return Ok(self.full_view(&s, &projection)?.remove(path));
        }
        Ok(match pending.last.get(path) {
            Some(Some(entry)) => Some(Revision::Local {
                id: entry.id.clone(),
                path: entry.path.clone(),
                size: entry.size,
                _lease: self.local_lease(&entry.id),
            }),
            Some(None) => None,
            None => projection.files.get(path).cloned(),
        })
    }
    /// `Namespace::visible_logical_used` from the cache.
    pub(crate) fn visible_used(&self) -> Result<u64> {
        let (_, _, pending) = self.locked_visible()?;
        pending.used.context("logical usage overflow")
    }
}
