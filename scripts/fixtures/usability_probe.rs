use lens_core::Result;
use lens_log::{parse_line, LogFilter, LogLevel};
use lens_ops::{log::*, Report};
use std::collections::BTreeMap;
use std::path::PathBuf;
fn old_stats(
    inputs: &[PathBuf],
    filter: &LogFilter,
    bucket_secs: Option<u64>,
) -> Result<Report<LogStats>> {
    let mut total_lines = 0u64;
    let mut lossy_lines = 0usize;
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
        lossy_lines += indexer.lossy_lines();
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
    let mut report = Report::new(LogStats {
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
    });
    if lossy_lines > 0 {
        report.complete = false;
        report.diagnostics.push(format!(
            "{lossy_lines} line(s) contained invalid UTF-8; parsed text is lossy"
        ));
    }
    Ok(report)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[2]);
    if args[1] == "index" {
        let index = lens_log::LogIndexer::open(&path).unwrap();
        println!(
            "lines={} span_bytes_per_line={}",
            index.len(),
            std::mem::size_of::<lens_log::indexer::LineSpan>()
        );
        return;
    }
    if args[1] == "cancel" {
        let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_token = token.clone();
        let start = std::time::Instant::now();
        let worker = std::thread::spawn(move || {
            lens_disk::DiskScanner::new(lens_disk::ScanOptions::default())
                .with_cancellation(worker_token)
                .scan(&path)
                .unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(2));
        let cancelled = std::time::Instant::now();
        token.store(true, std::sync::atomic::Ordering::Relaxed);
        let result = worker.join().unwrap();
        println!(
            "total_ms={:.3} cancellation_to_join_ms={:.3} scanned_entries={} errors={:?}",
            start.elapsed().as_secs_f64() * 1000.,
            cancelled.elapsed().as_secs_f64() * 1000.,
            result.scanned_entries,
            result.errors
        );
        return;
    }

    let report = if args[1] == "old" {
        old_stats(&[path], &LogFilter::new(), None)
    } else {
        stats(&[path], &LogFilter::new(), None)
    }
    .unwrap();
    println!("{}", serde_json::to_string(&report.data).unwrap());
}
