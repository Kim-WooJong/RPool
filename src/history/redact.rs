pub(crate) fn redact_text(input: &str, max_chars: usize) -> String {
    let flattened = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = flattened
        .split(' ')
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ");
    redacted.chars().take(max_chars).collect()
}

fn redact_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    for key in [
        "token=",
        "password=",
        "passwd=",
        "secret=",
        "apikey=",
        "api_key=",
        "authorization=",
        "authorization:",
    ] {
        if let Some(index) = lower.find(key) {
            let prefix_end = index + key.len();
            return format!("{}***", &token[..prefix_end]);
        }
    }

    if let Some(scheme_end) = token.find("://") {
        let authority_start = scheme_end + 3;
        let authority_end = token[authority_start..]
            .find('/')
            .map(|index| authority_start + index)
            .unwrap_or_else(|| token.find('?').unwrap_or(token.len()));
        let authority = &token[authority_start..authority_end];
        let host = authority.rsplit_once('@').map(|(_, host)| host).unwrap_or(authority);
        let mut value = if authority.contains('@') {
            format!("{}://***@{}{}", &token[..scheme_end], host, &token[authority_end..])
        } else {
            token.to_string()
        };
        if let Some(query) = value.find('?') {
            value.truncate(query);
        }
        if let Some(fragment) = value.find('#') {
            value.truncate(fragment);
        }
        return value;
    }

    token.to_string()
}
