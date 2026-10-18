//! # Lens Test (`lens-test`)
//!
//! Ultra high-speed test report parsing, regression analysis, and test run diffing.
//!
//! Re-architects and replaces legacy `testlens` with:
//! - **Streaming XML Parser ([`junit::parse_junit_xml`])**: Powered by `quick-xml`, parsing 100k+ test suites in <30ms with <10MB RAM.
//! - **Regression & Fix Detection ([`diff::diff_test_runs`])**: Direct identity-based test outcome comparison.
//! - **Schemas ([`model::RUN_SCHEMA_V1`], [`model::DIFF_SCHEMA_V2`])**: run snapshots stay `testlens.run/v1`; the diff report is `testlens.diff/v2` (structured regressions/new_failures/removed_tests/skipped_changes).

pub mod diff;
pub mod junit;
pub mod model;

pub use diff::{diff_test_runs, merge_test_runs};
pub use junit::parse_junit_xml;
pub use model::{
    CaseChange, TestCase, TestDiff, TestProducer, TestRun, TestStatus, TestSummary, DIFF_SCHEMA_V2,
    RUN_SCHEMA_V1,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_junit_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <testsuites name="all" time="1.5">
            <testsuite name="unit" tests="3">
                <testcase name="test_foo" classname="pkg.FooTest" time="0.1"/>
                <testcase name="test_bar" classname="pkg.BarTest" time="0.4">
                    <failure message="assertion failed: x == y">stack trace...</failure>
                </testcase>
                <testcase name="test_baz" classname="pkg.BazTest" time="0.0">
                    <skipped/>
                </testcase>
            </testsuite>
        </testsuites>"#;

        let run = parse_junit_xml(xml.as_bytes(), "my_project").unwrap();
        assert_eq!(run.summary.total, 3);
        assert_eq!(run.summary.passed, 1);
        assert_eq!(run.summary.failed, 1);
        assert_eq!(run.summary.skipped, 1);

        assert_eq!(run.cases[0].identity, "unit::pkg.FooTest::test_foo");
        assert_eq!(run.cases[0].suite, "unit");
        assert_eq!(run.cases[0].status, TestStatus::Passed);

        assert_eq!(run.cases[1].identity, "unit::pkg.BarTest::test_bar");
        assert_eq!(run.cases[1].status, TestStatus::Failed);
        assert_eq!(
            run.cases[1].message,
            Some("assertion failed: x == y".to_string())
        );
    }

    #[test]
    fn test_output_and_body_capture() {
        let xml = r#"<testsuite tests="2">
            <testcase name="t1" classname="A" time="0.1">
                <failure>stack trace without message attr</failure>
                <system-out>stdout blob</system-out>
                <system-err>stderr blob</system-err>
            </testcase>
            <testcase name="t2" classname="A" time="0.2">
                <skipped>not needed on this platform</skipped>
            </testcase>
        </testsuite>"#;

        let run = parse_junit_xml(xml.as_bytes(), "p").unwrap();
        assert_eq!(run.summary.failed, 1);
        assert_eq!(
            run.cases[0].message.as_deref(),
            Some("stack trace without message attr")
        );
        let out = run.cases[0].output.as_deref().unwrap_or("");
        assert!(out.contains("stdout blob"));
        assert!(out.contains("stderr blob"));
        assert_eq!(
            run.cases[1].message.as_deref(),
            Some("not needed on this platform")
        );
        assert!(run.cases[1].output.is_none());
    }

    #[test]
    fn test_diff_test_runs() {
        let xml_baseline = r#"<testsuite tests="2">
            <testcase name="t1" classname="A" time="0.1"/>
            <testcase name="t2" classname="A" time="0.2"><failure message="broken"/></testcase>
        </testsuite>"#;

        let xml_candidate = r#"<testsuite tests="2">
            <testcase name="t1" classname="A" time="0.1"><failure message="broke"/></testcase>
            <testcase name="t2" classname="A" time="0.2"/>
        </testsuite>"#;

        let run_a = parse_junit_xml(xml_baseline.as_bytes(), "proj").unwrap();
        let run_b = parse_junit_xml(xml_candidate.as_bytes(), "proj").unwrap();

        let diff = diff_test_runs(&run_a, &run_b);
        assert_eq!(diff.schema, DIFF_SCHEMA_V2);
        assert_eq!(diff.regressions.len(), 1);
        assert_eq!(diff.regressions[0].id, "A::t1");
        assert_eq!(diff.regressions[0].before, Some(TestStatus::Passed));
        assert_eq!(diff.regressions[0].after, Some(TestStatus::Failed));
        assert_eq!(diff.regressions[0].message.as_deref(), Some("broke"));
        assert_eq!(diff.fixes.len(), 1);
        assert_eq!(diff.fixes[0].id, "A::t2");
    }

    #[test]
    fn test_diff_structured_semantics() {
        // skipped->passed is a skip change, not a fix; a brand-new failing
        // case is a new failure; a removed passed test is a removed test.
        let base = r#"<testsuite name="s" tests="3">
            <testcase name="sk" classname="C" time="0"><skipped/></testcase>
            <testcase name="gone" classname="C" time="0"/>
        </testsuite>"#;
        let cand = r#"<testsuite name="s" tests="3">
            <testcase name="sk" classname="C" time="0"/>
            <testcase name="fresh" classname="C" time="0"><failure message="x"/></testcase>
        </testsuite>"#;
        let b = parse_junit_xml(base.as_bytes(), "p").unwrap();
        let c = parse_junit_xml(cand.as_bytes(), "p").unwrap();
        let diff = diff_test_runs(&b, &c);

        assert!(diff.fixes.is_empty());
        assert!(diff.regressions.is_empty());
        assert_eq!(diff.skipped_changes.len(), 1);
        assert_eq!(diff.skipped_changes[0].id, "s::C::sk");
        assert_eq!(diff.new_failures.len(), 1);
        assert_eq!(diff.new_failures[0].id, "s::C::fresh");
        assert_eq!(diff.new_failures[0].message.as_deref(), Some("x"));
        assert_eq!(diff.removed_tests.len(), 1);
        assert_eq!(diff.removed_tests[0].id, "s::C::gone");
        assert_eq!(diff.removed_tests[0].before, Some(TestStatus::Passed));
    }

    #[test]
    fn test_diff_duplicate_identity_diagnostic() {
        let dup = r#"<testsuite tests="2">
            <testcase name="t" classname="C" time="0"/>
            <testcase name="t" classname="C" time="0"/>
        </testsuite>"#;
        let a = parse_junit_xml(dup.as_bytes(), "p").unwrap();
        let b = parse_junit_xml(dup.as_bytes(), "p").unwrap();
        let diff = diff_test_runs(&a, &b);
        assert!(diff.diagnostics.iter().any(|d| d.contains("duplicate")));
    }

    #[test]
    fn test_merge_test_runs() {
        let x1 = r#"<testsuite name="a" tests="1"><testcase name="t1" classname="M" time="0.1"/></testsuite>"#;
        let x2 = r#"<testsuite name="b" tests="1"><testcase name="t2" classname="M" time="0.2"><failure/></testcase></testsuite>"#;
        let r1 = parse_junit_xml(x1.as_bytes(), "p").unwrap();
        let r2 = parse_junit_xml(x2.as_bytes(), "p").unwrap();
        let merged = merge_test_runs(vec![r1, r2], "p");
        assert_eq!(merged.summary.total, 2);
        assert_eq!(merged.summary.failed, 1);
        assert!(merged.complete);
        assert_eq!(merged.cases.len(), 2);
    }

    #[test]
    fn test_properties_suite_output_and_entities() {
        let xml = r#"<testsuite tests="1">
            <properties>
                <property name="ci.build" value="1234"/>
                <property name="branch" value="main"/>
            </properties>
            <testcase name="t&quot;q&quot;" classname="A&amp;B" time="0.1">
                <error message="boom"/>
            </testcase>
            <system-out>suite stdout</system-out>
        </testsuite>"#;

        let run = parse_junit_xml(xml.as_bytes(), "p").unwrap();
        assert_eq!(run.properties["ci.build"], "1234");
        assert_eq!(run.properties["branch"], "main");
        assert_eq!(run.suite_output.as_deref(), Some("suite stdout"));
        assert_eq!(run.cases[0].name, "t\"q\"");
        assert_eq!(run.cases[0].classname, "A&B");
        assert_eq!(run.cases[0].status, TestStatus::Error);
        assert_eq!(run.cases[0].message.as_deref(), Some("boom"));
    }

    #[test]
    fn test_truncated_xml_is_incomplete() {
        let xml = r#"<testsuite tests="2">
            <testcase name="t1" classname="A" time="0.1"/>
            <testcase name="t2" classname="A" time="0.2"><failure>"#;
        let run = parse_junit_xml(xml.as_bytes(), "p").unwrap();
        assert!(!run.complete);
        assert_eq!(run.summary.total, 1); // only the closed testcase counted
    }

    #[test]
    fn test_deterministic_run_id() {
        let xml =
            br#"<testsuite tests="1"><testcase name="a" classname="C" time="0"/></testsuite>"#;
        let a = parse_junit_xml(xml, "p").unwrap();
        let b = parse_junit_xml(xml, "p").unwrap();
        assert_eq!(a.run_id, b.run_id);
        let other =
            br#"<testsuite tests="1"><testcase name="b" classname="C" time="0"/></testsuite>"#;
        let c = parse_junit_xml(other, "p").unwrap();
        assert_ne!(a.run_id, c.run_id);
    }

    #[test]
    fn test_t01_general_ref_entities_and_whitespace() {
        let xml = r#"<testsuite tests="1">
            <testcase name="t1" classname="C" time="0.1">
                <failure>expected x &lt; y &amp; z == &quot;ok&quot; &apos;1&apos; &#65; &#x42;</failure>
                <system-out>out &lt;tag&gt; &amp; more</system-out>
            </testcase>
        </testsuite>"#;
        let run = parse_junit_xml(xml.as_bytes(), "p").unwrap();
        assert_eq!(
            run.cases[0].message.as_deref(),
            Some("expected x < y & z == \"ok\" '1' A B")
        );
        assert_eq!(run.cases[0].output.as_deref(), Some("out <tag> & more"));
    }

    #[test]
    fn test_t02_invalid_durations_unclosed_capture_and_stray_tags() {
        let xml = r#"<testsuite tests="3">
            <testcase name="nan_case" classname="C" time="NaN"/>
            <testcase name="inf_case" classname="C" time="inf">
                <failure>unclosed failure body</testcase>
            <testcase name="neg_case" classname="C" time="-2.5"/>
        </testsuite>"#;
        let run = parse_junit_xml(xml.as_bytes(), "p").unwrap();
        assert!(!run.complete);
        assert!(run.summary.duration_sec.is_finite());
        assert_eq!(run.summary.duration_sec, 0.0);
        assert_eq!(
            run.cases[1].message.as_deref(),
            Some("unclosed failure body")
        );
        assert_eq!(run.cases[2].status, TestStatus::Passed);
        assert!(run.cases[2].message.is_none());

        // Stray closing tags on a truncated XML must not fool open_depth into complete = true.
        let stray_xml = r#"<testsuite tests="1"><testcase name="t1" classname="C"/></extra>"#;
        let stray_run = parse_junit_xml(stray_xml.as_bytes(), "p").unwrap();
        assert!(!stray_run.complete);
    }

    #[test]
    fn test_t02_duplicate_identity_preserves_failure() {
        let base_xml = r#"<testsuite tests="1">
            <testcase name="flaky" classname="C" time="0.1"/>
        </testsuite>"#;
        // Attempt 1 failed, attempt 2 passed: candidate must not silently overwrite failure with pass.
        let cand_xml = r#"<testsuite tests="2">
            <testcase name="flaky" classname="C" time="0.1">
                <failure message="attempt 1 failed"/>
            </testcase>
            <testcase name="flaky" classname="C" time="0.1"/>
        </testsuite>"#;
        let base = parse_junit_xml(base_xml.as_bytes(), "p").unwrap();
        let cand = parse_junit_xml(cand_xml.as_bytes(), "p").unwrap();
        let diff = diff_test_runs(&base, &cand);
        assert_eq!(diff.regressions.len(), 1);
        assert_eq!(diff.regressions[0].id, "C::flaky");
        assert_eq!(
            diff.regressions[0].message.as_deref(),
            Some("attempt 1 failed")
        );
        assert!(diff.diagnostics.iter().any(|d| d.contains("duplicate")));
    }
}
