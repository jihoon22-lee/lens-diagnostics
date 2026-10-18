use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SNAPSHOT_SCHEMA_V1: &str = "tracelens.snapshot/v1";
pub const DIFF_SCHEMA_V1: &str = "tracelens.diff/v1";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyscallStats {
    pub count: u64,
    pub errors: u64,
    pub known_duration: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    #[serde(default)]
    pub p50_ns: u64,
    #[serde(default)]
    pub p95_ns: u64,
    #[serde(default)]
    pub p99_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessInfo {
    pub tid: u64,
    pub generation: u64,
    pub calls: u64,
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub relation: String,
    #[serde(default)]
    pub exited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub kind: String,
    pub syscall: String,
    pub arguments: String,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ns: Option<u64>,
    pub tid: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSnapshot {
    pub schema: String,
    pub version: String,
    pub total_events: u64,
    pub total_calls: u64,
    pub total_errors: u64,
    #[serde(default)]
    pub unparsed_lines: u64,
    #[serde(default)]
    pub unfinished_calls: u64,
    pub syscalls: BTreeMap<String, SyscallStats>,
    /// Error counts keyed by `syscall:errno` (e.g. `openat:ENOENT`).
    pub errors: BTreeMap<String, u64>,
    /// Representative arguments for the first occurrence of each
    /// `syscall:errno` error key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub error_samples: BTreeMap<String, String>,
    pub processes: BTreeMap<String, ProcessInfo>,
    pub events: Vec<TraceEvent>,
    /// Union of file descriptors left open when each process exited or when
    /// the trace ended (kept for v1 schema compatibility).
    #[serde(default)]
    pub fd_leaks: Vec<u64>,
    /// Per-process view of `fd_leaks`: tid -> fds still open at exit/end.
    /// fd numbers are per-process, so a global set misattributes closes across
    /// processes; this map preserves attribution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fd_leaks_by_process: BTreeMap<String, Vec<u64>>,
    #[serde(default)]
    pub io_read_bytes: u64,
    #[serde(default)]
    pub io_write_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyscallDelta {
    pub syscall: String,
    pub baseline_count: u64,
    pub candidate_count: u64,
    pub count_delta: i64,
    pub baseline_errors: u64,
    pub candidate_errors: u64,
    pub error_delta: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDiff {
    pub schema: String,
    pub baseline_calls: u64,
    pub candidate_calls: u64,
    pub call_delta: i64,
    pub baseline_errors: u64,
    pub candidate_errors: u64,
    pub error_delta: i64,
    pub new_errors: Vec<String>,
    pub resolved_errors: Vec<String>,
    pub significant_latency_shifts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syscall_deltas: Vec<SyscallDelta>,
}
