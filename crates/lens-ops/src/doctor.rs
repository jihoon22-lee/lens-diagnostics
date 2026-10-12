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

pub fn check_storage(root_path: &Path) -> DoctorCheck {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let c_path = std::ffi::CString::new(root_path.to_string_lossy().as_bytes()).unwrap_or_default();
    let res = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };

    // f_frsize can be 0 on unusual filesystems — guard to avoid a div/zero.
    if res == 0 && stat.f_blocks > 0 && stat.f_frsize > 0 {
        let total_bytes = stat.f_blocks as u64 * stat.f_frsize as u64;
        let avail_bytes = stat.f_bavail as u64 * stat.f_frsize as u64;
        let free_pct = (avail_bytes as f64 / total_bytes as f64) * 100.0;
        let avail_gb = avail_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

        if free_pct < 5.0 || avail_gb < 1.0 {
            DoctorCheck {
                category: "Storage".to_string(),
                name: "Root Disk Capacity".to_string(),
                status: HealthStatus::Fail,
                message: format!(
                    "Critically low disk space: {:.1}% free ({:.2} GB available)",
                    free_pct, avail_gb
                ),
                recommendation: Some(
                    "Clean up disk immediately using 'lens disk scan' and trash old files."
                        .to_string(),
                ),
            }
        } else if free_pct < 15.0 || avail_gb < 5.0 {
            DoctorCheck {
                category: "Storage".to_string(),
                name: "Root Disk Capacity".to_string(),
                status: HealthStatus::Warn,
                message: format!(
                    "Low disk space warning: {:.1}% free ({:.2} GB available)",
                    free_pct, avail_gb
                ),
                recommendation: Some(
                    "Consider pruning large unused logs or artifacts.".to_string(),
                ),
            }
        } else {
            DoctorCheck {
                category: "Storage".to_string(),
                name: "Root Disk Capacity".to_string(),
                status: HealthStatus::Pass,
                message: format!(
                    "Adequate disk space: {:.1}% free ({:.2} GB available)",
                    free_pct, avail_gb
                ),
                recommendation: None,
            }
        }
    } else {
        DoctorCheck {
            category: "Storage".to_string(),
            name: "Root Disk Capacity".to_string(),
            status: HealthStatus::Warn,
            message: "Unable to query filesystem statistics".to_string(),
            recommendation: None,
        }
    }
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

pub fn check_services(root: Option<&Path>, systemd_dir: Option<&Path>) -> DoctorCheck {
    let (dirs, explicit) = services_dirs(root, systemd_dir);
    if !dirs.iter().any(|d| d.exists()) {
        return DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Dependency Cycles".to_string(),
            status: HealthStatus::Skip,
            message: "No systemd unit directory to inspect; services check skipped".to_string(),
            recommendation: None,
        };
    }

    let units = match load_systemd_units(&dirs, explicit) {
        Ok(u) => u,
        Err(e) => {
            return DoctorCheck {
                category: "Services".to_string(),
                name: "Systemd Dependency Cycles".to_string(),
                status: HealthStatus::Warn,
                message: format!("Failed to load systemd units: {}", e),
                recommendation: Some(
                    "Check the unit directory for unreadable or malformed files.".to_string(),
                ),
            };
        }
    };
    let graph = OrderingGraph::build(&units);
    let cycles = graph.find_cycles();

    if !cycles.is_empty() {
        DoctorCheck {
            category: "Services".to_string(),
            name: "Systemd Dependency Cycles".to_string(),
            status: HealthStatus::Fail,
            message: format!(
                "Detected {} cyclic dependency loop(s) in systemd units",
                cycles.len()
            ),
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
            recommendation: None,
        }
    }
}

pub fn check_environment(root: Option<&Path>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    let preload = root
        .map(|r| r.join("etc/ld.so.preload"))
        .unwrap_or_else(|| PathBuf::from("/etc/ld.so.preload"));
    let preload_label = preload.display().to_string();
    if preload.exists() {
        let content = fs::read_to_string(&preload).unwrap_or_default();
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            checks.push(DoctorCheck {
                category: "Security".to_string(),
                name: "ld.so.preload Verification".to_string(),
                status: HealthStatus::Warn,
                message: format!("{preload_label} is present with entries: {trimmed}"),
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
                recommendation: None,
            });
        }
    } else {
        checks.push(DoctorCheck {
            category: "Security".to_string(),
            name: "ld.so.preload Verification".to_string(),
            status: HealthStatus::Pass,
            message: format!("{preload_label} not present (clean system default)"),
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
            recommendation: None,
        });
    } else if let Ok(path_var) = env::var("PATH") {
        let parts: Vec<&str> = path_var.split(':').collect();
        let mut suspicious = Vec::new();
        for p in &parts {
            if p.is_empty() || *p == "." {
                suspicious.push("Current directory '.' or empty entry in PATH".to_string());
            }
        }
        if !suspicious.is_empty() {
            checks.push(DoctorCheck {
                category: "Environment".to_string(),
                name: "PATH Sanity".to_string(),
                status: HealthStatus::Warn,
                message: format!(
                    "Suspicious entries found in PATH: {}",
                    suspicious.join("; ")
                ),
                recommendation: Some(
                    "Remove relative or empty entries from PATH to prevent binary spoofing."
                        .to_string(),
                ),
            });
        } else {
            checks.push(DoctorCheck {
                category: "Environment".to_string(),
                name: "PATH Sanity".to_string(),
                status: HealthStatus::Pass,
                message: format!(
                    "PATH contains {} search directories without relative entries",
                    parts.len()
                ),
                recommendation: None,
            });
        }
    }

    checks
}

pub fn run_doctor(
    root: Option<&Path>,
    procfs: Option<&Path>,
    sys_dir: Option<&Path>,
) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(check_storage(root.unwrap_or_else(|| Path::new("/"))));

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
                    recommendation: None,
                });
            }
        }
        (_, p) => checks.extend(check_network(p)),
    }

    checks.push(check_services(root, sys_dir));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_doctor_storage_check() {
        let check = check_storage(Path::new("/"));
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
}
