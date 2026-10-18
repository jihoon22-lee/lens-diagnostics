use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use lens_core::{LensError, Result};
use serde::{Deserialize, Serialize};

use crate::{Report, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthStatus {
    Pass,
    Warn,
    Fail,
    /// The check does not apply to the inspected root (e.g. host-only
    /// evidence under `--root`) — never counted as a finding.
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub category: String,
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
    pub recommendation: Option<String>,
    /// Suggested follow-up command, set only on actionable (Warn/Fail)
    /// checks. `default` keeps older `lens.doctor/v1` JSON deserializable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorSummary {
    pub total: usize,
    pub passed: usize,
    pub warnings: usize,
    pub failures: usize,
    #[serde(default)]
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub schema_version: String,
    pub overall_status: HealthStatus,
    pub checks: Vec<DoctorCheck>,
    pub summary: DoctorSummary,
}

/// statvfs one mount point and grade it: space free <5% (or <1 GiB) is
/// Fail, <15% (or <5 GiB) is Warn; when inode data exists (`f_files>0`)
/// the same thresholds apply to free-inode % and the final status is the
/// worse of the two.
fn capacity_check(mount: &Path, name: &str) -> DoctorCheck {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let c_path = std::ffi::CString::new(mount.to_string_lossy().as_bytes()).unwrap_or_default();
    let res = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    let next = format!("lens disk scan {} --top 20", mount.display());

    // f_frsize can be 0 on unusual filesystems — guard to avoid a div/zero.
    if res == 0 && stat.f_blocks > 0 && stat.f_frsize > 0 {
        let total_bytes = stat.f_blocks as u64 * stat.f_frsize as u64;
        let avail_bytes = stat.f_bavail as u64 * stat.f_frsize as u64;
        // Inode exhaustion fills a filesystem just as surely as bytes.
        // Some filesystems (9p, network fs) report f_favail > f_files —
        // clamp so the display stays sane.
        let inode = if stat.f_files > 0 {
            let ifree_pct =
                ((stat.f_favail as f64 / stat.f_files as f64) * 100.0).clamp(0.0, 100.0);
            Some(ifree_pct)
        } else {
            None
        };
        evaluate_capacity(total_bytes, avail_bytes, inode, mount, name)
    } else {
        DoctorCheck {
            category: "Storage".to_string(),
            name: name.to_string(),
            status: HealthStatus::Warn,
            message: "Unable to query filesystem statistics".to_string(),
            next_command: Some(next),
            recommendation: None,
        }
    }
}

pub(crate) fn evaluate_capacity(
    total_bytes: u64,
    avail_bytes: u64,
    inode: Option<f64>,
    mount: &Path,
    name: &str,
) -> DoctorCheck {
    let next = format!("lens disk scan {} --top 20", mount.display());
    let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let free_pct = (avail_bytes as f64 / total_bytes as f64) * 100.0;
    let avail_gb = avail_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

    // Small partitions (e.g. 256MB-1GB /boot/efi) naturally have < 1 GB free
    // even when 90% empty; only apply absolute GB thresholds on larger volumes.
    let space_status = if free_pct < 5.0 || (total_gb >= 10.0 && avail_gb < 1.0) {
        HealthStatus::Fail
    } else if free_pct < 15.0 || (total_gb >= 32.0 && avail_gb < 5.0) {
        HealthStatus::Warn
    } else {
        HealthStatus::Pass
    };
    let inode_status = match inode {
        Some(p) if p < 5.0 => HealthStatus::Fail,
        Some(p) if p < 15.0 => HealthStatus::Warn,
        _ => HealthStatus::Pass,
    };
    let status = match (space_status, inode_status) {
        (HealthStatus::Fail, _) | (_, HealthStatus::Fail) => HealthStatus::Fail,
        (HealthStatus::Warn, _) | (_, HealthStatus::Warn) => HealthStatus::Warn,
        _ => HealthStatus::Pass,
    };

    let inode_note = inode
        .map(|p| format!(", inodes {p:.1}% free"))
        .unwrap_or_default();
    // Inode-only branches must report the inode percentage, not the space one.
    let inode_pct = inode.unwrap_or(100.0);
    let (message, recommendation) = match status {
        HealthStatus::Fail
            if inode_status == HealthStatus::Fail && space_status != HealthStatus::Fail =>
        {
            (
                format!(
                    "Critically low free inodes: {:.1}% free ({:.2} GB space available){}",
                    inode_pct, avail_gb, inode_note
                ),
                Some(
                    "Clean up directories with excessive small files using 'lens disk scan'."
                        .to_string(),
                ),
            )
        }
        HealthStatus::Fail => (
            format!(
                "Critically low disk space: {:.1}% free ({:.2} GB available){}",
                free_pct, avail_gb, inode_note
            ),
            Some(
                "Clean up disk immediately using 'lens disk scan' and trash old files.".to_string(),
            ),
        ),
        HealthStatus::Warn
            if inode_status == HealthStatus::Warn && space_status == HealthStatus::Pass =>
        {
            (
                format!(
                    "Low free inodes warning: {:.1}% free ({:.2} GB space available){}",
                    inode_pct, avail_gb, inode_note
                ),
                Some("Consider pruning directories with many small files.".to_string()),
            )
        }
        HealthStatus::Warn => (
            format!(
                "Low disk space warning: {:.1}% free ({:.2} GB available){}",
                free_pct, avail_gb, inode_note
            ),
            Some("Consider pruning large unused logs or artifacts.".to_string()),
        ),
        _ => (
            format!(
                "Adequate disk space: {:.1}% free ({:.2} GB available){}",
                free_pct, avail_gb, inode_note
            ),
            None,
        ),
    };
    DoctorCheck {
        category: "Storage".to_string(),
        name: name.to_string(),
        status,
        message,
        next_command: if status == HealthStatus::Pass {
            None
        } else {
            Some(next)
        },
        recommendation,
    }
}

/// Filesystems that carry no user data — never worth a capacity check.
const PSEUDO_FSTYPES: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "securityfs",
    "pstore",
    "debugfs",
    "tracefs",
    "mqueue",
    "hugetlbfs",
    "fusectl",
    "configfs",
    "bpf",
    "autofs",
    "binfmt_misc",
    "nsfs",
    "ramfs",
    "efivarfs",
    "squashfs",
    "rpc_pipefs",
    // Read-only packaged mounts — immutable by construction, always
    // report 0% free, and can never be cleaned up.
    "snapfuse",
    "fuse.snapfuse",
    // Flatpak document portal — a synthetic FUSE view, not a volume.
    "fuse.portal",
];

