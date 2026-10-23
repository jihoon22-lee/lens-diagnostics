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

/// Scrub bytes, replacing invalid UTF-8 with U+FFFD before redacting.
/// Evidence collectors that must preserve encoding use [`try_redact_bytes`].
pub fn redact_bytes(data: &[u8]) -> Vec<u8> {
    redact_str(&String::from_utf8_lossy(data)).into_bytes()
}

/// Refuse evidence whose encoding cannot be safely preserved during redaction.
pub fn try_redact_bytes(data: &[u8]) -> crate::Result<Vec<u8>> {
    let text = std::str::from_utf8(data).map_err(|_| crate::LensError::InvalidInput {
        message: "redaction requires valid UTF-8; artifact omitted".to_string(),
    })?;
    Ok(redact_str(text).into_bytes())
}

/// Redact decoded JSON values so escaping and non-secret field types survive.
pub fn redact_json_value(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if SECRET_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k)) {
                    redact_secret_value(value);
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json_value),
        Value::String(s) => *s = redact_str(s),
        _ => {}
    }
}

fn redact_secret_value(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Null => {}
        Value::Array(items) => items.iter_mut().for_each(redact_secret_value),
        Value::Object(map) => map.values_mut().for_each(redact_secret_value),
        Value::String(secret) => {
            if !secret.is_empty() && !secret.starts_with("<REDACTED:") {
                *secret = token_for(secret);
            }
        }
        other => *other = Value::String(token_for(&other.to_string())),
    }
}

fn redact_pem_blocks(input: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(BEGIN) {
        out.push_str(&rest[..start]);
        let after_begin = &rest[start..];
        // The label sits between `-----BEGIN ` and the closing `-----`.
        let label_area = &after_begin[BEGIN.len()..];
        if let Some(label_len) = label_area.find("-----") {
            let label = &label_area[..label_len];
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
        out.push_str(BEGIN);
        rest = &after_begin[BEGIN.len()..];
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
                        let mut escaped = false;
                        while cursor < line.len() {
                            let ch = line[cursor..].chars().next().unwrap();
                            if escaped {
                                escaped = false;
                            } else if ch == '\\' {
                                escaped = true;
                            } else if ch == q {
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
    fn json_redaction_preserves_escaping_and_structure() {
        let mut value = serde_json::json!({
            "Environment": ["PASSWORD=\"synthetic-credential-123\"", "USER=alice"],
            "nested": {"password": "quote\"slash\\and\nnewline", "count": 7, "enabled": true},
            "api_key": 123456,
            "Password": ["array-secret-one", "array-secret-two"],
        });
        redact_json_value(&mut value);
        let bytes = serde_json::to_vec(&value).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("synthetic-credential-123"));
        assert!(!text.contains("quote"));
        assert!(!text.contains("123456"));
        assert!(!text.contains("array-secret"));
        assert_eq!(decoded["Password"].as_array().unwrap().len(), 2);
        assert_eq!(decoded["nested"]["count"], 7);
        assert_eq!(decoded["nested"]["enabled"], true);
        assert_eq!(decoded["Environment"][1], "USER=alice");
        let once = value.clone();
        redact_json_value(&mut value);
        assert_eq!(value, once);
    }

    #[test]
    fn invalid_utf8_never_returns_an_unredacted_secret() {
        let bytes = b"bad byte: \xff\npassword=synthetic-password-789\n";
        assert!(try_redact_bytes(bytes).is_err());
        let scrubbed = String::from_utf8(redact_bytes(bytes)).unwrap();
        assert!(!scrubbed.contains("synthetic-password-789"));
        assert!(scrubbed.contains('\u{fffd}'));
    }

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

    #[test]
    fn test_redact_pem_private_key_block() {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\nabcDEF123==";
        for label in ["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY"] {
            let input =
                format!("before\n-----BEGIN {label}-----\n{body}\n-----END {label}-----\nafter\n");
            let out = redact_str(&input);
            assert!(
                !out.contains("MIIEvQIBADANBg"),
                "{label} body leaked: {out}"
            );
            assert!(!out.contains("abcDEF123"), "{label} body leaked: {out}");
            assert!(out.contains(&format!("-----BEGIN {label}-----")));
            assert!(out.contains(&format!("-----END {label}-----")));
            assert!(out.contains("<REDACTED:sha256:"));
            assert!(out.starts_with("before\n") && out.ends_with("after\n"));
        }
        // Identical keys produce identical tokens (correlation across artifacts).
        let a = redact_str(&format!(
            "-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----"
        ));
        let b = redact_str(&format!(
            "x\n-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----"
        ));
        assert!(b.ends_with(&a));
    }

    #[test]
    fn test_redact_leaves_public_pem_and_unterminated_blocks_alone() {
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(redact_str(cert), cert);
    }

    #[test]
    fn test_redact_quoted_secret_with_escaped_quotes() {
        let input = r#"{"api_key": "abc\"def\\\"ghi", "next": 123}"#;
        let scrubbed = redact_str(input);
        assert!(!scrubbed.contains("abc"), "leaked abc: {scrubbed}");
        assert!(!scrubbed.contains("def"), "leaked def: {scrubbed}");
        assert!(!scrubbed.contains("ghi"), "leaked ghi: {scrubbed}");
        assert!(scrubbed.contains(r#"{"api_key": "<REDACTED:sha256:"#));
        assert!(scrubbed.ends_with(r#"", "next": 123}"#));
    }
}
