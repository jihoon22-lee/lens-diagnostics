use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

const CWD_NOTE: &str =
    " Relative paths resolve against the lens-mcp server process's working directory.";

fn desc(text: &str) -> String {
    format!("{}{}", text, CWD_NOTE)
}

/// Shared pagination arguments — every tool accepts `limit` (max items
/// per array, default 200), `page_path` (dot path of the one array that
/// `offset` applies to, e.g. `abi.imports`; array elements are addressed
/// as `[]`, e.g. `units[].diagnostics`), and `offset` (skip the first N
/// items of the `page_path` array). Cut arrays are reported in the
/// top-level `_pagination` object as `path -> {total, returned, offset}`;
/// no marker objects are inserted into the data itself.
fn pagination_props() -> serde_json::Map<String, Value> {
    serde_json::json!({
        "limit": { "type": "integer", "description": "Max items per array in the response (default 200)" },
        "page_path": { "type": "string", "description": "Dot path of the array `offset` applies to (e.g. 'abi.imports'; 'units[].diagnostics' addresses arrays inside array elements)" },
        "offset": { "type": "integer", "description": "Skip the first N items of the array named by 'page_path'" }
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

pub fn list_tools() -> Vec<McpTool> {
    let pag = pagination_props();
    let with_pag = |mut props: serde_json::Map<String, Value>| {
        for (k, v) in pag.clone() {
            props.insert(k, v);
        }
        Value::Object(props)
    };
    vec![
        McpTool {
            name: "lens_disk_scan".to_string(),
            description: desc(
                "Scan filesystem directory and return logical/allocated storage usage.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "path": { "type": "string", "description": "Directory path to scan (relative to server cwd if not absolute)" },
                    "json": { "type": "boolean", "description": "Return full snapshot JSON" }
                }).as_object().unwrap().clone()),
                "required": ["path"]
            }),
        },
        McpTool {
            name: "lens_disk_duplicates".to_string(),
            description: desc(
                "Find duplicate files based on SHA-256 hash, ignoring hardlink inodes.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "path": { "type": "string", "description": "Directory path to scan" },
                    "min_size": { "type": "integer", "description": "Minimum file size in bytes" }
                }).as_object().unwrap().clone()),
                "required": ["path"]
            }),
        },
        McpTool {
            name: "lens_abi_inspect".to_string(),
            description: desc(
                "Inspect ELF binary dynamic symbols, version definitions, and DWARF info.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "binary": { "type": "string", "description": "Path to ELF binary" }
                }).as_object().unwrap().clone()),
                "required": ["binary"]
            }),
        },
        McpTool {
            name: "lens_abi_diff".to_string(),
            description: desc(
                "Compare two ELF binaries and report added/removed symbols with 3-state compatibility (compatible/uncertain/incompatible).",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "baseline": { "type": "string", "description": "Older/reference ELF binary" },
                    "candidate": { "type": "string", "description": "Newer ELF binary" }
                }).as_object().unwrap().clone()),
                "required": ["baseline", "candidate"]
            }),
        },
        McpTool {
            name: "lens_log_filter".to_string(),
            description: desc(
                "Filter high-volume log files by substring query or regex, minimum log level, and time range. Use 'tail' to scan the last N lines (recent incident evidence); 'limit'/'offset' page the matched lines.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to log file" },
                    "query": { "type": "string", "description": "Case-sensitive substring query" },
                    "regex": { "type": "string", "description": "Regex pattern (alternative to query)" },
                    "min_level": { "type": "string", "description": "Minimum level (trace, debug, info, warn, error, fatal)" },
                    "include_unknown": { "type": "boolean", "description": "Keep lines whose level or timestamp could not be determined" },
                    "since": { "type": "string", "description": "Only lines at/after this timestamp (inclusive): RFC 3339 (2026-10-09T12:00:00Z, with offset), 'YYYY-MM-DD HH:MM:SS' or 'YYYY-MM-DD' (UTC)" },
                    "until": { "type": "string", "description": "Only lines at/before this timestamp (inclusive); same forms as 'since'" },
                    "year": { "type": "integer", "description": "Year assumed for year-less syslog timestamps like 'Oct  9 12:00:00' (default: current year)" },
                    "tail": { "type": "integer", "description": "Scan only the last N lines of the file" },
                    "limit": { "type": "integer", "description": "Max matched lines to return (default 200)" },
                    "offset": { "type": "integer", "description": "Skip the first N matched lines" }
                },
                "required": ["path"]
            }),
        },
        McpTool {
            name: "lens_trace_analyze".to_string(),
            description: desc(
                "Analyze raw strace log and extract syscall metrics, latencies, and errors.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "trace_file": { "type": "string", "description": "Path to strace log file" }
                }).as_object().unwrap().clone()),
                "required": ["trace_file"]
            }),
        },
        McpTool {
            name: "lens_sys_cycles".to_string(),
            description: desc(
                "Detect dependency ordering cycles among systemd service units.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "dir": { "type": "string", "description": "Directory containing systemd unit files" }
                }).as_object().unwrap().clone()),
                "required": ["dir"]
            }),
        },
        McpTool {
            name: "lens_sys_diff".to_string(),
            description: desc(
                "Compare two systemd inputs — saved snapshot JSON files or live unit directories — and report added/removed/modified units and new/resolved cycles.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "baseline": { "type": "string", "description": "Baseline snapshot JSON or unit directory" },
                    "candidate": { "type": "string", "description": "Candidate snapshot JSON or unit directory" }
                }).as_object().unwrap().clone()),
                "required": ["baseline", "candidate"]
            }),
        },
        McpTool {
            name: "lens_test_diff".to_string(),
            description: desc(
                "Compare two JUnit XML runs (file or directory of *.xml) and report regressions, fixes, and new failures.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "baseline": { "type": "string", "description": "Baseline JUnit XML file or directory" },
                    "candidate": { "type": "string", "description": "Candidate JUnit XML file or directory" }
                }).as_object().unwrap().clone()),
                "required": ["baseline", "candidate"]
            }),
        },
        McpTool {
            name: "lens_net_diff".to_string(),
            description: desc(
                "Compare two network snapshot JSON reports (produced by `lens net inspect --format json`) and report listener/connection deltas.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "baseline": { "type": "string", "description": "Baseline net snapshot JSON" },
                    "candidate": { "type": "string", "description": "Candidate net snapshot JSON" }
                }).as_object().unwrap().clone()),
                "required": ["baseline", "candidate"]
            }),
        },
        McpTool {
            name: "lens_build_impact".to_string(),
            description: desc(
                "Calculate transitive reverse compilation impact of changing a header file. A relative header resolves against the compile database directory and each entry's `directory`, not the server cwd.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "compile_commands": { "type": "string", "description": "Path to compile_commands.json" },
                    "header": { "type": "string", "description": "Target header file — absolute, or relative to the compile database/entry directory" }
                }).as_object().unwrap().clone()),
                "required": ["compile_commands", "header"]
            }),
        },
        McpTool {
            name: "lens_env_check".to_string(),
            description: desc(
                "Check Python virtual environment for missing or unsatisfied dependencies.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "venv_path": { "type": "string", "description": "Path to Python virtualenv root" },
                    "extras": { "type": "array", "items": { "type": "string" }, "description": "Optional extras to activate for `extra == \"name\"` dependency markers" }
                }).as_object().unwrap().clone()),
                "required": ["venv_path"]
            }),
        },
        McpTool {
            name: "lens_net_inspect".to_string(),
            description: desc(
                "Inspect Linux network sockets, listening ports, established connections, and map them to processes.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "proc_dir": { "type": "string", "description": "Optional proc filesystem root (default /proc)" }
                }).as_object().unwrap().clone()),
            }),
        },
        McpTool {
            name: "lens_bundle_diff".to_string(),
            description: desc(
                "Diff two .lens forensic bundles section by section (sys/test/trace/env/net/doctor/disk/log). Both bundles are verified first; returns lens.bundle-diff/v1 with per-section domain diffs.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "baseline": { "type": "string", "description": "Path to older/reference .lens bundle" },
                    "candidate": { "type": "string", "description": "Path to newer .lens bundle" }
                }).as_object().unwrap().clone()),
                "required": ["baseline", "candidate"]
            }),
        },
        McpTool {
            name: "lens_bundle_verify".to_string(),
            description: desc(
                "Verify a .lens forensic archive's contents against the SHA-256 checksums in its embedded manifest.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "bundle_path": { "type": "string", "description": "Path to .lens (.tar.gz) forensic bundle" }
                }).as_object().unwrap().clone()),
                "required": ["bundle_path"]
            }),
        },
        McpTool {
            name: "lens_doctor".to_string(),
            description: desc(
                "Run a comprehensive health check across storage capacity, open network ports, systemd cycles, and environment security. Nonexistent override paths return an in-band error.",
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": with_pag(serde_json::json!({
                    "root_path": { "type": "string", "description": "Optional root filesystem path (default /)" },
                    "proc_dir": { "type": "string", "description": "Optional procfs path (default /proc)" },
                    "systemd_dir": { "type": "string", "description": "Optional systemd unit dir (default: merged systemd search path)" }
                }).as_object().unwrap().clone()),
            }),
        },
    ]
}
