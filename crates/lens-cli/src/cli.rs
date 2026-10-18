use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// `--fail-on` severity threshold for `lens doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    Warn,
    Fail,
}

/// `--format` output selector, unified across report commands.
/// Commands keep their historical default when the flag is absent.
/// `jsonl` is honored by `log filter` (one object per matched line);
/// elsewhere it falls back to `json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

/// Global flags, resolved once and passed to `dispatch` — they live on
/// `Cli` (with `global = true`) so both `lens --format json net diff …`
/// and `lens net diff … --format json` parse identically.
#[derive(Debug, Clone, Copy)]
pub struct GlobalOpts {
    pub format: Option<OutputFormat>,
    pub json: bool,
    pub fail_on: FailOn,
    pub quiet: bool,
}

#[derive(Parser)]
#[command(name = "lens")]
#[command(version)]
#[command(about = "Unified Systems Diagnostics and Forensic Platform", long_about = None)]
pub struct Cli {
    /// Output format: text, json, or jsonl (log filter only). Each
    /// command keeps its historical default when the flag is absent.
    #[arg(long, value_enum, global = true)]
    pub format: Option<OutputFormat>,
    /// Emit the report as JSON (equivalent to --format json)
    #[arg(long, global = true, hide = true)]
    pub json: bool,
    /// Lowest finding severity that exits 1: `fail` (default) or `warn`
    #[arg(long, value_enum, global = true, default_value_t = FailOn::Fail)]
    pub fail_on: FailOn,
    /// Suppress non-error stderr diagnostics (stdout is unaffected)
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    pub fn global_opts(&self) -> GlobalOpts {
        GlobalOpts {
            format: self.format,
            json: self.json,
            fail_on: self.fail_on,
            quiet: self.quiet,
        }
    }
}

