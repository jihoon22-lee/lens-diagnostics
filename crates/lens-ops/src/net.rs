use std::path::Path;

use lens_core::Result;
use lens_net::{diff_net_reports, inspect_network, NetDiffReport, NetReport};

use crate::{json, Report, Severity};

/// Inspect sockets via procfs. `no_unix` drops unix-domain sockets.
/// Orphaned sockets are a Warn finding — they hint at evidence the
/// process table could not attribute.
pub fn inspect(proc_dir: Option<&Path>, no_unix: bool) -> Result<Report<NetReport>> {
    let mut net = inspect_network(proc_dir)?;
    if no_unix {
        if let Some(scope) = &mut net.scope {
            scope.capabilities.insert(
                "unix".into(),
                lens_core::collection::Capability {
                    state: lens_core::collection::CapabilityState::Excluded,
                    detail: Some("--no-unix".into()),
                },
            );
        }
        net.sockets.retain(|s| {
            !matches!(
                s.kind,
                lens_net::SocketKind::UnixStream | lens_net::SocketKind::UnixDgram
            )
        });
        net.refresh_summary();
    }
    let mut report = Report::new(net);
    if let Some(scope) = &report.data.scope {
        report.complete = scope.complete();
        report.diagnostics.push(format!("Network scope: {}; current network namespace only; optional absent protocols are unavailable, not zero sockets.", scope.source));
        for (name, capability) in &scope.capabilities {
            if let Some(detail) = &capability.detail {
                report.diagnostics.push(format!("{name}: {detail}"));
            }
        }
    }
    if report.data.summary.orphan_sockets > 0 {
        report.push(
            Severity::Warn,
            "net.orphan",
            format!(
                "{} orphaned socket(s) (no live process holds the inode)",
                report.data.summary.orphan_sockets
            ),
        );
    }
    Ok(report)
}

/// Diff two net snapshot JSONs (schema-checked). New listeners are Fail
/// findings — a port nobody opened before is the highest-value delta.
pub fn diff(baseline: &Path, candidate: &Path) -> Result<Report<NetDiffReport>> {
    let b: NetReport =
        json::read_json_schema(baseline, "schema_version", lens_net::REPORT_SCHEMA_V1)?;
    let c: NetReport =
        json::read_json_schema(candidate, "schema_version", lens_net::REPORT_SCHEMA_V1)?;
    Ok(diff_values(&b, &c))
}

/// Value-level classifier shared by the file diff and `bundle diff`.
pub fn diff_values(b: &NetReport, c: &NetReport) -> Report<NetDiffReport> {
    let diff = diff_net_reports(b, c);
    let (new_listeners, closed_listeners, new_conns, closed_conns, tw) = (
        diff.new_listeners.clone(),
        diff.closed_listeners.len(),
        diff.new_connections.len(),
        diff.closed_connections.len(),
        diff.time_wait_delta,
    );
    let mut report = Report::new(diff);
    if b.scope != c.scope {
        report.push(
            Severity::Warn,
            "net.scope_changed",
            "Collection scope changed; socket additions/removals may reflect visibility changes.",
        );
    }
    report.complete = b.scope.as_ref().is_none_or(|s| s.complete())
        && c.scope.as_ref().is_none_or(|s| s.complete());
    for l in &new_listeners {
        report.push(
            Severity::Fail,
            "net.new_listener",
            format!("{}:{}", l.local_address, l.local_port),
        );
    }
    if closed_listeners > 0 {
        report.push(
            Severity::Info,
            "net.closed_listeners",
            format!("{closed_listeners} listener(s) closed"),
        );
    }
    if new_conns > 0 || closed_conns > 0 {
        report.push(
            Severity::Info,
            "net.connection_delta",
            format!("+{new_conns} -{closed_conns} connection(s), TIME_WAIT {tw:+}"),
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluding_unix_sockets_also_excludes_their_findings_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("net");
        std::fs::create_dir(&net).unwrap();
        std::fs::write(net.join("tcp"), "header\n").unwrap();
        std::fs::write(net.join("unix"), "Num RefCount Protocol Flags Type St Inode Path\n0000000000000000: 00000002 00000000 00010000 0001 01 123456 /tmp/test.sock\n").unwrap();
        let unfiltered = inspect(Some(dir.path()), false).unwrap();
        assert_eq!(unfiltered.data.summary.orphan_sockets, 1);
        assert!(unfiltered.is_actionable(Severity::Warn));
        let filtered = inspect(Some(dir.path()), true).unwrap();
        assert!(filtered.data.sockets.is_empty());
        assert!(filtered.data.listening.is_empty());
        assert_eq!(filtered.data.summary.total_sockets, 0);
        assert_eq!(filtered.data.summary.unix_domain_sockets, 0);
        assert_eq!(filtered.data.summary.orphan_sockets, 0);
        assert_eq!(filtered.data.summary.owner_unknown_sockets, 0);
        assert!(!filtered.is_actionable(Severity::Warn));

        let mut report = unfiltered.data;
        report.summary.uninspectable_processes = 3;
        report.sockets.clear();
        report.refresh_summary();
        assert_eq!(report.summary.uninspectable_processes, 3);
        assert_eq!(report.summary.orphan_sockets, 0);
    }
}
