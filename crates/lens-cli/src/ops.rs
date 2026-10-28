use crate::cli::*;
use crate::doctor::HealthStatus;
use crate::output::{diagln, outln};
use lens_core::{to_deterministic_pretty, Result};
use lens_disk::{SnapshotV2, TrashManager};
use lens_log::parse_line;
use lens_ops::detect::InputKind;
use lens_ops::{abi, build, bundle, disk, doctor, env, log, net, sys, test, trace};
use lens_ops::{Finding, Report, Severity};
use std::path::{Path, PathBuf};

/// Resolve `--format` + legacy `--json` into a boolean.
/// `default_json` marks commands whose historical default is JSON.
fn wants_json(format: Option<OutputFormat>, json_flag: bool, default_json: bool) -> bool {
    match format {
        // `jsonl` is a `log filter`-only mode; other commands fall back to json.
        Some(OutputFormat::Json) | Some(OutputFormat::Jsonl) | Some(OutputFormat::Envelope) => true,
        Some(OutputFormat::Text) => false,
        None => json_flag || default_json,
    }
}

fn render_report<T, D: serde::Serialize>(
    data: &D,
    report: &Report<T>,
    opts: &GlobalOpts,
) -> Result<String> {
    if opts.format != Some(OutputFormat::Envelope) {
        return to_deterministic_pretty(data);
    }
    let threshold = match opts.fail_on {
        FailOn::Warn => Severity::Warn,
        FailOn::Fail => Severity::Fail,
    };
    let argv: Vec<String> = std::env::args_os()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    to_deterministic_pretty(&serde_json::json!({
        "schema_id": "lens.result", "schema_version": 1,
        "producer": {"name": "lens", "version": env!("CARGO_PKG_VERSION")},
        "run_id": std::env::var("IDK_RUN_ID").ok(),
        "execution": {"argv": argv, "cwd": std::env::current_dir().ok(), "exit_code": if report.is_actionable(threshold) {1} else {0}},
        "source": {"kind": "command", "identity": argv},
        "complete": report.complete, "findings": report.findings,
        "diagnostics": report.diagnostics, "data": data,
    }))
}

/// Human-readable binary size (KiB/MiB/GiB) for text-mode summaries.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", n)
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Keep summaries readable without changing machine output or source data.
fn text_excerpt(value: &str) -> String {
    let mut chars = value.chars();
    let mut out: String = chars
        .by_ref()
        .take(400)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        out.push_str("… [use --format json for full text]");
    }
    out
}

fn preview<I: ExactSizeIterator>(items: I) -> std::iter::Take<I> {
    let omitted = items.len().saturating_sub(20);
    if omitted > 0 {
        outln!(
            "  {} more items omitted; use --format json for all details.",
            omitted
        );
    }
    items.take(20)
}

/// Text-mode findings trailer: printed after the domain output on
/// stdout. Fail findings first, then Warn; Info is never printed.
/// `json_mode` suppresses the block so JSON stdout stays a pure report.
fn print_findings(findings: &[Finding], json_mode: bool) {
    if json_mode {
        return;
    }
    let fails = findings
        .iter()
        .filter(|f| f.severity == Severity::Fail)
        .count();
    let warns = findings
        .iter()
        .filter(|f| f.severity == Severity::Warn)
        .count();
    if fails + warns == 0 {
        return;
    }
    outln!("Findings: {} fail, {} warn", fails, warns);
    // Cap the detail lines: a 500-item regression list shouldn't flood
    // the terminal — JSON mode carries the full set for machines.
    const MAX_FINDING_LINES: usize = 20;
    let mut shown = 0usize;
    for f in findings
        .iter()
        .filter(|f| f.severity == Severity::Fail)
        .chain(findings.iter().filter(|f| f.severity == Severity::Warn))
    {
        if shown >= MAX_FINDING_LINES {
            break;
        }
        let tag = if f.severity == Severity::Fail {
            "FAIL"
        } else {
            "WARN"
        };
        outln!("  [{}] {}: {}", tag, f.code, text_excerpt(&f.message));
        if let Some(evidence) = &f.evidence {
            if let Some(path) = &evidence.path {
                outln!("    source: {}", text_excerpt(path));
            }
            outln!(
                "    evidence: data{} (--format envelope)",
                text_excerpt(&evidence.data_pointer)
            );
        }
        shown += 1;
    }
    let remaining = fails + warns - shown;
    if remaining > 0 {
        outln!(
            "  … and {} more (use --format json for the full report)",
            remaining
        );
    }
}

/// Fold a `Report`'s judgement into the dispatch outcome: incomplete
/// evidence or any finding at/above `threshold` is a finding (exit 1).
fn apply<T>(outcome: &mut Outcome, report: &Report<T>, threshold: Severity) {
    if report.is_actionable(threshold) {
        *outcome = Outcome::Findings;
    }
}

/// Command result per the exit-code convention: `Clean` exits 0,
/// `Findings` exits 1, and a returned `Err` exits 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Findings,
}

fn usage(msg: impl Into<String>) -> lens_core::LensError {
    lens_core::LensError::Usage {
        message: msg.into(),
    }
}

/// The diffable/inspectable domain a detected input belongs to. Several
/// kinds can share one domain (e.g. SystemdDir, SystemdUnit and
/// SysSnapshot are all `Sys`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    Abi,
    Test,
    Trace,
    Build,
    Sys,
    Env,
    Net,
    Bundle,
    Log,
    Disk,
}

impl Domain {
    fn name(self) -> &'static str {
        match self {
            Domain::Abi => "abi",
            Domain::Test => "test",
            Domain::Trace => "trace",
            Domain::Build => "build",
            Domain::Sys => "sys",
            Domain::Env => "env",
            Domain::Net => "net",
            Domain::Bundle => "bundle",
            Domain::Log => "log",
            Domain::Disk => "disk",
        }
    }
}

fn domain_of(kind: InputKind) -> Domain {
    match kind {
        InputKind::Elf => Domain::Abi,
        InputKind::Junit => Domain::Test,
        InputKind::Strace => Domain::Trace,
        InputKind::CompileDb => Domain::Build,
        InputKind::Venv | InputKind::EnvSnapshot => Domain::Env,
        InputKind::SystemdDir | InputKind::SystemdUnit | InputKind::SysSnapshot => Domain::Sys,
        InputKind::NetReport => Domain::Net,
        InputKind::Bundle => Domain::Bundle,
        InputKind::Log => Domain::Log,
        InputKind::Directory => Domain::Disk,
    }
}

fn domain_of_as(as_kind: AsKind) -> Domain {
    match as_kind {
        AsKind::Abi => Domain::Abi,
        AsKind::Test => Domain::Test,
        AsKind::Trace => Domain::Trace,
        AsKind::Build => Domain::Build,
        AsKind::Sys => Domain::Sys,
        AsKind::Env => Domain::Env,
        AsKind::Net => Domain::Net,
        AsKind::Log => Domain::Log,
        AsKind::Disk => Domain::Disk,
        AsKind::Bundle => Domain::Bundle,
    }
}

fn kind_name(kind: InputKind) -> &'static str {
    match kind {
        InputKind::Elf => "elf",
        InputKind::Junit => "junit",
        InputKind::Strace => "strace",
        InputKind::CompileDb => "compile_commands",
        InputKind::Venv => "venv",
        InputKind::SystemdDir => "systemd dir",
        InputKind::SystemdUnit => "systemd unit",
        InputKind::SysSnapshot => "sys snapshot",
        InputKind::EnvSnapshot => "env snapshot",
        InputKind::NetReport => "net report",
        InputKind::Bundle => "bundle",
        InputKind::Log => "log",
        InputKind::Directory => "directory",
    }
}

