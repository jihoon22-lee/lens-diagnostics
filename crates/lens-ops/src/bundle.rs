use std::path::PathBuf;

use lens_core::{
    create_bundle_archive, extract_bundle, inspect_bundle, read_bundle_entry,
    verify_bundle_archive, BundleArtifact, BundleInspection, BundleManifest,
    BundleVerificationReport, LensError, Result,
};
use lens_disk::{DiskScanner, ScanOptions, SnapshotV2};

use crate::{env, log, net, sys, test, trace, Report, Severity};

/// Cap on raw log bytes embedded in a bundle — keeps `.lens` files useful
/// as evidence without turning into multi-hundred-MB archives. The *tail*
/// is kept: incident investigations care about the most recent lines.
pub const BUNDLE_LOG_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Select up to `cap` bytes of log content for bundling: the whole input
/// when it fits, otherwise the tail starting on a line boundary. Returns
/// the slice and whether it was truncated.
fn tail_on_line_boundary(data: &[u8], cap: usize) -> (&[u8], bool) {
    if data.len() <= cap {
        return (data, false);
    }
    let tail = &data[data.len() - cap..];
    // Skip the partial first line so the excerpt starts cleanly.
    match tail.iter().position(|&b| b == b'\n') {
        Some(nl) => (&tail[nl + 1..], true),
        None => (tail, true),
    }
}

/// Tar entry name for bundled log content: stdin is `stdin.log`, and a
/// `.gz` suffix is dropped since the embedded bytes are decompressed.
fn log_entry_name(path: &std::path::Path) -> String {
    if path.as_os_str() == "-" {
        return "stdin.log".to_string();
    }
    let name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "stdin.log".to_string());
    let name = name.strip_suffix(".gz").unwrap_or(&name);
    name.replace(['/', '\\'], "_")
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Hostname from procfs, falling back to `gethostname(2)` — a fixture
/// procfs without `sys/kernel/hostname` still yields a name.
fn hostname(proc_root: &std::path::Path) -> Option<String> {
    read_trimmed(&proc_root.join("sys/kernel/hostname")).or_else(|| {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } != 0 {
            return None;
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).into_owned();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    })
}

fn os_pretty_name() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    content.lines().find_map(|l| {
        l.strip_prefix("PRETTY_NAME=")
            .map(|v| v.trim_matches('"').to_string())
    })
}

/// `lens.host/v1`: provenance for a snapshot bundle — who/what/when it
/// was collected on.
fn host_snapshot(procfs: Option<&std::path::Path>) -> serde_json::Value {
    let proc_root = procfs.unwrap_or_else(|| std::path::Path::new("/proc"));
    serde_json::json!({
        "schema": "lens.host/v1",
        "hostname": hostname(proc_root),
        "kernel_release": read_trimmed(&proc_root.join("sys/kernel/osrelease")),
        "os_pretty_name": os_pretty_name(),
        "lens_version": env!("CARGO_PKG_VERSION"),
        "collected_at": lens_core::time::utc_now_iso(),
    })
}

/// `bundle create`/`lens snapshot` inputs — mirrors the CLI flags.
#[derive(Debug, Default)]
pub struct BundleRequest {
    pub output: PathBuf,
    pub disk: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub trace: Option<PathBuf>,
    pub test: Option<PathBuf>,
    pub sys: Option<PathBuf>,
    pub env: Option<PathBuf>,
    pub net: bool,
    /// Embed a merged-search-path systemd snapshot (`lens snapshot`'s
    /// default when no `--systemd-dir` is given).
    pub sys_merged: bool,
    /// Embed `reports/doctor_report.json`.
    pub doctor: bool,
    /// Embed `reports/host.json` (hostname/kernel/os provenance).
    pub host: bool,
    /// procfs override for the net, host, and doctor collectors.
    pub procfs: Option<PathBuf>,
    /// systemd unit-dir override for the doctor collector.
    pub systemd_dir: Option<PathBuf>,
    pub force: bool,
    /// Redact secrets/tokens from all collected artifacts before hashing and archiving.
    pub redact: bool,
}

