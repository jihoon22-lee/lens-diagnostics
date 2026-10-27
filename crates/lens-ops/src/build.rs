use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lens_build::{
    diff_compilations, parse_command_entry, resolve_header_target, BuildDiff, BuildSnapshot,
    CompileCommandEntry, ImpactGraph, ImpactReport, ParsedUnit,
};
use lens_core::Result;

use crate::{json, Report, Severity};

/// Load a compile_commands.json and build the include-impact graph —
/// the shared front half of `build inspect` and `build impact`.
fn load_graph(file: &Path) -> Result<(Vec<CompileCommandEntry>, Vec<ParsedUnit>, ImpactGraph)> {
    let entries: Vec<CompileCommandEntry> = json::read_json(file)?;
    let units: Vec<ParsedUnit> = entries.iter().map(parse_command_entry).collect();
    let mut graph = ImpactGraph::new();
    for unit in &units {
        graph.add_translation_unit(unit);
    }
    Ok((entries, units, graph))
}

/// Every filesystem path the compile database makes a consumer read:
/// each entry's `directory`, its `file`, and every include dir /
/// forced include parsed from its command. Relative paths resolve
/// against the entry's `directory`; a relative `directory` resolves
/// against the database's parent. Callers that confine filesystem
/// access (e.g. `lens-mcp --allow-root`) check this list before
/// building the impact graph.
pub fn referenced_paths(file: &Path) -> Result<Vec<PathBuf>> {
    let entries: Vec<CompileCommandEntry> = json::read_json(file)?;
    let db_dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut out = Vec::new();
    for e in &entries {
        let unit = parse_command_entry(e);
        let dir = Path::new(&unit.directory);
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            db_dir.join(dir)
        };
        out.push(dir.clone());
        let mut push_rel = |rel: &str| {
            let p = Path::new(rel);
            if p.is_absolute() {
                out.push(p.to_path_buf());
            } else {
                out.push(dir.join(p));
            }
        };
        push_rel(&unit.file);
        for inc in &unit.includes {
            push_rel(inc);
        }
        for fi in &unit.forced_includes {
            push_rel(fi);
        }
    }
    Ok(out)
}

/// Emit the full `buildscope.snapshot/v4` snapshot, including the
/// reverse-impact graph and transitive impact for every known header.
pub fn inspect(file: &Path) -> Result<Report<BuildSnapshot>> {
    let (_entries, units, graph) = load_graph(file)?;
    let reverse_impact: BTreeMap<String, Vec<String>> = graph
        .header_to_units
        .iter()
        .map(|(h, us)| (h.clone(), us.iter().cloned().collect()))
        .collect();
    // Transitive impact for every known header: keys of both maps plus
    // headers reached only through other headers.
    let all_headers: BTreeSet<String> = graph
        .header_to_units
        .keys()
        .chain(graph.header_to_headers.keys())
        .cloned()
        .chain(graph.header_to_headers.values().flatten().cloned())
        .collect();
    let transitive_impact: BTreeMap<String, Vec<String>> = all_headers
        .iter()
        .map(|h| (h.clone(), graph.compute_impact(h).impacted_units))
        .collect();
    let snapshot = BuildSnapshot {
        schema: lens_build::SNAPSHOT_SCHEMA_V4.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        total_units: units.len(),
        units,
        reverse_impact,
        transitive_impact,
    };
    let mut report = Report::new(snapshot);
    graph_status(
        &mut report,
        graph.missing_sources,
        graph.unresolved_includes,
        graph.scan_truncated,
    );
    Ok(report)
}