/// `--as` domain override for `lens inspect`/`lens diff`: skip
/// auto-detection and treat the input(s) as this domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AsKind {
    Abi,
    Test,
    Trace,
    Build,
    Sys,
    Env,
    Net,
    Log,
    Disk,
    Bundle,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Filesystem footprint analysis, duplicate detection, and safe cleanup
    Disk {
        #[command(subcommand)]
        action: DiskCommands,
    },
    /// ELF binary inspection, dynamic symbol surface, and ABI diffing
    Abi {
        #[command(subcommand)]
        action: AbiCommands,
    },
    /// High-performance memory-mapped log viewer and filter
    Log {
        #[command(subcommand)]
        action: LogCommands,
    },
    /// High-speed JUnit test report parsing and regression diffing
    Test {
        #[command(subcommand)]
        action: TestCommands,
    },
    /// Linux syscall trace (strace) analysis and latency/error diffing
    Trace {
        #[command(subcommand)]
        action: TraceCommands,
    },
    /// Systemd service unit, drop-in override, and dependency DAG analysis
    Sys {
        #[command(subcommand)]
        action: SysCommands,
    },
    /// Compilation database analysis, compiler flags, and header impact DAG
    Build {
        #[command(subcommand)]
        action: BuildCommands,
    },
    /// Python virtual environment audit, dependency check, and shadowing detection
    Env {
        #[command(subcommand)]
        action: EnvCommands,
    },
    /// Linux socket, listening port, and network connection forensics
    Net {
        #[command(subcommand)]
        action: NetCommands,
    },
    /// Auto-detect what a path is and run the matching inspect command
    /// (ELF → `abi inspect`, JUnit → `test parse`, strace → `trace
    /// analyze`, compile DB → `build inspect`, venv → `env inspect`,
    /// unit dir/file → `sys inspect`, bundle → `bundle inspect`,
    /// log → `log inspect`, other dir → `disk scan`)
    Inspect {
        /// File or directory to inspect
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Treat the input as this domain instead of auto-detecting
        #[arg(long = "as", value_enum)]
        as_kind: Option<AsKind>,
    },
    /// Auto-detect both inputs and run the matching diff command
    /// (ELF→abi, JUnit→test, strace→trace, compile DB→build,
    /// systemd→sys, venv/env snapshot→env, net report→net)
    Diff {
        /// Older/reference input
        baseline: PathBuf,
        /// Newer input to compare against the baseline
        candidate: PathBuf,
        /// Treat both inputs as this domain instead of auto-detecting
        #[arg(long = "as", value_enum)]
        as_kind: Option<AsKind>,
    },
    /// Capture a point-in-time system snapshot bundle (host, network,
    /// systemd, doctor + optional sources) to diff later with `lens diff`
    Snapshot {
        /// Output .lens path (default: snapshot-<host>-<UTC>.lens in cwd)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// procfs root for host/network/doctor evidence (default /proc)
        #[arg(long)]
        procfs: Option<PathBuf>,
        /// systemd unit dir for the sys snapshot and doctor (default:
        /// merged systemd search path)
        #[arg(long)]
        systemd_dir: Option<PathBuf>,
        /// Log file to embed (default: first existing of
        /// /var/log/syslog, messages, kern.log, daemon.log)
        #[arg(long)]
        log: Option<PathBuf>,
        /// Directory to scan for a disk snapshot artifact
        #[arg(long)]
        disk: Option<PathBuf>,
        /// Virtualenv root for an environment snapshot artifact
        #[arg(long)]
        env: Option<PathBuf>,
        /// JUnit XML file/directory to parse into the snapshot
        #[arg(long)]
        test: Option<PathBuf>,
        /// strace output file to analyze into the snapshot
        #[arg(long)]
        trace: Option<PathBuf>,
        /// Overwrite the output file if it already exists
        #[arg(long)]
        force: bool,
        /// Redact secrets, tokens, and API keys from embedded artifacts
        #[arg(long)]
        redact: bool,
    },
    /// Forensic Flight Recorder: bundle multiple diagnostic artifacts into a .lens container
    Bundle {
        #[command(subcommand)]
        action: BundleCommands,
    },
    /// Unified system health check across storage, network, services, and environment
    Doctor {
        /// Filesystem root to check (default /)
        #[arg(long)]
        root: Option<PathBuf>,
        /// procfs mount to read instead of /proc
        #[arg(long)]
        procfs: Option<PathBuf>,
        /// Unit directory to load instead of the systemd search path
        #[arg(long)]
        systemd_dir: Option<PathBuf>,
    },
    /// Interactive terminal dashboard (storage, services, logs, network)
    Tui {
        /// Initial path to inspect
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Log file to show in the Logs tab instead of auto-detection
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Generate shell auto-completion script
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub enum DiskCommands {
    /// Scan directory and print space usage summary or snapshot JSON
    Scan {
        /// Directory to scan
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Parallelize per-entry stat with rayon
        #[arg(long)]
        parallel: bool,
        /// Maximum directory depth to descend
        #[arg(long)]
        max_depth: Option<usize>,
        /// Skip entries (repeatable): `name` matches the entry name
        /// exactly, `*.log`/`a?c` glob the name, `a/b` globs the path
        /// relative to the scan root
        #[arg(long)]
        exclude: Vec<String>,
        /// Do not cross filesystem boundaries
        #[arg(short = 'x', long)]
        one_file_system: bool,
        /// Show the N largest top-level entries in text output (0 disables)
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
    /// Find duplicate files with hard link deduplication
    Duplicates {
        /// Directory to scan for duplicates
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Minimum file size in bytes to consider
        #[arg(long, default_value = "1024")]
        min_size: u64,
        /// Skip entries (repeatable); same rules as `disk scan --exclude`
        #[arg(long)]
        exclude: Vec<String>,
        /// Do not cross filesystem boundaries
        #[arg(short = 'x', long)]
        one_file_system: bool,
        /// Maximum directory depth to descend
        #[arg(long)]
        max_depth: Option<usize>,
    },
    /// Safely move paths to trash following the FreeDesktop Trash spec;
    /// `list`/`restore` manage existing entries
    #[command(
        args_conflicts_with_subcommands = true,
        subcommand_precedence_over_arg = true
    )]
    Trash {
        #[command(subcommand)]
        action: Option<TrashCommands>,
        /// Paths to move to trash
        paths: Vec<PathBuf>,
        /// Report what would be trashed without moving anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum TrashCommands {
    /// List trash entries with their original paths
    List {
        /// Trash root to inspect instead of the home trash (e.g. a
        /// mount's `.Trash-$uid` after a cross-device fallback)
        #[arg(long)]
        trash_dir: Option<PathBuf>,
    },
    /// Restore a trashed entry by its trash name (see `trash list`)
    Restore {
        /// Trash-internal name, e.g. `foo.txt` or `foo.txt_1`
        name: String,
        /// Trash root containing the entry instead of the home trash
        #[arg(long)]
        trash_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum AbiCommands {
    /// Inspect an ELF binary and output JSON report
    Inspect {
        /// ELF binary or shared library to inspect
        binary: PathBuf,
    },
    /// Compare two ELF binaries and report 3-state ABI compatibility
    Diff {
        /// Older/reference ELF binary
        baseline: PathBuf,
        /// Newer ELF binary to compare against the baseline
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum LogCommands {
    /// Inspect log file metrics and line counts.
    /// Each PATH may be `-` for stdin, a `.gz` file, or a `*`/`?`
    /// filename glob (expanded against its parent directory).
    Inspect {
        /// Log file path(s), `-` for stdin, `.gz` files, or globs
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
    },
    /// Search and filter log lines (memory-mapped, memchr-accelerated).
    /// Each PATH may be `-` for stdin, a `.gz` file, or a `*`/`?`
    /// filename glob; with several inputs --limit is global and context
    /// never crosses a file boundary.
    Filter {
        /// Log file path(s), `-` for stdin, `.gz` files, or globs
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Case-insensitive substring to match
        #[arg(long)]
        query: Option<String>,
        /// Regex pattern alternative to --query's substring match
        #[arg(long, conflicts_with = "query")]
        regex: Option<String>,
        /// Lowest severity to emit: trace|debug|info|warn|error|fatal
        #[arg(long)]
        min_level: Option<String>,
        /// Keep lines whose level could not be determined when --min-level is set,
        /// or whose timestamp could not be determined when --since/--until is set
        #[arg(long)]
        include_unknown: bool,
        /// Emit only lines at/after this timestamp (inclusive). Accepts RFC 3339
        /// (`2026-10-09T12:00:00Z`, with offset), `YYYY-MM-DD HH:MM:SS`, or
        /// `YYYY-MM-DD`; forms without an offset are read as UTC
        #[arg(long)]
        since: Option<String>,
        /// Emit only lines at/before this timestamp (inclusive); same forms as --since
        #[arg(long)]
        until: Option<String>,
        /// Year assumed for year-less syslog timestamps like `Oct  9 12:00:00`
        /// (default: current UTC year)
        #[arg(long)]
        year: Option<i32>,
        /// Stop after N matched lines
        #[arg(long)]
        limit: Option<usize>,
        /// Show N lines of context around each match
        #[arg(long, default_value_t = 0)]
        context: usize,
        /// Keep polling the file for appended lines after the initial
        /// pass (like `tail -f`). Requires exactly one regular,
        /// non-gz, non-stdin input and --context 0.
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Level counts and a time histogram over the lines the filter
    /// keeps, across one or more inputs.
    Stats {
        /// Log file path(s), `-` for stdin, `.gz` files, or globs
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Histogram bucket width: 1m|5m|15m|1h|1d (default: auto-picked
        /// to keep the histogram at ~60 buckets)
        #[arg(long)]
        bucket: Option<String>,
        /// Case-insensitive substring to match
        #[arg(long)]
        query: Option<String>,
        /// Regex pattern alternative to --query's substring match
        #[arg(long, conflicts_with = "query")]
        regex: Option<String>,
        /// Lowest severity to count: trace|debug|info|warn|error|fatal
        #[arg(long)]
        min_level: Option<String>,
        /// Keep lines whose level could not be determined when --min-level is set,
        /// or whose timestamp could not be determined when --since/--until is set
        #[arg(long)]
        include_unknown: bool,
        /// Count only lines at/after this timestamp (inclusive). Accepts RFC 3339
        /// (`2026-10-09T12:00:00Z`, with offset), `YYYY-MM-DD HH:MM:SS`, or
        /// `YYYY-MM-DD`; forms without an offset are read as UTC
        #[arg(long)]
        since: Option<String>,
        /// Count only lines at/before this timestamp (inclusive); same forms as --since
        #[arg(long)]
        until: Option<String>,
        /// Year assumed for year-less syslog timestamps like `Oct  9 12:00:00`
        /// (default: current UTC year)
        #[arg(long)]
        year: Option<i32>,
    },
}

#[derive(Subcommand)]
pub enum TestCommands {
    /// Parse JUnit XML test report(s) — file, directory of *.xml, or glob
    Parse {
        /// JUnit XML file, directory, or glob pattern
        file: PathBuf,
        /// Project label recorded in the snapshot
        #[arg(long, default_value = "default")]
        project: String,
    },
    /// Compare two JUnit XML runs (file, directory of *.xml, or glob)
    /// and report structured regressions
    Diff {
        /// Older/reference JUnit XML file, directory, or glob
        baseline: PathBuf,
        /// Newer JUnit XML file, directory, or glob
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum TraceCommands {
    /// Analyze strace output log and output JSON snapshot
    Analyze {
        /// strace output file to analyze
        trace_file: PathBuf,
    },
    /// Compare two strace log runs and detect latency shifts/new errors
    Diff {
        /// Older/reference strace output file
        baseline: PathBuf,
        /// Newer strace output file
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum SysCommands {
    /// Inspect systemd unit file or unit directory; with no path, merges
    /// the systemd search path (/etc, /run, /usr/lib, /lib)
    Inspect {
        /// Unit file or directory; omit to merge the systemd search path
        path: Option<PathBuf>,
    },
    /// Detect ordering cycles in systemd units; with no dir, merges the
    /// systemd search path (/etc, /run, /usr/lib, /lib)
    Cycles {
        /// Unit directory; omit to merge the systemd search path
        dir: Option<PathBuf>,
    },
    /// Compare two systemd snapshots
    Diff {
        /// Older/reference snapshot JSON or unit directory
        baseline: PathBuf,
        /// Newer snapshot JSON or unit directory
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum BuildCommands {
    /// Inspect compile_commands.json database
    Inspect {
        /// Path to compile_commands.json
        file: PathBuf,
    },
    /// Calculate reverse compilation impact for a header
    Impact {
        /// Path to compile_commands.json
        file: PathBuf,
        /// Header to analyze; relative paths resolve against the
        /// compile database and each entry's `directory`
        #[arg(long)]
        header: String,
    },
    /// Diff two compile_commands.json databases
    Diff {
        /// Older/reference compile_commands.json
        baseline: PathBuf,
        /// Newer compile_commands.json
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Inspect Python virtualenv and optionally check local project shadowing
    Inspect {
        /// Path to the virtualenv root (contains pyvenv.cfg)
        venv_path: PathBuf,
        /// Project directory to scan for stdlib/package shadowing
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Check for missing dependencies in a virtualenv
    Check {
        /// Path to the virtualenv root (contains pyvenv.cfg)
        venv_path: PathBuf,
        /// Activate `extra == "name"` dependency markers (comma-separated)
        #[arg(long, value_delimiter = ',')]
        extras: Vec<String>,
    },
    /// Diff two env inputs — snapshot JSONs or live virtualenv
    /// directories, freely mixed
    Diff {
        /// Older/reference env snapshot JSON or virtualenv directory
        baseline: PathBuf,
        /// Newer env snapshot JSON or virtualenv directory
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum NetCommands {
    /// Inspect open listening ports, sockets, and associated processes
    Inspect {
        /// procfs root to read instead of /proc
        #[arg(long)]
        proc_dir: Option<PathBuf>,
        /// Omit unix-domain sockets from the output
        #[arg(long)]
        no_unix: bool,
    },
    /// Compare two network snapshot JSON reports
    Diff {
        /// Older/reference net snapshot JSON
        baseline: PathBuf,
        /// Newer net snapshot JSON
        candidate: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum BundleCommands {
    /// Create a consolidated .lens forensic archive (tar.gz + embedded manifest)
    Create {
        /// Output path for the .lens bundle
        output: PathBuf,
        /// Directory to scan for a disk snapshot artifact
        #[arg(long)]
        disk: Option<PathBuf>,
        /// Log file to embed (raw content, tail-bounded)
        #[arg(long)]
        log: Option<PathBuf>,
        /// strace output file to analyze into the bundle
        #[arg(long)]
        trace: Option<PathBuf>,
        /// JUnit XML file/directory to parse into the bundle
        #[arg(long)]
        test: Option<PathBuf>,
        /// Unit directory for a systemd snapshot artifact
        #[arg(long)]
        sys: Option<PathBuf>,
        /// Virtualenv root for an environment snapshot artifact
        #[arg(long)]
        env: Option<PathBuf>,
        /// Capture a live network socket report from procfs
        #[arg(long)]
        net: bool,
        /// Embed a doctor health report (host scope)
        #[arg(long)]
        doctor: bool,
        /// Overwrite the output file if it already exists
        #[arg(long)]
        force: bool,
        /// Redact secrets, tokens, and API keys from embedded artifacts
        #[arg(long)]
        redact: bool,
    },
    /// Diff two .lens bundles section by section (verified first)
    Diff {
        /// Older/reference .lens bundle
        baseline: PathBuf,
        /// Newer .lens bundle to compare against the baseline
        candidate: PathBuf,
    },
    /// Inspect contents and diagnostics of a .lens bundle
    Inspect {
        /// Path to the .lens bundle
        bundle: PathBuf,
    },
    /// Verify bundle contents against its embedded manifest's SHA-256 checksums
    Verify {
        /// Path to the .lens bundle
        bundle: PathBuf,
    },
    /// Print one archive entry's contents (e.g. reports/disk_snapshot.json)
    Show {
        /// Path to the .lens bundle
        bundle: PathBuf,
        /// Entry name as listed by `bundle inspect`
        entry: String,
    },
    /// Verify, then extract a bundle into a directory (no overwrite without --force)
    Extract {
        /// Path to the .lens bundle
        bundle: PathBuf,
        /// Destination directory
        dest: PathBuf,
        /// Overwrite existing files
        #[arg(long)]
        force: bool,
    },
}
