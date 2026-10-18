//! Timestamp extraction for `--since`/`--until` filtering. All values
//! are epoch milliseconds (`i64`); timestamps without an explicit
//! offset are read as UTC.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;

/// Year assumed for year-less syslog timestamps (`Oct  9 12:00:00`)
/// when the caller did not pass `--year`. Defaults to the current
/// UTC year, cached once per process to avoid per-line clock syscalls.
pub fn default_syslog_year() -> i32 {
    static CACHED_YEAR: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *CACHED_YEAR.get_or_init(|| jiff::Timestamp::now().to_zoned(TimeZone::UTC).year().into())
}

/// Epoch milliseconds → RFC 3339 UTC string (`2026-10-09T12:00:00Z`).
/// `None` when `ms` is out of representable range.
pub fn format_rfc3339_ms(ms: i64) -> Option<String> {
    jiff::Timestamp::from_millisecond(ms)
        .ok()
        .map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Epoch seconds → RFC 3339 UTC string (same shape as
/// [`format_rfc3339_ms`], used for bucket starts).
pub fn format_rfc3339_secs(secs: i64) -> Option<String> {
    format_rfc3339_ms(secs.checked_mul(1000)?)
}

/// Parse a `--since`/`--until` bound. Accepted forms:
///   RFC 3339 with offset (`2026-10-09T12:00:00Z`, `...+09:00`),
///   ISO date-time without offset (`2026-10-09T12:00:00`,
///   `2026-10-09 12:00:00`) read as UTC, and date-only (`2026-10-09`)
///   read as midnight UTC.
pub fn parse_bound(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Some(ts.as_millisecond());
    }
    if let Ok(dt) = s.parse::<DateTime>() {
        return civil_ms(dt);
    }
    if let Ok(d) = s.parse::<Date>() {
        return civil_ms(d.to_datetime(Time::MIN));
    }
    None
}

fn civil_ms(dt: DateTime) -> Option<i64> {
    dt.to_zoned(TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp().as_millisecond())
}

/// Extract a timestamp from a log line. Checked in order:
///   * JSONL object: `ts`/`time`/`timestamp` field — a number is epoch
///     milliseconds (the existing `timestamp_ms` contract), a string is
///     parsed with the `parse_bound` forms; or journald
///     `__REALTIME_TIMESTAMP` (epoch microseconds as string or integer);
///   * leading ISO/RFC3339 date-time or date prefix (`2026-10-09T…`,
///     `2026-10-09 …`), optionally preceded by a syslog `<PRI>` header;
///   * leading syslog `Mmm DD HH:MM:SS` (`Oct  9 12:00:00`) — yearless,
///     so `syslog_year` supplies the year.
///
/// Returns None when no timestamp can be determined.
pub fn extract_timestamp_ms(raw: &str, syslog_year: i32) -> Option<i64> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(trimmed)
        {
            let mut realtime_usec: Option<i64> = None;
            for (k, v) in &map {
                if k.eq_ignore_ascii_case("ts")
                    || k.eq_ignore_ascii_case("time")
                    || k.eq_ignore_ascii_case("timestamp")
                {
                    if let Some(n) = v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)) {
                        return Some(n as i64);
                    }
                    if let Some(f) = v.as_f64() {
                        return Some(f as i64);
                    }
                    if let Some(s) = v.as_str() {
                        return parse_bound(s);
                    }
                    return None;
                } else if k.eq_ignore_ascii_case("__REALTIME_TIMESTAMP") {
                    if let Some(n) = v
                        .as_u64()
                        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    {
                        realtime_usec = Some((n / 1000) as i64);
                    }
                }
            }
            return realtime_usec;
        }
    }
    text_prefix_ms(trimmed, syslog_year)
}