/// Parse `/proc/self/mounts` content into `(mount_point, fstype)` pairs
/// of real writable filesystems — pseudo filesystems and read-only (`ro`)
/// mounts are dropped, and an `overlay` mount counts only when it backs `/`
/// (container roots). Field-2 octal escapes (`\040` space, `\134` backslash, …)
/// are decoded.
pub fn parse_mounts(content: &str) -> Vec<(String, String)> {
    let decode = |s: &str| -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\'
                && i + 3 < b.len()
                && b[i + 1].is_ascii_digit()
                && b[i + 2].is_ascii_digit()
                && b[i + 3].is_ascii_digit()
            {
                let v = (b[i + 1] - b'0') as u32 * 64
                    + (b[i + 2] - b'0') as u32 * 8
                    + (b[i + 3] - b'0') as u32;
                if v <= 0o377 {
                    out.push(v as u8);
                    i += 4;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    content
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let _device = f.next()?;
            let point = decode(f.next()?);
            let fstype = f.next()?.to_string();
            let opts = f.next().unwrap_or("");
            if PSEUDO_FSTYPES.contains(&fstype.as_str()) {
                return None;
            }
            if opts.split(',').any(|o| o == "ro") {
                return None;
            }
            if fstype == "overlay" && point != "/" {
                return None;
            }
            Some((point, fstype))
        })
        .collect()
}

/// Storage capacity checks: the root check always runs (against `root`
/// or `/`); without `--root`, every real mount under
/// `<procfs or /proc>/self/mounts` gets its own check, deduplicated by
/// statvfs `f_fsid` so bind mounts and btrfs subvolumes do not multiply.
pub fn check_storage(root: Option<&Path>, procfs: Option<&Path>) -> Vec<DoctorCheck> {
    let root_check = capacity_check(root.unwrap_or_else(|| Path::new("/")), "Root Disk Capacity");
    if root.is_some() {
        return vec![root_check];
    }
    let mounts_path = procfs
        .unwrap_or_else(|| Path::new("/proc"))
        .join("self/mounts");
    let mut checks = vec![root_check];
    let Ok(content) = lens_core::read_policy::read_to_string(&mounts_path) else {
        // Unreadable mounts table → just the root check, as before.
        return checks;
    };
    let mut seen_fsid: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Seed with /'s filesystem id: a bind mount or subvolume of the
    // root filesystem exposed at another mountpoint must not get a
    // second capacity check.
    if let Some(fsid) = statvfs_fsid("/") {
        seen_fsid.insert(fsid);
    }
    let pairs: Vec<(String, Option<u64>)> = parse_mounts(&content)
        .into_iter()
        .map(|(point, _fstype)| (point.clone(), statvfs_fsid(&point)))
        .collect();
    for point in dedupe_mounts(&pairs, &mut seen_fsid) {
        checks.push(capacity_check(
            Path::new(&point),
            &format!("Disk Capacity {point}"),
        ));
    }
    checks
}

/// statvfs `f_fsid` of a mount point, `None` when the call fails.
fn statvfs_fsid(point: &str) -> Option<u64> {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let c = std::ffi::CString::new(point.as_bytes()).ok()?;
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0).then_some(stat.f_fsid)
}

