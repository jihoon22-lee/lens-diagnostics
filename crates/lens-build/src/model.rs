use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SNAPSHOT_SCHEMA_V4: &str = "buildscope.snapshot/v4";
pub const DIFF_SCHEMA_V1: &str = "buildscope.diff/v1";
pub const IMPACT_SCHEMA_V1: &str = "buildscope.impact/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileCommandEntry {
    pub directory: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedUnit {
    pub file: String,
    pub directory: String,
    pub compiler: String,
    pub includes: Vec<String>,
    pub defines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    /// Headers injected via `-include`, treated as includes of the unit file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forced_includes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSnapshot {
    pub schema: String,
    pub version: String,
    pub total_units: usize,
    pub units: Vec<ParsedUnit>,
    /// header -> translation units that include it *directly* (no
    /// intermediate header). Kept for v4 compatibility.
    pub reverse_impact: BTreeMap<String, Vec<String>>,
    /// header -> every translation unit transitively impacted by a
    /// change to it, including units that only reach it through an
    /// intermediate header.
    #[serde(default)]
    pub transitive_impact: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncludeOrderChange {
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnitDiff {
    pub file: String,
    pub added_flags: Vec<String>,
    pub removed_flags: Vec<String>,
    pub added_defines: Vec<String>,
    pub removed_defines: Vec<String>,
    pub added_includes: Vec<String>,
    pub removed_includes: Vec<String>,
    /// Ordered forced includes; absent in older buildscope.diff/v1 reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_include_order: Option<IncludeOrderChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildDiff {
    pub schema: String,
    pub added_units: Vec<String>,
    pub removed_units: Vec<String>,
    pub modified_units: Vec<UnitDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactReport {
    pub schema: String,
    pub target_header: String,
    pub impacted_units: Vec<String>,
    pub total_impacted: usize,
    /// True when the on-disk include scan hit its file-count bound — the
    /// transitive graph may be incomplete.
    #[serde(default)]
    pub scan_truncated: bool,
    /// Compile-database entries whose source file could not be read.
    #[serde(default)]
    pub missing_sources: usize,
    /// `#include` directives that resolved to no file on disk.
    #[serde(default)]
    pub unresolved_includes: usize,
    /// Populated when the target header is absent from the include graph:
    /// closest basename matches and how to spell the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