/// Strip an optional leading syslog `<PRI>` (or RFC 5424 `<PRI>1 `) prefix.
pub(crate) fn strip_syslog_pri_prefix(s: &str) -> &str {
    if let Some(inner) = s.strip_prefix('<') {
        if let Some(end) = inner.find('>') {
            let digits = &inner[..end];
            if !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit())
            {
                let after = &inner[end + 1..];
                let after_bytes = after.as_bytes();
                // RFC 5424 `<PRI>VERSION SP` (e.g. `<13>1 2026-10-09T...`)
                if after_bytes.len() >= 2
                    && after_bytes[0].is_ascii_digit()
                    && after_bytes[1].is_ascii_whitespace()
                {
                    return after[2..].trim_start();
                }
                return after.trim_start();
            }
        }
    }
    s
}

fn text_prefix_ms(s: &str, syslog_year: i32) -> Option<i64> {
    let s = strip_syslog_pri_prefix(s);
    let b = s.as_bytes();
    // ISO/RFC3339 family: YYYY-MM-DD prefix.
    if b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
    {
        // `T` form: the timestamp is a single whitespace-free token and
        // may carry an offset.
        if b.get(10) == Some(&b'T') {
            let end = s.find(|c: char| c.is_ascii_whitespace()).unwrap_or(s.len());
            if let Ok(ts) = s[..end].parse::<jiff::Timestamp>() {
                return Some(ts.as_millisecond());
            }
            if let Ok(dt) = s[..end].parse::<DateTime>() {
                return civil_ms(dt);
            }
            return None;
        }
        // ` ` form: "YYYY-MM-DD HH:MM:SS[.frac][Z|+HH:MM]"
        if b.get(10) == Some(&b' ') && looks_like_hhmmss(&b[11..]) {
            let time_end = s[11..]
                .find(|c: char| c.is_ascii_whitespace())
                .map(|idx| 11 + idx)
                .unwrap_or(s.len());
            let token = &s[..time_end];
            if let Ok(ts) = token.parse::<jiff::Timestamp>() {
                return Some(ts.as_millisecond());
            }
            if let Ok(dt) = token.parse::<DateTime>() {
                return civil_ms(dt);
            }
            return None;
        }
        // Bare date prefix — next char must end the token.
        if matches!(b.get(10), None | Some(&b' ') | Some(&b'\t')) {
            if let Ok(d) = s[..10].parse::<Date>() {
                return civil_ms(d.to_datetime(Time::MIN));
            }
        }
        return None;
    }
    syslog_prefix_ms(s, syslog_year)
}