/// Keep the mount points whose filesystem id is new to `seen_fsid`
/// (skipping `/`, already covered by the root check). A `None` fsid
/// (statvfs failed) dedupes by an artificial per-miss id so one bad
/// mount cannot mask another's check. Extracted for testing.
fn dedupe_mounts(
    mounts: &[(String, Option<u64>)],
    seen_fsid: &mut std::collections::HashSet<u64>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (point, fsid) in mounts {
        if point == "/" {
            continue; // already reported as Root Disk Capacity
        }
        let id = fsid.unwrap_or(u64::MAX - seen_fsid.len() as u64);
        if seen_fsid.insert(id) {
            out.push(point.clone());
        }
    }
    out
}

pub fn check_network(proc_path: Option<&Path>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    let net_report = match lens_net::parser::inspect_network(proc_path) {
        Ok(r) => r,
        Err(e) => {
            checks.push(DoctorCheck {
                category: "Network".to_string(),
                name: "Procfs Network Parsing".to_string(),
                status: HealthStatus::Warn,
                message: format!("Failed to parse network sockets: {}", e),
                next_command: None,
                recommendation: Some("Ensure /proc/net is mounted and accessible.".to_string()),
            });
            return checks;
        }
    };

    checks.push(DoctorCheck {
        category: "Network".to_string(),
        name: "Network Sockets Overview".to_string(),
        status: HealthStatus::Pass,
        message: format!(
            "Total sockets: {}, Active listeners: {}, Established: {}",
            net_report.summary.total_sockets,
            net_report.summary.listening_ports,
            net_report.summary.established_connections
        ),
        next_command: None,
        recommendation: None,
    });

    let sensitive_ports = [
        (22, "SSH"),
        (23, "Telnet"),
        (3306, "MySQL"),
        (5432, "PostgreSQL"),
        (6379, "Redis"),
        (27017, "MongoDB"),
    ];

    let mut exposed_sensitive = Vec::new();
    for p in &net_report.listening {
        let is_wildcard =
            p.local_address == "0.0.0.0" || p.local_address == "::" || p.local_address.is_empty();
        if is_wildcard {
            for &(port, name) in &sensitive_ports {
                if p.local_port == port {
                    let item = format!("{}(port {})", name, port);
                    if !exposed_sensitive.contains(&item) {
                        exposed_sensitive.push(item);
                    }
                }
            }
        }
    }

    if !exposed_sensitive.is_empty() {
        checks.push(DoctorCheck {
            category: "Network".to_string(),
            name: "Wildcard Sensitive Listeners".to_string(),
            status: HealthStatus::Warn,
            message: format!(
                "Sensitive services exposed on wildcard 0.0.0.0/:: address: {}",
                exposed_sensitive.join(", ")
            ),
            next_command: Some("lens net inspect".to_string()),
            recommendation: Some(
                "Bind sensitive services to 127.0.0.1 or configure a firewall rule.".to_string(),
            ),
        });
    } else {
        checks.push(DoctorCheck {
            category: "Network".to_string(),
            name: "Wildcard Sensitive Listeners".to_string(),
            status: HealthStatus::Pass,
            message:
                "No exposed sensitive database or remote management ports on wildcard addresses"
                    .to_string(),
            next_command: None,
            recommendation: None,
        });
    }

    if net_report.summary.orphan_sockets > 0 {
        checks.push(DoctorCheck {
            category: "Network".to_string(),
            name: "Orphan Socket Detection".to_string(),
            status: HealthStatus::Warn,
            message: format!(
                "Detected {} orphaned sockets (no live process holds the inode)",
                net_report.summary.orphan_sockets
            ),
            next_command: Some("lens net inspect --format json".to_string()),
            recommendation: Some(
                "Run with elevated permissions or inspect kernel socket tables.".to_string(),
            ),
        });
    } else if net_report.summary.owner_unknown_sockets > 0 {
        // Permission limits are not orphans — do not warn for sockets we
        // simply could not inspect.
        checks.push(DoctorCheck {
            category: "Network".to_string(),
            name: "Orphan Socket Detection".to_string(),
            status: HealthStatus::Pass,
            message: format!(
                "All attributable sockets have owners; {} socket(s) could not be inspected (permission denied on /proc/<pid>/fd)",
                net_report.summary.owner_unknown_sockets
            ),
            next_command: None,
            recommendation: Some(
                "Re-run with sudo to attribute those sockets.".to_string(),
            ),
        });
    } else {
        checks.push(DoctorCheck {
            category: "Network".to_string(),
            name: "Orphan Socket Detection".to_string(),
            status: HealthStatus::Pass,
            message: "All open sockets correlate with active processes".to_string(),
            next_command: None,
            recommendation: None,
        });
    }

    checks
}

use lens_sys::OrderingGraph;

