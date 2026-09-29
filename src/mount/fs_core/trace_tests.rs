//! Randomized operation traces against a reference model of the core's
//! contract, including crashes (drop without release, then reopen). Every
//! step compares each path's size and bytes and every result kind; the end
//! commits in `sync` order and requires no conflict copies. Seeds are fixed.
use super::*;
use crate::mount::virtual_drive::{fixture, fixture_reopen};
use crate::prelude::*;

const PATHS: &[&str] = &["a", "b", "d/c"];
const SEEDS: u64 = 24;
const STEPS: usize = 60;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self) -> Vec<u8> {
        let len = 1 + self.below(6);
        (0..len).map(|_| b'a' + self.below(26) as u8).collect()
    }
}

struct File {
    content: Vec<u8>,
    acked: Option<Vec<u8>>,
    dirty: bool,
    linked: bool,
}
struct Open {
    id: HandleId,
    file: usize,
    write: bool,
    append: bool,
    snapshot: Option<Vec<u8>>,
}
#[derive(Default)]
struct Model {
    files: Vec<File>,
    paths: BTreeMap<String, usize>,
    handles: Vec<Open>,
}
impl Model {
    fn ack(&mut self, file: usize) {
        let f = &mut self.files[file];
        f.acked = Some(f.content.clone());
        f.dirty = false;
    }
    fn unlink(&mut self, path: &str) {
        if let Some(file) = self.paths.remove(path) {
            self.files[file].linked = false;
        }
    }
    fn visible(&self, open: &Open) -> Vec<u8> {
        open.snapshot
            .clone()
            .unwrap_or_else(|| self.files[open.file].content.clone())
    }
}

fn outcome<T>(result: &FsResult<T>) -> String {
    match result {
        Ok(_) => "Ok".into(),
        Err(FsError::Io(error)) => panic!("unexpected I/O error: {error:#}"),
        Err(error) => format!("{error:?}"),
    }
}
fn read(core: &FsCore, path: &str) -> Vec<u8> {
    let handle = core.open(path, Access::Read, false, false).unwrap();
    let bytes = core.read_at(handle, 0, 1 << 16).unwrap();
    core.release(handle).unwrap();
    bytes
}
fn check_paths(core: &FsCore, model: &Model, context: &str) {
    for path in PATHS {
        match model.paths.get(*path) {
            Some(&file) => {
                let expected = &model.files[file].content;
                let attr = core
                    .lookup(path)
                    .unwrap_or_else(|e| panic!("{context}: {path} {e}"));
                assert_eq!(attr.size, expected.len() as u64, "{context}: {path} size");
                assert_eq!(&read(core, path), expected, "{context}: {path} bytes");
            }
            None => assert!(
                matches!(core.lookup(path), Err(FsError::NotFound)),
                "{context}: {path} should not exist"
            ),
        }
    }
}