/// Collect every requested artifact and write the bundle. Fail-closed
/// collection: each source's error is preserved in the manifest's
/// diagnostics rather than aborting the bundle; a bundle with no
/// artifacts at all is a usage error. The report is `complete=false`
/// when any collection diagnostic was recorded.
pub fn create(req: &BundleRequest) -> Result<Report<BundleManifest>> {
    // A bundle with zero artifact sources would produce an empty
    // manifest that still verifies OK — refuse to create it.
    let any_source = req.disk.is_some()
        || req.log.is_some()
        || req.trace.is_some()
        || req.test.is_some()
        || req.sys.is_some()
        || req.sys_merged
        || req.env.is_some()
        || req.net
        || req.doctor
        || req.host;
    if !any_source {
        return Err(LensError::Usage {
            message: "at least one artifact source is required \
                      (--disk/--log/--trace/--test/--sys/--env/--net/--doctor)"
                .to_string(),
        });
    }

    // Evidence is append-only: refuse to destroy an existing bundle
    // unless the caller explicitly passes --force.
    if req.output.try_exists().map_err(|e| LensError::Io {
        path: req.output.clone(),
        source: e,
    })? && !req.force
    {
        return Err(LensError::Usage {
            message: format!(
                "output bundle {:?} already exists; pass --force to overwrite",
                req.output
            ),
        });
    }

    let mut artifacts: Vec<BundleArtifact> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();

    // Each producer runs inside its own closure so `?` aborts only that
    // source's collection, never the whole operation.
    macro_rules! collect {
        ($name:literal, $entry:literal, $produce:expr) => {{
            #[allow(unused_mut)]
            let mut produce = || $produce;
            match produce().and_then(|v| BundleArtifact::json($entry, &v)) {
                Ok(artifact) => artifacts.push(artifact),
                Err(e) => diagnostics.push(format!("{}: {}", $name, e)),
            }
        }};
    }

    if let Some(ref d) = req.disk {
        collect!("disk", "reports/disk_snapshot.json", {
            collect_disk(d, ScanOptions::default(), &mut diagnostics)
        });
    }

    if let Some(ref l) = req.log {
        // The log is opened once: the summary JSON and the embedded
        // content (tail-bounded, so the bundle stays evidence rather than
        // just a line count) share the indexer's decoded bytes — `.gz`
        // and stdin (`-`) inputs therefore embed correctly too.
        match log::open(l) {
            Ok(indexer) => {
                match BundleArtifact::json(
                    "reports/log_summary.json",
                    &serde_json::json!({
                        "path": l.to_string_lossy(),
                        "total_lines": indexer.len(),
                        "lossy_lines": indexer.lossy_lines(),
                    }),
                ) {
                    Ok(artifact) => artifacts.push(artifact),
                    Err(e) => diagnostics.push(format!("log: {}", e)),
                }
                let (tail, truncated) =
                    tail_on_line_boundary(indexer.as_bytes(), BUNDLE_LOG_TAIL_BYTES as usize);
                if truncated {
                    diagnostics.push(format!(
                        "log: content truncated to the last {} bytes",
                        BUNDLE_LOG_TAIL_BYTES
                    ));
                }
                artifacts.push(BundleArtifact {
                    name: format!("logs/{}", log_entry_name(l)),
                    data: tail.to_vec(),
                });
            }
            Err(e) => diagnostics.push(format!("log: {}", e)),
        }
    }

    if let Some(ref t) = req.trace {
        collect!("trace", "reports/trace_snapshot.json", {
            trace::analyze_file(t)
        });
    }

    if let Some(ref t) = req.test {
        collect!("test", "reports/test_run.json", {
            let run = test::load_run(t, "bundle")?;
            if !run.complete {
                diagnostics.push("test: incomplete JUnit evidence".to_string());
            }
            Ok(run)
        });
    }

    if req.host {
        collect!(
            "host",
            "reports/host.json",
            Ok(host_snapshot(req.procfs.as_deref()))
        );
    }

    if let Some(ref s) = req.sys {
        collect!("sys", "reports/sys_snapshot.json", {
            sys::snapshot(Some(s))
        });
    } else if req.sys_merged {
        collect!("sys", "reports/sys_snapshot.json", { sys::snapshot(None) });
    }

    if let Some(ref e) = req.env {
        collect!("env", "reports/env_snapshot.json", {
            let venv = lens_env::inspect_venv(e, &[]).map_err(|err| LensError::Usage {
                message: format!("venv {}: {}", e.display(), err),
            })?;
            Ok(lens_env::EnvSnapshot {
                schema: lens_env::SNAPSHOT_SCHEMA_V1.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                venv,
            })
        });
    }

    if req.net {
        collect!("net", "reports/net_report.json", {
            lens_net::inspect_network(req.procfs.as_deref())
        });
    }

    if req.doctor {
        // Only the report data is embedded — doctor findings describe
        // the host's health, not the snapshot's completeness.
        collect!("doctor", "reports/doctor_report.json", {
            crate::doctor::run(None, req.procfs.as_deref(), req.systemd_dir.as_deref())
                .map(|r| r.data)
        });
    }

    if artifacts.is_empty() && !diagnostics.is_empty() {
        // Keep the failure detail in the usage error itself — the
        // diagnostics would otherwise be lost with the report.
        return Err(LensError::Usage {
            message: format!(
                "bundle contains no artifacts; all collections failed: {}",
                diagnostics.join("; ")
            ),
        });
    }

    if req.redact {
        for artifact in &mut artifacts {
            artifact.data = lens_core::redact_bytes(&artifact.data);
        }
    }

    let manifest = create_bundle_archive(
        &req.output,
        env!("CARGO_PKG_VERSION"),
        &artifacts,
        diagnostics.clone(),
        req.force,
    )?;

    let mut report = Report::new(manifest);
    report.complete = diagnostics.is_empty();
    report.diagnostics = diagnostics;
    Ok(report)
}

