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
            Ok(lens_sys::load_units_merged(&dirs))
        }
    }
}

/// Build a snapshot from units already loaded (units + cycles +
/// flattened diagnostics).
pub fn snapshot_from_units(units: BTreeMap<String, SystemdUnit>) -> SystemdSnapshot {
    let graph = OrderingGraph::build(&units);
    let diagnostics = unit_diagnostics(&units);
    SystemdSnapshot {
        schema: lens_sys::SNAPSHOT_SCHEMA_V1.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        semantics: "systemd-255-subset-v1".to_string(),
        units,
        cycles: graph.find_cycles(),
        diagnostics,
    }
}

/// Snapshot the systemd search path or an explicit unit directory.
pub fn snapshot(path: Option<&Path>) -> Result<SystemdSnapshot> {
    Ok(snapshot_from_units(load_units(path)?))
}

/// `sys diff` accepts either a saved snapshot JSON (schema-checked) or
/// a live unit directory, which is loaded and graphed into an
/// equivalent snapshot.
pub fn snapshot_from(path: &Path) -> Result<SystemdSnapshot> {
    if path.is_dir() {
        return Ok(snapshot_from_units(lens_sys::load_units(path)?));
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
    let graph = OrderingGraph::build(&units);
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