/// Effective systemd unit directories for the services check: an explicit
/// `systemd_dir` wins; under `--root` the standard search dirs are
/// re-rooted (`<root>/etc/systemd/system`, …); otherwise the host search
/// path. The bool marks an explicit dir whose load errors must surface.
fn services_dirs(root: Option<&Path>, systemd_dir: Option<&Path>) -> (Vec<PathBuf>, bool) {
    if let Some(d) = systemd_dir {
        return (vec![d.to_path_buf()], true);
    }
    let dirs: Vec<PathBuf> = lens_sys::SYSTEMD_SEARCH_DIRS
        .iter()
        .map(|d| match root {
            Some(r) => r.join(d.trim_start_matches('/')),
            None => PathBuf::from(d),
        })
        .collect();
    (dirs, false)
}

/// Load systemd units with `<unit>.d/*.conf` drop-ins applied. An
/// explicit directory goes through `load_units` and propagates its
/// error; merged search dirs tolerate missing entries. Unreadable units
/// are preserved as diagnostic stubs so a doctor run never silently
/// drops them.
pub fn load_systemd_units(
    dirs: &[PathBuf],
    explicit: bool,
) -> std::result::Result<std::collections::BTreeMap<String, lens_sys::SystemdUnit>, String> {
    if explicit {
        return lens_sys::load_units(&dirs[0]).map_err(|e| e.to_string());
    }
    Ok(lens_sys::load_units_merged(dirs))
}

/// The cycle check plus, when units loaded with parser/loader
/// diagnostics, a Warn check calling those out — malformed units are
/// evidence about the services picture, not silent gaps.
pub fn check_services(root: Option<&Path>, systemd_dir: Option<&Path>) -> Vec<DoctorCheck> {
    let (dirs, explicit) = services_dirs(root, systemd_dir);
    if !dirs.iter().any(|d| d.exists()) {
        return vec![DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Dependency Cycles".to_string(),
            status: HealthStatus::Skip,
            message: "No systemd unit directory to inspect; services check skipped".to_string(),
            next_command: None,
            recommendation: None,
        }];
    }

    let units = match load_systemd_units(&dirs, explicit) {
        Ok(u) => u,
        Err(e) => {
            return vec![DoctorCheck {
                category: "Services".to_string(),
                name: "Systemd Dependency Cycles".to_string(),
                status: HealthStatus::Warn,
                message: format!("Failed to load systemd units: {}", e),
                next_command: Some("lens sys inspect --format json".to_string()),
                recommendation: Some(
                    "Check the unit directory for unreadable or malformed files.".to_string(),
                ),
            }];
        }
    };
    let graph = OrderingGraph::build(&units);
    let cycles = graph.find_cycles();

    let cycle_check = if !cycles.is_empty() {
        DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Dependency Cycles".to_string(),
            status: HealthStatus::Fail,
            message: format!(
                "Detected {} cyclic dependency loop(s) in systemd units",
                cycles.len()
            ),
            next_command: Some("lens sys cycles".to_string()),
            recommendation: Some(
                "Resolve circular After=/Before=/Requires= directives to prevent boot deadlocks."
                    .to_string(),
            ),
        }
    } else {
        DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Dependency Cycles".to_string(),
            status: HealthStatus::Pass,
            message: format!(
                "Analyzed {} systemd units; 0 cyclic dependencies found",
                units.len()
            ),
            next_command: None,
            recommendation: None,
        }
    };

    let mut checks = vec![cycle_check];
    let diag_units = units.values().filter(|u| !u.diagnostics.is_empty()).count();
    let diag_total: usize = units.values().map(|u| u.diagnostics.len()).sum();
    if diag_total > 0 {
        checks.push(DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Unit Parsing".to_string(),
            status: HealthStatus::Warn,
            message: format!(
                "{diag_total} diagnostic(s) across {diag_units} unit(s)"
            ),
            next_command: Some("lens sys inspect --format json".to_string()),
            recommendation: Some(
                "Inspect the flagged units; unreadable or malformed files weaken the dependency analysis."
                    .to_string(),
            ),
        });
    }
    checks
}

pub fn check_environment(root: Option<&Path>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    let preload = root
        .map(|r| r.join("etc/ld.so.preload"))
        .unwrap_or_else(|| PathBuf::from("/etc/ld.so.preload"));
    let preload_label = preload.display().to_string();
    if preload.exists() {
        match lens_core::read_policy::read_to_string(&preload) {
            Ok(content) => {
                let entries: Vec<&str> = content
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .collect();
                if !entries.is_empty() {
                    checks.push(DoctorCheck {
                        category: "Security".to_string(),
                        name: "ld.so.preload Verification".to_string(),
                        status: HealthStatus::Warn,
                        message: format!(
                            "{preload_label} is present with entries: {}",
                            entries.join(", ")
                        ),
                        next_command: Some(format!("lens abi inspect {}", entries[0])),
                        recommendation: Some(
                            "Verify that preloaded shared libraries are intentional and trusted."
                                .to_string(),
                        ),
                    });
                } else {
                    checks.push(DoctorCheck {
                        category: "Security".to_string(),
                        name: "ld.so.preload Verification".to_string(),
                        status: HealthStatus::Pass,
                        message: format!("{preload_label} is empty"),
                        next_command: None,
                        recommendation: None,
                    });
                }
            }
            Err(e) => {
                checks.push(DoctorCheck {
                    category: "Security".to_string(),
                    name: "ld.so.preload Verification".to_string(),
                    status: HealthStatus::Warn,
                    message: format!("Could not read {preload_label}: {e}"),
                    next_command: None,
                    recommendation: Some(
                        "Inspect /etc/ld.so.preload with elevated permissions.".to_string(),
                    ),
                });
            }
        }
    } else {
        checks.push(DoctorCheck {
            category: "Security".to_string(),
            name: "ld.so.preload Verification".to_string(),
            status: HealthStatus::Pass,
            message: format!("{preload_label} not present (clean system default)"),
            next_command: None,
            recommendation: None,
        });
    }

    if root.is_some() {
        // PATH reflects the invoking process, not the inspected root —
        // reporting it under --root would attribute host state to the
        // image.
        checks.push(DoctorCheck {
            category: "Environment".to_string(),
            name: "PATH Sanity".to_string(),
            status: HealthStatus::Skip,
            message: "PATH check reflects the invoking process; skipped under --root".to_string(),
            next_command: None,
            recommendation: None,
        });
    } else if let Ok(path_var) = env::var("PATH") {
        checks.push(check_path_value(&path_var));
    }

    checks
}

