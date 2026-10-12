use crate::cli::*;
use crate::doctor::HealthStatus;
use crate::output::outln;
use lens_core::{to_deterministic_pretty, Result};
use lens_disk::{SnapshotV2, TrashManager};
use lens_log::parse_line;
use lens_ops::{abi, build, bundle, disk, doctor, env, log, net, sys, test, trace};
use lens_ops::{Finding, Report, Severity};
use std::path::{Path, PathBuf};

/// Resolve `--format` + legacy `--json` into a boolean.
/// `default_json` marks commands whose historical default is JSON.
fn wants_json(format: Option<OutputFormat>, json_flag: bool, default_json: bool) -> bool {
    match format {
        // `jsonl` is a `log filter`-only mode; other commands fall back to json.
        Some(OutputFormat::Json) | Some(OutputFormat::Jsonl) => true,
        Some(OutputFormat::Text) => false,
        None => json_flag || default_json,
    }
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
        outln!("  [{}] {}: {}", tag, f.code, f.message);
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

pub fn dispatch(command: Commands) -> Result<Outcome> {
    let mut outcome = Outcome::Clean;
    match command {
        Commands::Disk { action } => match action {
            DiskCommands::Scan {
                path,
                json,
                format,
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
                }
                let report = disk::scan(&path, scan_opts)?;
                let result = &report.data;
                // Scan errors are surfaced even in --json mode so evidence
                // gaps never go unnoticed.
                for e in &report.diagnostics {
                    eprintln!("scan warning: {e}");
                }
                if json {
                    let snapshot = SnapshotV2::from_tree(
                        &result.tree,
                        result.root_id,
                        result.complete,
                        result.truncated,
                    );
                    outln!("{}", to_deterministic_pretty(&snapshot)?);
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
                apply(&mut outcome, &report, Severity::Fail);
            }
            DiskCommands::Duplicates {
                path,
                min_size,
                format,
            } => {
                let report = disk::duplicates(&path, min_size)?;
                let dup = &report.data;
                for e in &report.diagnostics {
                    eprintln!("scan warning: {e}");
                }
                if wants_json(format, false, false) {
                    outln!("{}", to_deterministic_pretty(dup)?);
                } else {
                    for e in &dup.errors {
                        eprintln!("hash warning: {e}");
                    }
                    outln!("Found {} duplicate group(s):", dup.groups.len());
                    for (i, group) in dup.groups.iter().enumerate() {
                        outln!("\n[{}] Hash: {}", i + 1, group.sha256);
                        outln!(
                            "    File size: {}, Reclaimable: {}",
                            human_bytes(group.size),
                            human_bytes(group.reclaimable_bytes)
                        );
                        for file in &group.files {
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
                apply(&mut outcome, &report, Severity::Fail);
            }
            DiskCommands::Trash {
                action,
                paths,
                dry_run,
                format,
            } => {
                let default_trash = TrashManager::default();
                let trash_of = |dir: &Option<PathBuf>| match dir {
                    Some(d) => TrashManager::new(d.clone()),
                    None => TrashManager::default(),
                };
                match action {
                    Some(TrashCommands::List { trash_dir, format }) => {
                        let trash = trash_of(&trash_dir);
                        let entries = trash.list()?;
                        if wants_json(format, false, false) {
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
                        let json_mode = wants_json(format, false, false);
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
            AbiCommands::Inspect { binary, format } => {
                let report = abi::inspect(&binary, true)?;
                let elf = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(elf)?);
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
                apply(&mut outcome, &report, Severity::Fail);
            }
            AbiCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = abi::diff(&baseline, &candidate, true)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
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
                    for d in &diff.diagnostics {
                        outln!("  ! {}", d);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Log { action } => match action {
            LogCommands::Inspect { path, format } => {
                let indexer = log::open(&path)?;
                if wants_json(format, false, false) {
                    outln!(
                        "{}",
                        to_deterministic_pretty(&serde_json::json!({
                            "path": path.to_string_lossy(),
                            "total_lines": indexer.len(),
                            "lossy_lines": indexer.lossy_lines(),
                        }))?
                    );
                } else {
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
            LogCommands::Filter {
                path,
                query,
                regex,
                min_level,
                include_unknown,
                since,
                until,
                year,
                limit,
                context,
                format,
            } => {
                let indexer = log::open(&path)?;
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
                let json_mode = wants_json(format, false, false);
                let mut matched = 0usize;
                let mut excluded_unknown = 0usize;
                let mut excluded_unknown_ts = 0usize;
                let mut matched_lines: Vec<serde_json::Value> = Vec::new();
                // Context bookkeeping: `pending_before` holds the last
                // `context` non-matching lines; `after_left` counts the
                // trailing context still owed after the latest match.
                let mut pending_before: std::collections::VecDeque<(usize, String)> =
                    std::collections::VecDeque::with_capacity(context);
                let mut after_left = 0usize;
                let mut last_printed: Option<usize> = None;
                let mut limited = false;

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
                    if jsonl_mode {
                        // One JSON object per emitted line; match/context
                        // distinguished by the `context` flag.
                        outln!(
                            "{}",
                            serde_json::json!({
                                "line": idx + 1,
                                "context": !is_match,
                                "text": text,
                            })
                        );
                    } else if json_mode {
                        matched_lines.push(serde_json::json!({
                            "line": idx + 1,
                            "context": !is_match,
                            "text": text,
                        }));
                    } else if is_match {
                        outln!("[{}] {}", idx + 1, text);
                    } else {
                        outln!("[{}]- {}", idx + 1, text);
                    }
                    if !is_match {
                        *after_left = after_left.saturating_sub(1);
                    }
                };

                for idx in 0..indexer.len() {
                    // After --limit, only the owed trailing context is emitted.
                    if limited && after_left == 0 {
                        break;
                    }
                    // Lossy view: invalid-UTF-8 lines are still matched and
                    // shown (with U+FFFD) rather than silently skipped.
                    // Kept as a Cow: the borrowed fast path avoids one
                    // String allocation per scanned line; only context
                    // lines buffered in `pending_before` are owned.
                    let line = indexer.get_line_lossy(idx).unwrap_or_default();
                    let is_match = !limited && {
                        if filter.min_level.is_some() || filter.time_filter_active() {
                            // Level/timestamp filtering needs the parsed
                            // record; it also lets us count evidence
                            // discarded as undeterminable.
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
                        if let Some(l) = limit {
                            if matched >= l {
                                limited = true;
                            }
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
                if json_mode && !jsonl_mode {
                    outln!(
                        "{}",
                        to_deterministic_pretty(&serde_json::json!({
                            "path": path.to_string_lossy(),
                            "matched": matched,
                            "total": indexer.len(),
                            "excluded_unknown": excluded_unknown,
                            "excluded_unknown_ts": excluded_unknown_ts,
                            "lossy_lines": indexer.lossy_lines(),
                            "lines": matched_lines,
                        }))?
                    );
                }
                // Diagnostics stay on stderr so json/jsonl stdout stays clean.
                eprintln!("\nMatched {} of {} line(s).", matched, indexer.len());
                if limited {
                    eprintln!("Stopped after {} match(es): --limit reached.", matched);
                }
                if indexer.lossy_lines() > 0 {
                    eprintln!(
                        "{} line(s) contain invalid UTF-8 (shown lossy as U+FFFD).",
                        indexer.lossy_lines()
                    );
                }
                if excluded_unknown > 0 {
                    eprintln!(
                        "Excluded {} line(s) whose log level could not be determined (--include-unknown to keep them).",
                        excluded_unknown
                    );
                }
                if excluded_unknown_ts > 0 {
                    eprintln!(
                        "Excluded {} line(s) whose timestamp could not be determined (--include-unknown to keep them).",
                        excluded_unknown_ts
                    );
                }
            }
        },
        Commands::Test { action } => match action {
            TestCommands::Parse {
                file,
                project,
                format,
            } => {
                let report = test::parse(&file, &project)?;
                let run = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(run)?);
                } else {
                    let s = &run.summary;
                    outln!("Test run: {} (project {})", run.run_id, run.project);
                    outln!(
                        "  {} total | {} passed | {} failed | {} errors | {} skipped | {:.2}s",
                        s.total,
                        s.passed,
                        s.failed,
                        s.errors,
                        s.skipped,
                        s.duration_sec
                    );
                    if !run.complete {
                        outln!("  INCOMPLETE: the report was truncated or malformed");
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
            TestCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = test::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
                } else {
                    outln!(
                        "Test diff: {} regressions, {} fixes, {} new failures, {} removed, {} skip changes",
                        diff.regressions.len(),
                        diff.fixes.len(),
                        diff.new_failures.len(),
                        diff.removed_tests.len(),
                        diff.skipped_changes.len()
                    );
                    for r in diff.regressions.iter().chain(&diff.new_failures) {
                        outln!(
                            "  - {}{}",
                            r.id,
                            r.message
                                .as_deref()
                                .map(|m| format!(": {}", m))
                                .unwrap_or_default()
                        );
                    }
                    for d in &diff.diagnostics {
                        outln!("  ! {}", d);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Trace { action } => match action {
            TraceCommands::Analyze { trace_file, format } => {
                let report = trace::analyze(&trace_file)?;
                let snapshot = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(snapshot)?);
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
                        outln!("  fd leaks: {:?}", snapshot.fd_leaks);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
            TraceCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = trace::diff_files(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
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
                    for e in &diff.new_errors {
                        outln!("  + new error: {}", e);
                    }
                    for e in &diff.resolved_errors {
                        outln!("  - resolved: {}", e);
                    }
                    for s in &diff.significant_latency_shifts {
                        outln!("  ~ {}", s);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Sys { action } => match action {
            SysCommands::Inspect { path, format } => {
                let report = sys::inspect(path.as_deref())?;
                let snapshot = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(snapshot)?);
                } else {
                    outln!(
                        "Systemd units: {} | cycles: {}",
                        snapshot.units.len(),
                        snapshot.cycles.len()
                    );
                    for (name, u) in &snapshot.units {
                        outln!("  {} {}", name, u.exec_start.as_deref().unwrap_or("-"));
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
            SysCommands::Cycles { dir, format } => {
                let report = sys::cycles(dir.as_deref())?;
                let (cycles, scanned_units) = (&report.data.cycles, report.data.scanned_units);
                if wants_json(format, false, false) {
                    outln!("{}", to_deterministic_pretty(cycles)?);
                } else if cycles.is_empty() {
                    outln!(
                        "No dependency cycles detected among {} units.",
                        scanned_units
                    );
                } else {
                    outln!("Detected {} cycle(s):", cycles.len());
                    for (i, c) in cycles.iter().enumerate() {
                        if c.edges.is_empty() {
                            outln!("  [{}] {}", i + 1, c.members.join(" -> "));
                        } else {
                            // Real directed path: each hop carries the
                            // directive and file:line that produced it.
                            let mut line = String::new();
                            for e in &c.edges {
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
                            line.push_str(&c.edges.last().unwrap().after);
                            outln!("  [{}] {}", i + 1, line);
                        }
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, false));
            }
            SysCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                // Inputs may be saved snapshots or live unit directories.
                let report = sys::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
                } else {
                    outln!(
                        "Systemd diff: +{} -{} ~{} unit(s), {} new cycle(s), {} resolved",
                        diff.added_units.len(),
                        diff.removed_units.len(),
                        diff.modified_units.len(),
                        diff.new_cycles.len(),
                        diff.resolved_cycles.len()
                    );
                    for u in &diff.modified_units {
                        outln!("  ~ {}", u.unit);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Build { action } => match action {
            BuildCommands::Inspect { file, format } => {
                let report = build::inspect(&file)?;
                let snapshot = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(snapshot)?);
                } else {
                    outln!(
                        "Build database: {} translation units, {} headers tracked",
                        snapshot.total_units,
                        snapshot.reverse_impact.len()
                    );
                    for u in &snapshot.units {
                        outln!("  {} [{}]", u.file, u.compiler);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
            }
            BuildCommands::Impact {
                file,
                header,
                format,
            } => {
                let report = build::impact(&file, &header)?;
                let impact = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(impact)?);
                } else {
                    outln!(
                        "Impact of {}: {} translation unit(s) need rebuild",
                        impact.target_header,
                        impact.total_impacted
                    );
                    for u in &impact.impacted_units {
                        outln!("  - {}", u);
                    }
                }
                // Evidence gaps stay visible on stderr in both modes;
                // missing/unresolved includes and truncation are warnings,
                // not findings (system headers often resolve nowhere).
                if let Some(hint) = &impact.hint {
                    eprintln!("warning: {}", hint);
                }
                if impact.missing_sources > 0
                    || impact.unresolved_includes > 0
                    || impact.scan_truncated
                {
                    eprintln!(
                        "warning: impact graph incomplete: {} missing source(s), {} unresolved include(s), truncated={}",
                        impact.missing_sources, impact.unresolved_includes, impact.scan_truncated
                    );
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
            BuildCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = build::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
                } else {
                    outln!(
                        "Build diff: +{} -{} ~{} unit(s)",
                        diff.added_units.len(),
                        diff.removed_units.len(),
                        diff.modified_units.len()
                    );
                    for u in &diff.modified_units {
                        outln!("  ~ {}", u.file);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Env { action } => match action {
            EnvCommands::Inspect {
                venv_path,
                project,
                format,
            } => {
                let report = env::inspect(&venv_path, project.as_deref())?;
                let snapshot = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(snapshot)?);
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
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
            EnvCommands::Check {
                venv_path,
                extras,
                format,
            } => {
                let report = env::check(&venv_path, &extras)?;
                let venv = &report.data;
                if wants_json(format, false, false) {
                    outln!(
                        "{}",
                        to_deterministic_pretty(&serde_json::json!({
                            "venv": venv_path.to_string_lossy(),
                            "missing_dependencies": venv.missing_dependencies,
                            "version_conflicts": venv.version_conflicts,
                            "unevaluated_dependencies": venv.unevaluated_dependencies,
                        }))?
                    );
                } else {
                    if venv.missing_dependencies.is_empty() {
                        outln!("All dependencies satisfied in {:?}.", venv_path);
                    } else {
                        outln!(
                            "Found {} missing dependenc(ies):",
                            venv.missing_dependencies.len()
                        );
                        for d in &venv.missing_dependencies {
                            outln!("  - {}", d);
                        }
                    }
                    if !venv.version_conflicts.is_empty() {
                        outln!("Version conflicts:");
                        for d in &venv.version_conflicts {
                            outln!("  - {}", d);
                        }
                    }
                    if !venv.unevaluated_dependencies.is_empty() {
                        outln!("Unevaluated requirements (uncertain markers):");
                        for d in &venv.unevaluated_dependencies {
                            outln!("  - {}", d);
                        }
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, false));
            }
            EnvCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = env::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
                } else {
                    outln!(
                        "Environment diff: +{} -{} ~{} package(s)",
                        diff.added_packages.len(),
                        diff.removed_packages.len(),
                        diff.version_changes.len()
                    );
                    for p in &diff.version_changes {
                        outln!("  ~ {}: {} -> {}", p.package, p.old_version, p.new_version);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
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
                force,
                format,
            } => {
                let report = bundle::create(&bundle::BundleRequest {
                    output: output.clone(),
                    disk,
                    log,
                    trace,
                    test,
                    sys,
                    env,
                    net,
                    force,
                })?;
                let manifest = &report.data;
                if wants_json(format, false, false) {
                    outln!("{}", to_deterministic_pretty(manifest)?);
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
                apply(&mut outcome, &report, Severity::Fail);
            }
            BundleCommands::Inspect { bundle, format } => {
                let report = bundle::inspect(&bundle)?;
                let inspect = &report.data;
                if wants_json(format, false, false) {
                    outln!("{}", to_deterministic_pretty(inspect)?);
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
                        for s in &m.sources {
                            outln!("  - {} ({} bytes)", s.path, s.size);
                        }
                        if !m.diagnostics.is_empty() {
                            outln!("Capture Diagnostics:");
                            for d in &m.diagnostics {
                                outln!("  ! {}", d);
                            }
                        }
                    }
                    None => outln!("Manifest: NOT FOUND (untrusted bundle)"),
                }
                outln!("Archive Entries:");
                for e in &inspect.entries {
                    outln!("  - {} ({} bytes)", e.name, e.size);
                }
            }
            BundleCommands::Verify { bundle, format } => {
                let report = bundle::verify(&bundle)?;
                let vr = &report.data;
                if wants_json(format, false, false) {
                    outln!("{}", to_deterministic_pretty(vr)?);
                    apply(&mut outcome, &report, Severity::Fail);
                    return Ok(outcome);
                }
                if vr.is_valid {
                    outln!("Bundle verification SUCCESSFUL: {:?}", bundle);
                    outln!("  Total manifest files:    {}", vr.total_files);
                    outln!("  Verified SHA-256 files:  {}", vr.verified_files);
                    // The manifest has no signature: this reports integrity
                    // against the embedded manifest, not provenance.
                    outln!("  Integrity: all files match the embedded manifest");
                    for u in &vr.unexpected_files {
                        outln!("  Unlisted entry (not in manifest): {}", u);
                    }
                } else {
                    eprintln!("Bundle integrity check FAILED: {:?}", bundle);
                    if let Some(err) = &vr.error {
                        eprintln!("  Error: {}", err);
                    }
                    for t in &vr.tampered_files {
                        eprintln!("  Tampered: {}", t);
                    }
                    for m in &vr.missing_files {
                        eprintln!("  Missing:  {}", m);
                    }
                    for u in &vr.unexpected_files {
                        eprintln!("  Unlisted: {}", u);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
            }
            BundleCommands::Show {
                bundle,
                entry,
                format,
            } => {
                let report = bundle::show(&bundle, &entry)?;
                let bytes = &report.data;
                if wants_json(format, false, false) {
                    outln!(
                        "{}",
                        to_deterministic_pretty(&serde_json::json!({
                            "bundle": bundle.to_string_lossy(),
                            "entry": entry,
                            "size": bytes.len(),
                            "content": String::from_utf8_lossy(bytes),
                        }))?
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
                format,
            } => {
                let report = bundle::extract(&bundle, &dest, force)?;
                let written = &report.data;
                if wants_json(format, false, false) {
                    outln!(
                        "{}",
                        to_deterministic_pretty(&serde_json::json!({
                            "bundle": bundle.to_string_lossy(),
                            "dest": dest.to_string_lossy(),
                            "extracted": written
                                .iter()
                                .map(|p| p.to_string_lossy())
                                .collect::<Vec<_>>(),
                        }))?
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
            NetCommands::Inspect {
                proc_dir,
                json,
                format,
                no_unix,
            } => {
                let report = net::inspect(proc_dir.as_deref(), no_unix)?;
                let net_report = &report.data;
                if wants_json(format, json, false) {
                    outln!("{}", to_deterministic_pretty(net_report)?);
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
                    for l in &net_report.listening {
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
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, json, false));
            }
            NetCommands::Diff {
                baseline,
                candidate,
                format,
            } => {
                let report = net::diff(&baseline, &candidate)?;
                let diff = &report.data;
                if wants_json(format, false, true) {
                    outln!("{}", to_deterministic_pretty(diff)?);
                } else {
                    outln!(
                        "Network diff: +{} -{} listener(s), +{} -{} connection(s), TIME_WAIT {:+}",
                        diff.new_listeners.len(),
                        diff.closed_listeners.len(),
                        diff.new_connections.len(),
                        diff.closed_connections.len(),
                        diff.time_wait_delta
                    );
                    for l in &diff.new_listeners {
                        outln!("  + {}:{}", l.local_address, l.local_port);
                    }
                    for l in &diff.closed_listeners {
                        outln!("  - {}:{}", l.local_address, l.local_port);
                    }
                }
                apply(&mut outcome, &report, Severity::Fail);
                print_findings(&report.findings, wants_json(format, false, true));
            }
        },
        Commands::Doctor {
            root,
            procfs,
            systemd_dir,
            json,
            format,
            fail_on,
        } => {
            let report = doctor::run(root.as_deref(), procfs.as_deref(), systemd_dir.as_deref())?;
            let dr = &report.data;
            if wants_json(format, json, false) {
                outln!("{}", to_deterministic_pretty(dr)?);
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
                for c in &dr.checks {
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
                        c.message
                    );
                    if let Some(ref rec) = c.recommendation {
                        outln!("             -> Recommendation: {}", rec);
                    }
                }
            }
            // --fail-on picks the severity threshold that counts as a
            // finding. Doctor renders its own check table — no Findings
            // trailer.
            let threshold = match fail_on {
                FailOn::Warn => Severity::Warn,
                FailOn::Fail => Severity::Fail,
            };
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
