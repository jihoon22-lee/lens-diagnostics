use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use lens_core::{glob::glob_match, LensError, Result};
use lens_log::{parse_line, LogFilter, LogIndexer, LogLevel};

use crate::Report;

/// Open a `log` input: `-` reads stdin through a bounded buffer; `.gz`
/// files decompress through a bounded reader; the rest are memory-mapped.
pub fn open(path: &Path) -> Result<LogIndexer> {
    if path.as_os_str() == "-" {
        let stdin = std::io::stdin();
        let lock = stdin.lock();
        return LogIndexer::from_reader(lock, "-");
    }
    LogIndexer::open(path)
}

/// Filter arguments shared by the CLI flags and the MCP tool schema.
#[derive(Debug, Default, Clone)]
pub struct FilterArgs {
    pub query: Option<String>,
    pub regex: Option<String>,
    pub min_level: Option<String>,
    pub include_unknown: bool,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<i32>,
}

/// Build a `LogFilter` from `args` — one implementation of the
/// validation both `lens log filter` and `lens_log_filter` apply,
/// keeping the CLI's message texts.
pub fn build_filter(args: &FilterArgs) -> Result<LogFilter> {
    let mut filter = LogFilter::new();
    if let Some(q) = &args.query {
        filter = filter.with_query(q);
    }
    if let Some(re) = &args.regex {
        filter = filter.with_regex(re).map_err(|e| LensError::Usage {
            message: format!("invalid --regex {:?}: {}", re, e),
        })?;
    }
    if let Some(lvl_str) = &args.min_level {
        let lvl = LogLevel::parse(lvl_str);
        if lvl == LogLevel::Unknown && !lvl_str.eq_ignore_ascii_case("unknown") {
            return Err(LensError::Usage {
                message: format!(
                    "invalid --min-level {lvl_str:?}; expected trace|debug|info|warn|error|fatal"
                ),
            });
        }
        filter = filter.with_min_level(lvl);
    }
    filter = filter.with_include_unknown(args.include_unknown);
    let parse_ts = |flag: &str, s: &str| -> Result<i64> {
        lens_log::timestamp::parse_bound(s).ok_or_else(|| LensError::Usage {
            message: format!(
                "invalid {flag} {s:?}; expected RFC 3339 (e.g. 2026-10-09T12:00:00Z), 'YYYY-MM-DD HH:MM:SS' or 'YYYY-MM-DD' (UTC)"
            ),
        })
    };
    if let Some(s) = &args.since {
        filter = filter.with_since(Some(parse_ts("--since", s)?));
    }
    if let Some(u) = &args.until {
        filter = filter.with_until(Some(parse_ts("--until", u)?));
    }
    if let Some(y) = args.year {
        if !(1..=9999).contains(&y) {
            return Err(LensError::Usage {
                message: format!("invalid --year {y}; expected 1..=9999"),
            });
        }
        filter = filter.with_syslog_year(y);
    }
    Ok(filter)
}

/// Expand `log` input arguments: `-` (stdin, at most once), plain files,
/// `.gz` files, and `*`/`?` filename globs (expanded against the parent
/// directory, sorted). A glob matching nothing is a usage error — silent
/// empty input would look like an empty log.
pub fn expand_inputs(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stdin_seen = false;
    for path in paths {
        if path.as_os_str() == "-" {
            if stdin_seen {
                return Err(LensError::Usage {
                    message: "stdin (`-`) may appear at most once".to_string(),
                });
            }
            stdin_seen = true;
            out.push(path.clone());
            continue;
        }
        let s = path.to_string_lossy();
        if s.contains('*') || s.contains('?') {
            // `Path::parent` of a bare `*.log` is `""` — read "." instead.
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let pat = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut matches: Vec<PathBuf> = fs::read_dir(parent)
                .map_err(|e| LensError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .map(|f| glob_match(&pat, &f.to_string_lossy()))
                            .unwrap_or(false)
                })
                .collect();
            matches.sort();
            if matches.is_empty() {
                return Err(LensError::Usage {
                    message: format!("{s:?}: pattern matched nothing"),
                });
            }
            out.extend(matches);
        } else {
            out.push(path.clone());
        }
    }
    Ok(out)
}

