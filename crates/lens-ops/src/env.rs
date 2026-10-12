use std::path::Path;

use lens_core::{LensError, Result};
use lens_env::{detect_shadowing, diff_environments, inspect_venv, EnvDiff, EnvSnapshot, PyVenv};

use crate::{json, Finding, Report, Severity};

/// Shared env findings: missing/conflicting deps are Fail, shadowing is
/// Warn, unevaluated requirements are Info.
fn venv_findings(venv: &PyVenv) -> Vec<Finding> {
    let mut findings = Vec::new();
    for d in &venv.missing_dependencies {
        findings.push(Finding {
            severity: Severity::Fail,
            code: "env.missing".to_string(),
            message: d.clone(),
        });
    }
    for d in &venv.version_conflicts {
        findings.push(Finding {
            severity: Severity::Fail,
            code: "env.conflict".to_string(),
            message: d.clone(),
        });
    }
    for i in &venv.shadowing_issues {
        findings.push(Finding {
            severity: Severity::Warn,
            code: "env.shadowing".to_string(),
            message: format!("{} ({}) shadows {}", i.module_name, i.local_path, i.shadows),
        });
    }
    for d in &venv.unevaluated_dependencies {
        findings.push(Finding {
            severity: Severity::Info,
            code: "env.unevaluated".to_string(),
            message: format!("{d} — requirement marker could not be evaluated"),
        });
    }
    findings
}

fn open_venv(path: &Path, extras: &[String]) -> Result<PyVenv> {
    inspect_venv(path, extras).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Inspect a virtualenv (optionally checking `project` for stdlib
/// shadowing) and return the snapshot with findings.
pub fn inspect(venv_path: &Path, project: Option<&Path>) -> Result<Report<EnvSnapshot>> {
    let mut venv = open_venv(venv_path, &[])?;
    if let Some(proj) = project {
        venv.shadowing_issues = detect_shadowing(proj, &venv.packages);
    }
    let findings = venv_findings(&venv);
    let snapshot = EnvSnapshot {
        schema: lens_env::SNAPSHOT_SCHEMA_V1.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        venv,
    };
    let mut report = Report::new(snapshot);
    report.findings = findings;
    Ok(report)
}

/// Dependency check: same findings as `inspect`, without the snapshot
/// envelope.
pub fn check(venv_path: &Path, extras: &[String]) -> Result<Report<PyVenv>> {
    let venv = open_venv(venv_path, extras)?;
    let mut report = Report::new(venv);
    report.findings = venv_findings(&report.data);
    Ok(report)
}

/// An env diff input: a saved snapshot JSON (schema-checked) or a live
/// virtualenv directory inspected on the fly.
fn load_env_input(path: &Path) -> Result<EnvSnapshot> {
    if path.is_dir() {
        let venv = open_venv(path, &[])?;
        return Ok(EnvSnapshot {
            schema: lens_env::SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            venv,
        });
    }
    json::read_json_schema(path, "schema", lens_env::SNAPSHOT_SCHEMA_V1)
}

/// Diff two env inputs — saved snapshot JSONs (schema-checked) or live
/// virtualenv directories, freely mixed. Deltas are Info.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<EnvDiff>> {
    let s1 = load_env_input(baseline)?;
    let s2 = load_env_input(candidate)?;
    let diff = diff_environments(&s1.venv, &s2.venv);
    let mut report = Report::new(diff);
    let (a, r, m, s) = (
        report.data.added_packages.len(),
        report.data.removed_packages.len(),
        report.data.version_changes.len(),
        report.data.new_shadowing.len(),
    );
    if a + r + m + s > 0 {
        report.push(
            Severity::Info,
            "env.package_changes",
            format!("+{a} -{r} ~{m} package(s), {s} new shadowing issue(s)"),
        );
    }
    Ok(report)
}
