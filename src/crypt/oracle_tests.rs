//! Two-way compatibility against the installed rclone on local directories only
//! (no cloud access). Run with `cargo test --bin rpool -- --ignored crypt::oracle`.
//! `RPOOL_TEST_RCLONE` overrides the rclone binary.
use super::cipher::Cipher;
use super::data;
use super::obscure::{obscure, reveal};
use super::options::CryptConfig;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Oracle {
    temp: tempfile::TempDir,
    rclone: std::ffi::OsString,
}

/// (remote name, extra options, obscure the password with rclone instead of RPool)
type RemoteSpec = (&'static str, &'static [(&'static str, &'static str)], bool);

const REMOTES: &[RemoteSpec] = &[
    ("std", &[("password2", "salt-for-std")], true),
    (
        "b64",
        &[
            ("filename_encoding", "base64"),
            ("directory_name_encryption", "false"),
        ],
        false,
    ),
    (
        "obf",
        &[
            ("filename_encryption", "obfuscate"),
            ("password2", "obf salt"),
        ],
        true,
    ),
    ("off", &[("filename_encryption", "off")], false),
];

const NAMES: &[&str] = &[
    "a",
    "file.txt",
    "exactly16bytes!!",
    "폴더/파일 with spaces.iso",
    "dir/sub/!quoted!/Zz09 ÿ\u{1F600}",
    "deep/a/b/c/d/e/shard-000123.rpool",
];

impl Oracle {
    fn new() -> Self {
        let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/rclone".into()
            } else {
                "rclone".into()
            }
        });
        let oracle = Self {
            temp: tempfile::tempdir().unwrap(),
            rclone,
        };
        let mut conf = String::new();
        for (name, options, rclone_obscures) in REMOTES {
            let base = oracle.base(name);
            std::fs::create_dir_all(&base).unwrap();
            let obscured = |plain: &str| {
                if *rclone_obscures {
                    oracle.run(&["obscure", plain]).trim().to_string()
                } else {
                    obscure(plain).unwrap()
                }
            };
            conf += &format!(
                "[{name}]\ntype = crypt\nremote = {}\npassword = {}\n",
                base.display(),
                obscured(&format!("password of {name}"))
            );
            for (key, value) in *options {
                let value = if *key == "password2" {
                    obscured(value)
                } else {
                    value.to_string()
                };
                conf += &format!("{key} = {value}\n");
            }
            conf += "\n";
        }
        std::fs::write(oracle.config(), conf).unwrap();
        oracle
    }

    fn config(&self) -> PathBuf {
        self.temp.path().join("rclone.conf")
    }

    fn base(&self, remote: &str) -> PathBuf {
        self.temp.path().join("base").join(remote)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.rclone);
        command
            .args(args)
            .arg("--config")
            .arg(self.config())
            .arg("--cache-dir")
            .arg(self.temp.path().join("cache"))
            .env_clear()
            .env("HOME", self.temp.path());
        command
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "rclone {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn run_bytes(&self, args: &[&str]) -> Vec<u8> {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "rclone {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn cipher(&self, remote: &str) -> Cipher {
        let dump: BTreeMap<String, BTreeMap<String, String>> =
            serde_json::from_str(&self.run(&["config", "dump"])).unwrap();
        Cipher::new(&CryptConfig::from_section(&dump[remote]).unwrap()).unwrap()
    }

    /// Paths of all stored objects under a remote's base, relative and `/`-joined.
    fn stored(&self, remote: &str) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    let rel = path.strip_prefix(root).unwrap();
                    out.push(
                        rel.components()
                            .map(|c| c.as_os_str().to_str().unwrap())
                            .collect::<Vec<_>>()
                            .join("/"),
                    );
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.base(remote), &self.base(remote), &mut out);
        out.sort();
        out
    }
}