fn inspect_command(path: &Path, domain: Domain) -> Result<Commands> {
    let p = path.to_path_buf();
    Ok(match domain {
        Domain::Abi => Commands::Abi {
            action: AbiCommands::Inspect { binary: p },
        },
        Domain::Test => Commands::Test {
            action: TestCommands::Parse {
                file: p,
                project: "default".to_string(),
            },
        },
        Domain::Trace => Commands::Trace {
            action: TraceCommands::Analyze { trace_file: p },
        },
        Domain::Build => Commands::Build {
            action: BuildCommands::Inspect { file: p },
        },
        Domain::Sys => Commands::Sys {
            action: SysCommands::Inspect {
                path: Some(p),
                user: false,
            },
        },
        Domain::Env => Commands::Env {
            action: EnvCommands::Inspect {
                venv_path: p,
                project: None,
            },
        },
        Domain::Bundle => Commands::Bundle {
            action: BundleCommands::Inspect { bundle: p },
        },
        Domain::Log => Commands::Log {
            action: LogCommands::Inspect { paths: vec![p] },
        },
        Domain::Disk => Commands::Disk {
            action: DiskCommands::Scan {
                path: p,
                parallel: false,
                max_depth: None,
                exclude: Vec::new(),
                one_file_system: false,
                top: 0,
            },
        },
        Domain::Net => {
            // `net inspect` reads live procfs; a detected/--as'd net
            // input is a snapshot file, which is a diff input.
            return Err(usage("already a snapshot; use `lens diff`"));
        }
    })
}