/// `HH:MM:SS` shape at the start of `b`.
fn looks_like_hhmmss(b: &[u8]) -> bool {
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

fn month_num(b: &[u8]) -> Option<i8> {
    const MONTHS: [[u8; 3]; 12] = [
        *b"Jan", *b"Feb", *b"Mar", *b"Apr", *b"May", *b"Jun", *b"Jul", *b"Aug", *b"Sep", *b"Oct",
        *b"Nov", *b"Dec",
    ];
    MONTHS.iter().position(|m| m == b).map(|i| i as i8 + 1)
}

fn two_digits(b: &[u8]) -> Option<i8> {
    std::str::from_utf8(&b[..2]).ok()?.parse().ok()
}

/// Syslog `Mmm DD HH:MM:SS` prefix; the day may be space- or
/// zero-padded. `syslog_year` supplies the year the format lacks.
fn syslog_prefix_ms(s: &str, syslog_year: i32) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 15 {
        return None;
    }
    let mon = month_num(b.get(..3)?)?;
    let mut i = 3;
    while b.get(i) == Some(&b' ') {
        i += 1;
    }
    let day_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if !(1..=2).contains(&(i - day_start)) {
        return None;
    }
    let day: i8 = std::str::from_utf8(&b[day_start..i]).ok()?.parse().ok()?;
    if b.get(i) != Some(&b' ') || !looks_like_hhmmss(b.get(i + 1..)?) {
        return None;
    }
    let hms = &b[i + 1..i + 9];
    let dt = DateTime::new(
        syslog_year as i16,
        mon,
        day,
        two_digits(&hms[0..2])?,
        two_digits(&hms[3..5])?,
        two_digits(&hms[6..8])?,
        0,
    )
    .ok()?;
    civil_ms(dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(y: i16, m: i8, d: i8, h: i8, mi: i8, s: i8) -> i64 {
        DateTime::new(y, m, d, h, mi, s, 0)
            .unwrap()
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    #[test]
    fn parse_bound_accepts_documented_forms() {
        assert_eq!(
            parse_bound("2026-10-09T12:00:00Z"),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        // +09:00 offset shifts the instant three hours earlier in UTC.
        assert_eq!(
            parse_bound("2026-10-09T12:00:00+09:00"),
            Some(ms(2026, 10, 9, 3, 0, 0))
        );
        assert_eq!(
            parse_bound("2026-10-09 12:00:00"),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        assert_eq!(parse_bound("2026-10-09"), Some(ms(2026, 10, 9, 0, 0, 0)));
        assert_eq!(parse_bound("nonsense"), None);
        assert_eq!(parse_bound("2026-13-40"), None);
    }

    #[test]
    fn extract_iso_prefixes() {
        let y = 2026;
        assert_eq!(
            extract_timestamp_ms("2026-10-09T12:00:00Z INFO hello", y),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        assert_eq!(
            extract_timestamp_ms("2026-10-09T12:00:00.500+02:00 x", y),
            Some(ms(2026, 10, 9, 10, 0, 0) + 500)
        );
        assert_eq!(
            extract_timestamp_ms("2026-10-09 12:00:00 INFO hello", y),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        assert_eq!(
            extract_timestamp_ms("2026-10-09 something", y),
            Some(ms(2026, 10, 9, 0, 0, 0))
        );
    }

    #[test]
    fn extract_syslog_prefix_uses_given_year() {
        assert_eq!(
            extract_timestamp_ms("Oct  9 12:00:00 host kernel: x", 2025),
            Some(ms(2025, 10, 9, 12, 0, 0))
        );
        assert_eq!(
            extract_timestamp_ms("Oct 09 12:00:00 host kernel: x", 2026),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
    }

    #[test]
    fn extract_jsonl_ts_fields() {
        let y = 2026;
        // Numeric ts: epoch milliseconds (the existing contract).
        assert_eq!(
            extract_timestamp_ms(r#"{"ts":1791546720000,"msg":"x"}"#, y),
            Some(1_791_546_720_000)
        );
        // String ts: same forms as --since/--until.
        assert_eq!(
            extract_timestamp_ms(r#"{"time":"2026-10-09T12:00:00Z","msg":"x"}"#, y),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        assert_eq!(
            extract_timestamp_ms(r#"{"timestamp":"2026-10-09","msg":"x"}"#, y),
            Some(ms(2026, 10, 9, 0, 0, 0))
        );
        // No ts field, not JSON at all → undeterminable.
        assert_eq!(extract_timestamp_ms(r#"{"msg":"x"}"#, y), None);
        assert_eq!(extract_timestamp_ms("just a line", y), None);
        assert_eq!(extract_timestamp_ms("pid 1234 started", y), None);
    }

    #[test]
    fn test_l04_space_separated_iso_offset_syslog_pri_and_realtime_timestamp() {
        let y = 2026;
        // Space-separated ISO with timezone offset (+09:00)
        assert_eq!(
            extract_timestamp_ms("2026-10-09 12:00:00+09:00 INFO hello", y),
            Some(ms(2026, 10, 9, 3, 0, 0))
        );
        // Leading <PRI> on syslog line
        assert_eq!(
            extract_timestamp_ms("<134>Oct  9 12:00:00 host app: msg", 2026),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        // Leading <PRI>1 on RFC5424 ISO line
        assert_eq!(
            extract_timestamp_ms("<13>1 2026-10-09T12:00:00Z host app - - - msg", 2026),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
        // journald __REALTIME_TIMESTAMP in microseconds
        assert_eq!(
            extract_timestamp_ms(
                r#"{"__REALTIME_TIMESTAMP":"1791547200000000","MESSAGE":"hi"}"#,
                y
            ),
            Some(ms(2026, 10, 9, 12, 0, 0))
        );
    }
}