/// Reverse compilation impact of `header`. A relative header resolves
/// against the compile database directory, each entry's `directory`,
/// and the process cwd — in that order. A header absent from the graph
/// gets a `hint` with closest matches; that hint and an incomplete
/// graph (missing sources/unresolved includes/truncation) are Warn
/// findings.
pub fn impact(file: &Path, header: &str) -> Result<Report<ImpactReport>> {
    let (entries, _units, graph) = load_graph(file)?;
    let db_dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let mut bases: Vec<String> = vec![db_dir.clone()];
    for e in &entries {
        if !e.directory.is_empty() && !bases.contains(&e.directory) {
            bases.push(e.directory.clone());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.to_string_lossy().into_owned();
        if !bases.contains(&cwd) {
            bases.push(cwd);
        }
    }
    let target = resolve_header_target(&graph, header, &bases);
    let mut impact = graph.compute_impact(&target);

    if !graph.header_in_graph(&impact.target_header) {
        // Not in the include graph at all — likely a path-resolution
        // problem, not "no rebuilds needed". Show closest basename
        // matches so the user can re-spell it.
        let candidates = graph.closest_headers(header, 5);
        let mut hint = format!(
            "header {:?} is not in the include graph (resolved as {})",
            header, impact.target_header
        );
        if !candidates.is_empty() {
            hint.push_str(&format!("; similar: {}", candidates.join(", ")));
        }
        hint.push_str(&format!(
            "; pass an absolute path or one relative to the compile database directory ({})",
            db_dir
        ));
        impact.hint = Some(hint);
    }

    let hint = impact.hint.clone();
    let (missing, unresolved, truncated) = (
        impact.missing_sources,
        impact.unresolved_includes,
        impact.scan_truncated,
    );
    let mut report = Report::new(impact);
    if let Some(h) = hint {
        report.push(Severity::Warn, "build.not_in_graph", h);
    }
    graph_status(&mut report, missing, unresolved, truncated);
    Ok(report)
}

fn graph_status<T>(report: &mut Report<T>, missing: usize, unresolved: usize, truncated: bool) {
    report.diagnostics.push("Scope: static literal includes only; compiler builtin headers, macro expansion, conditional preprocessing and include_next search semantics are not evaluated. Empty impact does not prove that no rebuild is needed.".into());
    if missing > 0 || unresolved > 0 || truncated {
        report.complete = false;
        report.push(
            Severity::Warn,
            "build.incomplete_graph",
            format!("impact graph incomplete: {missing} missing source(s), {unresolved} unresolved include(s), truncated={truncated}"),
        );
    }
}

/// Diff two compile_commands.json databases. Deltas are Info.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<BuildDiff>> {
    let b_entries: Vec<CompileCommandEntry> = json::read_json(baseline)?;
    let c_entries: Vec<CompileCommandEntry> = json::read_json(candidate)?;
    let u1: Vec<ParsedUnit> = b_entries.iter().map(parse_command_entry).collect();
    let u2: Vec<ParsedUnit> = c_entries.iter().map(parse_command_entry).collect();
    let diff = diff_compilations(&u1, &u2);
    let (a, r, m) = (
        diff.added_units.len(),
        diff.removed_units.len(),
        diff.modified_units.len(),
    );
    let mut report = Report::new(diff);
    if a + r + m > 0 {
        report.push(
            Severity::Info,
            "build.unit_changes",
            format!("+{a} -{r} ~{m} unit(s)"),
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(dir: &Path, source: Option<&str>, args: &[&str]) -> PathBuf {
        if let Some(source) = source {
            std::fs::write(dir.join("a.c"), source).unwrap();
        }
        let path = dir.join("compile_commands.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!([{
                "directory": dir, "file": "a.c", "arguments": args
            }]))
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn missing_and_unresolved_are_incomplete_for_both_operations() {
        for source in [None, Some("#include \"missing.h\"\n")] {
            let dir = tempfile::tempdir().unwrap();
            let path = database(dir.path(), source, &["cc", "a.c"]);
            let impact = impact(&path, "missing.h").unwrap();
            assert!(!impact.complete);
            assert!(impact.is_actionable(Severity::Fail));
            assert!(!inspect(&path).unwrap().complete);
        }
    }

    #[test]
    fn complete_static_graph_keeps_scope_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(dir.path(), Some("#include \"ok.h\"\n"), &["cc", "a.c"]);
        std::fs::write(dir.path().join("ok.h"), "").unwrap();
        let report = impact(&path, "ok.h").unwrap();
        assert!(report.complete);
        assert_eq!(report.data.total_impacted, 1);
        assert!(!report.is_actionable(Severity::Fail));
        assert!(report.diagnostics[0].contains("static literal includes"));
    }

    #[test]
    fn truncated_graph_is_actionable_at_fail_threshold() {
        let mut report = Report::new(());
        graph_status(&mut report, 0, 0, true);
        assert!(!report.complete);
        assert!(report.is_actionable(Severity::Fail));
    }

    #[test]
    fn missing_forced_include_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let path = database(
            dir.path(),
            Some(""),
            &["cc", "-include", "missing.h", "a.c"],
        );
        assert!(!impact(&path, "missing.h").unwrap().complete);
    }
}
