use std::path::Path;

use lens_core::{LensError, Result};
use lens_trace::{diff_snapshots, TraceDiff, TraceSnapshot};

use crate::{Report, Severity};

/// Read a trace input and fail closed when it is not strace output —
/// shared by `trace analyze`, both sides of `trace diff`, the
/// `bundle create --trace` collector, and the MCP tool so they apply
/// identical rules.
pub fn analyze_file(path: &Path) -> Result<TraceSnapshot> {
    let content = std::fs::read_to_string(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    lens_trace::analyze_strace_text(&content).map_err(|e| LensError::InvalidInput {
        message: format!("{:?} {}", path, e),
    })
}

/// Analyze a strace file: leaked fds are a Fail finding.
pub fn analyze(path: &Path) -> Result<Report<TraceSnapshot>> {
    let snapshot = analyze_file(path)?;
    let leaks = snapshot.fd_leaks.clone();
    let mut report = Report::new(snapshot);
    if !leaks.is_empty() {
        report.push(
            Severity::Fail,
            "trace.fd_leak",
            format!("{} leaked fd(s): {:?}", leaks.len(), leaks),
        );
    }
    Ok(report)
}

/// Diff two strace inputs. New errors are Fail findings; significant
/// latency shifts are Warn.
pub fn diff_files(baseline: &Path, candidate: &Path) -> Result<Report<TraceDiff>> {
    let s1 = analyze_file(baseline)?;
    let s2 = analyze_file(candidate)?;
    Ok(diff_values(&s1, &s2))
}

/// Value-level classifier shared by the file diff and `bundle diff`.
pub fn diff_values(s1: &TraceSnapshot, s2: &TraceSnapshot) -> Report<TraceDiff> {
    let diff = diff_snapshots(s1, s2);
    let (new_errors, shifts) = (
        diff.new_errors.clone(),
        diff.significant_latency_shifts.clone(),
    );
    let mut report = Report::new(diff);
    for e in new_errors {
        report.push(Severity::Fail, "trace.new_error", e);
    }
    for s in shifts {
        report.push(Severity::Warn, "trace.latency_shift", s);
    }
    report
}
