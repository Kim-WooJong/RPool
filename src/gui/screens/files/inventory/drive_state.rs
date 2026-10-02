//! State of the Library's pool drive browser: the selected pool, the
//! background listing per pool, and a folder tree built from the flat,
//! read-only listing returned by `crate::pool::browse::browse`.

use super::explorer::ExplorerState;
use super::history::HistoryForm;
use crate::pool::browse::PoolBrowse;
use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{Receiver, TryRecvError};

pub(crate) type BrowseResult = Result<PoolBrowse, String>;

/// One file or folder of the drive tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriveNode {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    /// File size, or the total size of every file below a folder.
    pub(crate) size: u64,
    /// Files below a folder (recursively); 0 for files.
    pub(crate) files: usize,
    pub(crate) children: Vec<usize>,
    parent: Option<usize>,
    lower: String,
}

/// Folder tree of one pool's drive.
#[derive(Debug, Clone, Default)]
pub(crate) struct DriveTree {
    pub(crate) mode: String,
    pub(crate) notes: Vec<String>,
    pub(crate) nodes: Vec<DriveNode>,
    pub(crate) roots: Vec<usize>,
    pub(crate) files: usize,
    pub(crate) dirs: usize,
    pub(crate) bytes: u64,
    /// Node id by normalized path, for jumping to a folder by its path.
    index: HashMap<String, usize>,
}

impl DriveTree {
    /// Builds the tree. Tolerates unsorted input, leading/trailing or
    /// doubled slashes and folders that are only implied by a file path.
    pub(crate) fn build(browse: PoolBrowse) -> Self {
        let mut tree = Self {
            mode: browse.mode,
            notes: browse.notes,
            ..Default::default()
        };
        let mut index = HashMap::new();
        for entry in browse.entries {
            let path = normalize(&entry.path);
            if path.is_empty() {
                continue;
            }
            if entry.is_dir {
                tree.ensure_dir(&mut index, &path);
            } else if let Some(&existing) = index.get(&path) {
                // A duplicate: keep a folder a folder, otherwise take the size.
                let node: &mut DriveNode = &mut tree.nodes[existing];
                if !node.is_dir {
                    node.size = entry.size;
                }
            } else {
                let parent = parent_of(&path).map(|parent| tree.ensure_dir(&mut index, parent));
                tree.push(&mut index, path, false, entry.size, parent);
            }
        }
        tree.finish();
        tree.index = index;
        tree
    }

    /// The node at a normalized path ("" is the drive root, not a node).
    pub(crate) fn find(&self, path: &str) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// The folder at `path`: `Some(None)` for the root, `Some(Some(id))` for
    /// a folder, `None` when the path is missing or names a file.
    pub(crate) fn folder(&self, path: &str) -> Option<Option<usize>> {
        if path.is_empty() {
            return Some(None);
        }
        self.find(path)
            .filter(|&id| self.nodes[id].is_dir)
            .map(Some)
    }

    /// Sorted entries of a folder (`None` is the root).
    pub(crate) fn children(&self, folder: Option<usize>) -> &[usize] {
        match folder {
            Some(id) => &self.nodes[id].children,
            None => &self.roots,
        }
    }

    fn ensure_dir(&mut self, index: &mut HashMap<String, usize>, path: &str) -> usize {
        if let Some(&existing) = index.get(path) {
            self.nodes[existing].is_dir = true;
            return existing;
        }
        let parent = parent_of(path).map(|parent| self.ensure_dir(index, parent));
        self.push(index, path.to_string(), true, 0, parent)
    }

