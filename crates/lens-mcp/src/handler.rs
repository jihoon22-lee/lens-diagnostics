use serde_json::Value;
use std::path::{Path, PathBuf};

use lens_log::parse_line;
use lens_ops::{Finding, Report};

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

/// Walk `v` truncating every array to its first `limit` items. `offset`
/// applies only to the array at `page_path` (dot path; array elements
/// are traversed as `[]`, e.g. `units[].diagnostics`). Every cut is
/// merged into `meta` as `path -> {total, returned, offset}` — `[]`
/// paths sum total/returned across occurrences. No marker objects are
/// ever injected into the data.
fn paginate_arrays(
    v: &mut Value,
    path: &str,
    page_path: Option<&str>,
    offset: usize,
    limit: usize,
    meta: &mut serde_json::Map<String, Value>,
) {
    match v {
        Value::Array(arr) => {
            let total = arr.len();
            let off = if Some(path) == page_path {
                offset.min(total)
            } else {
                0
            };
            if off > 0 {
                arr.drain(..off);
            }
            let returned = arr.len().min(limit);
            arr.truncate(limit);
            // Elements live under `path[]` — their arrays and objects are
            // children of every occurrence of this array.
            let elem_path = format!("{path}[]");
            for item in arr.iter_mut() {
                paginate_arrays(item, &elem_path, page_path, offset, limit, meta);
            }
            if returned < total {
                let entry = meta
                    .entry(path.to_string())
                    .or_insert_with(|| serde_json::json!({"total": 0, "returned": 0, "offset": 0}));
                if let Some(m) = entry.as_object_mut() {
                    let t = m["total"].as_u64().unwrap_or(0) + total as u64;
                    let r = m["returned"].as_u64().unwrap_or(0) + returned as u64;
                    m.insert("total".to_string(), serde_json::json!(t));
                    m.insert("returned".to_string(), serde_json::json!(r));
                    if off > 0 {
                        m.insert("offset".to_string(), serde_json::json!(off));
                    }
                }
            }
        }
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                paginate_arrays(val, &child, page_path, offset, limit, meta);
            }
        }
        _ => {}
    }
}