/// Inspect a bundle's entries and manifest.
pub fn inspect(bundle: &PathBuf) -> Result<Report<BundleInspection>> {
    Ok(Report::new(inspect_bundle(bundle)?))
}

fn collect_disk(
    path: &std::path::Path,
    options: ScanOptions,
    diagnostics: &mut Vec<String>,
) -> Result<SnapshotV2> {
    let res = DiskScanner::new(options).scan(path)?;
    diagnostics.extend(res.errors.iter().map(|e| format!("disk scan: {e}")));
    if !res.complete || res.truncated {
        diagnostics.push(format!(
            "disk scan: incomplete evidence (truncated={})",
            res.truncated
        ));
    }
    Ok(SnapshotV2::from_tree(
        &res.tree,
        res.root_id,
        res.complete,
        res.truncated,
    ))
}

/// `lens.bundle-diff/v1`: section diffs of two verified bundles. Each
/// `sections` value is that domain's normal diff JSON (e.g. the
/// `sys` section is a `SystemdDiff`); `only_in_*` lists entries present
/// on just one side.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BundleDiff {
    pub schema: String,
    pub baseline: String,
    pub candidate: String,
    pub sections: std::collections::BTreeMap<String, serde_json::Value>,
    pub only_in_baseline: Vec<String>,
    pub only_in_candidate: Vec<String>,
}

/// Section entry name → short section key used in `BundleDiff.sections`.
const SECTION_ENTRIES: &[(&str, &str)] = &[
    ("reports/sys_snapshot.json", "sys"),
    ("reports/test_run.json", "test"),
    ("reports/trace_snapshot.json", "trace"),
    ("reports/env_snapshot.json", "env"),
    ("reports/net_report.json", "net"),
    ("reports/doctor_report.json", "doctor"),
    ("reports/disk_snapshot.json", "disk"),
    ("reports/log_summary.json", "log"),
];

trait SectionSchema {
    fn section_schema(&self) -> Option<&str>;
}

macro_rules! section_schema {
    ($type:ty, $field:ident) => {
        impl SectionSchema for $type {
            fn section_schema(&self) -> Option<&str> {
                Some(&self.$field)
            }
        }
    };
}
section_schema!(lens_sys::SystemdSnapshot, schema);
section_schema!(lens_env::EnvSnapshot, schema);
section_schema!(lens_net::NetReport, schema_version);
section_schema!(lens_test::TestRun, schema);
section_schema!(lens_trace::TraceSnapshot, schema);
section_schema!(crate::doctor::DoctorReport, schema_version);
section_schema!(lens_disk::SnapshotV2, schema_version);

