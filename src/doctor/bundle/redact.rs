//! RPool's own redaction pass over everything put in a diagnostics bundle.
//! It runs after rclone's `config redacted` and on every other file, and
//! errs on the side of removing too much. The rules are listed in
//! [`RULES`] and copied into the bundle's manifest.
use serde_json::Value;

pub(crate) const REDACTED: &str = "<redacted>";

pub(crate) const RULES: &[&str] = &[
    "values of keys/flags whose name contains: pass, token, secret, key, credential, cookie, salt, private, session, bearer, signature, authorization (INI `k = v`, `k: v`, `k=v`, JSON fields, `--flag value`)",
    "user:password@ in URLs",
    "values after `Bearer ` / `Basic `",
    "PEM private key blocks and AGE-SECRET-KEY-… identities",
    "JWT-like tokens (eyJ…)",
    "any run of 24+ base64/base64url characters mixing upper case, lower case and digits (rclone-obscured strings, OAuth tokens, API keys)",
    "e-mail addresses",
];

const SECRET_KEY_PARTS: &[&str] = &[
    "pass",
    "token",
    "secret",
    "key",
    "credential",
    "cookie",
    "salt",
    "private",
    "session",
    "bearer",
    "signature",
    "authorization",
];

/// Whether a field/flag named `name` holds a secret.
pub(crate) fn is_secret_key(name: &str) -> bool {
    let name = name.trim_start_matches('-').to_ascii_lowercase();
    SECRET_KEY_PARTS.iter().any(|part| name.contains(part))
}

/// Redacts a whole text file, line by line (PEM blocks span lines).
pub(crate) fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_private_block = false;
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        if in_private_block {
            if body.contains("-----END") {
                in_private_block = false;
            }
            continue;
        }
        if body.contains("-----BEGIN") && body.contains("PRIVATE KEY") {
            in_private_block = !body.contains("-----END");
            out.push_str("<redacted private key>");
            out.push_str(newline);
            continue;
        }
        out.push_str(&redact_line(body));
        out.push_str(newline);
    }
    out
}

/// Redacts one line of free text, INI or a log.
pub(crate) fn redact_line(line: &str) -> String {
    let line = redact_url_userinfo(line);
    let line = redact_after_scheme_word(&line);
    let line = redact_key_values(&line);
    let line = redact_token_runs(&line);
    redact_emails(&line)
}

/// A JSON document: secret-named fields are replaced, every other string is
/// redacted as text. Unparsable input falls back to [`redact_text`].
pub(crate) fn redact_json_bytes(bytes: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(mut value) => {
            redact_json(&mut value);
            let mut out = serde_json::to_vec_pretty(&value).unwrap_or_default();
            out.push(b'\n');
            out
        }
        Err(_) => redact_text(&String::from_utf8_lossy(bytes)).into_bytes(),
    }
}

/// JSON Lines: each line on its own.
pub(crate) fn redact_json_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(mut value) => {
                redact_json(&mut value);
                out.push_str(&value.to_string());
            }
            Err(_) => out.push_str(&redact_line(line)),
        }
        out.push('\n');
    }
    out
}

pub(crate) fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if is_secret_key(key) {
                    match field {
                        Value::Null | Value::Bool(_) => {}
                        Value::String(s) if s.is_empty() => {}
                        _ => *field = Value::String(REDACTED.into()),
                    }
                } else {
                    redact_json(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::String(text) => {
            let redacted = redact_text(text);
            if redacted != *text {
                *text = redacted;
            }
        }
        _ => {}
    }
}

/// `scheme://user:pass@host` → `scheme://<redacted>@host`.
fn redact_url_userinfo(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let authority_end = tail
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"')
            .unwrap_or(tail.len());
        let authority = &tail[..authority_end];
        if let Some(userinfo_end) = authority.rfind('@') {
            out.push_str(REDACTED);
            out.push_str(&authority[userinfo_end..]);
        } else {
            out.push_str(authority);
        }
        rest = &tail[authority_end..];
    }
    out.push_str(rest);
    out
}

