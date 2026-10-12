use std::path::Path;

use lens_core::{LensError, Result};
use lens_log::{LogFilter, LogIndexer, LogLevel};

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
