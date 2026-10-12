use std::path::{Path, PathBuf};

use lens_core::{glob::glob_match, LensError, Result};
use lens_test::{diff_test_runs, parse_junit_xml, TestDiff, TestRun};

use crate::{Report, Severity};

/// `test diff`/`test parse` (and MCP `lens_test_diff`) accept a file, a
/// directory (all `*.xml` inside, sorted), or a `*`/`?` filename glob.
/// CI pipelines commonly produce one JUnit file per module.
pub fn expand_test_inputs(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = lens_core::read_policy::read_dir(path)
            .map_err(|e| LensError::Io {
                path: path.to_path_buf(),
                source: e,
            })?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "xml"))
            .collect();
        files.sort();
        return Ok(files);
    }
    let s = path.to_string_lossy();
    if s.contains('*') || s.contains('?') {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let pat = path.file_name().map(|f| f.to_string_lossy().into_owned());
        if let Some(pat) = pat {
            let mut files: Vec<PathBuf> = lens_core::read_policy::read_dir(parent)
                .map_err(|e| LensError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|f| glob_match(&pat, &f.to_string_lossy()))
                        .unwrap_or(false)
                })
                .collect();
            files.sort();
            return Ok(files);
        }
    }
    Ok(vec![path.to_path_buf()])
}

/// Parse one or more JUnit files into a single merged run.
pub fn load_run(path: &Path, project: &str) -> Result<TestRun> {
    let files = expand_test_inputs(path)?;
    if files.is_empty() {
        return Err(LensError::Usage {
            message: format!("{:?} matched no JUnit XML files", path),
        });
    }
    let mut runs = Vec::new();
    for f in &files {
        let bytes = lens_core::read_policy::read(f).map_err(|e| LensError::Io {
            path: f.clone(),
            source: e,
        })?;
        runs.push(parse_junit_xml(&bytes, project)?);
    }
    Ok(lens_test::merge_test_runs(runs, project))
}

/// Parse a test run: a truncated/malformed report is incomplete
/// evidence (exit 1); failing cases are a Warn finding.
pub fn parse(path: &Path, project: &str) -> Result<Report<TestRun>> {
    let run = load_run(path, project)?;
    let mut report = Report::new(run);
    let bad = report.data.summary.failed + report.data.summary.errors;
    if bad > 0 {
        report.push(
            Severity::Warn,
            "test.failures",
            format!("{bad} failed/error test case(s) in the run"),
        );
    }
    report.complete = report.data.complete;
    Ok(report)
}

fn change_message(c: &lens_test::CaseChange) -> String {
    format!(
        "{}{}",
        c.id,
        c.message
            .as_deref()
            .map(|m| format!(": {m}"))
            .unwrap_or_default()
    )
}

/// Diff two test runs. Regressions and brand-new failures are Fail
/// findings; removed tests are Warn; an incomplete input marks the
/// report incomplete.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<TestDiff>> {
    let run_base = load_run(baseline, "baseline")?;
    let run_cand = load_run(candidate, "candidate")?;
    Ok(diff_runs(&run_base, &run_cand))
}

/// Value-level classifier shared by the file diff and `bundle diff`.
/// An incomplete run on either side marks the report incomplete.
pub fn diff_runs(run_base: &TestRun, run_cand: &TestRun) -> Report<TestDiff> {
    let complete = run_base.complete && run_cand.complete;
    let diff = diff_test_runs(run_base, run_cand);
    let (regressions, new_failures, removed): (Vec<String>, Vec<String>, Vec<String>) = (
        diff.regressions.iter().map(change_message).collect(),
        diff.new_failures.iter().map(change_message).collect(),
        diff.removed_tests.iter().map(|r| r.id.clone()).collect(),
    );
    let mut report = Report::new(diff);
    report.complete = complete;
    for r in regressions {
        report.push(Severity::Fail, "test.regression", r);
    }
    for r in new_failures {
        report.push(Severity::Fail, "test.new_failure", r);
    }
    for r in removed {
        report.push(
            Severity::Warn,
            "test.removed",
            format!("{r} removed from candidate run"),
        );
    }
    report
}
