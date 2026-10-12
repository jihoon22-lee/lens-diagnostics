use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lens_abi::inspect_elf;
use lens_build::{parse_command_entry, CompileCommandEntry, ImpactGraph};
use lens_disk::{DiskScanner, DuplicateFinder, ScanOptions, SnapshotV2};
use lens_env::inspect_venv;
use lens_log::{parse_line, LogFilter, LogIndexer, LogLevel};
use lens_sys::{OrderingGraph, SystemdSnapshot, SystemdUnit};

/// Cap on a serialized tool result — a single `tools/call` result must
/// stay well inside an LLM context window. When pagination cannot shrink
/// a response below this, the result is re-clamped and marked.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// Default cap on any array inside a tool result (override per call with
/// the `limit` argument).
const DEFAULT_ARRAY_LIMIT: usize = 200;
/// Fallback array cap when a serialized result still exceeds
/// MAX_RESPONSE_BYTES after pagination.
const CLAMPED_ARRAY_LIMIT: usize = 25;

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing '{}' argument", key))
}

fn arg_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

/// Keep `offset..offset+limit` of every array in `v` and append a
/// structured truncation marker so callers see the cut and can page
/// further. Applied uniformly to every tool result: a 700 KB ABI report
/// becomes pageable instead of blowing the context window.
fn paginate_arrays(v: &mut Value, offset: usize, limit: usize) {
    match v {
        Value::Array(arr) => {
            let total = arr.len();
            if offset > 0 {
                let drop = offset.min(total);
                arr.drain(..drop);
            }
            let omitted = arr.len().saturating_sub(limit);
            if omitted > 0 {
                arr.truncate(limit);
            }
            for item in arr.iter_mut() {
                paginate_arrays(item, offset, limit);
            }
            if omitted > 0 {
                arr.push(serde_json::json!({
                    "_truncated": true,
                    "total_before_truncation": total,
                    "omitted": omitted,
                }));
            }
        }
        Value::Object(map) => {
            for val in map.values_mut() {
                paginate_arrays(val, offset, limit);
            }
        }
        _ => {}
    }
}

/// Serialize a tool result: array pagination with `limit`/`offset` args,
/// then a hard size guard that re-clamps aggressively if the result still
/// exceeds the response budget. A tool that paginated its own output
/// (log_filter's `matches`) sets `_self_paginated` so the fallback does
/// not apply `offset` a second time.
fn serialize_result(mut v: Value, args: &Value) -> Result<String, String> {
    let limit = arg_usize(args, "limit", DEFAULT_ARRAY_LIMIT);
    let mut offset = arg_usize(args, "offset", 0);
    if let Value::Object(m) = &mut v {
        if m.remove("_self_paginated").is_some() {
            offset = 0;
        }
    }
    paginate_arrays(&mut v, offset, limit);
    let mut text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    if text.len() > MAX_RESPONSE_BYTES {
        paginate_arrays(&mut v, offset, CLAMPED_ARRAY_LIMIT);
        if let Value::Object(m) = &mut v {
            m.insert("_response_clamped".to_string(), serde_json::json!(true));
        }
        text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    }
    Ok(text)
}

/// Resolve a caller-supplied path string. Paths are interpreted relative
/// to the lens-mcp server process's working directory — the description
/// of every path argument says so, and errors name the resolved base.
fn tool_path(args: &Value, key: &str) -> Result<PathBuf, String> {
    let s = arg_str(args, key)?;
    let p = Path::new(s);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(cwd.join(p))
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    serde_json::from_str(&content).map_err(|e| format!("{}: {}", path.display(), e))
}

/// `lens_sys_diff` accepts a saved snapshot JSON or a live unit directory.
fn sys_snapshot_from(path: &Path) -> Result<SystemdSnapshot, String> {
    if path.is_dir() {
        let units = lens_sys::load_units(path).map_err(|e| e.to_string())?;
        let graph = OrderingGraph::build(&units);
        return Ok(SystemdSnapshot {
            schema: lens_sys::SNAPSHOT_SCHEMA_V1.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            semantics: "systemd-255-subset-v1".to_string(),
            diagnostics: units
                .values()
                .flat_map(|u| u.diagnostics.iter().cloned())
                .collect(),
            units,
            cycles: graph.find_cycles(),
        });
    }
    read_json_file(path)
}

