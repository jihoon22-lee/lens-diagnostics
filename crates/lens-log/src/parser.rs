use crate::record::{LogLevel, LogRecordView};
use std::borrow::Cow;

pub fn parse_line<'a>(raw: &'a str, line_number: usize) -> LogRecordView<'a> {
    let trimmed = raw.trim();

    // Check JSONL format
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(trimmed)
        {
            let mut level = LogLevel::Unknown;
            let mut message = String::new();
            let mut source = String::new();
            let mut source_priority: u8 = 0;
            let mut timestamp_ms = 0;
            let mut has_primary_ts = false;
            let mut fields = Vec::new();

            for (k, v) in map {
                // eq_ignore_ascii_case: no per-key lowercase allocation.
                if k.eq_ignore_ascii_case("level") || k.eq_ignore_ascii_case("severity") {
                    if let Some(s) = v.as_str() {
                        level = LogLevel::parse(s);
                        // journald-style numeric severity ("3" == err).
                        if level == LogLevel::Unknown {
                            if let Ok(n) = s.parse::<u32>() {
                                if n <= 7 {
                                    level = syslog_severity(n);
                                }
                            }
                        }
                    } else if let Some(n) = v.as_u64() {
                        if n <= 7 {
                            level = syslog_severity(n as u32);
                        }
                    }
                } else if k.eq_ignore_ascii_case("priority") {
                    // journald JSON export field — only a fallback when
                    // no textual level/severity was recognized.
                    if level == LogLevel::Unknown {
                        if let Some(n) = v
                            .as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                        {
                            if n <= 7 {
                                level = syslog_severity(n as u32);
                            }
                        }
                    }
                } else if k.eq_ignore_ascii_case("msg") || k.eq_ignore_ascii_case("message") {
                    if let Some(s) = v.as_str() {
                        message = s.to_string();
                    }
                } else if k.eq_ignore_ascii_case("source")
                    || k.eq_ignore_ascii_case("logger")
                    || k.eq_ignore_ascii_case("component")
                    || k.eq_ignore_ascii_case("service")
                {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() && source_priority < 3 {
                            source = s.to_string();
                            source_priority = 3;
                        }
                    }
                } else if k.eq_ignore_ascii_case("SYSLOG_IDENTIFIER") {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() && source_priority < 2 {
                            source = s.to_string();
                            source_priority = 2;
                        }
                    }
                } else if k.eq_ignore_ascii_case("_COMM") {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() && source_priority < 1 {
                            source = s.to_string();
                            source_priority = 1;
                        }
                    }
                } else if k.eq_ignore_ascii_case("ts")
                    || k.eq_ignore_ascii_case("time")
                    || k.eq_ignore_ascii_case("timestamp")
                {
                    has_primary_ts = true;
                    if let Some(n) = v.as_u64() {
                        timestamp_ms = n;
                    } else if let Some(n) = v.as_i64() {
                        timestamp_ms = n.max(0) as u64;
                    } else if let Some(n) = v.as_f64() {
                        timestamp_ms = n.max(0.0) as u64;
                    } else if let Some(s) = v.as_str() {
                        // ISO/RFC3339 string timestamp, same forms as
                        // --since/--until accept.
                        timestamp_ms = crate::timestamp::parse_bound(s)
                            .and_then(|t| u64::try_from(t).ok())
                            .unwrap_or(0);
                    }
                } else if k.eq_ignore_ascii_case("__REALTIME_TIMESTAMP") {
                    if !has_primary_ts {
                        if let Some(n) = v
                            .as_u64()
                            .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                        {
                            timestamp_ms = n / 1000;
                        }
                    }
                } else {
                    // Preserve unrecognized structured fields instead of
                    // dropping them silently.
                    let value = v
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| v.to_string());
                    fields.push((k, value));
                }
            }

            let final_message = if message.is_empty() {
                Cow::Borrowed(raw)
            } else {
                Cow::Owned(message)
            };

            return LogRecordView {
                timestamp_ms,
                level,
                source: Cow::Owned(source),
                message: final_message,
                raw,
                line_number,
                fields,
            };
        }
    }

    // Standard log format heuristic: [LEVEL] or LEVEL
    let detected_level = detect_level(raw);
    let extracted_source = extract_syslog_source(raw);

    // ISO/RFC3339 or syslog (`Oct  9 …`, current UTC year) prefixes
    // yield a usable timestamp; anything else stays 0 = unknown.
    let timestamp_ms =
        crate::timestamp::extract_timestamp_ms(raw, crate::timestamp::default_syslog_year())
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(0);

    LogRecordView {
        timestamp_ms,
        level: detected_level,
        source: Cow::Borrowed(extracted_source),
        message: Cow::Borrowed(raw),
        raw,
        line_number,
        fields: Vec::new(),
    }
}