#[derive(serde::Deserialize)]
struct LogSummary {
    #[serde(default)]
    total_lines: i64,
}

impl SectionSchema for LogSummary {
    fn section_schema(&self) -> Option<&str> {
        None
    }
}

fn read_entry_json<T: serde::de::DeserializeOwned + SectionSchema>(
    bundle: &PathBuf,
    entry: &str,
) -> Result<T> {
    let bytes = read_bundle_entry(bundle, entry)?;
    // Deserialize directly: unknown extensions are skipped, not allocated as
    // an intermediate JSON tree before being discarded by the typed parser.
    let value: T = serde_json::from_slice(&bytes)?;
    let expected = match entry {
        "reports/sys_snapshot.json" => Some(("schema", lens_sys::SNAPSHOT_SCHEMA_V1)),
        "reports/env_snapshot.json" => Some(("schema", lens_env::SNAPSHOT_SCHEMA_V1)),
        "reports/net_report.json" => Some(("schema_version", lens_net::REPORT_SCHEMA_V1)),
        "reports/test_run.json" => Some(("schema", "testlens.run/v1")),
        "reports/trace_snapshot.json" => Some(("schema", lens_trace::SNAPSHOT_SCHEMA_V1)),
        "reports/doctor_report.json" => Some(("schema_version", "lens.doctor/v1")),
        "reports/disk_snapshot.json" => Some(("schema_version", lens_disk::SCHEMA_V2)),
        _ => None,
    };
    if let Some((_, schema)) = expected {
        if value.section_schema() != Some(schema) {
            return Err(LensError::InvalidInput {
                message: format!("{}:{entry}: expected schema {schema}", bundle.display()),
            });
        }
    }
    Ok(value)
}

fn append_collection_diagnostics(output: &mut Vec<String>, side: &str, input: &[String]) {
    // Bound before cloning attacker-controlled strings. Reserve room for a
    // truncation notice on both sides, and never split UTF-8 code points.
    let mut remaining =
        (64 * 1024usize).saturating_sub(output.iter().map(String::len).sum::<usize>() + 128);
    let mut truncated = false;
    for diagnostic in input {
        if output.len() >= 128 || remaining < 128 {
            truncated = true;
            break;
        }
        let cap = remaining.min(4096) - side.len() - 32;
        let mut end = diagnostic.len().min(cap);
        while !diagnostic.is_char_boundary(end) {
            end -= 1;
        }
        let suffix = if end < diagnostic.len() {
            " [truncated]"
        } else {
            ""
        };
        let message = format!("{side}: {}{suffix}", &diagnostic[..end]);
        remaining -= message.len();
        output.push(message);
    }
    if truncated {
        output.push(format!(
            "{side}: collection diagnostics truncated ({} total)",
            input.len()
        ));
    }
}