/// Grade a `PATH` value: a `.` or empty or relative entry means the current
/// directory shadows the search path (spoofing risk), and an existing
/// directory that is world-writable (`mode & 0o002`) lets any user
/// plant binaries — both Warn. Factored out so the rule is testable
/// without touching the real environment.
pub fn check_path_value(path_var: &str) -> DoctorCheck {
    use std::os::unix::fs::PermissionsExt;
    let mut suspicious = Vec::new();
    for p in path_var.split(':') {
        if p.is_empty() || p == "." {
            suspicious.push("Current directory '.' or empty entry in PATH".to_string());
        } else if !p.starts_with('/') {
            suspicious.push(format!("Relative directory '{p}' in PATH"));
        } else if let Ok(meta) = fs::metadata(p) {
            if meta.permissions().mode() & 0o002 != 0 {
                suspicious.push(format!("World-writable directory {p} in PATH"));
            }
        }
    }
    if !suspicious.is_empty() {
        DoctorCheck {
            category: "Environment".to_string(),
            name: "PATH Sanity".to_string(),
            status: HealthStatus::Warn,
            message: format!(
                "Suspicious entries found in PATH: {}",
                suspicious.join("; ")
            ),
            next_command: None,
            recommendation: Some(
                "Remove relative, empty, or world-writable entries from PATH to prevent binary spoofing."
                    .to_string(),
            ),
        }
    } else {
        DoctorCheck {
            category: "Environment".to_string(),
            name: "PATH Sanity".to_string(),
            status: HealthStatus::Pass,
            message: format!(
                "PATH contains {} search directories without relative entries",
                path_var.split(':').count()
            ),
            next_command: None,
            recommendation: None,
        }
    }
}

pub fn run_doctor(
    root: Option<&Path>,
    procfs: Option<&Path>,
    sys_dir: Option<&Path>,
) -> DoctorReport {
    let mut checks = Vec::new();

    checks.extend(check_storage(root, procfs));

    // An explicit --procfs always wins; under --root the procfs is
    // <root>/proc, and its absence is a Skip — never a read of the host
    // /proc that would attribute host sockets to the image.
    match (root, procfs) {
        (Some(r), None) => {
            let proc_dir = r.join("proc");
            if proc_dir.exists() {
                checks.extend(check_network(Some(&proc_dir)));
            } else {
                checks.push(DoctorCheck {
                    category: "Network".to_string(),
                    name: "Network".to_string(),
                    status: HealthStatus::Skip,
                    message: format!("{} not present; network checks skipped", proc_dir.display()),
                    next_command: None,
                    recommendation: None,
                });
            }
        }
        (_, p) => checks.extend(check_network(p)),
    }

    checks.extend(check_services(root, sys_dir));
    checks.extend(check_environment(root));

    let mut passed = 0;
    let mut warnings = 0;
    let mut failures = 0;
    let mut skipped = 0;

    for c in &checks {
        match c.status {
            HealthStatus::Pass => passed += 1,
            HealthStatus::Warn => warnings += 1,
            HealthStatus::Fail => failures += 1,
            HealthStatus::Skip => skipped += 1,
        }
    }

    let overall_status = if failures > 0 {
        HealthStatus::Fail
    } else if warnings > 0 {
        HealthStatus::Warn
    } else {
        HealthStatus::Pass
    };

    let summary = DoctorSummary {
        total: checks.len(),
        passed,
        warnings,
        failures,
        skipped,
    };

    DoctorReport {
        schema_version: "lens.doctor/v1".to_string(),
        overall_status,
        checks,
        summary,
    }
}