fn sample(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
#[ignore = "requires rclone"]
fn oracle_obscure_is_interchangeable_with_rclone() {
    let oracle = Oracle::new();
    for plain in ["x", "pa55 wörd", "with spaces and = signs"] {
        let ours = obscure(plain).unwrap();
        assert_eq!(oracle.run(&["reveal", &ours]).trim_end(), plain);
        let theirs = oracle.run(&["obscure", plain]);
        assert_eq!(reveal(theirs.trim()).unwrap().as_bytes(), plain.as_bytes());
    }
}

#[test]
#[ignore = "requires rclone"]
fn oracle_name_encryption_matches_rclone_exactly() {
    let oracle = Oracle::new();
    for (remote, _, _) in REMOTES {
        let cipher = oracle.cipher(remote);
        let mut args = vec!["cryptdecode", "--reverse"];
        let target = format!("{remote}:");
        args.push(&target);
        args.extend(NAMES);
        let output = oracle.run(&args);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), NAMES.len(), "{output}");
        for (name, line) in NAMES.iter().zip(lines) {
            let (_, expected) = line.split_once(" \t ").unwrap();
            assert_eq!(
                cipher.encrypt_file_name(name).unwrap(),
                expected,
                "{remote} {name}"
            );
            assert_eq!(
                cipher.decrypt_file_name(expected).unwrap(),
                *name,
                "{remote}"
            );
        }
    }
}

#[test]
#[ignore = "requires rclone"]
fn oracle_objects_written_by_rclone_decrypt_in_rpool() {
    let oracle = Oracle::new();
    let sizes = [0usize, 1, 65536, 3 * 65536 + 17];
    let source = oracle.temp.path().join("plain");
    for (remote, _, _) in REMOTES {
        let cipher = oracle.cipher(remote);
        let mut expected = BTreeMap::new();
        for (i, size) in sizes.iter().enumerate() {
            let plain = sample(*size, i as u8);
            std::fs::write(&source, &plain).unwrap();
            let name = format!("dir/sub/file {i}.bin");
            oracle.run(&[
                "copyto",
                source.to_str().unwrap(),
                &format!("{remote}:{name}"),
            ]);
            expected.insert(name, plain);
        }
        let stored = oracle.stored(remote);
        assert_eq!(stored.len(), sizes.len(), "{stored:?}");
        for object in stored {
            let name = cipher.decrypt_file_name(&object).unwrap();
            let bytes = std::fs::read(oracle.base(remote).join(&object)).unwrap();
            let plain = &expected[&name];
            assert_eq!(bytes.len() as u64, data::encrypted_size(plain.len() as u64));
            assert_eq!(
                &cipher.decrypt_bytes(&bytes).unwrap(),
                plain,
                "{remote} {name}"
            );
            let nonce = data::parse_header(&bytes).unwrap();
            for offset in [1u64, 65535, 65536, 65537, 2 * 65536 + 3] {
                if offset as usize >= plain.len() {
                    continue;
                }
                let start = data::range_start(offset);
                let mut got = Vec::new();
                cipher
                    .decrypter_at(&bytes[start.encrypted_offset as usize..], nonce, start)
                    .take(1000)
                    .read_to_end(&mut got)
                    .unwrap();
                let end = (offset as usize + 1000).min(plain.len());
                assert_eq!(got, plain[offset as usize..end]);
            }
        }
    }
}

#[test]
#[ignore = "requires rclone"]
fn oracle_objects_written_by_rpool_read_back_through_rclone() {
    let oracle = Oracle::new();
    for (remote, _, _) in REMOTES {
        let cipher = oracle.cipher(remote);
        let mut listed = Vec::new();
        for (i, size) in [0usize, 5, 65536, 2 * 65536 + 1].into_iter().enumerate() {
            let name = format!("rpool/written/object {i}.shard");
            let plain = sample(size, 100 + i as u8);
            let object = cipher.encrypt_file_name(&name).unwrap();
            let path = oracle.base(remote).join(&object);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut encrypted = Vec::new();
            cipher
                .encrypter(plain.as_slice())
                .unwrap()
                .read_to_end(&mut encrypted)
                .unwrap();
            std::fs::write(&path, encrypted).unwrap();
            assert_eq!(
                oracle.run_bytes(&["cat", &format!("{remote}:{name}")]),
                plain,
                "{remote} {name}"
            );
            listed.push(name);
        }
        let output = oracle.run(&["lsf", "-R", "--files-only", &format!("{remote}:")]);
        let mut got: Vec<&str> = output.lines().collect();
        got.sort();
        listed.sort();
        assert_eq!(got, listed, "{remote}");
        let directory = cipher.encrypt_dir_name("rpool/written").unwrap();
        assert!(oracle.base(remote).join(directory).is_dir(), "{remote}");
    }
}
