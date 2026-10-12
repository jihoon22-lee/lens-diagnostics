use std::path::PathBuf;

use lens_core::{
    create_bundle_archive, extract_bundle, inspect_bundle, read_bundle_entry,
    verify_bundle_archive, BundleArtifact, BundleInspection, BundleManifest,
    BundleVerificationReport, LensError, Result,
};
use lens_disk::{DiskScanner, ScanOptions, SnapshotV2};

use crate::{log, sys, test, trace, Report, Severity};

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

/// `bundle create` inputs — mirrors the CLI flags.
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
    pub force: bool,
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
        || req.env.is_some()
        || req.net;
    if !any_source {
        return Err(LensError::Usage {
            message: "at least one artifact source is required \
                      (--disk/--log/--trace/--test/--sys/--env/--net)"
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
            let res = DiskScanner::new(ScanOptions::default()).scan(d)?;
            // Per-entry scan failures are evidence gaps — record them in
            // the manifest diagnostics.
            diagnostics.extend(res.errors.iter().map(|e| format!("disk scan: {e}")));
            Ok(SnapshotV2::from_tree(
                &res.tree,
                res.root_id,
                res.complete,
                res.truncated,
            ))
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
        collect!("test", "reports/test_run.json", test::load_run(t, "bundle"));
    }

    if let Some(ref s) = req.sys {
        collect!("sys", "reports/sys_snapshot.json", {
            sys::snapshot(Some(s))
        });
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
            lens_net::inspect_network(None)
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