    fn push(
        &mut self,
        index: &mut HashMap<String, usize>,
        path: String,
        is_dir: bool,
        size: u64,
        parent: Option<usize>,
    ) -> usize {
        let id = self.nodes.len();
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        self.nodes.push(DriveNode {
            lower: path.to_lowercase(),
            name,
            path: path.clone(),
            is_dir,
            size,
            files: 0,
            children: Vec::new(),
            parent,
        });
        index.insert(path, id);
        match parent {
            Some(parent) => self.nodes[parent].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    fn finish(&mut self) {
        for id in 0..self.nodes.len() {
            if self.nodes[id].is_dir {
                self.dirs += 1;
                continue;
            }
            let size = self.nodes[id].size;
            self.files += 1;
            self.bytes = self.bytes.saturating_add(size);
            let mut parent = self.nodes[id].parent;
            while let Some(dir) = parent {
                let node = &mut self.nodes[dir];
                node.size = node.size.saturating_add(size);
                node.files += 1;
                parent = node.parent;
            }
        }
        let nodes = &self.nodes;
        let order = |left: &usize, right: &usize| {
            let (a, b) = (&nodes[*left], &nodes[*right]);
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        };
        let mut roots = std::mem::take(&mut self.roots);
        roots.sort_by(order);
        let mut children: Vec<Vec<usize>> = self
            .nodes
            .iter()
            .map(|node| {
                let mut children = node.children.clone();
                children.sort_by(order);
                children
            })
            .collect();
        self.roots = roots;
        for (node, sorted) in self.nodes.iter_mut().zip(children.iter_mut()) {
            node.children = std::mem::take(sorted);
        }
    }
}

impl DriveNode {
    /// The lowercased name, for case-insensitive search.
    pub(crate) fn lower_name(&self) -> &str {
        self.lower.rsplit('/').next().unwrap_or(&self.lower)
    }

    /// Path of the containing folder ("" for top-level entries).
    pub(crate) fn folder_path(&self) -> &str {
        parent_of(&self.path).unwrap_or("")
    }
}

fn normalize(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn parent_of(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

#[derive(Debug, Clone)]
pub(crate) enum DriveLoad {
    Ready(DriveTree),
    Failed(String),
}

/// [`DriveForm::parts`].
pub(crate) struct DriveParts<'a> {
    pub(crate) pool: &'a str,
    pub(crate) load: Option<&'a DriveLoad>,
    pub(crate) explorer: &'a mut ExplorerState,
    pub(crate) query: &'a mut String,
    pub(crate) history: &'a mut HistoryForm,
}

#[derive(Debug, Default)]
pub(crate) struct DriveForm {
    /// The pool being browsed; empty until one is picked (no "All pools").
    pub(crate) pool: String,
    pub(crate) query: String,
    /// Current folder, history, sort, view and selection of the explorer.
    pub(crate) explorer: ExplorerState,
    /// Trash, versions and rollback (per pool, in memory).
    pub(crate) history: HistoryForm,
    results: BTreeMap<String, DriveLoad>,
    /// Listing runs off the UI thread: reading cloud metadata takes seconds.
    pending: Option<(String, Receiver<BrowseResult>)>,
    /// When each pool's listing arrived and whether it came from a drive
    /// mounted on this PC (cheap to re-read, so it is kept current).
    loaded: BTreeMap<String, (std::time::Instant, bool)>,
}

/// How often the listing of a pool mounted on this PC is re-read.
pub(crate) const MOUNTED_REFRESH: std::time::Duration = std::time::Duration::from_secs(3);

impl DriveForm {
    /// Keeps the selection valid for the current pools: preselects the only
    /// pool, clears a removed one, and forgets listings of removed pools.
    pub(crate) fn sync_pools(&mut self, pools: &[String]) {
        self.results.retain(|pool, _| pools.contains(pool));
        if !pools.contains(&self.pool) {
            let only = if pools.len() == 1 {
                pools[0].clone()
            } else {
                String::new()
            };
            self.select(only);
        }
        self.history.sync(pools, &self.pool);
    }

    pub(crate) fn select(&mut self, pool: String) {
        if pool != self.pool {
            self.pool = pool;
            self.query.clear();
            self.explorer = ExplorerState::default();
        }
    }

    pub(crate) fn current(&self) -> Option<&DriveLoad> {
        self.results.get(&self.pool)
    }

    /// The pool, its listing, the explorer, the search and the history
    /// views, borrowed apart.
    pub(crate) fn parts(&mut self) -> DriveParts<'_> {
        DriveParts {
            pool: &self.pool,
            load: self.results.get(&self.pool),
            explorer: &mut self.explorer,
            query: &mut self.query,
            history: &mut self.history,
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(pool, _)| *pool == self.pool)
    }

    pub(crate) fn needs_load(&self) -> bool {
        !self.pool.is_empty() && !self.results.contains_key(&self.pool) && !self.is_loading()
    }

    /// A listing read from this PC's mounted drive that is older than
    /// [`MOUNTED_REFRESH`]: re-read so new saves show up without a refresh.
    pub(crate) fn needs_mounted_refresh(&self) -> bool {
        !self.is_loading()
            && self
                .loaded
                .get(&self.pool)
                .is_some_and(|(at, mounted)| *mounted && at.elapsed() >= MOUNTED_REFRESH)
    }

    /// Lists the selected pool on a background thread, replacing any
    /// listing still running for another pool.
    pub(crate) fn start(&mut self, rclone: &str) {
        if self.pool.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (rclone, pool) = (rclone.to_string(), self.pool.clone());
        std::thread::spawn(move || {
            let result =
                crate::pool::browse::browse(&rclone, &pool).map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.pending = Some((self.pool.clone(), rx));
    }

    /// Collects a finished listing. Returns true while one is still running.
    pub(crate) fn poll(&mut self) -> bool {
        let Some((pool, rx)) = &self.pending else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => Err(crate::gui::i18n::tr(
                "Listing stopped unexpectedly; refresh to try again.",
            )
            .to_string()),
        };
        let pool = pool.clone();
        self.pending = None;
        self.apply(pool, result);
        false
    }

    pub(crate) fn apply(&mut self, pool: String, result: BrowseResult) {
        let mounted = result.as_ref().is_ok_and(|b| b.mode == "v6-mounted");
        self.loaded
            .insert(pool.clone(), (std::time::Instant::now(), mounted));
        let load = match result {
            Ok(browse) => DriveLoad::Ready(DriveTree::build(browse)),
            Err(error) => DriveLoad::Failed(error),
        };
        self.results.insert(pool, load);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::browse::BrowseEntry;

    fn entry(path: &str, size: u64, is_dir: bool) -> BrowseEntry {
        BrowseEntry {
            path: path.into(),
            size,
            is_dir,
        }
    }

    fn sample() -> PoolBrowse {
        PoolBrowse {
            pool: "family".into(),
            mode: "v6".into(),
            entries: vec![
                entry("Photos", 0, true),
                entry("Photos/2024", 0, true),
                entry("Photos/2024/b.jpg", 20, false),
                entry("Photos/2024/A.jpg", 10, false),
                entry("Photos/cover.png", 5, false),
                entry("docs/notes/todo.txt", 7, false),
                entry("readme.md", 3, false),
            ],
            notes: vec!["conflict copy".into()],
        }
    }

    #[test]
    fn tree_nests_sorts_and_totals() {
        let tree = DriveTree::build(sample());
        assert_eq!(tree.mode, "v6");
        assert_eq!((tree.files, tree.dirs, tree.bytes), (5, 4, 45));
        let roots: Vec<_> = tree
            .children(None)
            .iter()
            .map(|&id| tree.nodes[id].name.as_str())
            .collect();
        assert_eq!(roots, ["docs", "Photos", "readme.md"]);
        let photos = tree.nodes.iter().find(|n| n.path == "Photos").unwrap();
        assert_eq!(
            (photos.size, photos.files, photos.children.len()),
            (35, 3, 2)
        );
        // The implied "docs/notes" folder exists and is a folder.
        let notes = tree.nodes.iter().find(|n| n.path == "docs/notes").unwrap();
        assert!(notes.is_dir);
        assert_eq!((notes.size, notes.files), (7, 1));
    }

    #[test]
    fn paths_resolve_to_folders_and_files() {
        let tree = DriveTree::build(sample());
        assert_eq!(tree.folder(""), Some(None));
        let photos = tree.folder("Photos").unwrap().unwrap();
        assert_eq!(tree.children(Some(photos)).len(), 2);
        assert_eq!(tree.folder("readme.md"), None, "a file is not a folder");
        assert_eq!(tree.folder("missing"), None);
        let file = tree.find("Photos/2024/A.jpg").unwrap();
        assert_eq!(tree.nodes[file].lower_name(), "a.jpg");
        assert_eq!(tree.nodes[file].folder_path(), "Photos/2024");
        assert_eq!(
            tree.nodes[tree.find("readme.md").unwrap()].folder_path(),
            ""
        );
    }

    #[test]
    fn odd_paths_and_duplicates_are_tolerated() {
        let tree = DriveTree::build(PoolBrowse {
            mode: "v6".into(),
            entries: vec![
                entry("/a//b.txt", 4, false),
                entry("a/", 0, true),
                entry("a/b.txt", 9, false),
                entry("", 0, true),
            ],
            ..Default::default()
        });
        assert_eq!((tree.files, tree.dirs, tree.bytes), (1, 1, 9));
        assert_eq!(tree.nodes[tree.roots[0]].path, "a");
    }

    #[test]
    fn preselects_the_only_pool_and_requires_a_pick_otherwise() {
        let mut form = DriveForm::default();
        form.sync_pools(&["family".to_string()]);
        assert_eq!(form.pool, "family");
        assert!(form.needs_load());

        let mut form = DriveForm::default();
        form.sync_pools(&["a".to_string(), "b".to_string()]);
        assert!(form.pool.is_empty());
        assert!(!form.needs_load());
        form.select("b".into());
        form.sync_pools(&["a".to_string(), "b".to_string()]);
        assert_eq!(form.pool, "b");
        form.sync_pools(&["a".to_string()]);
        assert_eq!(form.pool, "a");
        form.sync_pools(&[]);
        assert!(form.pool.is_empty());
    }

    #[test]
    fn results_are_cached_per_pool() {
        let mut form = DriveForm::default();
        form.sync_pools(&["a".to_string(), "b".to_string()]);
        form.select("a".into());
        form.apply("a".into(), Ok(sample()));
        form.apply("b".into(), Err("offline".into()));
        assert!(!form.needs_load());
        assert!(matches!(form.current(), Some(DriveLoad::Ready(tree)) if tree.files == 5));
        form.explorer.nav.open("Photos");
        form.query = "jpg".into();
        form.select("b".into());
        assert_eq!(form.explorer.nav.current(), "");
        assert!(form.query.is_empty());
        assert!(matches!(form.current(), Some(DriveLoad::Failed(e)) if e == "offline"));
        form.sync_pools(&["a".to_string()]);
        assert_eq!(form.pool, "a");
        form.select("b".into());
        assert!(form.current().is_none());
    }

    #[test]
    fn background_listing_arrives() {
        let mut form = DriveForm::default();
        form.sync_pools(&["family".to_string()]);
        form.start("rclone-that-is-not-called");
        assert!(form.is_loading());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while form.poll() {
            assert!(
                std::time::Instant::now() < deadline,
                "listing never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!form.is_loading());
        assert!(form.current().is_some());
    }
}
