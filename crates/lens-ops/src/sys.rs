use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lens_core::Result;
use lens_sys::{
    diff_systemd, CyclePath, Diagnostic, OrderingGraph, SystemdDiff, SystemdSnapshot, SystemdUnit,
};

use crate::{json, Report, Severity};

/// Flatten per-unit parser/loader diagnostics for the snapshot-level
/// `diagnostics` array — previously always empty, which hid malformed
/// unit files and missing dependency references from `sys inspect`.
fn unit_diagnostics(units: &BTreeMap<String, SystemdUnit>) -> Vec<Diagnostic> {
    units
        .values()
        .flat_map(|u| u.diagnostics.iter().cloned())
        .collect()
}

/// Load systemd units for `sys` commands: an explicit path goes through
/// `load_units` (fail closed on missing paths); no path merges the
/// standard systemd search dirs like doctor/TUI.
pub fn load_units(path: Option<&Path>) -> Result<BTreeMap<String, SystemdUnit>> {
    match path {
        Some(p) => lens_sys::load_units(p),
        None => {
            let dirs: Vec<PathBuf> = lens_sys::SYSTEMD_SEARCH_DIRS
                .iter()
                .map(PathBuf::from)
                .collect();
            lens_sys::load_units_merged(&dirs)
        }
    }
}

/// Build a snapshot from units already loaded (units + cycles +
/// flattened diagnostics).
pub fn snapshot_from_units(units: BTreeMap<String, SystemdUnit>) -> Result<SystemdSnapshot> {
    let graph = OrderingGraph::build(&units)?;
    let diagnostics = unit_diagnostics(&units);
    Ok(SystemdSnapshot {
        schema: lens_sys::SNAPSHOT_SCHEMA_V1.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        semantics: "systemd-255-subset-v2".to_string(),
        units,
        cycles: graph.find_cycles(),
        diagnostics,
    })
}

/// Snapshot the systemd search path or an explicit unit directory.
pub fn snapshot(path: Option<&Path>) -> Result<SystemdSnapshot> {
    snapshot_from_units(load_units(path)?)
}

/// `sys diff` accepts either a saved snapshot JSON (schema-checked) or
/// a live unit directory, which is loaded and graphed into an
/// equivalent snapshot.
pub fn snapshot_from(path: &Path) -> Result<SystemdSnapshot> {
    if path.is_dir() || crate::detect::has_unit_extension(path) {
        return snapshot_from_units(lens_sys::load_units(path)?);
    }
    json::read_json_schema(path, "schema", lens_sys::SNAPSHOT_SCHEMA_V1)
}

fn push_cycle_findings<T>(report: &mut Report<T>, cycles: Vec<String>, diagnostics_count: usize) {
    for c in cycles {
        report.push(Severity::Fail, "sys.cycle", c);
    }
    if diagnostics_count > 0 {
        report.push(
            Severity::Warn,
            "sys.diagnostic",
            format!("{diagnostics_count} unit parser diagnostic(s)"),
        );
    }
}

/// Inspect units: every dependency cycle is a Fail finding; unit parser
/// diagnostics roll up into one Warn finding.
pub fn inspect(path: Option<&Path>) -> Result<Report<SystemdSnapshot>> {
    let snap = snapshot(path)?;
    let cycles = snap.cycles.iter().map(|c| c.join(" -> ")).collect();
    let diagnostics_count = snap.diagnostics.len();
    let mut report = Report::new(snap);
    push_cycle_findings(&mut report, cycles, diagnostics_count);
    Ok(report)
}

/// Cycle detection result: the directed cycle paths plus the unit count
/// for the "N units scanned" summary line.
#[derive(Debug)]
pub struct CyclesResult {
    pub cycles: Vec<CyclePath>,
    pub scanned_units: usize,
}

/// Detect ordering cycles. Every cycle is a Fail finding.
pub fn cycles(dir: Option<&Path>) -> Result<Report<CyclesResult>> {
    let units = load_units(dir)?;
    let diagnostics = unit_diagnostics(&units);
    let graph = OrderingGraph::build(&units)?;
    let cycles = graph.find_cycle_paths();
    let cycle_msgs: Vec<String> = cycles.iter().map(|c| c.members.join(" -> ")).collect();
    let mut report = Report::new(CyclesResult {
        scanned_units: units.len(),
        cycles,
    });
    push_cycle_findings(&mut report, cycle_msgs, diagnostics.len());
    Ok(report)
}

