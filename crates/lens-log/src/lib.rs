//! # Lens Log (`lens-log`)
//!
//! Ultra high-performance log inspection, memory-mapped line indexing, and investigation session engine.
//!
//! Re-architects and replaces legacy `loglens` with:
//! - **Memory-Mapped Line Indexer ([`indexer::LogIndexer`])**: Zero-copy indexing allowing gigabyte-scale logs to open in milliseconds — memory use is bounded by touched file pages plus ~16 bytes of index per line (bounded buffer for `.gz`/stdin).
//! - **Zero-Allocation Case-Insensitive Filter ([`filter::contains_insensitive`])**: Eliminates the 1,000,000+ heap allocations found in the C++ filter engine.
//! - **Structured JSONL & Syslog Parser ([`parser::parse_line`])**: Zero-copy view borrowing directly from memory-mapped slices.
//! - **Session V2 Schema ([`session::SessionV2`])**: 100% compliant with `loglens.session/v2`.

pub mod filter;
pub mod indexer;
pub mod parser;
pub mod record;
pub mod session;
pub mod timestamp;

pub use filter::{contains_insensitive, LogFilter};
pub use indexer::{LineSpan, LogIndexer};
pub use parser::{detect_level, parse_line};
pub use record::{LogLevel, LogRecordView, OwnedLogRecord};
pub use session::{SessionSource, SessionV2, SESSION_SCHEMA_V2};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_indexer_and_filtering() {
        let dir = tempdir().unwrap();
        let log_file = dir.path().join("app.log");
        let content = "2026-10-05 12:00:00 [INFO] System started\n\
                       2026-10-05 12:00:01 [WARN] High memory pressure detected\n\
                       2026-10-05 12:00:02 [ERROR] Database connection lost\n\
                       {\"ts\":12345,\"level\":\"error\",\"msg\":\"Payment gateway unreachable\"}\n";
        fs::write(&log_file, content).unwrap();

        let indexer = LogIndexer::open(&log_file).unwrap();
        assert_eq!(indexer.len(), 4);

        // Line 1: INFO
        let line0 = indexer.get_line(0).unwrap();
        let rec0 = parse_line(line0, 1);
        assert_eq!(rec0.level, LogLevel::Info);

        // Line 2: WARN
        let line1 = indexer.get_line(1).unwrap();
        let rec1 = parse_line(line1, 2);
        assert_eq!(rec1.level, LogLevel::Warn);

        // Line 3: ERROR
        let line2 = indexer.get_line(2).unwrap();
        let rec2 = parse_line(line2, 3);
        assert_eq!(rec2.level, LogLevel::Error);

        // Line 4: JSONL ERROR
        let line3 = indexer.get_line(3).unwrap();
        let rec3 = parse_line(line3, 4);
        assert_eq!(rec3.level, LogLevel::Error);
        assert_eq!(rec3.message, "Payment gateway unreachable");

        // Filter test: only ERROR lines matching "connection"
        let filter = LogFilter::new()
            .with_min_level(LogLevel::Error)
            .with_query("connection");

        assert!(!filter.matches(&rec0));
        assert!(!filter.matches(&rec1));
        assert!(filter.matches(&rec2));
        assert!(!filter.matches(&rec3));

        // L02: --source filter must reject records with empty source and match extracted source
        let src_filter = LogFilter::new().with_source("sshd");
        assert!(!src_filter.matches(&rec2)); // rec2 has empty source
        let ssh_rec = parse_line("Oct  9 12:00:00 host sshd[99]: Connection closed", 5);
        assert!(src_filter.matches(&ssh_rec));
    }
}
