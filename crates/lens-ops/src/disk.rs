use std::path::Path;

use lens_core::Result;
use lens_disk::{DiskScanner, DuplicateFinder, DuplicateReport, ScanOptions, ScanResult};

use crate::{Report, Severity};

/// Scan a directory tree. Scan errors become diagnostics and an
/// incomplete/truncated scan marks the report incomplete — evidence
/// gaps are actionable (exit 1) in both text and JSON modes.
pub fn scan(path: &Path, options: ScanOptions) -> Result<Report<ScanResult>> {
    let result = DiskScanner::new(options).scan(path)?;
    let mut report = Report::new(result);
    report.diagnostics = report.data.errors.clone();
    report.complete = report.data.complete && !report.data.truncated;
    Ok(report)
}

/// Find duplicate files. Scan gaps or hash failures mark the report
/// incomplete; found groups are an Info finding.
pub fn duplicates(path: &Path, min_size: u64) -> Result<Report<DuplicateReport>> {
    let res = DiskScanner::new(ScanOptions::default()).scan(path)?;
    let scan_complete = res.complete && !res.truncated;
    let scan_errors = res.errors.clone();
    let dup = DuplicateFinder::new(min_size).find_in_tree(&res.tree, path)?;
    let mut report = Report::new(dup);
    report.diagnostics = scan_errors;
    if !report.data.groups.is_empty() {
        report.push(
            Severity::Info,
            "disk.duplicates",
            format!(
                "{} duplicate group(s), {} bytes reclaimable",
                report.data.groups.len(),
                report.data.total_reclaimable_bytes
            ),
        );
    }
    report.complete = scan_complete && report.data.errors.is_empty();
    Ok(report)
}