fn looks_like_iso_date(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

fn looks_like_time_token(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() >= 8
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
        && b[6].is_ascii_digit()
        && b[7].is_ascii_digit()
}

fn is_syslog_month(w: &str) -> bool {
    matches!(
        w,
        "Jan"
            | "Feb"
            | "Mar"
            | "Apr"
            | "May"
            | "Jun"
            | "Jul"
            | "Aug"
            | "Sep"
            | "Oct"
            | "Nov"
            | "Dec"
    )
}

/// Extract the application/tag name from standard syslog or ISO-syslog
/// prefixes (`Oct  9 12:00:00 host app[123]: msg` -> `"app"`).
fn extract_syslog_source(raw: &str) -> &str {
    let s = crate::timestamp::strip_syslog_pri_prefix(raw.trim_start());
    let mut words = s.split_whitespace();
    let w0 = match words.next() {
        Some(w) => w,
        None => return "",
    };

    // Advance past a leading timestamp if present.
    let host_word = if is_syslog_month(w0) {
        // `Mmm DD HH:MM:SS <host> <tag>: ...`
        let day = match words.next() {
            Some(w) if (1..=2).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_digit()) => w,
            _ => return "",
        };
        let _ = day;
        match words.next() {
            Some(t) if looks_like_time_token(t) => {}
            _ => return "",
        }
        match words.next() {
            Some(h) => h,
            None => return "",
        }
    } else if looks_like_iso_date(w0) {
        if w0.len() == 10 {
            // `YYYY-MM-DD HH:MM:SS... <host> <tag>: ...`
            let next = match words.next() {
                Some(w) => w,
                None => return "",
            };
            if looks_like_time_token(next) {
                match words.next() {
                    Some(h) => h,
                    None => return "",
                }
            } else {
                next
            }
        } else {
            // `YYYY-MM-DDTHH:MM:SS... <host> <tag>: ...`
            match words.next() {
                Some(h) => h,
                None => return "",
            }
        }
    } else {
        w0
    };

    if host_word.ends_with(':') || host_word.starts_with('[') {
        return "";
    }

    let tag_word = match words.next() {
        Some(t) => t,
        None => return "",
    };

    let without_colon = match tag_word.strip_suffix(':') {
        Some(t) if !t.is_empty() => t,
        _ => return "",
    };

    // Strip optional `[pid]` suffix (`sshd[1234]` -> `sshd`).
    let tag = if let Some(bracket_start) = without_colon.find('[') {
        if without_colon.ends_with(']')
            && without_colon[bracket_start + 1..without_colon.len() - 1]
                .bytes()
                .all(|b| b.is_ascii_digit())
        {
            &without_colon[..bracket_start]
        } else {
            return "";
        }
    } else {
        without_colon
    };

    if tag.is_empty() || LogLevel::parse(tag) != LogLevel::Unknown {
        return "";
    }

    if tag
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'))
    {
        tag
    } else {
        ""
    }
}

