//! Minimal in-memory editor for an already-plaintext rclone.conf.
//!
//! rclone's normal config Save() deliberately creates a new config temp file
//! and an old-config backup beside the live file. For a plaintext config that
//! would create additional plaintext-at-rest copies. This module therefore
//! edits only the two crypt secret keys we own, entirely in memory. Structural
//! validation is still performed through `rclone config dump` before mutation,
//! and the live result is verified through rclone after commit.
use crate::models::secrets::SecretBundle;
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Result};

/// One config line; the body is zeroed on drop because it may hold secrets.
struct Line {
    /// Line content without the line ending.
    body: Vec<u8>,
    /// Original ending (`\n`, `\r\n`, or empty for a last line without one).
    ending: Vec<u8>,
}

impl Drop for Line {
    fn drop(&mut self) {
        self.body.fill(0);
    }
}

/// Splits UTF-8 config bytes (no BOM) into lines, keeping each line ending.
fn split_lines(raw: &[u8]) -> Result<Vec<Line>> {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) || std::str::from_utf8(raw).is_err() {
        bail!("unsupported plaintext rclone config encoding");
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in raw.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let mut body_end = index;
        let ending = if index > start && raw[index - 1] == b'\r' {
            body_end -= 1;
            b"\r\n".as_slice()
        } else {
            b"\n".as_slice()
        };
        lines.push(Line {
            body: raw[start..body_end].to_vec(),
            ending: ending.to_vec(),
        });
        start = index + 1;
    }
    if start < raw.len() {
        lines.push(Line {
            body: raw[start..].to_vec(),
            ending: Vec::new(),
        });
    } else if raw.is_empty() {
        lines.push(Line {
            body: Vec::new(),
            ending: Vec::new(),
        });
    }
    Ok(lines)
}

/// Trims ASCII whitespace on both ends.
fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// Section name of a `[name]` line, if the line is one.
fn section_name(body: &[u8]) -> Option<&[u8]> {
    let line = trim_ascii(body);
    if line.len() < 3 || line.first() != Some(&b'[') || line.last() != Some(&b']') {
        return None;
    }
    let name = trim_ascii(&line[1..line.len() - 1]);
    (!name.is_empty()).then_some(name)
}

/// `key = value` pair of a setting line (comments and sections excluded).
fn key_value(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let line = trim_ascii(body);
    if line.is_empty() || matches!(line.first(), Some(b'#' | b';' | b'[')) {
        return None;
    }
    let eq = line.iter().position(|byte| *byte == b'=')?;
    let key = trim_ascii(&line[..eq]);
    if key.is_empty() {
        return None;
    }
    Some((key, trim_ascii(&line[eq + 1..])))
}

/// Case-insensitive key comparison, as rclone's INI parser does.
fn key_is(key: &[u8], expected: &[u8]) -> bool {
    key.eq_ignore_ascii_case(expected)
}

/// Line ending used for inserted lines: the first ending found, else `\n`.
fn default_ending(lines: &[Line]) -> Vec<u8> {
    lines
        .iter()
        .find(|line| !line.ending.is_empty())
        .map(|line| line.ending.clone())
        .unwrap_or_else(|| b"\n".to_vec())
}

/// Line range `[start, end)` of the single section named `remote`; errors if
/// missing or duplicated.
fn find_section(lines: &[Line], remote: &str) -> Result<(usize, usize)> {
    let expected = remote.as_bytes();
    let mut found = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(name) = section_name(&line.body) else {
            continue;
        };
        if name != expected {
            continue;
        }
        if found.is_some() {
            bail!("duplicate crypt section in plaintext config");
        }
        found = Some(index);
    }
    let start = found.ok_or_else(|| anyhow!("crypt section is missing from plaintext config"))?;
    let end = (start + 1..lines.len())
        .find(|index| section_name(&lines[*index].body).is_some())
        .unwrap_or(lines.len());
    Ok((start, end))
}

/// Index of the only `key` line inside a section; errors on duplicates.
fn find_key(lines: &[Line], start: usize, end: usize, key: &[u8]) -> Result<Option<usize>> {
    let mut found = None;
    for (index, line) in lines.iter().enumerate().take(end).skip(start + 1) {
        let Some((candidate, _)) = key_value(&line.body) else {
            continue;
        };
        if !key_is(candidate, key) {
            continue;
        }
        if found.is_some() {
            bail!("duplicate crypt secret key in plaintext config");
        }
        found = Some(index);
    }
    Ok(found)
}

/// Builds a `key = value` line body.
fn secret_line(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(key.len() + value.len() + 3);
    output.extend_from_slice(key);
    output.extend_from_slice(b" = ");
    output.extend_from_slice(value);
    output
}

/// Inserts a line at `index`, first adding an ending to a final line that lacks one.
fn insert_line(lines: &mut Vec<Line>, index: usize, body: Vec<u8>, ending: &[u8]) {
    if index == lines.len() && index > 0 && lines[index - 1].ending.is_empty() {
        lines[index - 1].ending.extend_from_slice(ending);
        lines.push(Line {
            body,
            ending: Vec::new(),
        });
    } else {
        lines.insert(
            index,
            Line {
                body,
                ending: ending.to_vec(),
            },
        );
    }
}

