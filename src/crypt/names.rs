//! File and directory name encryption (`EncryptFileName`, `EncryptDirName`,
//! `DecryptFileName` and `DecryptDirName` in rclone's crypt `cipher.go`).
//! rclone's `--b2-versions` style version suffixes are not handled.
use super::cipher::Cipher;
use super::eme::{self, Direction};
use super::options::NameMode;
use anyhow::{anyhow, bail, Result};

const NAME_BLOCK: usize = 16;
const MAX_NAME_CIPHERTEXT: usize = 2048;
const QUOTE: char = '!';

fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad = NAME_BLOCK - data.len() % NAME_BLOCK;
    let mut out = Vec::with_capacity(data.len() + pad);
    out.extend_from_slice(data);
    out.resize(data.len() + pad, pad as u8);
    out
}

fn pkcs7_unpad(data: &[u8]) -> Result<&[u8]> {
    if data.is_empty() {
        bail!("crypt name padding too short");
    }
    if !data.len().is_multiple_of(NAME_BLOCK) {
        bail!("crypt name padding not a multiple of the block size");
    }
    let pad = usize::from(data[data.len() - 1]);
    if pad == 0 || pad > NAME_BLOCK {
        bail!("crypt name padding too long");
    }
    let (plain, padding) = data.split_at(data.len() - pad);
    if padding.iter().any(|&byte| usize::from(byte) != pad) {
        bail!("crypt name padding is inconsistent");
    }
    Ok(plain)
}

impl Cipher {
    fn encrypt_segment(&self, plain: &str) -> Result<String> {
        if plain.is_empty() {
            return Ok(String::new());
        }
        let padded = pkcs7_pad(plain.as_bytes());
        let encrypted = eme::transform(
            &self.name_block,
            &self.name_tweak,
            &padded,
            Direction::Encrypt,
        )
        .ok_or_else(|| anyhow!("file name segment is too long to encrypt"))?;
        Ok(self.name_encoding.encode(&encrypted))
    }

    fn decrypt_segment(&self, encoded: &str) -> Result<String> {
        if encoded.is_empty() {
            return Ok(String::new());
        }
        let raw = self.name_encoding.decode(encoded)?;
        if raw.len() % NAME_BLOCK != 0 {
            bail!("crypt name is not a multiple of the block size");
        }
        if raw.is_empty() {
            bail!("crypt name is too short after decoding");
        }
        if raw.len() > MAX_NAME_CIPHERTEXT {
            bail!("crypt name is too long after decoding");
        }
        let padded = eme::transform(&self.name_block, &self.name_tweak, &raw, Direction::Decrypt)
            .ok_or_else(|| anyhow!("crypt name has an invalid length"))?;
        let plain = pkcs7_unpad(&padded)?;
        String::from_utf8(plain.to_vec()).map_err(|_| anyhow!("decrypted crypt name is not UTF-8"))
    }

    fn name_key_sum(&self) -> i64 {
        self.name_key.iter().map(|&b| i64::from(b)).sum()
    }

