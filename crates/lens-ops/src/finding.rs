use serde::{Deserialize, Serialize};

/// Severity of a [`Finding`], ordered Info < Warn < Fail so callers can
/// pick a threshold (`report.is_actionable(threshold)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Fail,
}

/// One judgement attached to a [`Report`]: a stable machine-readable
/// `code` (e.g. `net.new_listener`) plus a human message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<FindingEvidence>,
}

/// Structured reference to observed domain data. Coordinates are never inferred
/// from human messages. `data_pointer` uses RFC 6901 relative to report data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub data_pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

/// Result of one operation: the data plus everything a front end needs
/// to decide the exit code and to notify about degraded evidence.
#[derive(Debug)]
pub struct Report<T> {
    pub data: T,
    pub findings: Vec<Finding>,
    /// False when the evidence behind `data` is incomplete (truncated
    /// scan/report, collection diagnostics) — always actionable
    /// regardless of finding severity.
    pub complete: bool,
    /// Non-fatal operational diagnostics (scan errors, collection
    /// warnings) for the caller to surface.
    pub diagnostics: Vec<String>,
}

impl<T> Report<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            findings: Vec::new(),
            complete: true,
            diagnostics: Vec::new(),
        }
    }

    pub fn push(&mut self, severity: Severity, code: &str, message: impl Into<String>) {
        self.findings.push(Finding {
            severity,
            code: code.to_string(),
            message: message.into(),
            evidence: None,
        });
    }

    /// Exit-1 decision: incomplete evidence is always actionable, or any
    /// finding at/above `threshold`.
    pub fn is_actionable(&self, threshold: Severity) -> bool {
        !self.complete || self.findings.iter().any(|f| f.severity >= threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(severities: &[Severity], complete: bool) -> Report<()> {
        let mut r = Report::new(());
        r.complete = complete;
        for (i, s) in severities.iter().enumerate() {
            r.push(*s, "test.code", format!("m{i}"));
        }
        r
    }

    #[test]
    fn is_actionable_threshold_semantics() {
        // Fail threshold ignores Warn and Info.
        assert!(!report_with(&[Severity::Info, Severity::Warn], true).is_actionable(Severity::Fail));
        // Warn threshold catches Warn and Fail.
        assert!(report_with(&[Severity::Warn], true).is_actionable(Severity::Warn));
        assert!(report_with(&[Severity::Fail], true).is_actionable(Severity::Fail));
        // A Fail finding is actionable even at the default threshold.
        assert!(report_with(&[Severity::Fail], true).is_actionable(Severity::Fail));
        // Info alone never triggers the default (Fail) threshold; it only
        // fires at an explicit Info threshold via the >= rule.
        assert!(!report_with(&[Severity::Info], true).is_actionable(Severity::Fail));
        assert!(report_with(&[Severity::Info], true).is_actionable(Severity::Info));
        assert!(!report_with(&[], true).is_actionable(Severity::Fail));
        // Incomplete evidence is actionable at any threshold.
        assert!(report_with(&[], false).is_actionable(Severity::Fail));
        assert!(report_with(&[Severity::Info], false).is_actionable(Severity::Fail));
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn legacy_findings_round_trip_without_new_fields() {
        let legacy =
            serde_json::json!({"severity":"warn","code":"test.old","message":"/not/a/location:23"});
        let finding: Finding = serde_json::from_value(legacy.clone()).unwrap();
        assert!(finding.evidence.is_none());
        assert_eq!(serde_json::to_value(finding).unwrap(), legacy);
    }

    #[test]
    fn consumer_corpus_retains_all_findings_and_valid_references() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/fixtures/result-v1/large.json"))
                .unwrap();
        let findings: Vec<Finding> = serde_json::from_value(value["findings"].clone()).unwrap();
        assert_eq!(findings.len(), 1024);
        for finding in findings {
            let evidence = finding.evidence.unwrap();
            assert_eq!(
                value["data"].pointer(&evidence.data_pointer).unwrap()["id"].as_str(),
                evidence.object.as_deref()
            );
        }
    }
}