/// Map a syslog severity (RFC 5424, 0=emerg .. 7=debug) to a `LogLevel`.
fn syslog_severity(sev: u32) -> LogLevel {
    match sev & 7 {
        // emerg, alert, crit
        0..=2 => LogLevel::Fatal,
        3 => LogLevel::Error,
        4 => LogLevel::Warn,
        // notice, info
        5 | 6 => LogLevel::Info,
        _ => LogLevel::Debug,
    }
}

/// A `<N>` token carrying a syslog PRI (`<13>`, RFC5424 `<13>1`) or a
/// kernel printk level (`<3>`). `prefix_form` allows a non-space suffix
/// (RFC5424 version digit or the message text fused to the marker).
fn syslog_priority_token(word: &str, prefix_form: bool, max: u32) -> Option<LogLevel> {
    let inner = word.strip_prefix('<')?;
    let end = inner.find('>')?;
    let digits = &inner[..end];
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !prefix_form && end != inner.len() - 1 {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if n > max {
        return None;
    }
    Some(syslog_severity(n))
}

/// Scan only the first few words of `raw` for a level token.
/// Used by callers that need the level without building a record.
pub fn detect_level(raw: &str) -> LogLevel {
    let trimmed = raw.trim();
    let mut after_tag = false;
    let mut seen_tag = false;
    let mut w0_is_iso = false;
    let mut w0_is_iso_date_only = false;
    let mut w1_is_time = false;

    for (i, word) in trimmed.split_whitespace().take(8).enumerate() {
        if i == 0 {
            // Syslog/RFC5424 line head: `<PRI>` / `<PRI>1`.
            if let Some(lvl) = syslog_priority_token(word, true, 191) {
                return lvl;
            }
            w0_is_iso = looks_like_iso_date(word);
            w0_is_iso_date_only = w0_is_iso && word.len() == 10;
        } else if i == 1 {
            w1_is_time = looks_like_time_token(word);
        }

        if i > 0 && after_tag {
            // printk level embedded at message start: `kernel: <3>msg`.
            // Restricted to 0-7 so hex dumps like `Code: <89> ...` are
            // not misread as levels.
            if let Some(lvl) = syslog_priority_token(word, true, 7) {
                return lvl;
            }
        }

        // journald short/verbose exports carry PRIORITY=<0-7>.
        if let Some(rest) = word.strip_prefix("PRIORITY=") {
            if let Ok(n) = rest.parse::<u32>() {
                if n <= 7 {
                    return syslog_severity(n);
                }
            }
        }

        // Key-value level tokens (`level=error`, `lvl=warn`, `severity=info`)
        if let Some((k, v)) = word.split_once('=') {
            if k.eq_ignore_ascii_case("level")
                || k.eq_ignore_ascii_case("lvl")
                || k.eq_ignore_ascii_case("severity")
            {
                let clean_v = v.trim_matches(|c| c == '"' || c == '\'' || c == ',' || c == ';');
                let lvl = LogLevel::parse(clean_v);
                if lvl != LogLevel::Unknown {
                    return lvl;
                }
            }
        }

        // Check if token is explicitly delimited (`[ERROR]`, `(WARN)`, `<INFO>`, `ERROR:`)
        let is_bracketed = (word.starts_with('[') && (word.ends_with(']') || word.ends_with("]:")))
            || (word.starts_with('(') && (word.ends_with(')') || word.ends_with("):")))
            || (word.starts_with('<') && (word.ends_with('>') || word.ends_with(">:")));
        let is_colon_suffixed = word.ends_with(':') && word.len() > 1;

        if is_bracketed || is_colon_suffixed {
            let clean = word.trim_matches(|c| {
                c == '[' || c == ']' || c == '(' || c == ')' || c == '<' || c == '>' || c == ':'
            });
            let lvl = LogLevel::parse(clean);
            if lvl != LogLevel::Unknown {
                return lvl;
            }
        } else {
            // Bare level word (`INFO`, `ERROR`, etc.): only match at line start
            // (`i == 0`) or immediately after a leading ISO timestamp before any tag.
            let allow_bare = !seen_tag
                && (i == 0
                    || (i == 1 && w0_is_iso)
                    || (i == 2 && w0_is_iso_date_only && w1_is_time));
            if allow_bare {
                let lvl = LogLevel::parse(word);
                if lvl != LogLevel::Unknown {
                    return lvl;
                }
            }
        }

        after_tag = word.ends_with(':');
        if after_tag {
            seen_tag = true;
        }
    }
    LogLevel::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_level_textual() {
        assert_eq!(detect_level("2026-10-05 [ERROR] boom"), LogLevel::Error);
        assert_eq!(detect_level("plain line without level"), LogLevel::Unknown);
    }

    #[test]
    fn test_detect_level_syslog_pri() {
        // rsyslog PRI: facility*8 + severity. 131 = local0.err.
        assert_eq!(
            detect_level("<131>Oct  9 12:00:00 host app: failure"),
            LogLevel::Error
        );
        // RFC5424: <PRI>VERSION ...
        assert_eq!(
            detect_level("<13>1 2026-10-09T00:00:00Z host app - - - notice"),
            LogLevel::Info
        );
        assert_eq!(detect_level("<27>Oct  9 host app: oops"), LogLevel::Error);
    }

    #[test]
    fn test_detect_level_kernel_printk() {
        assert_eq!(
            detect_level("Oct  9 12:00:00 host kernel: <3>ata1.00: failed command"),
            LogLevel::Error
        );
        assert_eq!(
            detect_level("host kernel: <6>usb 1-1: new device"),
            LogLevel::Info
        );
        // Hex dump markers are not levels.
        assert_eq!(
            detect_level("host kernel: Code: a2 fe <89> c2 3d 00"),
            LogLevel::Unknown
        );
        // kern.log-style lines carry no level token.
        assert_eq!(
            detect_level("2026-10-04T01:09:12.800201+09:00 Main kernel: Command line: initrd"),
            LogLevel::Unknown
        );
    }

    #[test]
    fn test_detect_level_journald_priority() {
        assert_eq!(detect_level("PRIORITY=3"), LogLevel::Error);
        let rec = parse_line(
            r#"{"PRIORITY":"4","MESSAGE":"degraded","_HOSTNAME":"h"}"#,
            1,
        );
        assert_eq!(rec.level, LogLevel::Warn);
        let rec2 = parse_line(r#"{"severity":"2","msg":"crit"}"#, 2);
        assert_eq!(rec2.level, LogLevel::Fatal);
    }

    #[test]
    fn test_l02_syslog_and_journald_source_extraction() {
        let sys1 = parse_line("Oct  9 12:00:00 myhost sshd[1234]: Accepted publickey", 1);
        assert_eq!(sys1.source, "sshd");

        let sys2 = parse_line("<131>Oct  9 12:00:00 myhost nginx: worker exited", 2);
        assert_eq!(sys2.source, "nginx");

        let j1 = parse_line(
            r#"{"SYSLOG_IDENTIFIER":"systemd","MESSAGE":"Started unit","__REALTIME_TIMESTAMP":"1791547200000000"}"#,
            3,
        );
        assert_eq!(j1.source, "systemd");
        assert_eq!(j1.timestamp_ms, 1_791_547_200_000);
    }

    #[test]
    fn test_l03_detect_level_avoids_prose_false_positives_and_reaches_after_syslog_tag() {
        // Delimited level at word index 5 (after syslog header) is detected
        assert_eq!(
            detect_level("Oct  9 12:00:00 host app[123]: [ERROR] disk full"),
            LogLevel::Error
        );
        assert_eq!(
            detect_level("Oct  9 12:00:00 host app[123]: level=warn disk almost full"),
            LogLevel::Warn
        );
        // Bare prose word "error" or "debug" after tag is NOT a log level
        assert_eq!(
            detect_level("2026-10-09T12:00:00Z host app: error count is 0"),
            LogLevel::Unknown
        );
        assert_eq!(
            detect_level("Oct  9 12:00:00 host app: debug mode is disabled"),
            LogLevel::Unknown
        );
    }
}