/// Diff two systemd inputs (snapshot JSON or unit directory). New
/// cycles are Fail findings; unit add/remove/modify counts are Info.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<SystemdDiff>> {
    let s1 = snapshot_from(baseline)?;
    let s2 = snapshot_from(candidate)?;
    Ok(diff_snapshots(&s1, &s2))
}

/// Value-level classifier shared by the file diff and `bundle diff`:
/// new cycles are Fail findings; unit add/remove/modify counts and
/// resolved cycles are Info.
pub fn diff_snapshots(s1: &SystemdSnapshot, s2: &SystemdSnapshot) -> Report<SystemdDiff> {
    let diff = diff_systemd(s1, s2);
    let (new_cycles, added, removed, modified, resolved) = (
        diff.new_cycles.clone(),
        diff.added_units.len(),
        diff.removed_units.len(),
        diff.modified_units.len(),
        diff.resolved_cycles.len(),
    );
    let mut report = Report::new(diff);
    for c in new_cycles {
        report.push(Severity::Fail, "sys.new_cycle", c.join(" -> "));
    }
    if added + removed + modified > 0 {
        report.push(
            Severity::Info,
            "sys.unit_changes",
            format!("+{added} -{removed} ~{modified} unit(s)"),
        );
    }
    if resolved > 0 {
        report.push(
            Severity::Info,
            "sys.resolved_cycles",
            format!("{resolved} cycle(s) resolved"),
        );
    }
    report
}

#[cfg(test)]
mod tests {
    #[test]
    fn expansion_failures_propagate_to_inspect_doctor_and_bundle_collection() {
        let dir = tempfile::tempdir().unwrap();
        let units = dir.path().join("units");
        std::fs::create_dir(&units).unwrap();
        std::fs::write(units.join("app.service"), "[Unit]\nWants=a@x.service\n").unwrap();
        std::fs::write(units.join("a@.service"), "[Unit]\nAfter=a@%i-x.service\n").unwrap();
        assert!(matches!(
            super::inspect(Some(&units)),
            Err(lens_core::LensError::LimitExceeded { .. })
        ));
        assert!(matches!(
            crate::doctor::run(Some(dir.path()), None, Some(&units)),
            Err(lens_core::LensError::LimitExceeded { .. })
        ));
        let log = dir.path().join("input.log");
        std::fs::write(&log, "INFO fixture\n").unwrap();
        let path = dir.path().join("partial.lens");
        let report = crate::bundle::create(&crate::bundle::BundleRequest {
            output: path.clone(),
            sys: Some(units),
            log: Some(log),
            ..Default::default()
        })
        .unwrap();
        assert!(!report.complete);
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.contains("Resource limit exceeded")));
        assert!(lens_core::verify_bundle_archive(&path).unwrap().is_valid);
        assert!(lens_core::read_bundle_entry(&path, "reports/sys_snapshot.json").is_err());
    }

    #[test]
    fn followup_all_supported_unit_suffixes_compare() {
        let dir = tempfile::tempdir().unwrap();
        for suffix in [
            "service",
            "socket",
            "target",
            "timer",
            "mount",
            "automount",
            "swap",
            "path",
            "slice",
            "scope",
        ] {
            let path = dir.path().join(format!("a.{suffix}"));
            std::fs::write(&path, "[Unit]\nDescription=valid unit\n").unwrap();
            assert_eq!(
                crate::detect::detect(&path).unwrap(),
                crate::detect::InputKind::SystemdUnit,
                "{suffix}"
            );
            assert!(super::diff(&path, &path).is_ok(), "{suffix}");
        }
    }
    #[test]
    fn diff_accepts_individual_units_and_dropins() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.service");
        let b = dir.path().join("b.service");
        std::fs::write(&a, "[Unit]\nDescription=baseline\n").unwrap();
        std::fs::write(&b, "[Unit]\nDescription=candidate\n").unwrap();
        std::fs::create_dir(dir.path().join("b.service.d")).unwrap();
        std::fs::write(
            dir.path().join("b.service.d/cycle.conf"),
            "[Unit]\nAfter=b.service\n",
        )
        .unwrap();
        let report = super::diff(&a, &b).unwrap();
        assert!(report.findings.iter().any(|f| f.code == "sys.new_cycle"));
    }
}
