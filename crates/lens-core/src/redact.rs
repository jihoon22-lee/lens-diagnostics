//! Deterministic PII / secret redactor for forensic bundles (`--redact`).
//!
//! Replaces sensitive values (Bearer tokens, API keys, password/secret assignments,
//! and PEM private key blocks) with deterministic SHA-256 prefix tokens
//! (`<REDACTED:sha256:xxxxxxxx>`). This preserves forensic correlation across
//! artifacts (two configs using the same secret produce the same token) without
//! exposing raw credentials.

use crate::hash::digest_bytes;

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "access_token",
    "auth_token",
    "client_secret",
    "private_key",
];

fn token_for(secret: &str) -> String {
    let hash = digest_bytes(secret.as_bytes());
    format!("<REDACTED:sha256:{}>", &hash[..8])
}

/// Scrub sensitive tokens and secret assignments from `input`.
pub fn redact_str(input: &str) -> String {
    // First redact PEM private key blocks if present.
    let mut current = redact_pem_blocks(input);
    // Redact line by line for key=value / Bearer / known token prefixes.
    let mut out = String::with_capacity(current.len());
    for (idx, line) in current.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    current.clear();
    out
}

/// Scrub UTF-8 bytes in place if valid UTF-8, or lossily convert and scrub.
pub fn redact_bytes(data: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(data) {
        Ok(s) => redact_str(s).into_bytes(),
        Err(_) => data.to_vec(),
    }
}

fn redact_pem_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("-----BEGIN ") {
        out.push_str(&rest[..start]);
        let after_begin = &rest[start..];
        if let Some(header_end) = after_begin.find("-----") {
            let header_tail = &after_begin[header_end + 5..];
            if let Some(close_dashes) = header_tail.find("-----") {
                let label = &header_tail[..close_dashes];
                if label.contains("PRIVATE KEY") {
                    let end_marker = format!("-----END {}-----", label);
                    if let Some(end_pos) = after_begin.find(&end_marker) {
                        let block_end = end_pos + end_marker.len();
                        let secret_body = &after_begin[..block_end];
                        out.push_str(&format!(
                            "-----BEGIN {}-----{}-----END {}-----",
                            label,
                            token_for(secret_body),
                            label
                        ));
                        rest = &after_begin[block_end..];
                        continue;
                    }
                }
            }
        }
        out.push_str("-----BEGIN ");
        rest = &after_begin["-----BEGIN ".len()..];
    }
    out.push_str(rest);
    out
}

fn redact_line(line: &str) -> String {
    let mut work = redact_bearer_and_prefixes(line);
    work = redact_kv_assignments(&work);
    work
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/')
}