    fn obfuscate_segment(&self, plain: &str) -> String {
        if plain.is_empty() {
            return String::new();
        }
        let mut dir: i64 = plain.chars().map(|c| i64::from(u32::from(c))).sum::<i64>() % 256;
        let mut out = format!("{dir}.");
        dir += self.name_key_sum();
        for c in plain.chars() {
            let value = i64::from(u32::from(c));
            match c {
                QUOTE => {
                    out.push(QUOTE);
                    out.push(QUOTE);
                }
                '0'..='9' => {
                    let shift = dir % 9 + 1;
                    out.push(char_at('0' as i64 + (value - '0' as i64 + shift) % 10));
                }
                'A'..='Z' | 'a'..='z' => {
                    let shift = dir % 25 + 1;
                    let mut pos = value - 'A' as i64;
                    if pos >= 26 {
                        pos -= 6;
                    }
                    pos = (pos + shift) % 52;
                    if pos >= 26 {
                        pos += 6;
                    }
                    out.push(char_at('A' as i64 + pos));
                }
                '\u{A0}'..='\u{FF}' => {
                    let shift = dir % 95 + 1;
                    out.push(char_at(0xA0 + (value - 0xA0 + shift) % 96));
                }
                _ if value >= 0x100 => {
                    let shift = dir % 127 + 1;
                    let base = value - value % 256;
                    match u32::try_from(base + (value - base + shift) % 256)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        Some(rotated) => out.push(rotated),
                        None => {
                            out.push(QUOTE);
                            out.push(c);
                        }
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn deobfuscate_segment(&self, text: &str) -> Result<String> {
        if text.is_empty() {
            return Ok(String::new());
        }
        let (number, body) = text
            .split_once('.')
            .ok_or_else(|| anyhow!("not an encrypted file name"))?;
        if number == "!" {
            return Ok(body.to_string());
        }
        let mut dir: i64 = number
            .parse()
            .map_err(|_| anyhow!("not an encrypted file name"))?;
        dir += self.name_key_sum();
        let mut out = String::with_capacity(body.len());
        let mut quoted = false;
        for c in body.chars() {
            let value = i64::from(u32::from(c));
            if quoted {
                out.push(c);
                quoted = false;
                continue;
            }
            match c {
                QUOTE => quoted = true,
                '0'..='9' => {
                    let mut rotated = value - (dir % 9 + 1);
                    if rotated < '0' as i64 {
                        rotated += 10;
                    }
                    out.push(char_at(rotated));
                }
                'A'..='Z' | 'a'..='z' => {
                    let mut pos = value - 'A' as i64;
                    if pos >= 26 {
                        pos -= 6;
                    }
                    pos -= dir % 25 + 1;
                    if pos < 0 {
                        pos += 52;
                    }
                    if pos >= 26 {
                        pos += 6;
                    }
                    out.push(char_at('A' as i64 + pos));
                }
                '\u{A0}'..='\u{FF}' => {
                    let mut rotated = value - (dir % 95 + 1);
                    if rotated < 0xA0 {
                        rotated += 96;
                    }
                    out.push(char_at(rotated));
                }
                _ if value >= 0x100 => {
                    let base = value - value % 256;
                    let mut rotated = value - (dir % 127 + 1);
                    if rotated < base {
                        rotated += 256;
                    }
                    out.push(
                        u32::try_from(rotated)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or_else(|| anyhow!("invalid obfuscated file name"))?,
                    );
                }
                _ => out.push(c),
            }
        }
        Ok(out)
    }

    fn map_segments(
        &self,
        path: &str,
        mut segment: impl FnMut(&Self, &str) -> Result<String>,
    ) -> Result<String> {
        let parts: Vec<&str> = path.split('/').collect();
        let last = parts.len() - 1;
        let mut out = Vec::with_capacity(parts.len());
        for (i, part) in parts.into_iter().enumerate() {
            if !self.directory_name_encryption && i != last {
                out.push(part.to_string());
            } else {
                out.push(segment(self, part)?);
            }
        }
        Ok(out.join("/"))
    }

    /// rclone `EncryptFileName`: a `/`-separated path whose last segment is a file.
    pub(crate) fn encrypt_file_name(&self, path: &str) -> Result<String> {
        match self.name_mode {
            NameMode::Off => Ok(format!("{path}{}", self.suffix)),
            NameMode::Standard => self.map_segments(path, Self::encrypt_segment),
            NameMode::Obfuscate => self.map_segments(path, |c, s| Ok(c.obfuscate_segment(s))),
        }
    }

    /// rclone `EncryptDirName`.
    pub(crate) fn encrypt_dir_name(&self, path: &str) -> Result<String> {
        if self.name_mode == NameMode::Off || !self.directory_name_encryption {
            return Ok(path.to_string());
        }
        self.encrypt_file_name(path)
    }

    /// rclone `DecryptFileName`.
    pub(crate) fn decrypt_file_name(&self, path: &str) -> Result<String> {
        match self.name_mode {
            NameMode::Off => path
                .strip_suffix(self.suffix.as_str())
                .filter(|plain| !plain.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("not an encrypted file name")),
            NameMode::Standard => self.map_segments(path, Self::decrypt_segment),
            NameMode::Obfuscate => self.map_segments(path, Self::deobfuscate_segment),
        }
    }

    /// rclone `DecryptDirName`.
    pub(crate) fn decrypt_dir_name(&self, path: &str) -> Result<String> {
        if self.name_mode == NameMode::Off || !self.directory_name_encryption {
            return Ok(path.to_string());
        }
        self.decrypt_file_name(path)
    }
}

fn char_at(value: i64) -> char {
    u32::try_from(value)
        .ok()
        .and_then(char::from_u32)
        .expect("rotation stays inside an ASCII or Latin-1 range")
}
