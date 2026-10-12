use crate::record::{LogLevel, LogRecordView};

/// Checks if `haystack` contains `needle_lower` ignoring ASCII case without allocating any memory.
///
/// Invariant: `needle_lower` MUST be pre-lowercased.
///
/// Candidate search: only positions where the needle's first byte can
/// match are verified, so the common reject path is a single memchr scan
/// instead of a per-window comparison.
pub fn contains_insensitive(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let n = needle_lower.as_bytes();
    let hay = haystack.as_bytes();
    if n.len() > hay.len() {
        return false;
    }
    let b = n[0];
    let hit =
        |pos: usize| hay[pos..].len() >= n.len() && hay[pos..pos + n.len()].eq_ignore_ascii_case(n);
    // `needle_lower` is lowercased, so for ASCII letters the byte can
    // appear in either case; other bytes only match themselves.
    if b.is_ascii_alphabetic() {
        memchr::memchr2_iter(b, b.to_ascii_uppercase(), hay).any(hit)
    } else {
        memchr::memchr_iter(b, hay).any(hit)
    }
}

#[derive(Debug, Clone)]
pub struct LogFilter {
    pub min_level: Option<LogLevel>,
    pub query_lower: Option<String>,
    /// Regex alternative to `query_lower` for pattern matching (`--regex`).
    pub query_regex: Option<regex::Regex>,
    pub source: Option<String>,
    /// Keep lines whose level could not be determined even when a
    /// `--min-level` threshold is active (off by default: unknown lines
    /// cannot prove they meet the threshold). Also keeps lines whose
    /// timestamp could not be determined under `--since`/`--until`.
    pub include_unknown: bool,
    /// Inclusive epoch-ms bounds for `--since`/`--until`.
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    /// Year assumed for year-less syslog timestamps (`Oct  9 12:00:00`).
    pub syslog_year: i32,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            min_level: None,
            query_lower: None,
            query_regex: None,
            source: None,
            include_unknown: false,
            since_ms: None,
            until_ms: None,
            syslog_year: crate::timestamp::default_syslog_year(),
        }
    }
}