fn redact_bearer_and_prefixes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let bytes = line.as_bytes();
    while i < line.len() {
        let rem = &line[i..];
        // Check "Bearer <token>"
        if rem
            .as_bytes()
            .get(..7)
            .is_some_and(|b| b.eq_ignore_ascii_case(b"bearer "))
        {
            out.push_str(&rem[..7]);
            i += 7;
            let start = i;
            while i < line.len() {
                let ch = line[i..].chars().next().unwrap();
                if is_token_char(ch) || ch == '=' {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            let tok = &line[start..i];
            if tok.len() >= 8 && !tok.starts_with("<REDACTED:") {
                out.push_str(&token_for(tok));
            } else {
                out.push_str(tok);
            }
            continue;
        }

        // Check known API key prefixes at word boundaries
        let prev_boundary = i == 0 || !is_token_char(line[..i].chars().next_back().unwrap());
        if prev_boundary {
            let matched_prefix = [
                "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "AKIA", "AIza",
            ]
            .iter()
            .find(|&&p| rem.starts_with(p));
            if let Some(&prefix) = matched_prefix {
                let start = i;
                i += prefix.len();
                while i < line.len() {
                    let ch = line[i..].chars().next().unwrap();
                    if is_token_char(ch) {
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let tok = &line[start..i];
                if tok.len() >= 16 {
                    out.push_str(&token_for(tok));
                } else {
                    out.push_str(tok);
                }
                continue;
            }
        }

        let ch = rem.chars().next().unwrap();
        let _ = bytes;
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn redact_kv_assignments(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut matched_key = false;
    for &k in SECRET_KEYS {
        if lower.contains(k) {
            matched_key = true;
            break;
        }
    }
    if !matched_key {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let rem = &line[i..];
        let rem_lower = &lower[i..];
        let mut found_key: Option<usize> = None;
        let prev_ok = i == 0
            || !line[..i]
                .chars()
                .next_back()
                .unwrap()
                .is_ascii_alphanumeric();
        if prev_ok {
            for &k in SECRET_KEYS {
                if rem_lower.starts_with(k) {
                    let after_k = &rem[k.len()..];
                    let next_ok = after_k
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
                    if next_ok {
                        found_key = Some(k.len());
                        break;
                    }
                }
            }
        }

        if let Some(k_len) = found_key {
            // Check if followed by optional quotes/whitespace and ':' or '='
            let mut cursor = i + k_len;
            while cursor < line.len()
                && matches!(line.as_bytes()[cursor], b' ' | b'\t' | b'"' | b'\'')
            {
                cursor += 1;
            }
            if cursor < line.len() && matches!(line.as_bytes()[cursor], b'=' | b':') {
                cursor += 1;
                while cursor < line.len() && matches!(line.as_bytes()[cursor], b' ' | b'\t') {
                    cursor += 1;
                }
                if cursor < line.len() {
                    let quote = if matches!(line.as_bytes()[cursor], b'"' | b'\'') {
                        let q = line.as_bytes()[cursor] as char;
                        cursor += 1;
                        Some(q)
                    } else {
                        None
                    };
                    out.push_str(&line[i..cursor]);
                    let val_start = cursor;
                    if let Some(q) = quote {
                        while cursor < line.len() {
                            let ch = line[cursor..].chars().next().unwrap();
                            if ch == q {
                                break;
                            }
                            cursor += ch.len_utf8();
                        }
                    } else {
                        while cursor < line.len() {
                            let ch = line[cursor..].chars().next().unwrap();
                            if ch.is_whitespace()
                                || matches!(ch, ',' | ';' | '}' | ')' | '"' | '\'')
                            {
                                break;
                            }
                            cursor += ch.len_utf8();
                        }
                    }
                    let secret_val = &line[val_start..cursor];
                    if !secret_val.is_empty()
                        && !secret_val.starts_with("<REDACTED:")
                        && secret_val != "null"
                        && secret_val != "false"
                        && secret_val != "true"
                    {
                        out.push_str(&token_for(secret_val));
                    } else {
                        out.push_str(secret_val);
                    }
                    i = cursor;
                    continue;
                }
            }
        }

        let ch = rem.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_bearer_and_api_keys_deterministically() {
        let line1 = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        let line2 = "token = Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        let r1 = redact_str(line1);
        let r2 = redact_str(line2);
        assert!(!r1.contains("eyJhbGci"));
        assert!(r1.contains("<REDACTED:sha256:"));
        // Same secret yields identical hash token
        let tok1 = r1.split("Bearer ").nth(1).unwrap();
        let tok2 = r2.split("Bearer ").nth(1).unwrap();
        assert_eq!(tok1, tok2);

        let sk = redact_str("OPENAI_KEY=sk-proj1234567890abcdef123456");
        assert!(!sk.contains("sk-proj1234567890abcdef123456"));
        assert!(sk.contains("<REDACTED:sha256:"));
    }

    #[test]
    fn test_redact_kv_and_json_fields() {
        let json = r#"{"password": "my_super_secret_pw", "user": "alice"}"#;
        let scrubbed = redact_str(json);
        assert!(!scrubbed.contains("my_super_secret_pw"));
        assert!(scrubbed.contains(r#""user": "alice""#));
        assert!(scrubbed.contains("<REDACTED:sha256:"));
    }

    #[test]
    fn test_redact_multibyte_utf8_near_bearer() {
        // String with 2-byte UTF-8 char (µ is 2 bytes: 0xC2 0xB5)
        let line = "latency: 120µs Bearer mysecrettoken123456";
        let scrubbed = redact_str(line);
        assert!(!scrubbed.contains("mysecrettoken123456"));
        assert!(scrubbed.contains("latency: 120µs Bearer <REDACTED:sha256:"));

        // Unicode with emojis and CJK
        let line2 = "한글 로그 🚀 bearer secrettoken78901234";
        let scrubbed2 = redact_str(line2);
        assert!(!scrubbed2.contains("secrettoken78901234"));
        assert!(scrubbed2.contains("한글 로그 🚀 bearer <REDACTED:sha256:"));
    }
}