/// Validate the explicit override paths, run all checks, and wrap the
/// report in a [`Report`] whose findings mirror each Warn/Fail check
/// (code `doctor.<category lowercase>`) — shared by the CLI, where
/// `--fail-on` picks the actionable threshold, and the MCP tool.
pub fn run(
    root: Option<&Path>,
    procfs: Option<&Path>,
    systemd_dir: Option<&Path>,
) -> Result<Report<DoctorReport>> {
    // An explicitly passed path that does not exist is a usage error,
    // not a healthy-looking "non-systemd environment" report.
    for (flag, dir) in [
        ("--root", &root),
        ("--procfs", &procfs),
        ("--systemd-dir", &systemd_dir),
    ] {
        if let Some(p) = dir {
            if !p.exists() {
                return Err(LensError::Usage {
                    message: format!("{flag} path {:?} does not exist", p),
                });
            }
        }
    }

    let data = run_doctor(root, procfs, systemd_dir);
    let findings = data
        .checks
        .iter()
        .filter_map(|c| {
            let severity = match c.status {
                HealthStatus::Warn => Severity::Warn,
                HealthStatus::Fail => Severity::Fail,
                _ => return None,
            };
            Some(crate::Finding {
                severity,
                code: format!("doctor.{}", c.category.to_lowercase()),
                message: format!("{}: {}", c.name, c.message),
            })
        })
        .collect();
    let mut report = Report::new(data);
    report.findings = findings;
    Ok(report)
}

/// One check whose health status changed between two doctor runs.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheckChange {
    pub category: String,
    pub name: String,
    pub from: HealthStatus,
    pub to: HealthStatus,
    pub message: String,
}

/// Check-level diff of two doctor reports, keyed by (category, name).
#[derive(Debug, Clone, Serialize)]
pub struct DoctorDiff {
    pub schema: String,
    pub regressed: Vec<DoctorCheckChange>,
    pub improved: Vec<DoctorCheckChange>,
    /// "category/name" of checks present only in the candidate.
    pub added: Vec<String>,
    /// "category/name" of checks present only in the baseline.
    pub removed: Vec<String>,
}

/// Severity rank for diffing: Skip counts as Pass (it carries no
/// verdict), Warn below Fail.
fn status_rank(s: HealthStatus) -> u8 {
    match s {
        HealthStatus::Pass | HealthStatus::Skip => 0,
        HealthStatus::Warn => 1,
        HealthStatus::Fail => 2,
    }
}

