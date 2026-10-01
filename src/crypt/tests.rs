use super::cipher::Cipher;
use super::data::{self, Nonce, BLOCK_DATA};
use super::encoding::NameEncoding;
use super::obscure::{obscure, reveal};
use super::options::{CryptConfig, NameMode};
use std::collections::BTreeMap;
use std::io::Read;

fn cipher(mode: NameMode, dirs: bool, encoding: NameEncoding) -> Cipher {
    let mut key = [0u8; 80];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = (i * 7 + 3) as u8;
    }
    Cipher::from_key(&key, mode, dirs, encoding, ".bin".into())
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

#[test]
fn nonce_increment_and_add_carry_like_rclone() {
    let mut n = Nonce([0xff; 24]);
    n.0[3] = 0x00;
    n.increment();
    assert_eq!(&n.0[..4], &[0, 0, 0, 1]);
    let mut n = Nonce([0; 24]);
    n.0[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    n.add(1);
    assert_eq!(&n.0[..9], &[0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let mut a = Nonce([0x5a; 24]);
    let mut b = a;
    a.add(300);
    for _ in 0..300 {
        b.increment();
    }
    assert_eq!(a, b);
}

#[test]
fn sizes_match_shard_size_helper_and_invert() {
    for plain in [0u64, 1, 65535, 65536, 65537, 64 << 20, 220 << 20] {
        let encrypted = data::encrypted_size(plain);
        assert_eq!(encrypted, crate::models::shard_size::crypt_size(plain));
        assert_eq!(data::decrypted_size(encrypted).unwrap(), plain);
    }
    assert!(data::decrypted_size(31).is_err());
    assert!(data::decrypted_size(32 + 16).is_err());
    assert_eq!(data::decrypted_size(32 + 17).unwrap(), 1);
}

#[test]
fn data_round_trips_and_ranged_reads_match() {
    let c = cipher(NameMode::Standard, true, NameEncoding::Base32);
    let nonce = Nonce([9; 24]);
    for len in [0usize, 1, 65535, 65536, 65537, 3 * 65536 + 17] {
        let plain = sample(len);
        let encrypted = c.encrypt_bytes(&plain, nonce);
        assert_eq!(encrypted.len() as u64, data::encrypted_size(len as u64));
        assert_eq!(&encrypted[..8], b"RCLONE\0\0");
        assert_eq!(c.decrypt_bytes(&encrypted).unwrap(), plain);
        let header_nonce = data::parse_header(&encrypted).unwrap();
        for offset in [
            0u64,
            1,
            BLOCK_DATA - 1,
            BLOCK_DATA,
            BLOCK_DATA + 5,
            len as u64,
        ] {
            if offset > len as u64 {
                continue;
            }
            let start = data::range_start(offset);
            let tail = &encrypted[(start.encrypted_offset as usize).min(encrypted.len())..];
            let mut got = Vec::new();
            c.decrypter_at(tail, header_nonce, start)
                .take(70_000)
                .read_to_end(&mut got)
                .unwrap();
            let end = (offset as usize + 70_000).min(len);
            assert_eq!(
                got,
                plain[offset as usize..end],
                "len {len} offset {offset}"
            );
        }
    }
}

#[test]
fn tampering_truncation_and_wrong_key_are_rejected() {
    let c = cipher(NameMode::Standard, true, NameEncoding::Base32);
    let encrypted = c.encrypt_bytes(&sample(70_000), Nonce([1; 24]));
    let mut flipped = encrypted.clone();
    flipped[100] ^= 1;
    assert!(c.decrypt_bytes(&flipped).is_err());
    assert!(c.decrypt_bytes(&encrypted[..encrypted.len() - 1]).is_err());
    assert!(c.decrypt_bytes(&encrypted[..32 + 10]).is_err());
    assert!(c.decrypt_bytes(&encrypted[..20]).is_err());
    let mut magic = encrypted.clone();
    magic[0] = b'X';
    assert!(c.decrypt_bytes(&magic).is_err());
    let other = Cipher::from_key(
        &[1; 80],
        NameMode::Standard,
        true,
        NameEncoding::Base32,
        String::new(),
    );
    assert!(other.decrypt_bytes(&encrypted).is_err());
    // Empty plaintext is a bare header.
    assert_eq!(c.encrypt_bytes(b"", Nonce([0; 24])).len(), 32);
}

#[test]
fn names_round_trip_in_every_mode() {
    let names = [
        "a",
        "file.txt",
        "exactly16bytes!!",
        "폴더/파일 with spaces.iso",
        "dir/sub/!quoted!/Zz09\u{A0}\u{FF}\u{1F600}\u{D7FF}",
        &"x".repeat(2047),
    ];
    for (mode, dirs, encoding) in [
        (NameMode::Standard, true, NameEncoding::Base32),
        (NameMode::Standard, false, NameEncoding::Base64),
        (NameMode::Standard, true, NameEncoding::Base32768),
        (NameMode::Obfuscate, true, NameEncoding::Base32),
        (NameMode::Obfuscate, false, NameEncoding::Base32),
        (NameMode::Off, true, NameEncoding::Base32),
    ] {
        let c = cipher(mode, dirs, encoding);
        for name in names {
            if mode == NameMode::Obfuscate && name.len() > 255 {
                continue;
            }
            let encrypted = c.encrypt_file_name(name).unwrap();
            if mode != NameMode::Off {
                assert_ne!(encrypted, name);
            }
            assert_eq!(c.decrypt_file_name(&encrypted).unwrap(), name, "{mode:?}");
            let dir = c.encrypt_dir_name(name).unwrap();
            assert_eq!(c.decrypt_dir_name(&dir).unwrap(), name);
        }
    }
    let c = cipher(NameMode::Standard, false, NameEncoding::Base32);
    let encrypted = c.encrypt_file_name("plain/dir/file").unwrap();
    assert!(encrypted.starts_with("plain/dir/"));
    assert!(c.encrypt_file_name(&"x".repeat(2048)).is_err());
    assert!(c.decrypt_file_name("not-base32!").is_err());
    assert!(cipher(NameMode::Off, true, NameEncoding::Base32)
        .decrypt_file_name("file.txt")
        .is_err());
}

fn section(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn config_refuses_what_it_cannot_reproduce() {
    let password = obscure("secret").unwrap();
    let base = [
        ("type", "crypt"),
        ("remote", "local:/x"),
        ("password", password.as_str()),
    ];
    let config = CryptConfig::from_section(&section(&base)).unwrap();
    assert_eq!(config.remote, "local:/x");
    assert_eq!(config.name_mode, NameMode::Standard);
    assert!(config.directory_name_encryption);
    assert_eq!(config.suffix.as_deref(), Some(".bin"));
    assert!(config.salt.is_empty());
    for extra in [
        ("no_data_encryption", "true"),
        ("pass_bad_blocks", "true"),
        ("filename_encoding", "base99"),
        ("filename_encryption", "weird"),
        ("unknown_option", "1"),
        ("directory_name_encryption", "maybe"),
    ] {
        let mut s = section(&base);
        s.insert(extra.0.into(), extra.1.into());
        let error = CryptConfig::from_section(&s).unwrap_err().to_string();
        assert!(!error.contains("secret"), "{error}");
    }
    assert!(CryptConfig::from_section(&section(&base[..2])).is_err());
    let mut s = section(&base);
    s.insert("type".into(), "local".into());
    assert!(CryptConfig::from_section(&s).is_err());
    let debug = format!("{config:?}");
    assert!(!debug.contains("secret"), "{debug}");
}

#[test]
fn obscure_round_trips_and_redacts() {
    let obscured = obscure("pa55 wörd").unwrap();
    assert_eq!(
        reveal(&obscured).unwrap().as_bytes(),
        "pa55 wörd".as_bytes()
    );
    assert_ne!(obscure("x").unwrap(), obscure("x").unwrap());
    assert!(reveal("short").is_err());
    assert!(reveal("*not base64*").is_err());
    assert_eq!(
        format!("{:?}", reveal(&obscured).unwrap()),
        "SensitiveString(<redacted>)"
    );
}