/// What one `Follower::poll` call observed: any complete new lines plus
/// an advisory note when the file was truncated or rotated underneath us.
#[derive(Debug, Default)]
pub struct FollowPoll {
    pub lines: Vec<String>,
    pub note: Option<String>,
}

/// Tail-over-new-appends for `log filter --follow`. Tracks the byte
/// offset and the (dev, inode) identity so truncation (file shrank)
/// and rotation (path now points at a different file) both restart
/// from offset 0 instead of silently missing or repeating content.
pub struct Follower {
    path: PathBuf,
    offset: u64,
    identity: (u64, u64),
    /// Bytes after the last complete `\n` — emitted once the newline
    /// (or more of the line) arrives on a later poll.
    partial: Vec<u8>,
}

impl Follower {
    /// Start following `path` at `start_offset` (typically the size the
    /// initial filtered pass already consumed).
    pub fn new(path: &Path, start_offset: u64) -> Result<Self> {
        let md = fs::metadata(path).map_err(|e| LensError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            offset: start_offset,
            identity: identity_of(&md),
            partial: Vec::new(),
        })
    }

    /// Return complete lines appended since the last poll. Invalid UTF-8
    /// is lossy-decoded (U+FFFD) and a trailing `\r` is stripped, matching
    /// how indexed lines are shown.
    pub fn poll(&mut self) -> Result<FollowPoll> {
        // Open first: metadata and bytes must refer to the same inode even
        // if rotation happens during this poll.
        let mut file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FollowPoll::default()),
            Err(source) => {
                return Err(LensError::Io {
                    path: self.path.clone(),
                    source,
                })
            }
        };
        let md = file.metadata().map_err(|e| LensError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let identity = identity_of(&md);
        let mut note = None;
        if identity != self.identity {
            note = Some(format!(
                "{}: file rotated, restarting from the beginning",
                self.path.display()
            ));
            self.identity = identity;
            self.offset = 0;
            self.partial.clear();
        } else if md.len() < self.offset {
            note = Some(format!(
                "{}: file truncated, restarting from the beginning",
                self.path.display()
            ));
            self.offset = 0;
            self.partial.clear();
        }
        if md.len() == self.offset {
            return Ok(FollowPoll {
                lines: vec![],
                note,
            });
        }
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| LensError::Io {
                path: self.path.clone(),
                source: e,
            })?;
        let mut data = std::mem::take(&mut self.partial);
        let old_len = data.len() as u64;
        file.read_to_end(&mut data).map_err(|e| LensError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        self.offset += data.len() as u64 - old_len;

        let mut lines = Vec::new();
        let mut start = 0usize;
        for (i, b) in data.iter().enumerate() {
            if *b == b'\n' {
                let mut line = &data[start..i];
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                lines.push(String::from_utf8_lossy(line).into_owned());
                start = i + 1;
            }
        }
        self.partial = data[start..].to_vec();
        Ok(FollowPoll { lines, note })
    }
}

#[cfg(unix)]
fn identity_of(md: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn identity_of(md: &fs::Metadata) -> (u64, u64) {
    (0, md.len())
}

/// One time bucket in a `log stats` histogram.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogBucket {
    /// Bucket start as RFC 3339 UTC (`2026-10-09T12:00Z`).
    pub start: String,
    pub total: u64,
    pub warn_or_higher: u64,
}