/// Applies one random operation; returns `true` for an abrupt exit.
fn step(core: &FsCore, model: &mut Model, rng: &mut Rng, context: &str) -> bool {
    let path = PATHS[rng.below(PATHS.len())].to_string();
    let pick = |rng: &mut Rng, model: &Model| {
        (!model.handles.is_empty()).then(|| rng.below(model.handles.len()))
    };
    match rng.below(20) {
        0..=2 => {
            let truncate = rng.below(3) == 0;
            let append = !truncate && rng.below(4) == 0;
            let handle = core
                .open(&path, Access::Write { truncate, append }, true, false)
                .unwrap_or_else(|e| panic!("{context}: open {path}: {e}"));
            let file = match model.paths.get(&path) {
                Some(&file) => {
                    if truncate {
                        model.files[file].content.clear();
                        model.files[file].dirty = true;
                    }
                    file
                }
                None => {
                    model.files.push(File {
                        content: vec![],
                        acked: None,
                        dirty: true,
                        linked: true,
                    });
                    model.paths.insert(path.clone(), model.files.len() - 1);
                    model.files.len() - 1
                }
            };
            model.handles.push(Open {
                id: handle,
                file,
                write: true,
                append,
                snapshot: None,
            });
        }
        3..=4 => {
            let result = core.open(&path, Access::Read, false, false);
            match model.paths.get(&path) {
                Some(&file) => {
                    let f = &model.files[file];
                    let snapshot = (!f.dirty).then(|| f.content.clone());
                    model.handles.push(Open {
                        id: result.unwrap_or_else(|e| panic!("{context}: read open: {e}")),
                        file,
                        write: false,
                        append: false,
                        snapshot,
                    });
                }
                None => assert_eq!(outcome(&result), "NotFound", "{context}"),
            }
        }
        5..=8 => {
            let Some(index) = pick(rng, model) else {
                return false;
            };
            let (offset, bytes) = (rng.below(12) as u64, rng.bytes());
            let open = &model.handles[index];
            let result = core.write_at(open.id, offset, &bytes);
            let f = &model.files[open.file];
            let expected = if !open.write {
                "ReadOnly"
            } else if !f.linked && !f.dirty {
                "Stale"
            } else {
                "Ok"
            };
            assert_eq!(outcome(&result), expected, "{context}: write");
            if expected == "Ok" {
                let append = open.append;
                let f = &mut model.files[open.file];
                let at = if append {
                    f.content.len()
                } else {
                    offset as usize
                };
                if f.content.len() < at + bytes.len() {
                    f.content.resize(at + bytes.len(), 0);
                }
                f.content[at..at + bytes.len()].copy_from_slice(&bytes);
                f.dirty = true;
            }
        }
        9 => {
            let Some(index) = pick(rng, model) else {
                return false;
            };
            let len = rng.below(12);
            let open = &model.handles[index];
            let result = core.truncate(open.id, len as u64);
            let f = &model.files[open.file];
            let expected = if !open.write {
                "ReadOnly"
            } else if !f.linked && !f.dirty {
                "Stale"
            } else {
                "Ok"
            };
            assert_eq!(outcome(&result), expected, "{context}: truncate");
            if expected == "Ok" {
                let f = &mut model.files[open.file];
                f.content.resize(len, 0);
                f.dirty = true;
            }
        }
        10..=11 => {
            let Some(index) = pick(rng, model) else {
                return false;
            };
            let (offset, count) = (rng.below(12), 1 + rng.below(16));
            let open = &model.handles[index];
            let actual = core.read_at(open.id, offset as u64, count).unwrap();
            let visible = model.visible(open);
            let expected = visible
                .get(offset.min(visible.len())..(offset + count).min(visible.len()))
                .unwrap_or_default();
            assert_eq!(actual, expected, "{context}: read_at");
        }
        12..=13 => {
            let Some(index) = pick(rng, model) else {
                return false;
            };
            let open = &model.handles[index];
            let result = core.fsync(open.id);
            let file = open.file;
            let f = &model.files[file];
            let expected = if f.dirty && !f.linked { "Stale" } else { "Ok" };
            assert_eq!(outcome(&result), expected, "{context}: fsync");
            if f.dirty && f.linked {
                model.ack(file);
            }
        }
        14..=15 => {
            let Some(index) = pick(rng, model) else {
                return false;
            };
            let open = model.handles.remove(index);
            core.release(open.id)
                .unwrap_or_else(|e| panic!("{context}: release: {e}"));
            let writers = model
                .handles
                .iter()
                .filter(|h| h.file == open.file && h.write)
                .count();
            if open.write && writers == 0 && model.files[open.file].dirty {
                if model.files[open.file].linked {
                    model.ack(open.file);
                } else {
                    let f = &mut model.files[open.file];
                    f.content = f.acked.clone().unwrap_or_default();
                    f.dirty = false;
                }
            }
        }
        16..=17 => {
            let to = PATHS[rng.below(PATHS.len())].to_string();
            let result = core.rename(&path, &to);
            let Some(&file) = model.paths.get(&path) else {
                assert_eq!(outcome(&result), "NotFound", "{context}: rename");
                return false;
            };
            assert_eq!(outcome(&result), "Ok", "{context}: rename {path} -> {to}");
            if path != to {
                if model.files[file].dirty {
                    model.ack(file);
                }
                model.unlink(&to);
                model.paths.remove(&path);
                model.paths.insert(to, file);
            }
        }
        18 => {
            let result = core.delete(&path);
            if model.paths.contains_key(&path) {
                assert_eq!(outcome(&result), "Ok", "{context}: delete");
                model.unlink(&path);
            } else {
                assert_eq!(outcome(&result), "NotFound", "{context}: delete");
            }
        }
        _ => {
            // Abrupt exit: no release, no fsync. Only acknowledged data survives.
            model.handles.clear();
            let linked: Vec<(String, usize)> =
                model.paths.iter().map(|(p, f)| (p.clone(), *f)).collect();
            for (path, file) in linked {
                match model.files[file].acked.clone() {
                    Some(acked) => {
                        model.files[file].content = acked;
                        model.files[file].dirty = false;
                    }
                    None => model.unlink(&path),
                }
            }
            return true;
        }
    }
    false
}

/// Releases every handle in open order, updating the model like `release`.
fn release_all(core: &FsCore, model: &mut Model) {
    while !model.handles.is_empty() {
        let open = model.handles.remove(0);
        core.release(open.id).unwrap();
        let writers = model
            .handles
            .iter()
            .filter(|h| h.file == open.file && h.write)
            .count();
        let f = &mut model.files[open.file];
        if open.write && writers == 0 && f.dirty {
            if f.linked {
                f.acked = Some(f.content.clone());
            } else {
                f.content = f.acked.clone().unwrap_or_default();
            }
            f.dirty = false;
        }
    }
}

#[test]
fn randomized_traces_match_the_reference_model_through_crashes() {
    let mut total_crashes = 0;
    for seed in 0..SEEDS {
        let root = tempfile::tempdir().unwrap();
        let mut core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
        let mut crashes = 0;
        let mut model = Model::default();
        let mut rng = Rng(seed);
        for index in 0..STEPS {
            let context = format!("seed {seed} step {index}");
            if step(&core, &mut model, &mut rng, &context) {
                drop(core);
                core = FsCore::new(Arc::new(fixture_reopen(root.path()))).unwrap();
                crashes += 1;
            }
            check_paths(&core, &model, &context);
        }
        release_all(&core, &mut model);
        check_paths(&core, &model, &format!("seed {seed} final"));
        super::tests::commit_all(&core.drive);
        let view = core.drive.view().unwrap();
        let expected: Vec<&String> = model.paths.keys().collect();
        assert_eq!(
            view.keys().collect::<Vec<_>>(),
            expected,
            "seed {seed}: conflict copies"
        );
        for (path, file) in &model.paths {
            assert_eq!(view[path].size(), model.files[*file].content.len() as u64);
        }
        total_crashes += crashes;
    }
    assert!(total_crashes > SEEDS as usize / 2, "traces rarely crashed");
}
