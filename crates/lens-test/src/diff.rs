use lens_core::SetDiff;
use std::collections::{BTreeMap, HashMap};

use crate::model::*;

/// Merge multiple parsed JUnit runs (per-module reports) into one logical
/// run for diffing. Summaries are summed, `complete` is the conjunction,
/// and the run id is recomputed over the merged case set so identical
/// inputs still produce identical ids.
pub fn merge_test_runs(runs: Vec<TestRun>, project_name: &str) -> TestRun {
    let mut cases = Vec::new();
    let mut summary = TestSummary::default();
    let mut properties = BTreeMap::new();
    let mut suite_output = String::new();
    let mut complete = true;
    for run in runs {
        complete &= run.complete;
        summary.total += run.summary.total;
        summary.passed += run.summary.passed;
        summary.failed += run.summary.failed;
        summary.errors += run.summary.errors;
        summary.skipped += run.summary.skipped;
        summary.duration_sec += run.summary.duration_sec;
        properties.extend(run.properties);
        if let Some(out) = run.suite_output {
            if !suite_output.is_empty() {
                suite_output.push('\n');
            }
            suite_output.push_str(&out);
        }
        cases.extend(run.cases);
    }

    let run_id = {
        let mut h = lens_core::IncrementalHasher::new();
        h.update(project_name.as_bytes());
        for c in &cases {
            h.update(c.identity.as_bytes());
            h.update(&[c.status as u8]);
        }
        let digest = h.finish();
        format!("{}-{}", project_name, &digest[..12])
    };

    TestRun {
        schema: RUN_SCHEMA_V1.to_string(),
        producer: TestProducer {
            name: "testlens".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        run_id,
        project: project_name.to_string(),
        collected_at: lens_core::time::utc_now_iso(),
        complete,
        summary,
        cases,
        properties,
        suite_output: if suite_output.is_empty() {
            None
        } else {
            Some(suite_output)
        },
    }
}

fn change(
    id: &str,
    before: Option<TestStatus>,
    after: Option<TestStatus>,
    msg: Option<String>,
) -> CaseChange {
    CaseChange {
        id: id.to_string(),
        before,
        after,
        message: msg,
    }
}

fn status_severity(status: TestStatus) -> u8 {
    match status {
        TestStatus::Skipped => 0,
        TestStatus::Passed => 1,
        TestStatus::Failed => 2,
        TestStatus::Error => 3,
    }
}

fn should_replace_duplicate(existing: &TestCase, incoming: &TestCase) -> bool {
    let e_sev = status_severity(existing.status);
    let i_sev = status_severity(incoming.status);
    i_sev > e_sev || (i_sev == e_sev && existing.message.is_none() && incoming.message.is_some())
}

pub fn diff_test_runs(baseline: &TestRun, candidate: &TestRun) -> TestDiff {
    let mut diagnostics = Vec::new();
    let mut baseline_map: HashMap<&str, &TestCase> = HashMap::new();
    let mut candidate_map: HashMap<&str, &TestCase> = HashMap::new();
    for c in &baseline.cases {
        match baseline_map.get_mut(c.identity.as_str()) {
            Some(existing) => {
                diagnostics.push(format!(
                    "duplicate case identity in baseline: {}",
                    c.identity
                ));
                if should_replace_duplicate(existing, c) {
                    *existing = c;
                }
            }
            None => {
                baseline_map.insert(c.identity.as_str(), c);
            }
        }
    }
    for c in &candidate.cases {
        match candidate_map.get_mut(c.identity.as_str()) {
            Some(existing) => {
                diagnostics.push(format!(
                    "duplicate case identity in candidate: {}",
                    c.identity
                ));
                if should_replace_duplicate(existing, c) {
                    *existing = c;
                }
            }
            None => {
                candidate_map.insert(c.identity.as_str(), c);
            }
        }
    }

    let mut regressions = Vec::new();
    let mut fixes = Vec::new();
    let mut new_failures = Vec::new();
    let mut removed_tests = Vec::new();
    let mut skipped_changes = Vec::new();

    for (&identity, &c_case) in &candidate_map {
        match baseline_map.get(identity) {
            Some(&b_case) => {
                let b = b_case.status;
                let c = c_case.status;
                if b == c {
                    continue;
                }
                let msg = c_case.message.clone();
                if b == TestStatus::Skipped || c == TestStatus::Skipped {
                    // Any transition to or from skipped is reported as a
                    // skip change, never as a fix or regression.
                    skipped_changes.push(change(identity, Some(b), Some(c), msg));
                } else if b == TestStatus::Passed {
                    // passed -> failed/error
                    regressions.push(change(identity, Some(b), Some(c), msg));
                } else if c == TestStatus::Passed {
                    // failed/error -> passed
                    fixes.push(change(identity, Some(b), Some(c), msg));
                }
                // failed <-> error transitions are status noise; they stay
                // in cases/summary_delta only.
            }
            None => {
                if matches!(c_case.status, TestStatus::Failed | TestStatus::Error) {
                    new_failures.push(change(
                        identity,
                        None,
                        Some(c_case.status),
                        c_case.message.clone(),
                    ));
                }
            }
        }
    }
    for (&identity, &b_case) in &baseline_map {
        if !candidate_map.contains_key(identity) {
            removed_tests.push(change(identity, Some(b_case.status), None, None));
        }
    }

    let by_id = |a: &CaseChange, b: &CaseChange| a.id.cmp(&b.id);
    regressions.sort_by(by_id);
    fixes.sort_by(by_id);
    new_failures.sort_by(by_id);
    removed_tests.sort_by(by_id);
    skipped_changes.sort_by(by_id);
    diagnostics.sort();
    diagnostics.dedup();

    let baseline_ids: Vec<String> = baseline.cases.iter().map(|c| c.identity.clone()).collect();
    let candidate_ids: Vec<String> = candidate.cases.iter().map(|c| c.identity.clone()).collect();
    let cases_diff = SetDiff::compute(baseline_ids, candidate_ids);

    let failed_delta = (candidate.summary.failed + candidate.summary.errors) as i64
        - (baseline.summary.failed + baseline.summary.errors) as i64;
    let passed_delta = candidate.summary.passed as i64 - baseline.summary.passed as i64;
    let duration_delta_sec = candidate.summary.duration_sec - baseline.summary.duration_sec;

    TestDiff {
        schema: DIFF_SCHEMA_V2.to_string(),
        baseline_run_id: baseline.run_id.clone(),
        candidate_run_id: candidate.run_id.clone(),
        regressions,
        fixes,
        new_failures,
        removed_tests,
        skipped_changes,
        cases: cases_diff,
        summary_delta: SummaryDelta {
            failed_delta,
            passed_delta,
            duration_delta_sec,
        },
        diagnostics,
    }
}