/// Diff two forensic bundles section by section. Both bundles are
/// verified first — a failed verification is an input error, never a
/// diff. Sections present on both sides are compared with the same
/// value-level classifiers the file diffs use, so a bundle diff
/// produces identical findings codes (`sys.new_cycle`, …). A section
/// that fails to deserialize is a diagnostic + incomplete report, not
/// an abort.
pub fn diff(base: &PathBuf, cand: &PathBuf) -> Result<Report<BundleDiff>> {
    for p in [base, cand] {
        let vr = verify_bundle_archive(p)?;
        if !vr.is_valid {
            return Err(LensError::InvalidInput {
                message: format!(
                    "bundle {:?} failed verification: {}",
                    p,
                    vr.error
                        .unwrap_or_else(|| "integrity check failed".to_string())
                ),
            });
        }
    }

    let ins_a = inspect_bundle(base)?;
    let ins_b = inspect_bundle(cand)?;
    // Compare under the same normalized names verification uses — a
    // bundle packed with `./reports/…` must diff section-for-section
    // against one packed with `reports/…`.
    let names_a: std::collections::BTreeSet<String> = ins_a
        .entries
        .iter()
        .map(|e| lens_core::normalize_entry_name(&e.name).to_string())
        .collect();
    let names_b: std::collections::BTreeSet<String> = ins_b
        .entries
        .iter()
        .map(|e| lens_core::normalize_entry_name(&e.name).to_string())
        .collect();

    let only_in_baseline: Vec<String> = names_a
        .difference(&names_b)
        .filter(|n| n.as_str() != "manifest.json")
        .cloned()
        .collect();
    let only_in_candidate: Vec<String> = names_b
        .difference(&names_a)
        .filter(|n| n.as_str() != "manifest.json")
        .cloned()
        .collect();

    let mut report = Report::new(BundleDiff {
        schema: "lens.bundle-diff/v1".to_string(),
        baseline: base.to_string_lossy().into_owned(),
        candidate: cand.to_string_lossy().into_owned(),
        sections: std::collections::BTreeMap::new(),
        only_in_baseline: only_in_baseline.clone(),
        only_in_candidate: only_in_candidate.clone(),
    });

    for (side, inspection) in [("baseline", &ins_a), ("candidate", &ins_b)] {
        if let Some(manifest) = &inspection.manifest {
            if !manifest.diagnostics.is_empty() {
                report.complete = false;
                append_collection_diagnostics(&mut report.diagnostics, side, &manifest.diagnostics);
            }
        }
    }

    for n in &only_in_baseline {
        report.push(
            Severity::Info,
            "bundle.only_in_baseline",
            format!("entry only in baseline: {n}"),
        );
    }
    for n in &only_in_candidate {
        report.push(
            Severity::Info,
            "bundle.only_in_candidate",
            format!("entry only in candidate: {n}"),
        );
    }

    // Per-section comparison. Each closure returns the section diff
    // report; a `?` inside it aborts only that section (recorded as a
    // diagnostic + incomplete evidence).
    for (entry, key) in SECTION_ENTRIES {
        if !(names_a.contains(*entry) && names_b.contains(*entry)) {
            continue;
        }
        let section: Result<Report<serde_json::Value>> = (|| {
            let sub: Report<serde_json::Value> = match *key {
                "sys" => {
                    let a = read_entry_json::<lens_sys::SystemdSnapshot>(base, entry)?;
                    let b = read_entry_json::<lens_sys::SystemdSnapshot>(cand, entry)?;
                    let r = sys::diff_snapshots(&a, &b);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "test" => {
                    let a = read_entry_json::<lens_test::TestRun>(base, entry)?;
                    let b = read_entry_json::<lens_test::TestRun>(cand, entry)?;
                    let r = test::diff_runs(&a, &b);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "trace" => {
                    let a = read_entry_json::<lens_trace::TraceSnapshot>(base, entry)?;
                    let b = read_entry_json::<lens_trace::TraceSnapshot>(cand, entry)?;
                    let r = trace::diff_values(&a, &b);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "env" => {
                    let a = read_entry_json::<lens_env::EnvSnapshot>(base, entry)?;
                    let b = read_entry_json::<lens_env::EnvSnapshot>(cand, entry)?;
                    let r = env::diff_values(&a.venv, &b.venv);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "net" => {
                    let a = read_entry_json::<lens_net::NetReport>(base, entry)?;
                    let b = read_entry_json::<lens_net::NetReport>(cand, entry)?;
                    let r = net::diff_values(&a, &b);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "doctor" => {
                    let a = read_entry_json::<crate::doctor::DoctorReport>(base, entry)?;
                    let b = read_entry_json::<crate::doctor::DoctorReport>(cand, entry)?;
                    let r = crate::doctor::diff_reports(&a, &b);
                    Report {
                        data: serde_json::to_value(&r.data)?,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "disk" => {
                    let a = read_entry_json::<lens_disk::SnapshotV2>(base, entry)?;
                    let b = read_entry_json::<lens_disk::SnapshotV2>(cand, entry)?;
                    let value = serde_json::json!({
                        "root_size_delta": b.root.size as i64 - a.root.size as i64,
                        "root_allocated_delta": b.root.allocated_size as i64
                            - a.root.allocated_size as i64,
                        "node_count_delta": b.node_count as i64 - a.node_count as i64,
                    });
                    let mut r = Report::new(value);
                    r.complete = a.complete && b.complete && !a.truncated && !b.truncated;
                    if !r.complete {
                        r.diagnostics
                            .push("disk: incomplete snapshot evidence".to_string());
                    }
                    let any = r.data["root_size_delta"] != 0
                        || r.data["root_allocated_delta"] != 0
                        || r.data["node_count_delta"] != 0;
                    if any {
                        r.push(
                            Severity::Info,
                            "bundle.disk_delta",
                            format!(
                                "root size {:+} B, allocated {:+} B, nodes {:+}",
                                r.data["root_size_delta"],
                                r.data["root_allocated_delta"],
                                r.data["node_count_delta"]
                            ),
                        );
                    }
                    Report {
                        data: r.data,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                "log" => {
                    let a = read_entry_json::<LogSummary>(base, entry)?;
                    let b = read_entry_json::<LogSummary>(cand, entry)?;
                    let delta = b.total_lines - a.total_lines;
                    let mut r = Report::new(serde_json::json!({
                        "total_lines_delta": delta,
                    }));
                    if delta != 0 {
                        r.push(
                            Severity::Info,
                            "bundle.log_delta",
                            format!("log line count {delta:+}"),
                        );
                    }
                    Report {
                        data: r.data,
                        findings: r.findings,
                        complete: r.complete,
                        diagnostics: r.diagnostics,
                    }
                }
                _ => unreachable!("SECTION_ENTRIES covers every key"),
            };
            Ok(sub)
        })();
        match section {
            Ok(sub) => {
                report.data.sections.insert(key.to_string(), sub.data);
                report.findings.extend(sub.findings);
                report.diagnostics.extend(sub.diagnostics);
                if !sub.complete {
                    report.complete = false;
                }
            }
            Err(e) => {
                report.diagnostics.push(format!("section {key}: {e}"));
                report.complete = false;
            }
        }
    }

    Ok(report)
}

/// Verify a bundle against its embedded manifest. A negative result is
/// a Fail finding (exit 1), not an operational error.
pub fn verify(bundle: &PathBuf) -> Result<Report<BundleVerificationReport>> {
    let verification = verify_bundle_archive(bundle)?;
    let mut report = Report::new(verification);
    if !report.data.is_valid {
        report.push(
            Severity::Fail,
            "bundle.integrity",
            report
                .data
                .error
                .clone()
                .unwrap_or_else(|| "integrity check failed".to_string()),
        );
    }
    Ok(report)
}

/// Read one archive entry's raw bytes.
pub fn show(bundle: &PathBuf, entry: &str) -> Result<Report<Vec<u8>>> {
    Ok(Report::new(read_bundle_entry(bundle, entry)?))
}

/// Verify then extract; returns the sorted list of written paths.
pub fn extract(bundle: &PathBuf, dest: &PathBuf, force: bool) -> Result<Report<Vec<PathBuf>>> {
    Ok(Report::new(extract_bundle(bundle, dest, force)?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn review2_manifest_diagnostics_are_bounded_before_cloning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.lens");
        create_bundle_archive(
            &path,
            "test",
            &[json_artifact(
                "reports/log_summary.json",
                serde_json::json!({"total_lines":0}),
            )],
            vec!["x".repeat(256 * 1024)],
            false,
        )
        .unwrap();
        let report = diff(&path, &path).unwrap();
        assert!(!report.complete);
        assert!(report.diagnostics.iter().map(String::len).sum::<usize>() <= 64 * 1024);
        assert!(report.diagnostics.iter().any(|d| d.contains("truncated")));
        let mut messages = Vec::new();
        let input = vec!["한글".repeat(1024); 300];
        append_collection_diagnostics(&mut messages, "baseline", &input);
        append_collection_diagnostics(&mut messages, "candidate", &input);
        assert!(messages.iter().map(String::len).sum::<usize>() <= 64 * 1024);
        assert!(messages.len() <= 130);
    }
    #[test]
    fn bundle_records_recoverable_junit_and_disk_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let xml = dir.path().join("truncated.xml");
        std::fs::write(&xml, "<testsuite><testcase name=\"ok\"/>").unwrap();
        let report = create(&BundleRequest {
            output: dir.path().join("test.lens"),
            test: Some(xml),
            ..Default::default()
        })
        .unwrap();
        assert!(!report.complete);
        assert!(report.data.diagnostics.iter().any(|d| d.contains("JUnit")));
        let mut diagnostics = Vec::new();
        let snapshot = collect_disk(
            dir.path(),
            ScanOptions {
                max_entries: 1,
                ..Default::default()
            },
            &mut diagnostics,
        )
        .unwrap();
        assert!(snapshot.truncated);
        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn bundle_sections_reject_foreign_schemas() {
        let dir = tempfile::tempdir().unwrap();
        for (entry, _) in SECTION_ENTRIES.iter().filter(|(_, key)| *key != "log") {
            let path = dir.path().join("schema.lens");
            create_bundle_archive(
                &path,
                "test",
                &[json_artifact(
                    entry,
                    serde_json::json!({"schema": "foreign/v99", "schema_version": "foreign/v99"}),
                )],
                vec![],
                true,
            )
            .unwrap();
            let report = diff(&path, &path).unwrap();
            assert!(!report.complete, "{entry}");
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|d| d.contains("expected schema") || d.contains("missing field")),
                "{:?}",
                report.diagnostics
            );
        }
    }

    #[test]
    fn bundle_diff_preserves_collection_failures() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.lens");
        let b = dir.path().join("b.lens");
        for path in [&a, &b] {
            create_bundle_archive(
                path,
                "test",
                &[json_artifact(
                    "reports/log_summary.json",
                    serde_json::json!({"total_lines": 0}),
                )],
                vec!["sys: collection failed".into()],
                false,
            )
            .unwrap();
        }
        let report = diff(&a, &b).unwrap();
        assert!(
            !report.complete,
            "failed collections must never appear complete"
        );
        assert_eq!(report.diagnostics.len(), 2);
    }
    use super::*;

    fn bundle_with(dir: &std::path::Path, name: &str, artifacts: Vec<BundleArtifact>) -> PathBuf {
        let path = dir.join(name);
        create_bundle_archive(&path, "0.0.0-test", &artifacts, Vec::new(), false).unwrap();
        path
    }

    fn json_artifact(name: &str, v: serde_json::Value) -> BundleArtifact {
        BundleArtifact {
            name: name.to_string(),
            data: serde_json::to_vec(&v).unwrap(),
        }
    }

    fn junit_run(status: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": "testlens.run/v1",
            "version": "0.6.1",
            "producer": {"name": "testlens", "version": "0.6.1"},
            "project": "p",
            "run_id": "r",
            "collected_at": "2026-01-01T00:00:00Z",
            "complete": true,
            "cases": [{
                "identity": "C.t1",
                "name": "t1",
                "classname": "C",
                "status": status,
                "duration_sec": 0.0
            }],
            "summary": {"total": 1, "passed": 1, "failed": 0,
                        "errors": 0, "skipped": 0, "duration_sec": 0.0}
        })
    }

    #[test]
    fn bundle_diff_reports_only_in_and_section_findings() {
        let dir = tempfile::tempdir().unwrap();
        // Baseline: a passing test run + a log summary + an extra entry.
        let a = bundle_with(
            dir.path(),
            "a.lens",
            vec![
                json_artifact("reports/test_run.json", junit_run("passed")),
                json_artifact(
                    "reports/log_summary.json",
                    serde_json::json!({"path": "x.log", "total_lines": 10, "lossy_lines": 0}),
                ),
                json_artifact("extra/base_only.txt", serde_json::json!("x")),
            ],
        );
        // Candidate: the same test now fails and the extra entry is gone.
        let b = bundle_with(
            dir.path(),
            "b.lens",
            vec![
                json_artifact("reports/test_run.json", junit_run("failed")),
                json_artifact(
                    "reports/log_summary.json",
                    serde_json::json!({"path": "x.log", "total_lines": 25, "lossy_lines": 0}),
                ),
            ],
        );

        let report = diff(&a, &b).unwrap();
        assert_eq!(report.data.sections.len(), 2, "{:?}", report.data.sections);
        assert!(report.data.sections.contains_key("test"));
        assert!(report.data.sections.contains_key("log"));
        assert_eq!(report.data.only_in_baseline, vec!["extra/base_only.txt"]);
        assert!(report.data.only_in_candidate.is_empty());
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        // passed -> failed on the same case is a regression via the
        // shared test classifier.
        assert!(codes.contains(&"test.regression"), "{codes:?}");
        assert!(codes.contains(&"bundle.only_in_baseline"), "{codes:?}");
        assert!(codes.contains(&"bundle.log_delta"), "{codes:?}");
    }

    /// Repack a bundle with a name mutation applied to each entry —
    /// lets tests build `./`-prefixed archives `create_bundle_archive`
    /// refuses to write.
    fn repack_names(path: &std::path::Path, map: impl Fn(&str) -> String) {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let raw = std::fs::read(path).unwrap();
        let mut archive = tar::Archive::new(GzDecoder::new(&raw[..]));
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for e in archive.entries().unwrap().flatten() {
            let mut e = e;
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut data).unwrap();
            entries.push((e.path().unwrap().to_string_lossy().into_owned(), data));
        }
        let out = std::fs::File::create(path).unwrap();
        let mut builder = tar::Builder::new(GzEncoder::new(out, Compression::default()));
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, map(&name), data.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn bundle_diff_normalizes_dot_slash_entry_names() {
        let dir = tempfile::tempdir().unwrap();
        // Baseline packed the "legacy" way: every entry ./-prefixed.
        let a = bundle_with(
            dir.path(),
            "a.lens",
            vec![json_artifact("reports/test_run.json", junit_run("passed"))],
        );
        repack_names(&a, |n| format!("./{n}"));
        let b = bundle_with(
            dir.path(),
            "b.lens",
            vec![json_artifact("reports/test_run.json", junit_run("failed"))],
        );

        let report = diff(&a, &b).unwrap();
        // Sections must be matched under normalized names — no
        // only_in_* noise purely from the `./` packing difference.
        assert!(
            report.data.sections.contains_key("test"),
            "{:?}",
            report.data.sections
        );
        assert!(
            report.data.only_in_baseline.is_empty(),
            "{:?}",
            report.data.only_in_baseline
        );
        assert!(report.data.only_in_candidate.is_empty());
        let codes: Vec<&str> = report.findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"test.regression"), "{codes:?}");
    }

    #[test]
    fn bundle_diff_rejects_tampered_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let a = bundle_with(
            dir.path(),
            "a.lens",
            vec![json_artifact("x.txt", serde_json::json!(1))],
        );
        let b = dir.path().join("b.lens");
        std::fs::write(&b, b"not a bundle").unwrap();
        assert!(diff(&a, &b).is_err());
    }

    #[test]
    fn bundle_diff_bad_section_is_incomplete_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |dir: &std::path::Path, name: &str| {
            bundle_with(
                dir,
                name,
                vec![BundleArtifact {
                    name: "reports/test_run.json".to_string(),
                    data: b"not json".to_vec(),
                }],
            )
        };
        let a = mk(dir.path(), "a.lens");
        let b = mk(dir.path(), "b.lens");
        let report = diff(&a, &b).unwrap();
        assert!(!report.complete);
        assert!(report.diagnostics.iter().any(|d| d.contains("test")));
    }

    #[test]
    fn review3_unknown_section_fields_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let run = serde_json::to_string(&junit_run("passed")).unwrap();
        // Typed deserialization skips unknown structures without building a
        // Value tree (which also imposes its recursion limit on unknown data).
        let payload = format!(
            "{{\"future_extension\":{}0{},{}",
            "[".repeat(150),
            "]".repeat(150),
            &run[1..]
        );
        let path = bundle_with(
            dir.path(),
            "unknown.lens",
            vec![BundleArtifact {
                name: "reports/test_run.json".into(),
                data: payload.into_bytes(),
            }],
        );
        let report = diff(&path, &path).unwrap();
        assert!(report.complete, "{:?}", report.diagnostics);
        assert!(report.data.sections.contains_key("test"));
        let mut foreign = junit_run("passed");
        foreign["schema"] = serde_json::json!("foreign/v99");
        let path = bundle_with(
            dir.path(),
            "foreign.lens",
            vec![json_artifact("reports/test_run.json", foreign)],
        );
        let report = diff(&path, &path).unwrap();
        assert!(!report.complete);
        assert!(report
            .diagnostics
            .iter()
            .any(|d| d.contains("expected schema")));
    }
}
