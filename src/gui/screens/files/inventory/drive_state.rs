//! State of the Library's pool drive browser: the selected pool, the
//! background listing per pool, and a folder tree built from the flat,
//! read-only listing returned by `crate::pool::browse::browse`.

use crate::pool::browse::PoolBrowse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
        tree
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

    /// Rows of the tree as `(node, depth)`: only children of expanded
    /// folders are visited, so a collapsed large drive stays cheap.
    pub(crate) fn visible(&self, expanded: &BTreeSet<String>) -> Vec<(usize, usize)> {
        let mut rows = Vec::new();
        let mut stack: Vec<(usize, usize)> = self.roots.iter().rev().map(|&id| (id, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            rows.push((id, depth));
            let node = &self.nodes[id];
            if node.is_dir && expanded.contains(&node.path) {
                stack.extend(node.children.iter().rev().map(|&child| (child, depth + 1)));
            }
        }
        rows
    }

    /// Entries whose path contains `query` (case-insensitive), by path.
    pub(crate) fn search(&self, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let mut found: Vec<usize> = (0..self.nodes.len())
            .filter(|&id| self.nodes[id].lower.contains(&query))
            .collect();
        found.sort_by(|a, b| self.nodes[*a].path.cmp(&self.nodes[*b].path));
        found
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

#[derive(Debug, Default)]
pub(crate) struct DriveForm {
    /// The pool being browsed; empty until one is picked (no "All pools").
    pub(crate) pool: String,
    pub(crate) query: String,
    pub(crate) expanded: BTreeSet<String>,
    results: BTreeMap<String, DriveLoad>,
    /// Listing runs off the UI thread: reading cloud metadata takes seconds.
    pending: Option<(String, Receiver<BrowseResult>)>,
}

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
    }

    pub(crate) fn select(&mut self, pool: String) {
        if pool != self.pool {
            self.pool = pool;
            self.expanded.clear();
        }
    }

    pub(crate) fn current(&self) -> Option<&DriveLoad> {
        self.results.get(&self.pool)
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(pool, _)| *pool == self.pool)
    }

    pub(crate) fn needs_load(&self) -> bool {
        !self.pool.is_empty() && !self.results.contains_key(&self.pool) && !self.is_loading()
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
            Err(TryRecvError::Disconnected) => {
                Err("Listing stopped unexpectedly; refresh to try again.".to_string())
            }
        };
        let pool = pool.clone();
        self.pending = None;
        self.apply(pool, result);
        false
    }

    pub(crate) fn apply(&mut self, pool: String, result: BrowseResult) {
        let load = match result {
            Ok(browse) => DriveLoad::Ready(DriveTree::build(browse)),
            Err(error) => DriveLoad::Failed(error),
        };
        self.results.insert(pool, load);
    }

    pub(crate) fn toggle(&mut self, path: &str) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
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
            mode: "v7".into(),
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

    fn names(tree: &DriveTree, rows: &[(usize, usize)]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|&(id, depth)| (tree.nodes[id].name.clone(), depth))
            .collect()
    }

    #[test]
    fn tree_nests_sorts_and_totals() {
        let tree = DriveTree::build(sample());
        assert_eq!(tree.mode, "v7");
        assert_eq!((tree.files, tree.dirs, tree.bytes), (5, 4, 45));
        let collapsed = tree.visible(&BTreeSet::new());
        assert_eq!(
            names(&tree, &collapsed),
            vec![
                ("docs".to_string(), 0),
                ("Photos".to_string(), 0),
                ("readme.md".to_string(), 0)
            ]
        );
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
    fn only_expanded_folders_show_children() {
        let tree = DriveTree::build(sample());
        let expanded = BTreeSet::from(["Photos".to_string(), "Photos/2024".to_string()]);
        assert_eq!(
            names(&tree, &tree.visible(&expanded)),
            vec![
                ("docs".to_string(), 0),
                ("Photos".to_string(), 0),
                ("2024".to_string(), 1),
                ("A.jpg".to_string(), 2),
                ("b.jpg".to_string(), 2),
                ("cover.png".to_string(), 1),
                ("readme.md".to_string(), 0),
            ]
        );
        // A collapsed parent hides an expanded child.
        let expanded = BTreeSet::from(["Photos/2024".to_string()]);
        assert_eq!(tree.visible(&expanded).len(), 3);
    }

    #[test]
    fn search_is_case_insensitive_and_flat() {
        let tree = DriveTree::build(sample());
        let found: Vec<_> = tree
            .search(" JPG ")
            .into_iter()
            .map(|id| tree.nodes[id].path.clone())
            .collect();
        assert_eq!(found, vec!["Photos/2024/A.jpg", "Photos/2024/b.jpg"]);
        let found: Vec<_> = tree
            .search("notes")
            .into_iter()
            .map(|id| tree.nodes[id].path.clone())
            .collect();
        assert_eq!(found, vec!["docs/notes", "docs/notes/todo.txt"]);
        assert!(tree.search("missing").is_empty());
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
        form.toggle("Photos");
        assert!(form.expanded.contains("Photos"));
        form.select("b".into());
        assert!(form.expanded.is_empty());
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