impl LogFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_level(mut self, level: LogLevel) -> Self {
        self.min_level = Some(level);
        self
    }

    pub fn with_include_unknown(mut self, include_unknown: bool) -> Self {
        self.include_unknown = include_unknown;
        self
    }

    pub fn with_query(mut self, query: &str) -> Self {
        if query.trim().is_empty() {
            self.query_lower = None;
        } else {
            self.query_lower = Some(query.to_ascii_lowercase());
        }
        self
    }

    /// Regex query; mutually exclusive with the substring `query` — the
    /// caller decides precedence. Invalid patterns are a usage error.
    pub fn with_regex(mut self, pattern: &str) -> Result<Self, regex::Error> {
        self.query_regex = Some(regex::Regex::new(pattern)?);
        Ok(self)
    }

    pub fn with_source(mut self, source: &str) -> Self {
        self.source = Some(source.to_string());
        self
    }

    /// Inclusive lower timestamp bound (epoch ms); `None` disables.
    pub fn with_since(mut self, since_ms: Option<i64>) -> Self {
        self.since_ms = since_ms;
        self
    }

    /// Inclusive upper timestamp bound (epoch ms); `None` disables.
    pub fn with_until(mut self, until_ms: Option<i64>) -> Self {
        self.until_ms = until_ms;
        self
    }

    pub fn with_syslog_year(mut self, year: i32) -> Self {
        self.syslog_year = year;
        self
    }

    /// True when `--since`/`--until` is active: lines whose timestamp
    /// cannot be determined are excluded unless `include_unknown`.
    pub fn time_filter_active(&self) -> bool {
        self.since_ms.is_some() || self.until_ms.is_some()
    }

    fn ts_in_bounds(&self, t: i64) -> bool {
        self.since_ms.is_none_or(|s| t >= s) && self.until_ms.is_none_or(|u| t <= u)
    }

    /// Timestamp check shared by `matches`/`matches_line`. Returns
    /// Some(verdict) when the time filter decides, None when inactive.
    fn time_verdict(&self, raw: &str) -> Option<bool> {
        if !self.time_filter_active() {
            return None;
        }
        match crate::timestamp::extract_timestamp_ms(raw, self.syslog_year) {
            Some(t) => Some(self.ts_in_bounds(t)),
            None => Some(self.include_unknown),
        }
    }

    /// Match a raw line without constructing a `LogRecordView` when the
    /// configured filters allow it: the substring check alone can reject, and
    /// a no-op filter accepts everything without parsing.
    pub fn matches_line(&self, raw: &str, line_number: usize) -> bool {
        if let Some(needle_lower) = &self.query_lower {
            if !contains_insensitive(raw, needle_lower) {
                return false;
            }
        }
        if let Some(re) = &self.query_regex {
            if !re.is_match(raw) {
                return false;
            }
        }
        if let Some(verdict) = self.time_verdict(raw) {
            if !verdict {
                return false;
            }
        }
        if self.min_level.is_none() && self.source.is_none() {
            return true;
        }
        self.matches(&crate::parse_line(raw, line_number))
    }

    pub fn matches(&self, record: &LogRecordView) -> bool {
        if !self.min_level_ok(record) {
            return false;
        }
        if let Some(verdict) = self.time_verdict(record.raw) {
            if !verdict {
                return false;
            }
        }

        self.passes_rest(record)
    }

    /// The `--min-level` part of the match, factored out so the
    /// rejected-only-by-unknown-* helpers can check the other filters.
    fn min_level_ok(&self, record: &LogRecordView) -> bool {
        if let Some(min_lvl) = self.min_level {
            if record.level == LogLevel::Unknown {
                // An unidentified level cannot prove it meets the
                // threshold; `--min-level unknown` selects exactly the
                // unknown lines.
                if !self.include_unknown && min_lvl != LogLevel::Unknown {
                    return false;
                }
            } else if record.level < min_lvl {
                return false;
            }
        }
        true
    }

    /// The record passes the source/query filters but is dropped solely
    /// because its level is `Unknown` under a `--min-level` threshold.
    /// Callers use this to report how much evidence was discarded.
    pub fn rejected_only_by_unknown_level(&self, record: &LogRecordView) -> bool {
        matches!(self.min_level, Some(l) if l != LogLevel::Unknown)
            && !self.include_unknown
            && record.level == LogLevel::Unknown
            && self.passes_rest(record)
    }

    /// The record passes level/source/query filters but is dropped
    /// solely because its timestamp cannot be determined while
    /// `--since`/`--until` is active. Callers use this to report how
    /// much evidence was discarded.
    pub fn rejected_only_by_unknown_timestamp(&self, record: &LogRecordView) -> bool {
        self.time_filter_active()
            && !self.include_unknown
            && crate::timestamp::extract_timestamp_ms(record.raw, self.syslog_year).is_none()
            && self.min_level_ok(record)
            && self.passes_rest(record)
    }

    fn passes_rest(&self, record: &LogRecordView) -> bool {
        if let Some(src) = &self.source {
            if !record.source.is_empty() && record.source != src.as_str() {
                return false;
            }
        }

        if let Some(needle_lower) = &self.query_lower {
            if !contains_insensitive(record.raw, needle_lower) {
                return false;
            }
        }
        if let Some(re) = &self.query_regex {
            if !re.is_match(record.raw) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_allocating_contains_insensitive() {
        assert!(contains_insensitive("Error occurred in subsystem", "error"));
        assert!(contains_insensitive("ERROR occurred in subsystem", "error"));
        assert!(contains_insensitive("system error", "error"));
        assert!(!contains_insensitive("All clean", "error"));
    }

    #[test]
    fn test_contains_insensitive_edge_cases() {
        // Match anchored at the very end of the haystack.
        assert!(contains_insensitive("all quiet until ERROR", "error"));
        assert!(contains_insensitive("ERROR", "error"));
        // Overlapping prefix: first-byte candidates at 0 and 1.
        assert!(contains_insensitive("aaab", "aab"));
        // Non-ASCII needle matches by exact bytes (memchr path, not the
        // ASCII-letter memchr2 path).
        assert!(contains_insensitive("[에러] 디스크 오류", "에러"));
        assert!(!contains_insensitive("[경고] 디스크 오류", "에러"));
        // Needle longer than the haystack never matches.
        assert!(!contains_insensitive("err", "error"));
        // Digit/punctuation first byte of the needle.
        assert!(contains_insensitive("exit code 500", "500"));
        assert!(contains_insensitive("tag=[ERROR]", "[error]"));
        assert!(!contains_insensitive("tag=(ERROR)", "[error]"));
        // First-byte candidate exists but the rest does not.
        assert!(!contains_insensitive("erroneous", "error"));
    }

    #[test]
    fn test_min_level_excludes_unknown() {
        let unknown = crate::parse_line("a line with no level token", 1);
        let err = crate::parse_line("[ERROR] boom", 2);
        let filter = LogFilter::new().with_min_level(LogLevel::Error);

        assert!(!filter.matches(&unknown));
        assert!(filter.matches(&err));
        assert!(filter.rejected_only_by_unknown_level(&unknown));
        assert!(!filter.rejected_only_by_unknown_level(&err));

        // Opt-in keeps unknown lines.
        let filter = filter.with_include_unknown(true);
        assert!(filter.matches(&unknown));
        assert!(!filter.rejected_only_by_unknown_level(&unknown));
    }

    #[test]
    fn test_min_level_unknown_selects_only_unknown() {
        let unknown = crate::parse_line("a line with no level token", 1);
        let err = crate::parse_line("[ERROR] boom", 2);
        let filter = LogFilter::new().with_min_level(LogLevel::Unknown);
        assert!(filter.matches(&unknown));
        assert!(!filter.matches(&err));
    }
}