/// `lens.log-stats/v1` — level counts and a time histogram over the
/// lines the filter kept.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogStats {
    pub schema: &'static str,
    pub inputs: Vec<String>,
    pub total_lines: u64,
    pub matched_lines: u64,
    /// Level name (`trace`…`fatal`, `unknown`) → matched line count.
    pub by_level: BTreeMap<String, u64>,
    /// RFC 3339 UTC of the first/last matched timestamped line.
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    /// Matched lines whose timestamp could not be determined.
    pub unknown_timestamp: u64,
    /// Non-empty buckets, sorted by start.
    pub buckets: Vec<LogBucket>,
}

/// Bucket widths selectable via `--bucket`/`bucket`.
pub const BUCKET_CHOICES: [u64; 5] = [60, 300, 900, 3600, 86_400];

/// Pick the smallest standard bucket giving at most 60 buckets across
/// `span_secs`; falls back to 1h for degenerate spans.
pub fn pick_bucket_secs(span_secs: u64) -> u64 {
    BUCKET_CHOICES
        .iter()
        .copied()
        .find(|b| span_secs / b <= 60)
        .unwrap_or(86_400)
}

/// Count lines, levels, and timestamp buckets across `inputs` for the
/// lines `filter` keeps. `bucket_secs=None` auto-picks from
/// [`BUCKET_CHOICES`]. Stats describe the matched view — filtering to
/// `--min-level warn` scopes the histogram to warn+ lines.
pub fn stats(
    inputs: &[PathBuf],
    filter: &LogFilter,
    bucket_secs: Option<u64>,
) -> Result<Report<LogStats>> {
    let mut total_lines = 0u64;
    let mut matched_lines = 0u64;
    let mut by_level: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown_timestamp = 0u64;
    let mut first_ms = i64::MAX;
    let mut last_ms = i64::MIN;
    // (timestamp_ms, warn_or_higher) per matched, timestamped line —
    // remembered so the bucket width can be chosen after the full span
    // is known.
    let mut stamped: Vec<(i64, bool)> = Vec::new();

    for path in inputs {
        let indexer = open(path)?;
        total_lines += indexer.len() as u64;
        for idx in 0..indexer.len() {
            let line = indexer.get_line_lossy(idx).unwrap_or_default();
            let record = parse_line(&line, idx + 1);
            if !filter.matches(&record) {
                continue;
            }
            matched_lines += 1;
            *by_level
                .entry(record.level.as_str().to_string())
                .or_insert(0) += 1;
            // Aggregation uses the filter's syslog_year — `parse_line`
            // would stamp yearless syslog lines with the *current*
            // year, scattering them into wrong buckets under --year.
            if let Some(ts) = lens_log::timestamp::extract_timestamp_ms(&line, filter.syslog_year) {
                first_ms = first_ms.min(ts);
                last_ms = last_ms.max(ts);
                // `Unknown` sorts above `Fatal` in the derived Ord, so
                // it must be excluded explicitly — an undetermined
                // level is not warn-or-higher evidence.
                stamped.push((
                    ts,
                    record.level >= LogLevel::Warn && record.level != LogLevel::Unknown,
                ));
            } else {
                unknown_timestamp += 1;
            }
        }
    }

    let bucket_secs = bucket_secs.unwrap_or_else(|| {
        if first_ms <= last_ms {
            pick_bucket_secs(((last_ms - first_ms) / 1000 + 1) as u64)
        } else {
            3600
        }
    });
    let mut acc: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
    for (ts, warn_plus) in stamped {
        let b = ts.div_euclid(1000).div_euclid(bucket_secs as i64) * bucket_secs as i64;
        let e = acc.entry(b).or_default();
        e.0 += 1;
        if warn_plus {
            e.1 += 1;
        }
    }
    let buckets = acc
        .into_iter()
        .map(|(start, (total, warn_or_higher))| LogBucket {
            start: lens_log::timestamp::format_rfc3339_secs(start).unwrap_or_default(),
            total,
            warn_or_higher,
        })
        .collect();

    let fmt = lens_log::timestamp::format_rfc3339_ms;
    Ok(Report::new(LogStats {
        schema: "lens.log-stats/v1",
        inputs: inputs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        total_lines,
        matched_lines,
        by_level,
        first_ts: (first_ms != i64::MAX).then(|| fmt(first_ms)).flatten(),
        last_ts: (last_ms != i64::MIN).then(|| fmt(last_ms)).flatten(),
        // `flatten` above keeps Option<Option<String>> honest: a parse
        // failure surfaces as null, never a fabricated timestamp.
        unknown_timestamp,
        buckets,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn follower_waits_for_rotation_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "old\npartial").unwrap();
        let mut follower = Follower::new(&path, 0).unwrap();
        assert_eq!(follower.poll().unwrap().lines, vec!["old"]);
        std::fs::rename(&path, dir.path().join("log.1")).unwrap();
        assert!(follower.poll().unwrap().lines.is_empty());
        assert!(follower.poll().unwrap().lines.is_empty());
        std::fs::write(&path, "replacement\n").unwrap();
        assert_eq!(follower.poll().unwrap().lines, vec!["replacement"]);
    }

    #[test]
    fn stats_preserve_epoch_and_negative_timestamp_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "1969-12-31T23:59:59.500Z INFO before\n1970-01-01T00:00:00Z INFO epoch\nDec 31 23:59:59 host kernel: <6>syslog\nunknown\n").unwrap();
        let report = stats(&[path], &LogFilter::new().with_syslog_year(1969), Some(60)).unwrap();
        assert_eq!(report.data.unknown_timestamp, 1);
        assert_eq!(report.data.buckets.len(), 2);
        assert_eq!(report.data.buckets[0].start, "1969-12-31T23:59:00Z");
        assert_eq!(report.data.buckets[0].total, 2);
        assert_eq!(report.data.buckets[1].total, 1);
        assert_eq!(report.data.last_ts.as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    use super::*;
    use std::io::Write;

    fn write(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
    }

    #[test]
    fn expand_inputs_globs_sorts_and_rejects_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("b.log"), b"b\n");
        write(&dir.path().join("a.log"), b"a\n");
        write(&dir.path().join("c.txt"), b"c\n");

        let glob = dir.path().join("*.log");
        let out = expand_inputs(&[glob]).unwrap();
        assert_eq!(
            out,
            vec![dir.path().join("a.log"), dir.path().join("b.log")]
        );

        // `-` at most once.
        let err = expand_inputs(&[PathBuf::from("-"), PathBuf::from("-")]).unwrap_err();
        assert!(err.to_string().contains("at most once"), "{err}");

        // A glob matching nothing is a usage error, not an empty input.
        let err = expand_inputs(&[dir.path().join("*.nope")]).unwrap_err();
        assert!(err.to_string().contains("matched nothing"), "{err}");
    }

    #[test]
    fn follower_reads_appends_and_completes_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.log");
        write(&p, b"first\n");
        let mut f = Follower::new(&p, fs::metadata(&p).unwrap().len()).unwrap();

        // No change → empty poll.
        assert!(f.poll().unwrap().lines.is_empty());

        // A line split across two appends completes on the second poll.
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"par")
            .unwrap();
        assert!(f.poll().unwrap().lines.is_empty());
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"tial\nsecond\r\n")
            .unwrap();
        let poll = f.poll().unwrap();
        assert_eq!(
            poll.lines,
            vec!["partial".to_string(), "second".to_string()]
        );
        assert!(poll.note.is_none());
    }

    #[test]
    fn follower_restarts_on_truncation_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.log");
        write(&p, b"aaaaaaaaaaaaaaaa\n");
        let mut f = Follower::new(&p, fs::metadata(&p).unwrap().len()).unwrap();

        // Truncate: file shrank below the offset → restart at 0.
        write(&p, b"new\n");
        let poll = f.poll().unwrap();
        assert_eq!(poll.lines, vec!["new".to_string()]);
        assert!(poll.note.unwrap().contains("truncated"));

        // Rotate: rename + recreate changes (dev, inode) → restart at 0.
        fs::rename(&p, dir.path().join("f.log.1")).unwrap();
        write(&p, b"rotated\n");
        let poll = f.poll().unwrap();
        assert_eq!(poll.lines, vec!["rotated".to_string()]);
        assert!(poll.note.unwrap().contains("rotated"));
    }

    #[test]
    fn stats_counts_levels_buckets_and_unknown_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.log");
        write(
            &p,
            b"2026-10-09T12:00:01Z INFO boot\n\
              2026-10-09T12:30:02Z WARN low\n\
              2026-10-09T12:30:03Z ERROR boom\n\
              2026-10-09T12:45:04Z some undetermined line\n\
              Oct  9 13:15:00 h kernel: <3>again\n\
              no timestamp at all\n",
        );
        let filter = LogFilter::new().with_syslog_year(2026);
        let report = stats(std::slice::from_ref(&p), &filter, Some(3600)).unwrap();
        let s = &report.data;
        assert_eq!(s.schema, "lens.log-stats/v1");
        assert_eq!(s.total_lines, 6);
        assert_eq!(s.matched_lines, 6);
        assert_eq!(s.by_level.get("info"), Some(&1));
        assert_eq!(s.by_level.get("warn"), Some(&1));
        assert_eq!(s.by_level.get("error"), Some(&2));
        assert_eq!(s.by_level.get("unknown"), Some(&2));
        assert_eq!(s.unknown_timestamp, 1);
        assert_eq!(s.first_ts.as_deref(), Some("2026-10-09T12:00:01Z"));
        // Two buckets: 12:00 (4 lines) and 13:00 (1 syslog line).
        assert_eq!(s.buckets.len(), 2, "{:?}", s.buckets);
        assert_eq!(s.buckets[0].start, "2026-10-09T12:00:00Z");
        assert_eq!(s.buckets[0].total, 4);
        // The timestamped unknown-level line must NOT count as warn+.
        assert_eq!(s.buckets[0].warn_or_higher, 2);
        assert_eq!(s.buckets[1].total, 1);
        assert_eq!(s.buckets[1].warn_or_higher, 1);

        // A min-level filter scopes stats to the matched view.
        let filter = LogFilter::new()
            .with_min_level(LogLevel::Warn)
            .with_include_unknown(false)
            .with_syslog_year(2026);
        let report = stats(&[p], &filter, Some(3600)).unwrap();
        assert_eq!(report.data.matched_lines, 3);
        assert_eq!(report.data.total_lines, 6);
    }

    #[test]
    fn stats_buckets_use_configured_syslog_year() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("y.log");
        // Yearless syslog lines must aggregate under the filter's year,
        // not the host's current year.
        write(
            &p,
            b"Oct  9 13:15:00 h kernel: <6>one\nOct  9 13:45:00 h kernel: <4>two\n",
        );
        let filter = LogFilter::new().with_syslog_year(2020);
        let report = stats(std::slice::from_ref(&p), &filter, Some(3600)).unwrap();
        let s = &report.data;
        assert_eq!(s.first_ts.as_deref(), Some("2020-10-09T13:15:00Z"));
        assert_eq!(s.last_ts.as_deref(), Some("2020-10-09T13:45:00Z"));
        assert_eq!(s.buckets.len(), 1);
        assert_eq!(s.buckets[0].start, "2020-10-09T13:00:00Z");
        assert_eq!(s.buckets[0].total, 2);
    }

    #[test]
    fn pick_bucket_caps_at_sixty_buckets() {
        // ≤60 per choice; a 1-hour span picks 1m, >60 1h buckets picks 1d.
        assert_eq!(pick_bucket_secs(3600), 60);
        assert_eq!(pick_bucket_secs(3700), 300);
        assert_eq!(pick_bucket_secs(61 * 3600), 86_400);
    }
}