/// Collect JUnit XML inputs: a single file, or a directory scanned for
/// `*.xml` (sorted for determinism). No shell globs — MCP callers pass
/// concrete paths.
fn junit_inputs(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("{}: not a file or directory", path.display()));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(path)
        .map_err(|e| format!("{}: {}", path.display(), e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "xml").unwrap_or(false))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("{} contains no *.xml files", path.display()));
    }
    Ok(files)
}

fn parse_test_run(path: &Path, label: &str) -> Result<lens_test::TestRun, String> {
    let files = junit_inputs(path)?;
    let mut runs = Vec::new();
    for f in &files {
        let bytes = fs::read(f).map_err(|e| format!("{}: {}", f.display(), e))?;
        runs.push(lens_test::parse_junit_xml(&bytes, label).map_err(|e| e.to_string())?);
    }
    Ok(lens_test::merge_test_runs(runs, label))
}

fn execute_tool_value(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "lens_disk_scan" => {
            let path = tool_path(args, "path")?;
            let scanner = DiskScanner::new(ScanOptions::default());
            let res = scanner.scan(&path).map_err(|e| e.to_string())?;

            if args.get("json").and_then(|v| v.as_bool()).unwrap_or(false) {
                let snap =
                    SnapshotV2::from_tree(&res.tree, res.root_id, res.complete, res.truncated);
                return serde_json::to_value(&snap).map_err(|e| e.to_string());
            }
            let children_count = res.tree.children_ids(res.root_id).len();
            let root = &res.tree.nodes[res.root_id as usize];
            Ok(serde_json::json!({
                "path": path.to_string_lossy(),
                "scanned_entries": res.scanned_entries,
                "logical_bytes": root.size,
                "allocated_bytes": root.allocated_size,
                "children_count": children_count,
                "complete": res.complete,
                "errors": res.errors,
            }))
        }
        "lens_disk_duplicates" => {
            let path = tool_path(args, "path")?;
            let min_size = args
                .get("min_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024);
            let scanner = DiskScanner::new(ScanOptions::default());
            let res = scanner.scan(&path).map_err(|e| e.to_string())?;
            let finder = DuplicateFinder::new(min_size);
            let report = finder
                .find_in_tree(&res.tree, &path)
                .map_err(|e| e.to_string())?;
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        "lens_abi_inspect" => {
            let p = tool_path(args, "binary")?;
            let bytes = fs::read(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
            let report = inspect_elf(&p, &bytes);
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        "lens_abi_diff" => {
            let b = tool_path(args, "baseline")?;
            let c = tool_path(args, "candidate")?;
            let b_bytes = fs::read(&b).map_err(|e| format!("{}: {}", b.display(), e))?;
            let c_bytes = fs::read(&c).map_err(|e| format!("{}: {}", c.display(), e))?;
            let left = inspect_elf(&b, &b_bytes);
            let right = inspect_elf(&c, &c_bytes);
            let diff = lens_abi::diff_reports(&left, &right);
            serde_json::to_value(&diff).map_err(|e| e.to_string())
        }
        "lens_log_filter" => {
            let path = tool_path(args, "path")?;
            let indexer = LogIndexer::open(&path).map_err(|e| e.to_string())?;
            let mut filter = LogFilter::new();
            if let Some(q) = arg_opt(args, "query") {
                filter = filter.with_query(q);
            }
            if let Some(re) = arg_opt(args, "regex") {
                filter = filter
                    .with_regex(re)
                    .map_err(|e| format!("Invalid 'regex' {re:?}: {e}"))?;
            }
            let include_unknown = args
                .get("include_unknown")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(lvl) = arg_opt(args, "min_level") {
                let parsed = LogLevel::parse(lvl);
                if parsed == LogLevel::Unknown && !lvl.eq_ignore_ascii_case("unknown") {
                    return Err(format!(
                        "Invalid 'min_level' {lvl:?}; expected trace|debug|info|warn|error|fatal"
                    ));
                }
                filter = filter.with_min_level(parsed);
            }
            filter = filter.with_include_unknown(include_unknown);
            for (key, set) in [
                (
                    "since",
                    LogFilter::with_since as fn(LogFilter, Option<i64>) -> LogFilter,
                ),
                (
                    "until",
                    LogFilter::with_until as fn(LogFilter, Option<i64>) -> LogFilter,
                ),
            ] {
                if let Some(bound) = arg_opt(args, key) {
                    let ms = lens_log::timestamp::parse_bound(bound).ok_or_else(|| {
                        format!(
                            "Invalid '{key}' {bound:?}; expected RFC 3339 (e.g. 2026-10-09T12:00:00Z), 'YYYY-MM-DD HH:MM:SS' or 'YYYY-MM-DD' (UTC)"
                        )
                    })?;
                    filter = set(filter, Some(ms));
                }
            }
            if let Some(y) = args.get("year").and_then(|v| v.as_i64()) {
                if !(1..=9999).contains(&y) {
                    return Err(format!("Invalid 'year' {y}; expected 1..=9999"));
                }
                filter = filter.with_syslog_year(y as i32);
            }

            // `tail` scans only the last N lines — recent lines are where
            // incident evidence lives. Without it the whole (bounded)
            // file is scanned; `limit`/`offset` page the matches.
            let total = indexer.len();
            let tail = args
                .get("tail")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let start = tail.map(|t| total.saturating_sub(t)).unwrap_or(0);
            let limit = arg_usize(args, "limit", DEFAULT_ARRAY_LIMIT);
            let offset = arg_usize(args, "offset", 0);

            let mut matches = Vec::new();
            let mut truncated = false;
            let mut scanned = 0usize;
            let mut seen = 0usize;
            let mut lossy_lines = 0usize;
            for idx in start..total {
                scanned += 1;
                // Lossy view: invalid-UTF-8 lines are still matched (shown
                // with U+FFFD) and counted, not silently skipped. An Owned
                // Cow means from_utf8_lossy had to repair invalid bytes.
                if let Some(line) = indexer.get_line_lossy(idx) {
                    if matches!(line, std::borrow::Cow::Owned(_)) {
                        lossy_lines += 1;
                    }
                    let record = parse_line(&line, idx + 1);
                    if filter.matches(&record) {
                        seen += 1;
                        if seen > offset + limit {
                            truncated = true;
                            break;
                        }
                        if seen > offset {
                            matches.push(serde_json::json!({
                                "line": record.line_number,
                                "level": record.level.as_str(),
                                "raw": record.raw,
                            }));
                        }
                    }
                }
            }
            Ok(serde_json::json!({
                "_self_paginated": true,
                "matches": matches,
                "scanned_lines": scanned,
                "total_lines": total,
                "lossy_lines": lossy_lines,
                "tail": tail,
                "truncated": truncated,
            }))
        }
        "lens_trace_analyze" => {
            let p = tool_path(args, "trace_file")?;
            let content = fs::read_to_string(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
            // Same fail-closed gate as the CLI: non-strace input is an
            // in-band tool error, not a meaningless snapshot.
            let snap = lens_trace::analyze_strace_text(&content)
                .map_err(|e| format!("{}: {}", p.display(), e))?;
            serde_json::to_value(&snap).map_err(|e| e.to_string())
        }
        "lens_sys_cycles" => {
            let dir = tool_path(args, "dir")?;
            let units = load_sys_units(&dir).map_err(|e| e.to_string())?;
            let graph = OrderingGraph::build(&units);
            let cycles = graph.find_cycles();
            Ok(serde_json::json!({
                "scanned_units": units.len(),
                "cycle_count": cycles.len(),
                "cycles": cycles,
            }))
        }
        "lens_sys_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            let s1 = sys_snapshot_from(&baseline)?;
            let s2 = sys_snapshot_from(&candidate)?;
            let diff = lens_sys::diff_systemd(&s1, &s2);
            serde_json::to_value(&diff).map_err(|e| e.to_string())
        }
        "lens_test_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            let r1 = parse_test_run(&baseline, "baseline")?;
            let r2 = parse_test_run(&candidate, "candidate")?;
            let diff = lens_test::diff_test_runs(&r1, &r2);
            serde_json::to_value(&diff).map_err(|e| e.to_string())
        }
        "lens_net_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            let b: lens_net::NetReport = read_json_file(&baseline)?;
            let c: lens_net::NetReport = read_json_file(&candidate)?;
            let diff = lens_net::diff_net_reports(&b, &c);
            serde_json::to_value(&diff).map_err(|e| e.to_string())
        }
        "lens_build_impact" => {
            let file = tool_path(args, "compile_commands")?;
            let header = arg_str(args, "header")?;
            let entries: Vec<CompileCommandEntry> = read_json_file(&file)?;
            let mut graph = ImpactGraph::new();
            for entry in &entries {
                let unit = parse_command_entry(entry);
                graph.add_translation_unit(&unit);
            }
            // Relative headers resolve against the compile database
            // directory and each entry's `directory` — the server cwd is
            // only a last resort.
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
            let target = lens_build::resolve_header_target(&graph, header, &bases);
            let mut report = graph.compute_impact(&target);
            if !graph.header_in_graph(&report.target_header) {
                let candidates = graph.closest_headers(header, 5);
                let mut hint = format!(
                    "header {:?} is not in the include graph (resolved as {})",
                    header, report.target_header
                );
                if !candidates.is_empty() {
                    hint.push_str(&format!("; similar: {}", candidates.join(", ")));
                }
                hint.push_str(&format!(
                    "; pass an absolute path or one relative to the compile database directory ({})",
                    db_dir
                ));
                report.hint = Some(hint);
            }
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        "lens_env_check" => {
            let path = tool_path(args, "venv_path")?;
            let extras: Vec<String> = args
                .get("extras")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let venv = inspect_venv(&path, &extras).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "python_version": venv.python_version,
                "packages_count": venv.packages.len(),
                "missing_dependencies": venv.missing_dependencies,
                "version_conflicts": venv.version_conflicts,
                "unevaluated_dependencies": venv.unevaluated_dependencies,
                "shadowing_issues": venv.shadowing_issues,
            }))
        }
        "lens_net_inspect" => {
            let proc_dir = arg_opt(args, "proc_dir").map(Path::new);
            let report = lens_net::inspect_network(proc_dir).map_err(|e| e.to_string())?;
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        "lens_bundle_verify" => {
            let p = tool_path(args, "bundle_path")?;
            let report = lens_core::verify_bundle_archive(&p).map_err(|e| e.to_string())?;
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        "lens_doctor" => {
            // Explicit override paths are validated up front — a typo'd
            // --systemd-dir must be an in-band tool error (isError), not
            // a vacuous "everything looks fine" report.
            let root_path = arg_opt(args, "root_path").map(Path::new);
            let proc_dir = arg_opt(args, "proc_dir").map(Path::new);
            let systemd_dir = arg_opt(args, "systemd_dir").map(Path::new);
            for (flag, dir) in [
                ("root_path", root_path),
                ("proc_dir", proc_dir),
                ("systemd_dir", systemd_dir),
            ] {
                if let Some(p) = dir {
                    if !p.exists() {
                        return Err(format!("{} path {:?} does not exist", flag, p));
                    }
                }
            }
            let report = lens_cli::doctor::run_doctor(root_path, proc_dir, systemd_dir);
            serde_json::to_value(&report).map_err(|e| e.to_string())
        }
        other => Err(format!("Unknown tool: {}", other)),
    }
}

pub fn execute_tool(name: &str, args: &Value) -> Result<String, String> {
    let value = execute_tool_value(name, args)?;
    serialize_result(value, args)
}

fn load_sys_units(dir: &Path) -> std::io::Result<BTreeMap<String, SystemdUnit>> {
    lens_sys::load_units(dir).map_err(|e| std::io::Error::other(e.to_string()))
}