fn diff_command(domain: Domain, baseline: &Path, candidate: &Path) -> Result<Commands> {
    let a = baseline.to_path_buf();
    let b = candidate.to_path_buf();
    Ok(match domain {
        Domain::Abi => Commands::Abi {
            action: AbiCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Test => Commands::Test {
            action: TestCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Trace => Commands::Trace {
            action: TraceCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Build => Commands::Build {
            action: BuildCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Sys => Commands::Sys {
            action: SysCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Env => Commands::Env {
            action: EnvCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Net => Commands::Net {
            action: NetCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Bundle => Commands::Bundle {
            action: BundleCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Log => Commands::Log {
            action: LogCommands::Diff {
                baseline: a,
                candidate: b,
            },
        },
        Domain::Disk => {
            return Err(usage(format!(
                "cannot diff {} inputs; pass --as",
                domain.name()
            )));
        }
    })
}

/// `lens inspect PATH [--as KIND]`: detect (or honor --as), announce
/// the verdict, and return the equivalent domain command.
fn resolve_inspect(path: &Path, as_kind: Option<AsKind>) -> Result<Commands> {
    match as_kind {
        Some(as_kind) => inspect_command(path, domain_of_as(as_kind)),
        None => {
            let kind = lens_ops::detect::detect(path)?;
            match kind {
                InputKind::SysSnapshot | InputKind::EnvSnapshot | InputKind::NetReport => {
                    Err(usage("already a snapshot; use `lens diff`"))
                }
                _ => {
                    let domain = domain_of(kind);
                    diagln!("detected: {} ({})", domain.name(), kind_name(kind));
                    inspect_command(path, domain)
                }
            }
        }
    }
}

/// Default snapshot filename: `snapshot-<hostname>-<UTC>.lens` with the
/// name sanitized to `[A-Za-z0-9._-]` so the path is always shell-safe.
fn default_snapshot_name(procfs: Option<&Path>) -> PathBuf {
    let proc_root = procfs.unwrap_or_else(|| Path::new("/proc"));
    let raw_host = std::fs::read_to_string(proc_root.join("sys/kernel/hostname"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "host".to_string());
    let host: String = raw_host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    // `YYYYMMDDTHHMMSSZ` — cut the RFC 3339 stamp at the fractional
    // second, then drop the punctuation.
    let iso = lens_core::time::utc_now_iso();
    let seconds = iso.split('.').next().unwrap_or(&iso);
    let stamp: String = seconds
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == 'T' || *c == 'Z')
        .collect();
    PathBuf::from(format!("snapshot-{host}-{stamp}.lens"))
}

/// One-line text summary of a bundle-diff section, for the `lens
/// bundle diff` text renderer.
fn bundle_section_summary(name: &str, v: &serde_json::Value) -> String {
    let n = |key: &str| v[key].as_array().map(|a| a.len()).unwrap_or(0);
    let i = |key: &str| v[key].as_i64().unwrap_or(0);
    match name {
        "sys" => format!(
            "+{} -{} ~{} unit(s), {} new cycle(s)",
            n("added_units"),
            n("removed_units"),
            n("modified_units"),
            n("new_cycles")
        ),
        "test" => format!(
            "{} regression(s), {} new failure(s), {} removed",
            n("regressions"),
            n("new_failures"),
            n("removed_tests")
        ),
        "trace" => format!(
            "{} new error(s), {} latency shift(s)",
            n("new_errors"),
            n("significant_latency_shifts")
        ),
        "env" => format!(
            "+{} -{} ~{} package(s), {} new shadowing",
            n("added_packages"),
            n("removed_packages"),
            n("version_changes"),
            n("new_shadowing")
        ),
        "net" => format!(
            "+{} -{} listener(s), +{} -{} connection(s)",
            n("new_listeners"),
            n("closed_listeners"),
            n("new_connections"),
            n("closed_connections")
        ),
        "doctor" => format!("{} regressed, {} improved", n("regressed"), n("improved")),
        "disk" => format!(
            "root size {:+} B, nodes {:+}",
            i("root_size_delta"),
            i("node_count_delta")
        ),
        "log" => format!("{:+} line(s)", i("total_lines_delta")),
        _ => "compared".to_string(),
    }
}

/// `lens diff A B [--as KIND]`: both inputs must land in the same
/// diffable domain.
fn resolve_diff(baseline: &Path, candidate: &Path, as_kind: Option<AsKind>) -> Result<Commands> {
    match as_kind {
        Some(as_kind) => diff_command(domain_of_as(as_kind), baseline, candidate),
        None => {
            let kind_a = lens_ops::detect::detect(baseline)?;
            let kind_b = lens_ops::detect::detect(candidate)?;
            let domain_a = domain_of(kind_a);
            let domain_b = domain_of(kind_b);
            if domain_a != domain_b {
                return Err(usage(format!(
                    "cannot diff {} with {}; pass --as",
                    kind_name(kind_a),
                    kind_name(kind_b)
                )));
            }
            diagln!(
                "detected: {} ({} + {})",
                domain_a.name(),
                kind_name(kind_a),
                kind_name(kind_b)
            );
            diff_command(domain_a, baseline, candidate)
        }
    }
}

pub fn dispatch(command: Commands, opts: &GlobalOpts) -> Result<Outcome> {
    if opts.format == Some(OutputFormat::Envelope)
        && matches!(
            &command,
            Commands::Tui { .. }
                | Commands::Completion { .. }
                | Commands::Disk {
                    action: DiskCommands::Trash { .. }
                }
                | Commands::Log {
                    action: LogCommands::Filter { follow: true, .. }
                }
        )
    {
        return Err(usage(
            "--format envelope requires a finite report command; use text/jsonl for --follow",
        ));
    }
    let mut outcome = Outcome::Clean;
    // Global options: every arm reads the same --format/--json/--fail-on.
    let format = opts.format;
    let json = opts.json;
    let threshold = match opts.fail_on {
        FailOn::Warn => Severity::Warn,
        FailOn::Fail => Severity::Fail,
    };
    match command {
        Commands::Config { .. } | Commands::Run { .. } => {
            unreachable!("configuration commands are resolved before dispatch")
        }
        Commands::Collect {
            source,
            target,
            output,
            user,
            since,
            lines,
            timeout,
            force,
            redact,
        } => {
            let adapter = match source {
                CollectSource::Journal => lens_ops::collect::Adapter::Journal,
                CollectSource::Docker => lens_ops::collect::Adapter::Docker,
                CollectSource::Podman => lens_ops::collect::Adapter::Podman,
            };
            let report = lens_ops::collect::collect(&lens_ops::collect::CollectRequest {
                adapter,
                target,
                user,
                since,
                lines,
                timeout: std::time::Duration::from_secs(timeout),
                output,
                force,
                redact,
            })?;
            if wants_json(format, json, true) {
                outln!("{}", render_report(&report.data, &report, opts)?);
            } else {
                outln!(
                    "Captured {} stdout bytes and {} stderr bytes into {}",
                    report.data.stdout_bytes,
                    report.data.stderr_bytes,
                    report.data.output.display()
                );
            }
            apply(&mut outcome, &report, threshold);
            print_findings(&report.findings, wants_json(format, json, true));
        }
        // `inspect`/`diff` resolve detection into the equivalent domain
        // command and re-enter dispatch, so output and exit codes are
        // identical to spelling the command out.
        Commands::Inspect { path, as_kind } => {
            let cmd = resolve_inspect(&path, as_kind)?;
            return dispatch(cmd, opts);
        }
        Commands::Diff {
            baseline,
            candidate,
            as_kind,
        } => {
            let cmd = resolve_diff(&baseline, &candidate, as_kind)?;
            return dispatch(cmd, opts);
        }
        Commands::Disk { action } => match action {
            DiskCommands::Scan {
                path,
                parallel,
                max_depth,
                exclude,
                one_file_system,
                top,
            } => {
                let json = wants_json(format, json, false);
                let mut scan_opts = lens_disk::ScanOptions {
                    parallel,
                    one_file_system,
                    exclude_patterns: exclude,
                    ..Default::default()
                };
                if let Some(d) = max_depth {
                    scan_opts.max_depth = d;
                    // A depth limit the user asked for is intent, not an
                    // evidence gap — don't fail the exit code over it.
                    scan_opts.depth_limit_is_error = false;
                }
                let report = disk::scan(&path, scan_opts)?;
                let result = &report.data;
                // Scan errors are surfaced even in --json mode so evidence
                // gaps never go unnoticed.
                for e in &report.diagnostics {
                    diagln!("scan warning: {e}");
                }
                if json {
                    let snapshot = SnapshotV2::from_tree(
                        &result.tree,
                        result.root_id,
                        result.complete,
                        result.truncated,
                    );
                    outln!("{}", render_report(&snapshot, &report, opts)?);
                } else {
                    let root_node = &result.tree.nodes[result.root_id as usize];
                    // Unreadable entries or truncation mean the totals
                    // undercount reality — say so instead of claiming success.
                    if result.complete && !result.truncated {
                        outln!("Scan completed successfully for: {:?}", path);
                    } else {
                        outln!(
                            "Scan INCOMPLETE for: {:?} ({} scan error(s), truncated={})",
                            path,
                            result.errors.len(),
                            result.truncated
                        );
                    }
                    outln!("Total entries: {}", result.scanned_entries);
                    outln!(
                        "Total logical size: {} bytes ({})",
                        root_node.size,
                        human_bytes(root_node.size)
                    );
                    outln!(
                        "Total allocated size: {} bytes ({})",
                        root_node.allocated_size,
                        human_bytes(root_node.allocated_size)
                    );
                    if result.depth_limited > 0 && max_depth.is_some() {
                        outln!(
                            "Depth limit {}: {} director{} not descended",
                            max_depth.unwrap_or_default(),
                            result.depth_limited,
                            if result.depth_limited == 1 {
                                "y"
                            } else {
                                "ies"
                            }
                        );
                    }
                    if top > 0 {
                        let mut ranked: Vec<_> = result
                            .tree
                            .children_ids(result.root_id)
                            .into_iter()
                            .filter_map(|id| result.tree.get(id))
                            .collect();
                        ranked.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
                        if !ranked.is_empty() {
                            outln!("Largest top-level entries:");
                            for node in ranked.iter().take(top) {
                                outln!("    {:>10}  {}", human_bytes(node.size), node.name);
                            }
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            DiskCommands::Duplicates {
                path,
                min_size,
                exclude,
                one_file_system,
                max_depth,
            } => {
                let mut scan_opts = lens_disk::ScanOptions {
                    one_file_system,
                    exclude_patterns: exclude,
                    ..Default::default()
                };
                if let Some(d) = max_depth {
                    scan_opts.max_depth = d;
                    scan_opts.depth_limit_is_error = false;
                }
                let report = disk::duplicates(&path, min_size, scan_opts)?;
                let dup = &report.data;
                for e in &report.diagnostics {
                    diagln!("scan warning: {e}");
                }
                if wants_json(format, json, false) {
                    outln!("{}", render_report(dup, &report, opts)?);
                } else {
                    for e in &dup.errors {
                        diagln!("hash warning: {e}");
                    }
                    outln!("Found {} duplicate group(s):", dup.groups.len());
                    for (i, group) in preview(dup.groups.iter()).enumerate() {
                        outln!("\n[{}] Hash: {}", i + 1, group.sha256);
                        outln!(
                            "    File size: {}, Reclaimable: {}",
                            human_bytes(group.size),
                            human_bytes(group.reclaimable_bytes)
                        );
                        for file in preview(group.files.iter()) {
                            outln!("    - {}", file.display());
                        }
                    }
                    outln!(
                        "Total reclaimable: {} ({} bytes)",
                        human_bytes(dup.total_reclaimable_bytes),
                        dup.total_reclaimable_bytes
                    );
                    if !dup.errors.is_empty() {
                        outln!("Hash errors: {}", dup.errors.len());
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            DiskCommands::Trash {
                action,
                paths,
                dry_run,
            } => {
                let default_trash = TrashManager::default();
                let trash_of = |dir: &Option<PathBuf>| match dir {
                    Some(d) => TrashManager::new(d.clone()),
                    None => TrashManager::default(),
                };
                match action {
                    Some(TrashCommands::List { trash_dir }) => {
                        let trash = trash_of(&trash_dir);
                        let entries = trash.list()?;
                        if wants_json(format, json, false) {
                            outln!("{}", to_deterministic_pretty(&entries)?);
                        } else if entries.is_empty() {
                            outln!("Trash is empty.");
                        } else {
                            outln!("{} item(s) in {:?}:", entries.len(), trash.files_dir());
                            for e in &entries {
                                outln!(
                                    "  {:<24} {} {}{}",
                                    e.name,
                                    e.deletion_date,
                                    e.original_path.display(),
                                    if e.present { "" } else { "  (file missing)" }
                                );
                            }
                        }
                    }
                    Some(TrashCommands::Restore { name, trash_dir }) => {
                        let receipt = trash_of(&trash_dir).restore_by_name(&name)?;
                        outln!("Restored: {:?}", receipt.original_path);
                    }
                    None => {
                        if paths.is_empty() {
                            return Err(lens_core::LensError::Usage {
                                message: "no paths given; use `trash list`/`trash restore` \
                                          to manage existing entries"
                                    .to_string(),
                            });
                        }
                        let json_mode = wants_json(format, json, false);
                        let mut failures: Vec<String> = Vec::new();
                        let mut receipts: Vec<lens_disk::TrashReceipt> = Vec::new();
                        for path in &paths {
                            if dry_run {
                                let msg = if path.symlink_metadata().is_ok() {
                                    format!(
                                        "would trash {:?} -> {:?}",
                                        path,
                                        default_trash.files_dir()
                                    )
                                } else {
                                    format!("cannot trash {:?}: does not exist", path)
                                };
                                outln!("{}", msg);
                                continue;
                            }
                            match default_trash.move_to_trash(path) {
                                Ok(receipt) => {
                                    if !json_mode {
                                        outln!("Successfully trashed: {:?}", receipt.original_path);
                                        outln!("Trash location: {:?}", receipt.trashed_file_path);
                                        outln!("Info receipt: {:?}", receipt.info_path);
                                    }
                                    receipts.push(receipt);
                                }
                                Err(e) => {
                                    // One bad path must not block the rest —
                                    // collect the failures and fail at the end.
                                    failures.push(format!("{:?}: {}", path, e));
                                    eprintln!("trash error: {:?}: {}", path, e);
                                }
                            }
                        }
                        if dry_run {
                            outln!("(dry-run: nothing was moved)");
                        } else if json_mode {
                            outln!("{}", to_deterministic_pretty(&receipts)?);
                        }
                        if !failures.is_empty() {
                            return Err(lens_core::LensError::Usage {
                                message: format!(
                                    "{} of {} path(s) could not be trashed",
                                    failures.len(),
                                    paths.len()
                                ),
                            });
                        }
                    }
                }
            }
        },
        Commands::Abi { action } => match action {
            AbiCommands::Inspect { binary } => {
                let report = abi::inspect(&binary, true)?;
                let elf = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(elf, &report, opts)?);
                } else {
                    // Human summary: the JSON report can be thousands of
                    // lines; text mode is the at-a-glance answer.
                    let weak = elf.evidence.iter().filter(|s| s.binding == "weak").count();
                    outln!("ABI summary: {}", binary.display());
                    outln!(
                        "  SONAME:   {}",
                        elf.dependencies.soname.as_deref().unwrap_or("-")
                    );
                    outln!("  NEEDED:   {}", elf.dependencies.needed.join(", "));
                    outln!(
                        "  Exports:  {} defined ({} weak) | Imports: {}",
                        elf.abi.symbols.len(),
                        weak,
                        elf.abi.imports.len()
                    );
                    outln!("  Version requirements: {}", elf.abi.versions.len());
                    if !elf.diagnostics.is_empty() {
                        outln!("  Diagnostics: {}", elf.diagnostics.len());
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            AbiCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = abi::diff(&baseline, &candidate, true)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "ABI diff: {}",
                        format!("{:?}", diff.compatibility).to_lowercase()
                    );
                    outln!(
                        "  Exports: +{} -{} | Imports: +{} -{} | Deps: +{} -{}",
                        diff.symbols.added.len(),
                        diff.symbols.removed.len(),
                        diff.imports.added.len(),
                        diff.imports.removed.len(),
                        diff.dependencies.needed.added.len(),
                        diff.dependencies.needed.removed.len()
                    );
                    for s in diff.symbols.removed.iter().take(10) {
                        outln!("  - export removed: {}", s);
                    }
                    for d in preview(diff.diagnostics.iter()) {
                        outln!("  ! {}", text_excerpt(d));
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Log { action } => match action {
            LogCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = log::diff(&baseline, &candidate)?;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(&report.data, &report, opts)?);
                } else {
                    outln!("Log diff: {:+} line(s)", report.data.total_lines_delta);
                    for note in preview(report.diagnostics.iter()) {
                        outln!("  {}", text_excerpt(note));
                    }
                    for (level, delta) in &report.data.level_delta {
                        outln!("  {level}: {delta:+}");
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            LogCommands::Inspect { paths } => {
                let paths = log::expand_inputs(&paths)?;
                let mut indexers = Vec::with_capacity(paths.len());
                for path in &paths {
                    indexers.push(log::open(path)?);
                }
                if wants_json(format, json, false) {
                    let blocks: Vec<serde_json::Value> = paths
                        .iter()
                        .zip(&indexers)
                        .map(|(path, indexer)| {
                            serde_json::json!({
                                "path": path.to_string_lossy(),
                                "total_lines": indexer.len(),
                                "lossy_lines": indexer.lossy_lines(),
                            })
                        })
                        .collect();
                    let value = if blocks.len() == 1 {
                        blocks.into_iter().next().unwrap()
                    } else {
                        serde_json::Value::Array(blocks)
                    };
                    let report = Report::new(value);
                    outln!("{}", render_report(&report.data, &report, opts)?);
                } else {
                    for (i, (path, indexer)) in paths.iter().zip(&indexers).enumerate() {
                        if i > 0 {
                            outln!();
                        }
                        outln!("Log file: {:?}", path);
                        outln!("Total lines indexed: {}", indexer.len());
                        if indexer.lossy_lines() > 0 {
                            outln!(
                                "Lossy lines (invalid UTF-8, shown with U+FFFD): {}",
                                indexer.lossy_lines()
                            );
                        }
                    }
                }
            }
            LogCommands::Filter {
                paths,
                query,
                regex,
                min_level,
                include_unknown,
                since,
                until,
                year,
                limit,
                context,
                follow,
            } => {
                let paths = log::expand_inputs(&paths)?;
                let filter = log::build_filter(&log::FilterArgs {
                    query,
                    regex,
                    min_level,
                    include_unknown,
                    since,
                    until,
                    year,
                })?;

                let jsonl_mode = format == Some(OutputFormat::Jsonl);
                let json_mode = wants_json(format, json, false);
                let multi = paths.len() > 1;

                if follow {
                    // Polling only makes sense for a live regular file.
                    if multi {
                        return Err(usage("--follow accepts exactly one log file input"));
                    }
                    let p = &paths[0];
                    if p.as_os_str() == "-" {
                        return Err(usage("--follow cannot tail stdin (`-`)"));
                    }
                    if p.extension().is_some_and(|e| e == "gz") {
                        return Err(usage("--follow cannot tail a compressed (.gz) file"));
                    }
                    if json_mode && !jsonl_mode {
                        return Err(usage("--follow requires --format text or jsonl output"));
                    }
                    if context != 0 {
                        return Err(usage(
                            "--follow does not support --context (appended matches have no surrounding lines)",
                        ));
                    }
                }

                let mut budget = limit;
                let mut matched = 0usize;
                let mut total_lines = 0usize;
                let mut total_lossy = 0usize;
                let mut excluded_unknown = 0usize;
                let mut excluded_unknown_ts = 0usize;
                let mut stopped = false;
                let mut inputs_json: Vec<serde_json::Value> = Vec::new();
                let mut follow_indexer = None;

                for path in &paths {
                    let indexer = log::open(path)?;
                    total_lines += indexer.len();
                    total_lossy += indexer.lossy_lines();
                    let label = path.to_string_lossy();
                    // In --follow mode an unterminated tail line is not
                    // emitted by the initial pass — it is re-read once
                    // completed by a later append.
                    let scan_lines =
                        if follow && !indexer.is_empty() && !indexer.as_bytes().ends_with(b"\n") {
                            indexer.len() - 1
                        } else {
                            indexer.len()
                        };
                    let res = filter_log_lines(
                        &indexer, &label, multi, &filter, context, budget, jsonl_mode, json_mode,
                        scan_lines,
                    );
                    matched += res.matched;
                    excluded_unknown += res.excluded_unknown;
                    excluded_unknown_ts += res.excluded_unknown_ts;
                    stopped |= res.stopped_by_limit;
                    if let Some(b) = budget.as_mut() {
                        *b = b.saturating_sub(res.matched);
                    }
                    if json_mode && !jsonl_mode {
                        inputs_json.push(serde_json::json!({
                            "path": label,
                            "matched": res.matched,
                            "total": indexer.len(),
                            "excluded_unknown": res.excluded_unknown,
                            "excluded_unknown_ts": res.excluded_unknown_ts,
                            "lossy_lines": indexer.lossy_lines(),
                            "lines": res.lines,
                        }));
                    }
                    if follow {
                        follow_indexer = Some(indexer);
                    }
                }

                if json_mode && !jsonl_mode {
                    let value = if multi {
                        serde_json::json!({
                            "inputs": inputs_json,
                            "matched": matched,
                            "total": total_lines,
                        })
                    } else {
                        inputs_json.into_iter().next().unwrap()
                    };
                    let mut report = Report::new(value);
                    report.complete = total_lossy == 0;
                    report.diagnostics.push(format!("Scope: requested filter and match limit {:?}; excluded {} unknown-level and {} unknown-time lines; {} lossy lines", limit, excluded_unknown, excluded_unknown_ts, total_lossy));
                    outln!("{}", render_report(&report.data, &report, opts)?);
                    apply(&mut outcome, &report, threshold);
                }

                if follow && !stopped {
                    crate::output::flush();
                    let indexer = follow_indexer.unwrap();
                    // Resume exactly at the unterminated tail: the partial
                    // line's bytes belong to the next appended line, so
                    // the follower starts where it begins (not at EOF)
                    // and its line number is the next one.
                    let (follow_offset, mut line_no) =
                        if !indexer.is_empty() && !indexer.as_bytes().ends_with(b"\n") {
                            (
                                indexer.line_offset(indexer.len() - 1).unwrap_or(0) as u64,
                                indexer.len() - 1,
                            )
                        } else {
                            (indexer.as_bytes().len() as u64, indexer.len())
                        };
                    let mut follower = log::Follower::new(&paths[0], follow_offset)?;
                    'follow: loop {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        let poll = follower.poll()?;
                        if let Some(note) = &poll.note {
                            diagln!("{note}");
                            if note.contains("rotated") || note.contains("truncated") {
                                line_no = 0;
                            }
                        }
                        let mut emitted = false;
                        for line in poll.lines {
                            line_no += 1;
                            let is_match =
                                if filter.min_level.is_some() || filter.time_filter_active() {
                                    filter.matches(&parse_line(&line, line_no))
                                } else {
                                    filter.matches_line(&line, line_no)
                                };
                            if !is_match {
                                continue;
                            }
                            matched += 1;
                            emitted = true;
                            if jsonl_mode {
                                outln!(
                                    "{}",
                                    serde_json::json!({
                                        "line": line_no,
                                        "context": false,
                                        "text": line,
                                    })
                                );
                            } else {
                                outln!("[{}] {}", line_no, line);
                            }
                            if limit.is_some_and(|l| matched >= l) {
                                stopped = true;
                                break 'follow;
                            }
                        }
                        if emitted {
                            crate::output::flush();
                        }
                    }
                }

                // Diagnostics stay on stderr so json/jsonl stdout stays clean.
                diagln!("\nMatched {} of {} line(s).", matched, total_lines);
                if stopped {
                    diagln!("Stopped after {} match(es): --limit reached.", matched);
                }
                if total_lossy > 0 {
                    outcome = Outcome::Findings;
                    diagln!(
                        "{} line(s) contain invalid UTF-8 (shown lossy as U+FFFD).",
                        total_lossy
                    );
                }
                if excluded_unknown > 0 {
                    diagln!(
                        "Excluded {} line(s) whose log level could not be determined (--include-unknown to keep them).",
                        excluded_unknown
                    );
                }
                if excluded_unknown_ts > 0 {
                    diagln!(
                        "Excluded {} line(s) whose timestamp could not be determined (--include-unknown to keep them).",
                        excluded_unknown_ts
                    );
                }
            }
            LogCommands::Stats {
                paths,
                bucket,
                query,
                regex,
                min_level,
                include_unknown,
                since,
                until,
                year,
            } => {
                let paths = log::expand_inputs(&paths)?;
                let filter = log::build_filter(&log::FilterArgs {
                    query,
                    regex,
                    min_level,
                    include_unknown,
                    since,
                    until,
                    year,
                })?;
                let bucket_secs = bucket
                    .as_deref()
                    .map(|b| match b {
                        "1m" => Ok(60u64),
                        "5m" => Ok(300),
                        "15m" => Ok(900),
                        "1h" => Ok(3600),
                        "1d" => Ok(86_400),
                        _ => Err(usage(format!(
                            "invalid --bucket {b:?}; expected 1m|5m|15m|1h|1d"
                        ))),
                    })
                    .transpose()?;
                let report = log::stats(&paths, &filter, bucket_secs)?;
                let s = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(s, &report, opts)?);
                } else {
                    outln!(
                        "Log stats: {} input(s), {} line(s) scanned, {} matched",
                        s.inputs.len(),
                        s.total_lines,
                        s.matched_lines
                    );
                    if !s.by_level.is_empty() {
                        outln!("Levels:");
                        for (lvl, n) in &s.by_level {
                            outln!("  {:<8} {}", lvl, n);
                        }
                    }
                    if s.unknown_timestamp > 0 {
                        outln!("  {:<8} {}", "no-ts", s.unknown_timestamp);
                    }
                    if !s.buckets.is_empty() {
                        outln!("Histogram:");
                        let max = s.buckets.iter().map(|b| b.total).max().unwrap_or(1);
                        for b in preview(s.buckets.iter()) {
                            let width = ((b.total * 40).div_ceil(max)).max(1) as usize;
                            let bar = "#".repeat(width.min(40));
                            if b.warn_or_higher > 0 {
                                outln!(
                                    "{}  {:>6}  {} !{}",
                                    b.start,
                                    b.total,
                                    bar,
                                    b.warn_or_higher
                                );
                            } else {
                                outln!("{}  {:>6}  {}", b.start, b.total, bar);
                            }
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
        },
        Commands::Test { action } => match action {
            TestCommands::Parse { file, project } => {
                let report = test::parse(&file, &project)?;
                let run = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(run, &report, opts)?);
                } else {
                    let s = &run.summary;
                    outln!(
                        "Test run: {} (project {}) from {}",
                        run.run_id,
                        text_excerpt(&run.project),
                        file.display()
                    );
                    outln!(
                        "  {} total | {} passed | {} failed | {} errors | {} skipped | {:.2}s",
                        s.total,
                        s.passed,
                        s.failed,
                        s.errors,
                        s.skipped,
                        s.duration_sec
                    );
                    let failed: Vec<_> = run
                        .cases
                        .iter()
                        .filter(|case| {
                            matches!(
                                case.status,
                                lens_test::TestStatus::Failed | lens_test::TestStatus::Error
                            )
                        })
                        .collect();
                    for case in failed.iter().take(20) {
                        outln!(
                            "  {:?}: {} — {}",
                            case.status,
                            text_excerpt(&case.identity),
                            text_excerpt(
                                case.message
                                    .as_deref()
                                    .unwrap_or("No failure message supplied")
                            )
                        );
                    }
                    if failed.len() > 20 {
                        outln!(
                            "  {} more failing cases omitted; use --format json for all cases.",
                            failed.len() - 20
                        );
                    }
                    if !run.complete {
                        outln!("  INCOMPLETE: the report was truncated or malformed");
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            TestCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = test::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Test diff: {} regressions, {} fixes, {} new failures, {} removed, {} skip changes",
                        diff.regressions.len(),
                        diff.fixes.len(),
                        diff.new_failures.len(),
                        diff.removed_tests.len(),
                        diff.skipped_changes.len()
                    );
                    let failures = diff.regressions.len() + diff.new_failures.len();
                    if failures > 20 {
                        outln!(
                            "  {} more failing cases omitted; use --format json for all cases.",
                            failures - 20
                        );
                    }
                    for r in diff.regressions.iter().chain(&diff.new_failures).take(20) {
                        outln!(
                            "  - {}{}",
                            text_excerpt(&r.id),
                            r.message
                                .as_deref()
                                .map(|m| format!(": {}", text_excerpt(m)))
                                .unwrap_or_default()
                        );
                    }
                    for d in preview(diff.diagnostics.iter()) {
                        outln!("  ! {}", text_excerpt(d));
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Trace { action } => match action {
            TraceCommands::Analyze { trace_file } => {
                let report = trace::analyze(&trace_file)?;
                let snapshot = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(snapshot, &report, opts)?);
                } else {
                    outln!("Trace summary: {}", trace_file.display());
                    outln!(
                        "  {} events | {} calls | {} errors | {} processes",
                        snapshot.total_events,
                        snapshot.total_calls,
                        snapshot.total_errors,
                        snapshot.processes.len()
                    );
                    let mut top: Vec<_> = snapshot.syscalls.iter().collect();
                    top.sort_by_key(|e| std::cmp::Reverse(e.1.count));
                    for (name, st) in top.iter().take(5) {
                        outln!("    {}: {} calls, {} errors", name, st.count, st.errors);
                    }
                    if !snapshot.fd_leaks.is_empty() {
                        outln!(
                            "  fd leaks: {} (use --format json for details)",
                            snapshot.fd_leaks.len()
                        );
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            TraceCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = trace::diff_files(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Trace diff: calls {:+} ({} -> {}), errors {:+} ({} -> {})",
                        diff.call_delta,
                        diff.baseline_calls,
                        diff.candidate_calls,
                        diff.error_delta,
                        diff.baseline_errors,
                        diff.candidate_errors
                    );
                    for e in preview(diff.new_errors.iter()) {
                        outln!("  + new error: {}", e);
                    }
                    for e in preview(diff.resolved_errors.iter()) {
                        outln!("  - resolved: {}", e);
                    }
                    for s in preview(diff.significant_latency_shifts.iter()) {
                        outln!("  ~ {}", s);
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Sys { action } => match action {
            SysCommands::Inspect { path, user } => {
                let report = sys::inspect_manager(path.as_deref(), user)?;
                let snapshot = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(snapshot, &report, opts)?);
                } else {
                    outln!(
                        "Systemd units: {} | cycles: {}",
                        snapshot.units.len(),
                        snapshot.cycles.len()
                    );
                    for (name, u) in preview(snapshot.units.iter()) {
                        outln!(
                            "  {} {}",
                            text_excerpt(name),
                            text_excerpt(u.exec_start.as_deref().unwrap_or("-"))
                        );
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            SysCommands::Cycles { dir, user } => {
                let report = sys::cycles_manager(dir.as_deref(), user)?;
                let (cycles, scanned_units) = (&report.data.cycles, report.data.scanned_units);
                if wants_json(format, json, false) {
                    outln!("{}", render_report(cycles, &report, opts)?);
                } else if cycles.is_empty() {
                    outln!(
                        "No dependency cycles detected among {} units.",
                        scanned_units
                    );
                } else {
                    outln!("Detected {} cycle(s):", cycles.len());
                    for (i, c) in preview(cycles.iter()).enumerate() {
                        if c.edges.is_empty() {
                            outln!(
                                "  [{}] {}",
                                i + 1,
                                text_excerpt(
                                    &c.members
                                        .iter()
                                        .take(20)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(" -> ")
                                )
                            );
                        } else {
                            // Real directed path: each hop carries the
                            // directive and file:line that produced it.
                            let mut line = String::new();
                            for e in c.edges.iter().take(20) {
                                let origin = match (&e.path, e.line) {
                                    (Some(p), Some(l)) => format!(
                                        "{}:{}",
                                        Path::new(p)
                                            .file_name()
                                            .map(|f| f.to_string_lossy().into_owned())
                                            .unwrap_or_else(|| p.clone()),
                                        l
                                    ),
                                    (Some(p), None) => p.clone(),
                                    _ => e.declared_by.clone(),
                                };
                                line.push_str(&format!(
                                    "{} --{}({})--> ",
                                    e.before, e.directive, origin
                                ));
                            }
                            if c.edges.len() > 20 {
                                line.push_str("… [remaining edges omitted; use --format json]");
                            } else {
                                line.push_str(&c.edges.last().unwrap().after);
                            }
                            outln!("  [{}] {}", i + 1, text_excerpt(&line));
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, false));
            }
            SysCommands::Diff {
                baseline,
                candidate,
            } => {
                // Inputs may be saved snapshots or live unit directories.
                let report = sys::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Systemd diff: +{} -{} ~{} unit(s), {} new cycle(s), {} resolved",
                        diff.added_units.len(),
                        diff.removed_units.len(),
                        diff.modified_units.len(),
                        diff.new_cycles.len(),
                        diff.resolved_cycles.len()
                    );
                    for u in preview(diff.modified_units.iter()) {
                        outln!("  ~ {}", text_excerpt(&u.unit));
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Build { action } => match action {
            BuildCommands::Inspect { file } => {
                let report = build::inspect(&file)?;
                let snapshot = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(snapshot, &report, opts)?);
                } else {
                    outln!(
                        "Build database: {} translation units, {} headers tracked",
                        snapshot.total_units,
                        snapshot.reverse_impact.len()
                    );
                    for u in preview(snapshot.units.iter()) {
                        outln!(
                            "  {} [{}]",
                            text_excerpt(&u.file),
                            text_excerpt(&u.compiler)
                        );
                    }
                }
                for diagnostic in &report.diagnostics {
                    diagln!("{diagnostic}");
                }
                if !report.complete {
                    for finding in &report.findings {
                        diagln!("warning: {}", finding.message);
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            BuildCommands::Impact { file, header } => {
                let report = build::impact(&file, &header)?;
                let impact = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(impact, &report, opts)?);
                } else {
                    outln!(
                        "Potential impact of {}: {} translation unit(s) may require rebuild",
                        impact.target_header,
                        impact.total_impacted
                    );
                    for u in preview(impact.impacted_units.iter()) {
                        outln!("  - {}", u);
                    }
                }
                // Static scope and evidence gaps stay visible in both modes.
                // Incomplete evidence exits 1 regardless of severity threshold.
                for diagnostic in &report.diagnostics {
                    diagln!("{diagnostic}");
                }
                if let Some(hint) = &impact.hint {
                    diagln!("warning: {}", hint);
                }
                if impact.missing_sources > 0
                    || impact.unresolved_includes > 0
                    || impact.scan_truncated
                {
                    diagln!(
                        "warning: impact graph incomplete: {} missing source(s), {} unresolved include(s), truncated={}",
                        impact.missing_sources, impact.unresolved_includes, impact.scan_truncated
                    );
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            BuildCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = build::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Build diff: +{} -{} ~{} unit(s)",
                        diff.added_units.len(),
                        diff.removed_units.len(),
                        diff.modified_units.len()
                    );
                    for u in preview(diff.modified_units.iter()) {
                        outln!("  ~ {}", text_excerpt(&u.file));
                        if let Some(order) = &u.forced_include_order {
                            outln!(
                                "    forced includes: {} -> {}",
                                text_excerpt(
                                    &order
                                        .before
                                        .iter()
                                        .take(20)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                                text_excerpt(
                                    &order
                                        .after
                                        .iter()
                                        .take(20)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )
                            );
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Env { action } => match action {
            EnvCommands::Inspect { venv_path, project } => {
                let report = env::inspect(&venv_path, project.as_deref())?;
                let snapshot = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(snapshot, &report, opts)?);
                } else {
                    let v = &snapshot.venv;
                    outln!("Virtualenv: {}", venv_path.display());
                    outln!(
                        "  Python: {} | packages: {}",
                        if v.python_version.is_empty() {
                            "?"
                        } else {
                            v.python_version.as_str()
                        },
                        v.packages.len()
                    );
                    outln!("  Missing deps: {}", v.missing_dependencies.len());
                    if !v.shadowing_issues.is_empty() {
                        outln!("  Shadowing issues: {}", v.shadowing_issues.len());
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
            EnvCommands::Check { venv_path, extras } => {
                let report = env::check(&venv_path, &extras)?;
                let venv = &report.data;
                if wants_json(format, json, false) {
                    outln!(
                        "{}",
                        render_report(
                            &serde_json::json!({
                                "venv": venv_path.to_string_lossy(),
                                "missing_dependencies": venv.missing_dependencies,
                                "version_conflicts": venv.version_conflicts,
                                "unevaluated_dependencies": venv.unevaluated_dependencies,
                            }),
                            &report,
                            opts
                        )?
                    );
                } else {
                    if venv.missing_dependencies.is_empty() {
                        outln!("All dependencies satisfied in {:?}.", venv_path);
                    } else {
                        outln!(
                            "Found {} missing dependenc(ies):",
                            venv.missing_dependencies.len()
                        );
                        for d in preview(venv.missing_dependencies.iter()) {
                            outln!("  - {}", text_excerpt(d));
                        }
                    }
                    if !venv.version_conflicts.is_empty() {
                        outln!("Version conflicts:");
                        for d in preview(venv.version_conflicts.iter()) {
                            outln!("  - {}", text_excerpt(d));
                        }
                    }
                    if !venv.unevaluated_dependencies.is_empty() {
                        outln!("Unevaluated requirements (uncertain markers):");
                        for d in preview(venv.unevaluated_dependencies.iter()) {
                            outln!("  - {}", text_excerpt(d));
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, false));
            }
            EnvCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = env::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Environment diff: +{} -{} ~{} package(s)",
                        diff.added_packages.len(),
                        diff.removed_packages.len(),
                        diff.version_changes.len()
                    );
                    for p in preview(diff.version_changes.iter()) {
                        outln!("  ~ {}: {} -> {}", p.package, p.old_version, p.new_version);
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Snapshot {
            output,
            procfs,
            systemd_dir,
            log,
            disk,
            env,
            test,
            trace,
            force,
            redact,
        } => {
            // The log source defaults to the first conventional syslog
            // path — its absence is noted quietly, not counted as a
            // collection failure.
            let log = log.or_else(|| {
                const CANDIDATES: &[&str] = &[
                    "/var/log/syslog",
                    "/var/log/messages",
                    "/var/log/kern.log",
                    "/var/log/daemon.log",
                ];
                let found = CANDIDATES.iter().map(PathBuf::from).find(|p| p.is_file());
                if found.is_none() {
                    diagln!("no standard syslog found under /var/log; log entry omitted");
                }
                found
            });
            let output = output.unwrap_or_else(|| default_snapshot_name(procfs.as_deref()));
            let report = bundle::create(&bundle::BundleRequest {
                output: output.clone(),
                disk,
                log,
                trace,
                test,
                env,
                net: true,
                host: true,
                doctor: true,
                sys_merged: systemd_dir.is_none(),
                sys: systemd_dir.clone(),
                systemd_dir,
                procfs,
                force,
                redact,
            })?;
            let manifest = &report.data;
            if wants_json(format, json, false) {
                outln!("{}", render_report(manifest, &report, opts)?);
            } else {
                outln!("Snapshot bundle written: {:?}", output);
                outln!("  Artifacts embedded: {}", manifest.sources.len());
                if !report.diagnostics.is_empty() {
                    outln!("  Collection warnings recorded in manifest:");
                    for d in &report.diagnostics {
                        outln!("    - {}", d);
                    }
                }
                outln!(
                    "Compare later with: lens diff {:?} <newer snapshot>",
                    output
                );
            }
            apply(&mut outcome, &report, threshold);
        }
        Commands::Bundle { action } => match action {
            BundleCommands::Create {
                output,
                disk,
                log,
                trace,
                test,
                sys,
                env,
                net,
                doctor,
                force,
                redact,
            } => {
                let report = bundle::create(&bundle::BundleRequest {
                    output: output.clone(),
                    disk,
                    log,
                    trace,
                    test,
                    env,
                    net,
                    force,
                    redact,
                    doctor,
                    systemd_dir: sys.clone(),
                    sys,
                    ..Default::default()
                })?;
                let manifest = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(manifest, &report, opts)?);
                } else {
                    outln!(
                        "Successfully created forensic flight recorder bundle: {:?}",
                        output
                    );
                    outln!("  Artifacts embedded: {}", manifest.sources.len());
                    if !report.diagnostics.is_empty() {
                        outln!("  Collection warnings recorded in manifest:");
                        for d in &report.diagnostics {
                            outln!("    - {}", d);
                        }
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            BundleCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = bundle::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(diff, &report, opts)?);
                    apply(&mut outcome, &report, threshold);
                    return Ok(outcome);
                }
                outln!("Bundle diff: {:?} -> {:?}", baseline, candidate);
                for (name, section) in &diff.sections {
                    outln!("  {}: {}", name, bundle_section_summary(name, section));
                }
                for n in &diff.only_in_baseline {
                    outln!("  only in baseline: {}", n);
                }
                for n in &diff.only_in_candidate {
                    outln!("  only in candidate: {}", n);
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, false);
            }
            BundleCommands::Inspect { bundle } => {
                let report = bundle::inspect(&bundle)?;
                let inspect = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(inspect, &report, opts)?);
                    return Ok(outcome);
                }
                outln!("=== Lens Forensic Flight Recorder Bundle ===");
                outln!("Entries: {}", inspect.entries.len());
                match &inspect.manifest {
                    Some(m) => {
                        outln!("Schema:        {}", m.schema);
                        outln!("Tool:          {} v{}", m.tool, m.version);
                        outln!("Created At:    {}", m.created_at);
                        outln!("Manifest Files:");
                        for s in preview(m.sources.iter()) {
                            outln!("  - {} ({} bytes)", text_excerpt(&s.path), s.size);
                        }
                        if !m.diagnostics.is_empty() {
                            outln!("Capture Diagnostics:");
                            for d in preview(m.diagnostics.iter()) {
                                outln!("  ! {}", text_excerpt(d));
                            }
                        }
                    }
                    None => outln!("Manifest: NOT FOUND (untrusted bundle)"),
                }
                outln!("Archive Entries:");
                for e in preview(inspect.entries.iter()) {
                    outln!("  - {} ({} bytes)", text_excerpt(&e.name), e.size);
                }
            }
            BundleCommands::Verify { bundle } => {
                let report = bundle::verify(&bundle)?;
                let vr = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(vr, &report, opts)?);
                    apply(&mut outcome, &report, threshold);
                    return Ok(outcome);
                }
                if vr.is_valid {
                    outln!("Bundle verification SUCCESSFUL: {:?}", bundle);
                    outln!("  Total manifest files:    {}", vr.total_files);
                    outln!("  Verified SHA-256 files:  {}", vr.verified_files);
                    // The manifest has no signature: this reports integrity
                    // against the embedded manifest, not provenance.
                    outln!("  Integrity: all files match the embedded manifest");
                    for u in preview(vr.unexpected_files.iter()) {
                        outln!("  Unlisted entry (not in manifest): {}", u);
                    }
                } else {
                    eprintln!("Bundle integrity check FAILED: {:?}", bundle);
                    if let Some(err) = &vr.error {
                        eprintln!("  Error: {}", err);
                    }
                    for t in preview(vr.tampered_files.iter()) {
                        eprintln!("  Tampered: {}", t);
                    }
                    for m in preview(vr.missing_files.iter()) {
                        eprintln!("  Missing:  {}", m);
                    }
                    for u in preview(vr.unexpected_files.iter()) {
                        eprintln!("  Unlisted: {}", u);
                    }
                }
                apply(&mut outcome, &report, threshold);
            }
            BundleCommands::Show { bundle, entry } => {
                let report = bundle::show(&bundle, &entry)?;
                let bytes = &report.data;
                if wants_json(format, json, false) {
                    outln!(
                        "{}",
                        render_report(
                            &serde_json::json!({
                                "bundle": bundle.to_string_lossy(),
                                "entry": entry,
                                "size": bytes.len(),
                                "content": String::from_utf8_lossy(bytes),
                            }),
                            &report,
                            opts
                        )?
                    );
                } else {
                    // Raw entry content — typically already pretty JSON.
                    outln!("{}", String::from_utf8_lossy(bytes));
                }
            }
            BundleCommands::Extract {
                bundle,
                dest,
                force,
            } => {
                let report = bundle::extract(&bundle, &dest, force)?;
                let written = &report.data;
                if wants_json(format, json, false) {
                    outln!(
                        "{}",
                        render_report(
                            &serde_json::json!({
                                "bundle": bundle.to_string_lossy(),
                                "dest": dest.to_string_lossy(),
                                "extracted": written
                                    .iter()
                                    .map(|p| p.to_string_lossy())
                                    .collect::<Vec<_>>(),
                            }),
                            &report,
                            opts
                        )?
                    );
                } else {
                    outln!(
                        "Verified and extracted {} file(s) to {:?}",
                        written.len(),
                        dest
                    );
                    for w in written {
                        outln!("  - {}", w.display());
                    }
                }
            }
        },
        Commands::Net { action } => match action {
            NetCommands::Inspect { proc_dir, no_unix } => {
                let report = net::inspect(proc_dir.as_deref(), no_unix)?;
                let net_report = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", render_report(net_report, &report, opts)?);
                } else {
                    outln!("Network & Socket Inspection Summary:");
                    outln!(
                        "  Total Sockets:          {}",
                        net_report.summary.total_sockets
                    );
                    outln!(
                        "  Listening Ports:        {}",
                        net_report.summary.listening_ports
                    );
                    outln!(
                        "  Established Conns:      {}",
                        net_report.summary.established_connections
                    );
                    outln!(
                        "  TIME_WAIT Sockets:      {}",
                        net_report.summary.time_wait_sockets
                    );
                    outln!(
                        "  Orphan Sockets:         {}",
                        net_report.summary.orphan_sockets
                    );
                    outln!(
                        "  Owner-Unknown Sockets:  {}",
                        net_report.summary.owner_unknown_sockets
                    );
                    if net_report.summary.uninspectable_processes > 0 {
                        outln!(
                            "  Uninspectable Procs:    {} (re-run with sudo to attribute)",
                            net_report.summary.uninspectable_processes
                        );
                    }
                    outln!(
                        "  UNIX Domain Sockets:    {}",
                        net_report.summary.unix_domain_sockets
                    );
                    outln!("\nActive Listening Ports:");
                    outln!(
                        "{:<8} {:<24} {:<10} {:<8} {:<8} {:<16}",
                        "PROTO",
                        "LOCAL ADDRESS",
                        "INODE",
                        "UID",
                        "PID",
                        "PROCESS"
                    );
                    for l in preview(net_report.listening.iter()) {
                        let proc_str = l
                            .process
                            .as_ref()
                            .map(|p| p.name.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| match l.owner_state {
                                Some(lens_net::OwnerState::OwnerUnknown) => {
                                    "<owner unknown>".to_string()
                                }
                                _ => "<orphan>".to_string(),
                            });
                        let pid_str = l
                            .process
                            .as_ref()
                            .map(|p| p.pid.to_string())
                            .unwrap_or_else(|| "-".to_string());
                        let addr = format!("{}:{}", l.local_address, l.local_port);
                        outln!(
                            "{:<8} {:<24} {:<10} {:<8} {:<8} {}",
                            format!("{:?}", l.kind),
                            addr,
                            l.inode,
                            l.uid,
                            pid_str,
                            proc_str
                        );
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, false));
            }
            NetCommands::Diff {
                baseline,
                candidate,
            } => {
                let report = net::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, json, true) {
                    outln!("{}", render_report(diff, &report, opts)?);
                } else {
                    outln!(
                        "Network diff: +{} -{} listener(s), +{} -{} connection(s), TIME_WAIT {:+}",
                        diff.new_listeners.len(),
                        diff.closed_listeners.len(),
                        diff.new_connections.len(),
                        diff.closed_connections.len(),
                        diff.time_wait_delta
                    );
                    for l in preview(diff.new_listeners.iter()) {
                        outln!("  + {}:{}", l.local_address, l.local_port);
                    }
                    for l in preview(diff.closed_listeners.iter()) {
                        outln!("  - {}:{}", l.local_address, l.local_port);
                    }
                }
                apply(&mut outcome, &report, threshold);
                print_findings(&report.findings, wants_json(format, json, true));
            }
        },
        Commands::Doctor {
            root,
            procfs,
            systemd_dir,
        } => {
            let report = doctor::run(root.as_deref(), procfs.as_deref(), systemd_dir.as_deref())?;
            let dr = &report.data;
            if wants_json(format, json, false) {
                outln!("{}", render_report(dr, &report, opts)?);
            } else {
                outln!("=== Lens System Doctor Diagnosis ===");
                outln!("Overall Status: {:?}", dr.overall_status);
                outln!(
                    "Summary: {} Total | {} Passed | {} Warnings | {} Failures\n",
                    dr.summary.total,
                    dr.summary.passed,
                    dr.summary.warnings,
                    dr.summary.failures
                );
                outln!("{:<12} {:<30} {:<8} MESSAGE", "CATEGORY", "CHECK", "STATUS");
                for c in preview(dr.checks.iter()) {
                    let status_str = match c.status {
                        HealthStatus::Pass => "[PASS]",
                        HealthStatus::Warn => "[WARN]",
                        HealthStatus::Fail => "[FAIL]",
                        HealthStatus::Skip => "[SKIP]",
                    };
                    outln!(
                        "{:<12} {:<30} {:<8} {}",
                        c.category,
                        c.name,
                        status_str,
                        text_excerpt(&c.message)
                    );
                    if let Some(ref rec) = c.recommendation {
                        outln!("             -> Recommendation: {}", text_excerpt(rec));
                    }
                    if let Some(ref cmd) = c.next_command {
                        outln!("             -> Next: {}", text_excerpt(cmd));
                    }
                }
            }
            // --fail-on picks the severity threshold that counts as a
            // finding. Doctor renders its own check table — no Findings
            // trailer.
            apply(&mut outcome, &report, threshold);
        }
        Commands::Tui { path, log } => {
            lens_tui::run(&path, log.as_deref()).map_err(|e| lens_core::LensError::Io {
                path: path.clone(),
                source: e,
            })?;
        }
        Commands::Completion { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut cmd, "lens", &mut script);
            crate::output::bytes(&script);
        }
    }

    Ok(outcome)
}

/// Result of filtering one log input — counts plus the collected line
/// objects for `--format json` (empty under text/jsonl).
struct LogFileFilterOut {
    matched: usize,
    excluded_unknown: usize,
    excluded_unknown_ts: usize,
    lines: Vec<serde_json::Value>,
    /// The global --limit was exhausted while scanning this input.
    stopped_by_limit: bool,
}

/// Scan one indexed log input and emit matching/context lines.
/// `label` is only used when `show_file` (multi-input mode) prefixes
/// text lines as `[<file>:<line>]` and adds `"file"` to emitted objects;
/// `limit` is the remaining global match budget. Context bookkeeping is
/// local to this call — context never crosses input boundaries.
#[allow(clippy::too_many_arguments)]
fn filter_log_lines(
    indexer: &lens_log::LogIndexer,
    label: &str,
    show_file: bool,
    filter: &lens_log::LogFilter,
    context: usize,
    limit: Option<usize>,
    jsonl_mode: bool,
    json_mode: bool,
    scan_lines: usize,
) -> LogFileFilterOut {
    let mut matched = 0usize;
    let mut excluded_unknown = 0usize;
    let mut excluded_unknown_ts = 0usize;
    let mut matched_lines: Vec<serde_json::Value> = Vec::new();
    // Context bookkeeping: `pending_before` holds the last `context`
    // non-matching lines; `after_left` counts the trailing context still
    // owed after the latest match.
    let mut pending_before: std::collections::VecDeque<(usize, String)> =
        std::collections::VecDeque::with_capacity(context);
    let mut after_left = 0usize;
    let mut last_printed: Option<usize> = None;
    // A zero remaining budget (limit hit in an earlier input) means this
    // file contributes nothing at all — not even context.
    let mut limited = limit == Some(0);

    let emit = |idx: usize,
                text: &str,
                is_match: bool,
                matched_lines: &mut Vec<serde_json::Value>,
                last_printed: &mut Option<usize>,
                after_left: &mut usize| {
        if *last_printed == Some(idx) {
            return;
        }
        *last_printed = Some(idx);
        let obj = |v: &mut serde_json::Map<String, serde_json::Value>| {
            if show_file {
                v.insert("file".to_string(), serde_json::json!(label));
            }
        };
        if jsonl_mode {
            // One JSON object per emitted line; match/context
            // distinguished by the `context` flag.
            let mut v = serde_json::json!({
                "line": idx + 1,
                "context": !is_match,
                "text": text,
            });
            obj(v.as_object_mut().unwrap());
            outln!("{}", v);
        } else if json_mode {
            let mut v = serde_json::json!({
                "line": idx + 1,
                "context": !is_match,
                "text": text,
            });
            obj(v.as_object_mut().unwrap());
            matched_lines.push(v);
        } else if is_match {
            if show_file {
                outln!("[{}:{}] {}", label, idx + 1, text);
            } else {
                outln!("[{}] {}", idx + 1, text);
            }
        } else if show_file {
            outln!("[{}:{}]- {}", label, idx + 1, text);
        } else {
            outln!("[{}]- {}", idx + 1, text);
        }
        if !is_match {
            *after_left = after_left.saturating_sub(1);
        }
    };

    for idx in 0..scan_lines {
        // After --limit, only the owed trailing context is emitted.
        if limited && after_left == 0 {
            break;
        }
        // Lossy view: invalid-UTF-8 lines are still matched and shown
        // (with U+FFFD) rather than silently skipped. Kept as a Cow:
        // the borrowed fast path avoids one String allocation per
        // scanned line; only context lines buffered in `pending_before`
        // are owned.
        let line = indexer.get_line_lossy(idx).unwrap_or_default();
        let is_match = !limited && {
            if filter.min_level.is_some() || filter.time_filter_active() {
                // Level/timestamp filtering needs the parsed record; it
                // also lets us count evidence discarded as undeterminable.
                let record = parse_line(&line, idx + 1);
                if filter.rejected_only_by_unknown_level(&record) {
                    excluded_unknown += 1;
                    false
                } else if filter.rejected_only_by_unknown_timestamp(&record) {
                    excluded_unknown_ts += 1;
                    false
                } else {
                    filter.matches(&record)
                }
            } else {
                filter.matches_line(&line, idx + 1)
            }
        };

        if is_match {
            // Flush the buffered leading context.
            for (cidx, ctext) in pending_before.drain(..) {
                emit(
                    cidx,
                    &ctext,
                    false,
                    &mut matched_lines,
                    &mut last_printed,
                    &mut after_left,
                );
            }
            emit(
                idx,
                &line,
                true,
                &mut matched_lines,
                &mut last_printed,
                &mut after_left,
            );
            matched += 1;
            after_left = context;
            if limit.is_some_and(|l| matched >= l) {
                limited = true;
            }
        } else if after_left > 0 {
            emit(
                idx,
                &line,
                false,
                &mut matched_lines,
                &mut last_printed,
                &mut after_left,
            );
        } else if context > 0 {
            pending_before.push_back((idx, line.into_owned()));
            while pending_before.len() > context {
                pending_before.pop_front();
            }
        }
    }

    LogFileFilterOut {
        matched,
        excluded_unknown,
        excluded_unknown_ts,
        lines: matched_lines,
        stopped_by_limit: limited,
    }
}