/// Serialize a tool result: array truncation with `limit`, `offset`
/// applied only at `page_path`, then a hard size guard that re-clamps
/// aggressively if the result still exceeds the response budget. A tool
/// that paginated its own output (log_filter's `matches`) sets
/// `_self_paginated` so the fallback does not apply `offset` a second
/// time.
fn serialize_result(v: Value, args: &Value) -> Result<String, String> {
    let limit = arg_usize(args, "limit", DEFAULT_ARRAY_LIMIT);
    let page_path = arg_opt(args, "page_path").map(str::to_string);
    let mut offset = arg_usize(args, "offset", 0);
    let mut value = v;
    if let Value::Object(m) = &mut value {
        if m.remove("_self_paginated").is_some() {
            offset = 0;
        }
    }
    // Re-pagination always starts from the untruncated value so the
    // `_pagination` totals describe the real data, not the first pass.
    for clamp in [limit, CLAMPED_ARRAY_LIMIT] {
        let mut out = value.clone();
        let mut meta = serde_json::Map::new();
        paginate_arrays(&mut out, "", page_path.as_deref(), offset, clamp, &mut meta);
        if let Value::Object(m) = &mut out {
            if !meta.is_empty() {
                m.insert("_pagination".to_string(), Value::Object(meta));
            }
            if clamp != limit {
                m.insert("_response_clamped".to_string(), serde_json::json!(true));
            }
        }
        let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
        if text.len() <= MAX_RESPONSE_BYTES || clamp == CLAMPED_ARRAY_LIMIT {
            return Ok(text);
        }
    }
    unreachable!()
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

/// What a tool produced: the JSON payload plus the `Report` judgement —
/// findings and the completeness flag ride to the caller as a second
/// content item so agents see degraded evidence.
struct ToolOutcome {
    value: Value,
    findings: Vec<Finding>,
    complete: bool,
}

impl<T: serde::Serialize> From<Report<T>> for ToolOutcome {
    fn from(report: Report<T>) -> Self {
        Self {
            value: serde_json::to_value(&report.data).unwrap_or(Value::Null),
            findings: report.findings,
            complete: report.complete,
        }
    }
}

fn execute_tool_value(name: &str, args: &Value) -> Result<ToolOutcome, String> {
    match name {
        "lens_disk_scan" => {
            let path = tool_path(args, "path")?;
            let report = lens_ops::disk::scan(&path, lens_disk::ScanOptions::default())
                .map_err(|e| e.to_string())?;
            let res = &report.data;
            let value = if args.get("json").and_then(|v| v.as_bool()).unwrap_or(false) {
                let snap = lens_disk::SnapshotV2::from_tree(
                    &res.tree,
                    res.root_id,
                    res.complete,
                    res.truncated,
                );
                serde_json::to_value(&snap).map_err(|e| e.to_string())?
            } else {
                let children_count = res.tree.children_ids(res.root_id).len();
                let root = &res.tree.nodes[res.root_id as usize];
                serde_json::json!({
                    "path": path.to_string_lossy(),
                    "scanned_entries": res.scanned_entries,
                    "logical_bytes": root.size,
                    "allocated_bytes": root.allocated_size,
                    "children_count": children_count,
                    "complete": res.complete,
                    "errors": res.errors,
                })
            };
            Ok(ToolOutcome {
                value,
                findings: report.findings,
                complete: report.complete,
            })
        }
        "lens_disk_duplicates" => {
            let path = tool_path(args, "path")?;
            let min_size = args
                .get("min_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024);
            lens_ops::disk::duplicates(&path, min_size, lens_disk::ScanOptions::default())
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_abi_inspect" => {
            let p = tool_path(args, "binary")?;
            lens_ops::abi::inspect(&p, false)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_abi_diff" => {
            let b = tool_path(args, "baseline")?;
            let c = tool_path(args, "candidate")?;
            lens_ops::abi::diff(&b, &c, false)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_log_filter" => {
            let path = tool_path(args, "path")?;
            let indexer = lens_ops::log::open(&path).map_err(|e| e.to_string())?;
            let filter = lens_ops::log::build_filter(&lens_ops::log::FilterArgs {
                query: arg_opt(args, "query").map(String::from),
                regex: arg_opt(args, "regex").map(String::from),
                min_level: arg_opt(args, "min_level").map(String::from),
                include_unknown: args
                    .get("include_unknown")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                since: arg_opt(args, "since").map(String::from),
                until: arg_opt(args, "until").map(String::from),
                year: args.get("year").and_then(|v| v.as_i64()).map(|y| y as i32),
            })
            .map_err(|e| e.to_string())?;

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
            Ok(ToolOutcome {
                value: serde_json::json!({
                    "_self_paginated": true,
                    "matches": matches,
                    "scanned_lines": scanned,
                    "total_lines": total,
                    "lossy_lines": lossy_lines,
                    "tail": tail,
                    "truncated": truncated,
                }),
                findings: Vec::new(),
                complete: true,
            })
        }
        "lens_trace_analyze" => {
            let p = tool_path(args, "trace_file")?;
            lens_ops::trace::analyze(&p)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_sys_cycles" => {
            let dir = tool_path(args, "dir")?;
            let report = lens_ops::sys::cycles(Some(&dir)).map_err(|e| e.to_string())?;
            let value = serde_json::json!({
                "scanned_units": report.data.scanned_units,
                "cycle_count": report.data.cycles.len(),
                "cycles": report
                    .data
                    .cycles
                    .iter()
                    .map(|c| c.members.clone())
                    .collect::<Vec<_>>(),
            });
            Ok(ToolOutcome {
                value,
                findings: report.findings,
                complete: report.complete,
            })
        }
        "lens_sys_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            lens_ops::sys::diff(&baseline, &candidate)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_test_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            lens_ops::test::diff(&baseline, &candidate)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_net_diff" => {
            let baseline = tool_path(args, "baseline")?;
            let candidate = tool_path(args, "candidate")?;
            lens_ops::net::diff(&baseline, &candidate)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_build_impact" => {
            let file = tool_path(args, "compile_commands")?;
            let header = arg_str(args, "header")?;
            lens_ops::build::impact(&file, header)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
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
            let report = lens_ops::env::check(&path, &extras).map_err(|e| e.to_string())?;
            let venv = &report.data;
            Ok(ToolOutcome {
                value: serde_json::json!({
                    "python_version": venv.python_version,
                    "packages_count": venv.packages.len(),
                    "missing_dependencies": venv.missing_dependencies,
                    "version_conflicts": venv.version_conflicts,
                    "unevaluated_dependencies": venv.unevaluated_dependencies,
                    "shadowing_issues": venv.shadowing_issues,
                }),
                findings: report.findings,
                complete: report.complete,
            })
        }
        "lens_net_inspect" => {
            let proc_dir = arg_opt(args, "proc_dir").map(Path::new);
            lens_ops::net::inspect(proc_dir, false)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_bundle_verify" => {
            let p = tool_path(args, "bundle_path")?;
            lens_ops::bundle::verify(&p)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        "lens_doctor" => {
            // Explicit override paths are validated inside lens_ops —
            // a typo'd `systemd_dir` is an in-band tool error (isError),
            // not a vacuous "everything looks fine" report.
            let root_path = arg_opt(args, "root_path").map(Path::new);
            let proc_dir = arg_opt(args, "proc_dir").map(Path::new);
            let systemd_dir = arg_opt(args, "systemd_dir").map(Path::new);
            lens_ops::doctor::run(root_path, proc_dir, systemd_dir)
                .map(ToolOutcome::from)
                .map_err(|e| e.to_string())
        }
        other => Err(format!("Unknown tool: {}", other)),
    }
}

/// Serialized tool result: the data JSON text plus, when the operation
/// produced findings or incomplete evidence, a second content item's
/// text — `{"findings":[...], "complete":bool}` as compact JSON.
pub struct ToolOutput {
    pub data: String,
    pub findings: Option<String>,
}

pub fn execute_tool(name: &str, args: &Value) -> Result<ToolOutput, String> {
    let outcome = execute_tool_value(name, args)?;
    let findings = if !outcome.findings.is_empty() || !outcome.complete {
        Some(
            serde_json::json!({
                "findings": outcome.findings,
                "complete": outcome.complete,
            })
            .to_string(),
        )
    } else {
        None
    };
    Ok(ToolOutput {
        data: serialize_result(outcome.value, args)?,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paginate(v: &Value, page_path: Option<&str>, offset: usize, limit: usize) -> Value {
        let mut out = v.clone();
        let mut meta = serde_json::Map::new();
        paginate_arrays(&mut out, "", page_path, offset, limit, &mut meta);
        if let Value::Object(m) = &mut out {
            if !meta.is_empty() {
                m.insert("_pagination".to_string(), Value::Object(meta));
            }
        }
        out
    }

    #[test]
    fn pagination_truncates_without_marker_objects() {
        // String arrays must never gain injected marker objects.
        let v = json!({
            "items": ["a", "b", "c", "d", "e"],
            "nested": { "deps": ["x", "y", "z"] }
        });
        let out = paginate(&v, None, 0, 2);
        assert_eq!(out["items"], json!(["a", "b"]));
        assert_eq!(out["nested"]["deps"], json!(["x", "y"]));
        assert!(out["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.is_string()));
        let p = &out["_pagination"];
        assert_eq!(p["items"]["total"], 5);
        assert_eq!(p["items"]["returned"], 2);
        assert_eq!(p["nested.deps"]["total"], 3);
    }

    #[test]
    fn pagination_offset_only_at_page_path() {
        // offset must not shift every array — only the paged one.
        let v = json!({
            "imports": ["i0", "i1", "i2", "i3", "i4"],
            "needed": ["libc", "libm"]
        });
        let out = paginate(&v, Some("imports"), 2, 2);
        assert_eq!(out["imports"], json!(["i2", "i3"]));
        assert_eq!(out["needed"], json!(["libc", "libm"]));
        assert_eq!(out["_pagination"]["imports"]["offset"], 2);
        assert_eq!(out["_pagination"]["imports"]["total"], 5);
        // `needed` was under the limit — no metadata entry.
        assert!(out["_pagination"]["needed"].is_null());
    }

    #[test]
    fn pagination_bracket_path_aggregates_occurrences() {
        // `units[].diagnostics` traverses array elements; metadata sums
        // total/returned across every occurrence.
        let v = json!({
            "units": [
                {"name": "a", "diagnostics": ["d1", "d2", "d3"]},
                {"name": "b", "diagnostics": ["d4", "d5", "d6"]}
            ]
        });
        let out = paginate(&v, None, 0, 2);
        assert_eq!(out["units"][0]["diagnostics"], json!(["d1", "d2"]));
        assert_eq!(out["units"][1]["diagnostics"], json!(["d4", "d5"]));
        let m = &out["_pagination"]["units[].diagnostics"];
        assert_eq!(m["total"], 6);
        assert_eq!(m["returned"], 4);
        // The outer `units` array fit — no entry for it.
        assert!(out["_pagination"]["units"].is_null());
    }

    #[test]
    fn pagination_self_paginated_disables_offset() {
        let args = json!({"limit": 2, "offset": 3});
        let text = serialize_result(
            json!({"_self_paginated": true, "matches": ["m1", "m2", "m3", "m4"]}),
            &args,
        )
        .unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        // The tool already applied offset — the paginator must not eat
        // the first items again.
        assert_eq!(out["matches"], json!(["m1", "m2"]));
        assert!(out.get("_self_paginated").is_none());
    }
}