/// `Bearer abc` / `Basic abc` → `Bearer <redacted>`.
fn redact_after_scheme_word(line: &str) -> String {
    let mut out = line.to_string();
    for word in ["bearer ", "basic "] {
        let mut search_from = 0;
        loop {
            let lower = out.to_ascii_lowercase();
            let Some(found) = lower[search_from..].find(word) else {
                break;
            };
            let start = search_from + found + word.len();
            let value_end = out[start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
                .map_or(out.len(), |end| start + end);
            if value_end > start && &out[start..value_end] != REDACTED {
                out.replace_range(start..value_end, REDACTED);
            }
            search_from = start + REDACTED.len().min(out.len() - start);
            if search_from >= out.len() {
                break;
            }
        }
    }
    out
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Redacts the value after every secret-named key: `k = v`, `k: v`, `k=v`,
/// `"k": "v"`, `--k v` and `--k=v`.
fn redact_key_values(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_name_char(chars[i]) || (i > 0 && is_name_char(chars[i - 1])) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_name_char(chars[i]) {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        out.push_str(&name);
        if !is_secret_key(&name) {
            continue;
        }
        // Optional closing quote of a JSON key, then spaces, then a separator.
        let mut j = i;
        if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
            j += 1;
        }
        let spaces_before = j;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        let separator = chars.get(j).copied();
        let flag = name.starts_with("--");
        let value_start = match separator {
            Some('=') | Some(':') => {
                j += 1;
                while j < chars.len() && chars[j] == ' ' {
                    j += 1;
                }
                j
            }
            // `--webdav-pass value`: a flag followed by its value.
            Some(_) if flag && j > spaces_before => j,
            _ => continue,
        };
        let quote = chars
            .get(value_start)
            .copied()
            .filter(|c| *c == '"' || *c == '\'');
        let body_start = value_start + usize::from(quote.is_some());
        let mut end = body_start;
        match quote {
            Some(q) => {
                while end < chars.len() && chars[end] != q {
                    if chars[end] == '\\' {
                        end += 1;
                    }
                    end += 1;
                }
                end = end.min(chars.len());
            }
            None if !flag && separator != Some(':') && !line.trim_start().starts_with('"') => {
                // INI/env style: the rest of the line is the value.
                end = chars.len();
                while end > body_start && chars[end - 1] == ' ' {
                    end -= 1;
                }
            }
            None => {
                while end < chars.len()
                    && !chars[end].is_whitespace()
                    && !matches!(chars[end], ',' | ';' | '&' | '}' | ']' | ')')
                {
                    end += 1;
                }
            }
        }
        out.extend(&chars[i..body_start]);
        if end > body_start {
            out.push_str(REDACTED);
        }
        i = end;
    }
    out
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=')
}

/// Whether a run of token characters is secret-shaped.
fn looks_like_secret(run: &str) -> bool {
    if run.starts_with("AGE-SECRET-KEY-") || (run.starts_with("eyJ") && run.len() >= 20) {
        return true;
    }
    run.len() >= 24
        && run.chars().any(|c| c.is_ascii_uppercase())
        && run.chars().any(|c| c.is_ascii_lowercase())
        && run.chars().any(|c| c.is_ascii_digit())
}

fn redact_token_runs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if looks_like_secret(run) && run != REDACTED {
            out.push_str(REDACTED);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in line.chars() {
        if is_token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn redact_emails(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let local = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let domain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut copied = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = i;
            while start > copied && local(chars[start - 1]) {
                start -= 1;
            }
            let mut end = i + 1;
            while end < chars.len() && domain(chars[end]) {
                end += 1;
            }
            let host: String = chars[i + 1..end].iter().collect();
            let host = host.trim_end_matches('.');
            if start < i && host.contains('.') && !host.starts_with('.') {
                out.extend(&chars[copied..start]);
                out.push_str("<email>");
                copied = i + 1 + host.chars().count();
                i = copied;
                continue;
            }
        }
        i += 1;
    }
    out.extend(&chars[copied..]);
    out
}