/// Diff two doctor reports. Worsening to Fail → Fail `doctor.regressed`,
/// worsening to Warn → Warn `doctor.regressed`, improvements → Info
/// `doctor.improved`, checks present on only one side → Info.
pub fn diff_reports(base: &DoctorReport, cand: &DoctorReport) -> Report<DoctorDiff> {
    let mut regressed = Vec::new();
    let mut improved = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();

    let key = |c: &DoctorCheck| format!("{}/{}", c.category, c.name);
    for c in &cand.checks {
        match base
            .checks
            .iter()
            .find(|b| b.category == c.category && b.name == c.name)
        {
            Some(b) => {
                let (from, to) = (status_rank(b.status), status_rank(c.status));
                let change = || DoctorCheckChange {
                    category: c.category.clone(),
                    name: c.name.clone(),
                    from: b.status,
                    to: c.status,
                    message: c.message.clone(),
                };
                if to > from {
                    regressed.push(change());
                } else if to < from {
                    improved.push(change());
                }
            }
            None => added.push(key(c)),
        }
    }
    for b in &base.checks {
        if !cand
            .checks
            .iter()
            .any(|c| c.category == b.category && c.name == b.name)
        {
            removed.push(key(b));
        }
    }

    let mut report = Report::new(DoctorDiff {
        schema: "lens.doctor-diff/v1".to_string(),
        regressed: regressed.clone(),
        improved: improved.clone(),
        added,
        removed,
    });
    for c in &regressed {
        report.push(
            if c.to == HealthStatus::Fail {
                Severity::Fail
            } else {
                Severity::Warn
            },
            "doctor.regressed",
            format!("{}: {:?} -> {:?}", c.name, c.from, c.to),
        );
    }
    for c in &improved {
        report.push(
            Severity::Info,
            "doctor.improved",
            format!("{}: {:?} -> {:?}", c.name, c.from, c.to),
        );
    }
    if !report.data.added.is_empty() {
        report.push(
            Severity::Info,
            "doctor.added",
            format!("{} check(s) only in candidate", report.data.added.len()),
        );
    }
    if !report.data.removed.is_empty() {
        report.push(
            Severity::Info,
            "doctor.removed",
            format!("{} check(s) only in baseline", report.data.removed.len()),
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_only_failure_reports_inode_percentage() {
        const GB: u64 = 1024 * 1024 * 1024;
        // 80% of space free, only 2% of inodes free.
        let c = evaluate_capacity(100 * GB, 80 * GB, Some(2.0), Path::new("/data"), "Data");
        assert_eq!(c.status, HealthStatus::Fail);
        assert!(
            c.message
                .starts_with("Critically low free inodes: 2.0% free"),
            "{}",
            c.message
        );
        assert!(!c.message.contains("80.0% free ("), "{}", c.message);
    }

    #[test]
    fn test_doctor_storage_check() {
        // At minimum the root check always runs; extra per-mount checks
        // depend on the host's mounts table.
        let checks = check_storage(Some(Path::new("/")), None);
        assert_eq!(checks.len(), 1);
        let check = &checks[0];
        assert_eq!(check.category, "Storage");
        assert!(check.status == HealthStatus::Pass || check.status == HealthStatus::Warn);
    }

    #[test]
    fn test_doctor_environment_check() {
        let checks = check_environment(None);
        assert!(!checks.is_empty());
        assert!(checks.iter().any(|c| c.name.contains("PATH")));
    }

    #[test]
    fn test_doctor_environment_reads_preload_under_root() {
        // --root must scope /etc/ld.so.preload to <root>/etc/ld.so.preload
        // instead of reading the host file.
        let root = tempfile::Builder::new()
            .prefix("lens-doctor-root-")
            .tempdir()
            .unwrap();
        fs::create_dir_all(root.path().join("etc")).unwrap();
        fs::write(root.path().join("etc/ld.so.preload"), "/lib/evil.so\n").unwrap();

        let checks = check_environment(Some(root.path()));
        let preload_check = checks
            .iter()
            .find(|c| c.name == "ld.so.preload Verification")
            .unwrap();
        assert_eq!(preload_check.status, HealthStatus::Warn);
        assert!(preload_check.message.contains("/lib/evil.so"));

        // A root without the file reports "not present" regardless of the
        // host's /etc/ld.so.preload state.
        let empty = tempfile::Builder::new()
            .prefix("lens-doctor-empty-")
            .tempdir()
            .unwrap();
        let checks = check_environment(Some(empty.path()));
        let preload_check = checks
            .iter()
            .find(|c| c.name == "ld.so.preload Verification")
            .unwrap();
        assert_eq!(preload_check.status, HealthStatus::Pass);
        assert!(preload_check.message.contains("not present"));
    }

    #[test]
    fn test_run_doctor_summary() {
        let report = run_doctor(Some(Path::new("/")), None, None);
        assert_eq!(report.schema_version, "lens.doctor/v1");
        assert!(report.summary.total >= 4);
        assert_eq!(
            report.summary.total,
            report.summary.passed
                + report.summary.warnings
                + report.summary.failures
                + report.summary.skipped
        );
    }

    #[test]
    fn test_run_doctor_empty_root_never_reads_host() {
        // An empty --root must not leak host evidence: network, services,
        // and PATH checks all skip; only the root-scoped preload check
        // still runs.
        let root = tempfile::Builder::new()
            .prefix("lens-doctor-fakeroot-")
            .tempdir()
            .unwrap();
        let report = run_doctor(Some(root.path()), None, None);

        let status_of = |name: &str| {
            report
                .checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("missing check {name}"))
                .status
        };
        assert_eq!(status_of("Network"), HealthStatus::Skip);
        assert_eq!(status_of("Systemd Dependency Cycles"), HealthStatus::Skip);
        assert_eq!(status_of("PATH Sanity"), HealthStatus::Skip);
        assert_eq!(status_of("ld.so.preload Verification"), HealthStatus::Pass);
        assert_eq!(report.summary.skipped, 3);
        // Skips are not findings — every non-Storage check is Pass or
        // Skip. (Storage itself statvfs's the tempdir's filesystem and
        // may legitimately Warn/Fail on a low-disk host.)
        assert!(report
            .checks
            .iter()
            .filter(|c| c.category != "Storage")
            .all(|c| matches!(c.status, HealthStatus::Pass | HealthStatus::Skip)));
        assert!(
            report
                .checks
                .iter()
                .all(|c| !c.message.contains("Total sockets")),
            "host socket data must not appear: {:?}",
            report.checks
        );
    }

    fn check(category: &str, name: &str, status: HealthStatus) -> DoctorCheck {
        DoctorCheck {
            category: category.to_string(),
            name: name.to_string(),
            status,
            message: format!("{name} message"),
            next_command: None,
            recommendation: None,
        }
    }

    fn report_of(checks: Vec<DoctorCheck>) -> DoctorReport {
        DoctorReport {
            schema_version: "lens.doctor/v1".to_string(),
            overall_status: HealthStatus::Pass,
            checks,
            summary: DoctorSummary {
                total: 0,
                passed: 0,
                warnings: 0,
                failures: 0,
                skipped: 0,
            },
        }
    }

    #[test]
    fn doctor_diff_classifies_regressed_improved_and_side_only() {
        let base = report_of(vec![
            check("Storage", "Root Disk Capacity", HealthStatus::Pass),
            check("Services", "Systemd Dependency Cycles", HealthStatus::Warn),
            check(
                "Network",
                "Wildcard Sensitive Listeners",
                HealthStatus::Pass,
            ),
            check("Environment", "Old Check", HealthStatus::Pass),
            // Skip ranks as Pass — a Skip->Warn change must count as a
            // regression, and Pass->Skip is not an improvement.
            check("Services", "Skipped Before", HealthStatus::Skip),
        ]);
        let cand = report_of(vec![
            check("Storage", "Root Disk Capacity", HealthStatus::Fail),
            check("Services", "Systemd Dependency Cycles", HealthStatus::Pass),
            check(
                "Network",
                "Wildcard Sensitive Listeners",
                HealthStatus::Warn,
            ),
            check("Environment", "Brand New", HealthStatus::Warn),
            check("Services", "Skipped Before", HealthStatus::Warn),
        ]);

        let report = diff_reports(&base, &cand);
        let diff = &report.data;
        let regressed: Vec<&str> = diff.regressed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            regressed,
            vec![
                "Root Disk Capacity",
                "Wildcard Sensitive Listeners",
                "Skipped Before"
            ]
        );
        assert_eq!(diff.improved.len(), 1);
        assert_eq!(diff.improved[0].name, "Systemd Dependency Cycles");
        assert_eq!(diff.added, vec!["Environment/Brand New"]);
        assert_eq!(diff.removed, vec!["Environment/Old Check"]);

        // Fail for the ->Fail regression, Warn for ->Warn.
        let severities: Vec<(Severity, &str)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.code.as_str()))
            .collect();
        assert!(severities.contains(&(Severity::Fail, "doctor.regressed")));
        assert!(severities.contains(&(Severity::Warn, "doctor.regressed")));
        assert!(severities.contains(&(Severity::Info, "doctor.improved")));
        assert!(report.is_actionable(Severity::Fail));
    }

    #[test]
    fn parse_mounts_filters_pseudo_and_decodes_escapes() {
        let content = "\
proc /proc proc rw,nosuid 0 0
sysfs /sys sysfs rw 0 0
/dev/sda1 / ext4 rw,relatime 0 0
overlay / overlay rw 0 0
/dev/sdb1 /mnt/data\\040disk xfs rw 0 0
cgroup2 /sys/fs/cgroup cgroup2 rw 0 0
snapfuse /snap/pkg/1 fuse.snapfuse ro 0 0
tmpfs /run tmpfs rw 0 0
";
        let mounts = parse_mounts(content);
        assert!(mounts.contains(&("/".to_string(), "ext4".to_string())));
        assert!(mounts.contains(&("/".to_string(), "overlay".to_string())));
        assert!(mounts.contains(&("/mnt/data disk".to_string(), "xfs".to_string())));
        assert_eq!(mounts.len(), 3, "{mounts:?}");
    }

    #[test]
    fn dedupe_mounts_seeds_root_fsid() {
        // A bind mount / btrfs subvolume of / shares the root fsid — it
        // must not get a second capacity check of its own.
        let mut seen: std::collections::HashSet<u64> = [42u64].into_iter().collect();
        let pairs = vec![
            ("/".to_string(), Some(42u64)),
            ("/bind-of-root".to_string(), Some(42u64)),
            ("/mnt/data".to_string(), Some(7u64)),
            ("/unknown-fs".to_string(), None),
        ];
        let kept = dedupe_mounts(&pairs, &mut seen);
        assert_eq!(
            kept,
            vec!["/mnt/data".to_string(), "/unknown-fs".to_string()]
        );

        // Without the root fsid seeded, / itself is skipped but its
        // bind-mount twin would leak through — the seeding in
        // check_storage is what prevents that second check.
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let kept = dedupe_mounts(&pairs, &mut seen);
        assert_eq!(
            kept,
            vec![
                "/bind-of-root".to_string(),
                "/mnt/data".to_string(),
                "/unknown-fs".to_string()
            ]
        );
    }

    #[test]
    fn check_path_value_flags_world_writable_and_relative_dirs() {
        // A world-writable PATH directory lets any user plant binaries.
        let dir = tempfile::Builder::new()
            .prefix("lens-path-ww-")
            .tempdir()
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let check = check_path_value(&format!("/usr/bin:{}", dir.path().display()));
        assert_eq!(check.status, HealthStatus::Warn, "{}", check.message);
        assert!(check.message.contains("writable"), "{}", check.message);

        let check = check_path_value("/usr/bin:.:/bin");
        assert_eq!(check.status, HealthStatus::Warn);
        // Trailing ':' → empty entry = implicit current directory.
        let check = check_path_value("/usr/bin:");
        assert_eq!(check.status, HealthStatus::Warn);

        let check = check_path_value("/usr/bin:/bin");
        assert_eq!(check.status, HealthStatus::Pass);
    }

    #[test]
    fn next_command_only_on_actionable_checks() {
        // Every check that renders a `-> Next:` suggestion must be
        // Warn or Fail — a PASS check never prescribes a follow-up.
        let report = run_doctor(None, None, None);
        for c in &report.checks {
            if c.next_command.is_some() {
                assert!(
                    matches!(c.status, HealthStatus::Warn | HealthStatus::Fail),
                    "{} has next_command but status {:?}",
                    c.name,
                    c.status
                );
            }
        }
    }
}