/// Sets, inserts or (for `None`) removes `key` in the section of `remote`,
/// zeroing the replaced value.
fn set_secret(
    lines: &mut Vec<Line>,
    remote: &str,
    key: &[u8],
    value: Option<&str>,
    ending: &[u8],
) -> Result<()> {
    let (start, end) = find_section(lines, remote)?;
    let existing = find_key(lines, start, end, key)?;
    match (existing, value) {
        (Some(index), Some(secret)) => {
            let mut old =
                std::mem::replace(&mut lines[index].body, secret_line(key, secret.as_bytes()));
            old.fill(0);
        }
        (None, Some(secret)) => {
            insert_line(lines, end, secret_line(key, secret.as_bytes()), ending)
        }
        (Some(index), None) => {
            lines.remove(index);
        }
        (None, None) => (),
    }
    Ok(())
}

/// Joins the lines back into config bytes.
fn encode(lines: Vec<Line>) -> SensitiveBytes {
    let capacity = lines
        .iter()
        .map(|line| line.body.len() + line.ending.len())
        .sum();
    let mut output = Vec::with_capacity(capacity);
    for line in &lines {
        output.extend_from_slice(&line.body);
        output.extend_from_slice(&line.ending);
    }
    // `Line::drop` clears the temporary per-line plaintext copies.
    SensitiveBytes(output)
}

/// Returns `original` with each bundle remote's `password`/`password2` set to the
/// obscured values, then verifies the result. Refuses an encrypted config.
/// Used by the crypt restore driver for plaintext rclone.conf files.
pub(super) fn apply_crypt_secrets(
    original: &[u8],
    secrets: &SecretBundle,
) -> Result<SensitiveBytes> {
    secrets.validate()?;
    if super::transaction::files::encrypted_header(original) {
        bail!("plaintext editor refuses encrypted rclone config");
    }
    let mut lines = split_lines(original)?;
    let ending = default_ending(&lines);
    for (remote, secret) in &secrets.rclone.crypt {
        set_secret(
            &mut lines,
            remote,
            b"password",
            Some(secret.obscured_password.as_str()),
            &ending,
        )?;
        set_secret(
            &mut lines,
            remote,
            b"password2",
            secret.obscured_password2.as_ref().map(|v| v.as_str()),
            &ending,
        )?;
    }
    let output = encode(lines);
    verify_crypt_secrets(&output.0, secrets)?;
    Ok(output)
}

/// Checks that each bundle remote's `password` and `password2` in `raw` equal
/// the expected obscured values (an empty `password2` counts as absent).
pub(super) fn verify_crypt_secrets(raw: &[u8], secrets: &SecretBundle) -> Result<()> {
    let lines = split_lines(raw)?;
    for (remote, secret) in &secrets.rclone.crypt {
        let (start, end) = find_section(&lines, remote)?;
        let password = find_key(&lines, start, end, b"password")?
            .and_then(|index| key_value(&lines[index].body).map(|(_, value)| value));
        if password != Some(secret.obscured_password.as_str().as_bytes()) {
            bail!("plaintext crypt password preservation check failed");
        }
        let password2 = find_key(&lines, start, end, b"password2")?
            .and_then(|index| key_value(&lines[index].body).map(|(_, value)| value))
            .filter(|value| !value.is_empty());
        let expected2 = secret
            .obscured_password2
            .as_ref()
            .map(|value| value.as_str().as_bytes());
        if password2 != expected2 {
            bail!("plaintext crypt password2 preservation check failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::secrets::CryptSecret;
    use crate::models::sensitive::SensitiveText;
    use std::collections::BTreeMap;

    fn bundle(password2: Option<&str>) -> SecretBundle {
        SecretBundle::new(BTreeMap::from([(
            "vault".into(),
            CryptSecret {
                obscured_password: SensitiveText::new("AAAAAAAAAAAAAAAAAAAAAAA".into()),
                obscured_password2: password2.map(|v| SensitiveText::new(v.into())),
            },
        )]))
    }

    #[test]
    fn patches_only_crypt_secret_lines() {
        let raw = b"[cloud]\r\ntype = drive\r\ntoken = DO-NOT-TOUCH\r\n\r\n[vault]\r\ntype = crypt\r\nremote = cloud:rpool\r\npassword = OLDOLDOLDOLDOLDOLDOLD12\r\n";
        let out = apply_crypt_secrets(raw, &bundle(Some("BBBBBBBBBBBBBBBBBBBBBBB"))).unwrap();
        let text = std::str::from_utf8(&out.0).unwrap();
        assert!(text.contains("token = DO-NOT-TOUCH"));
        assert!(text.contains("password = AAAAAAAAAAAAAAAAAAAAAAA"));
        assert!(text.contains("password2 = BBBBBBBBBBBBBBBBBBBBBBB"));
        assert!(text.contains("\r\n"));
    }

    #[test]
    fn absent_password2_removes_existing_key() {
        let raw = b"[vault]\ntype = crypt\npassword = OLDOLDOLDOLDOLDOLDOLD12\npassword2 = OLDOLDOLDOLDOLDOLDOLD34\n";
        let out = apply_crypt_secrets(raw, &bundle(None)).unwrap();
        let text = std::str::from_utf8(&out.0).unwrap();
        assert!(!text.contains("password2"));
    }

    #[test]
    fn duplicate_section_or_secret_key_is_rejected() {
        let duplicated_section = b"[vault]\npassword = x\n[vault]\npassword = y\n";
        assert!(apply_crypt_secrets(duplicated_section, &bundle(None)).is_err());
        let duplicated_key = b"[vault]\npassword = x\npassword = y\n";
        assert!(apply_crypt_secrets(duplicated_key, &bundle(None)).is_err());
    }

    #[test]
    fn encrypted_input_is_never_treated_as_plaintext() {
        assert!(apply_crypt_secrets(b"RCLONE_ENCRYPT_V0:\nanything\n", &bundle(None)).is_err());
    }
}
